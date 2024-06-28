use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
};

use crate::models;

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0_u8; 8];

#[derive(Debug)]
pub struct PeerConnection {
    ip: String,
    port: u16,
    stream: TcpStream,
    event_tx: Arc<Mutex<Sender<models::TorrentEvent>>>,
    control_rx: Arc<Mutex<Receiver<models::PeerControlCommand>>>,
}

#[derive(Debug)]
pub struct PeerActiveConnection {
    peer_conn: PeerConnection,
    peer_state: Arc<Mutex<models::PeerState>>,
    piece_count: usize,
}

impl PeerConnection {
    pub fn new(ip: String, port: u16, event_tx: Arc<Mutex<Sender<models::TorrentEvent>>>) -> Self {
        let stream =
            TcpStream::connect(SocketAddr::new(ip.parse::<IpAddr>().unwrap(), port)).unwrap();
        let (control_tx, control_rx) = channel::<models::PeerControlCommand>();
        event_tx
            .lock()
            .unwrap()
            .send(models::TorrentEvent::Peer(
                ip.clone(),
                port,
                models::PeerEvent::Control(control_tx),
            ))
            .unwrap();
        PeerConnection {
            ip: ip.clone(),
            port,
            stream,
            event_tx,
            control_rx: Arc::new(Mutex::new(control_rx)),
        }
    }

    pub fn activate(
        mut self,
        torrent_info_hash: [u8; models::INFO_HASH_BYTE_LEN],
        client_peer_id: [u8; models::PEER_ID_BYTE_LEN],
        piece_count: usize,
    ) -> Result<PeerActiveConnection, io::Error> {
        let mut handshake_msg = [
            HANSHAKE_PSTR_LEN,
            HANSHAKE_PSTR,
            HANSHAKE_RESTRICTED,
            torrent_info_hash.as_slice(),
            client_peer_id.as_slice(),
        ]
        .concat();
        // Send handshake
        let _ = self.stream.write(handshake_msg.as_slice()).unwrap();
        // Receive handshake ack
        let _ = self.stream.read(handshake_msg.as_mut_slice()).unwrap();
        // Verify handshake
        assert_eq!(torrent_info_hash, handshake_msg[28..48]);
        // Send unchoke message.
        let _ = self.stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 1_u8]).unwrap();
        // Send interested message.
        let _ = self.stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 2_u8]).unwrap();
        Ok(PeerActiveConnection::new(
            self,
            models::PeerState {
                handshake: true,
                choked: true,
                interested: true,
                bitfield: None,
            },
            piece_count,
        ))
    }
}

impl PeerActiveConnection {
    pub fn new(
        peer_conn: PeerConnection,
        peer_state: models::PeerState,
        piece_count: usize,
    ) -> Self {
        let peer_active_conn = PeerActiveConnection {
            peer_conn,
            peer_state: Arc::new(Mutex::new(peer_state.clone())),
            piece_count,
        };
        peer_active_conn.send_state_event(models::PeerStateEvent::Init(peer_state));
        peer_active_conn
    }

    pub fn start_exchange(self) -> Result<Arc<Self>, io::Error> {
        let listener_stream = self.peer_conn.stream.try_clone().unwrap();
        let writer_stream = self.peer_conn.stream.try_clone().unwrap();
        let self_arc = Arc::new(self);
        let listener = self_arc.clone();
        let listener_handle = thread::spawn(move || {
            listener.start_listener(listener_stream).unwrap();
        });
        let writer = self_arc.clone();
        let writer_handle = thread::spawn(move || {
            writer.start_writer(writer_stream).unwrap();
        });
        listener_handle.join().unwrap();
        writer_handle.join().unwrap();
        Ok(self_arc)
    }

    fn start_listener(&self, mut stream: TcpStream) -> Result<(), io::Error> {
        let mut len_buf = [0_u8; 4];
        let max_size = 1 << 17;
        let bitfield_byte_count = (self.piece_count + 7) / 8;
        self.log("listener started");
        loop {
            stream.read_exact(&mut len_buf).unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > 0 && len < max_size {
                match len {
                    0 => {
                        // keep-alive message
                    }
                    1.. => {
                        let mut msg_type = [0_u8];
                        stream.read_exact(&mut msg_type).unwrap();
                        match (len - 1, msg_type[0]) {
                            (0, 0_u8) => self.mark_choked(true),
                            (0, 1_u8) => self.mark_choked(false),
                            (4, 4_u8) => {
                                let mut index_buf = [0_u8; 4];
                                stream.read_exact(&mut index_buf).unwrap();
                                let index = u32::from_be_bytes(index_buf) as usize;
                                self.set_peer_bitfield_index(index);
                            }
                            (_, 5_u8) => {
                                if len - 1 != bitfield_byte_count {
                                    self.log(
                                        format!(
                                            "extra bf msg size - ex - {} actu - {}, reading only till required bit-count",
                                            bitfield_byte_count,
                                            len - 1
                                        )
                                        .as_str(),
                                    );
                                }
                                let mut bitfield_buf = vec![0_u8; len - 1];
                                stream.read_exact(&mut bitfield_buf).unwrap();
                                let mut bitfield = vec![false; self.piece_count];
                                let mut index = 0;
                                for byte in &bitfield_buf[..bitfield_byte_count] {
                                    for i in 0..8 {
                                        let bit = byte >> (7 - i) & 1;
                                        bitfield[index] = bit == 1;
                                        index += 1;
                                        if index >= self.piece_count {
                                            break;
                                        }
                                    }
                                }
                                self.set_peer_bitfield(bitfield);
                            }
                            (8.., 7_u8) => {
                                let msg_len = len - 1;
                                let mut msg_buf = vec![0_u8; msg_len];
                                stream.read_exact(&mut msg_buf).unwrap();
                                let mut index_buf = [0_u8; 4];
                                let mut begin_buf = [0_u8; 4];
                                index_buf.copy_from_slice(&msg_buf[0..4]);
                                begin_buf.copy_from_slice(&msg_buf[4..8]);
                                let piece_index = u32::from_be_bytes(index_buf) as usize;
                                let begin = u32::from_be_bytes(begin_buf) as usize;
                                let data = msg_buf[8..msg_len].to_owned();
                                self.peer_conn
                                    .event_tx
                                    .lock()
                                    .unwrap()
                                    .send(models::TorrentEvent::Block(piece_index, begin, data))
                                    .unwrap();
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn start_writer(&self, mut stream: TcpStream) -> Result<(), io::Error> {
        self.log("writer started");
        while let Ok(cmd) = self.peer_conn.control_rx.lock().unwrap().recv() {
            self.log(format!("received cmd: [{:?}]", cmd).as_str());
            match cmd {
                models::PeerControlCommand::PieceBlockRequest(index, begin, length) => {
                    self.request(&mut stream, index, begin, length).unwrap();
                }
                models::PeerControlCommand::PieceBlockCancel(index, begin, length) => {
                    self.cancel(&mut stream, index, begin, length).unwrap();
                }
                models::PeerControlCommand::Shutdown => {
                    stream.shutdown(std::net::Shutdown::Both).unwrap();
                }
            }
            self.log(format!("executed cmd: [{:?}]", cmd).as_str());
        }
        Ok(())
    }

    fn log(&self, msg: &str) {
        println!("{}:{}: {msg}", self.peer_conn.ip, self.peer_conn.port);
    }

    fn send_state_event(&self, event: models::PeerStateEvent) {
        self.peer_conn
            .event_tx
            .lock()
            .unwrap()
            .send(models::TorrentEvent::Peer(
                self.peer_conn.ip.clone(),
                self.peer_conn.port,
                models::PeerEvent::State(event),
            ))
            .unwrap();
    }

    fn mark_choked(&self, choked: bool) {
        self.peer_state.lock().unwrap().choked = choked;
        self.send_state_event(models::PeerStateEvent::FieldChoked(choked));
    }

    fn set_peer_bitfield_index(&self, index: usize) {
        if self.peer_state.lock().unwrap().bitfield.is_none() {
            self.peer_state.lock().unwrap().bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state.lock().unwrap().bitfield.as_mut().unwrap()[index] = true;
        self.send_state_event(models::PeerStateEvent::FieldHave(index));
    }

    fn set_peer_bitfield(&self, bitfield: Vec<bool>) {
        if self.peer_state.lock().unwrap().bitfield.is_none() {
            self.peer_state.lock().unwrap().bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state
            .lock()
            .unwrap()
            .bitfield
            .as_mut()
            .unwrap()
            .copy_from_slice(&bitfield[..self.piece_count]);
        self.send_state_event(models::PeerStateEvent::FieldBitfield(bitfield.clone()));
    }

    fn request(
        &self,
        stream: &mut TcpStream,
        index: usize,
        begin: usize,
        length: usize,
    ) -> Result<(), io::Error> {
        let mut msg = [0_u8; 17];
        msg[0..4].copy_from_slice(&[0_u8, 0_u8, 0_u8, 0xd_u8]);
        msg[4..5].copy_from_slice(&[6_u8]);
        msg[5..9].copy_from_slice(&(index as u32).to_be_bytes());
        msg[9..13].copy_from_slice(&(begin as u32).to_be_bytes());
        msg[13..17].copy_from_slice(&(length as u32).to_be_bytes());
        let _wb = stream.write(&msg).unwrap();
        Ok(())
    }

    fn cancel(
        &self,
        stream: &mut TcpStream,
        index: usize,
        begin: usize,
        length: usize,
    ) -> Result<(), io::Error> {
        let mut msg = [0_u8; 17];
        msg[0..4].copy_from_slice(&[0_u8, 0_u8, 0_u8, 0xd_u8]);
        msg[4..5].copy_from_slice(&[8_u8]);
        msg[5..9].copy_from_slice(&(index as u32).to_be_bytes());
        msg[9..13].copy_from_slice(&(begin as u32).to_be_bytes());
        msg[13..17].copy_from_slice(&(length as u32).to_be_bytes());
        let _wb = stream.write(&msg).unwrap();
        Ok(())
    }
}
