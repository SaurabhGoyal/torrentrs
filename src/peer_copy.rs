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
            .send(models::TorrentEvent::PeerControlChange(
                ip.clone(),
                port,
                Some(control_tx),
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
        let _ = self.stream.write(handshake_msg.as_slice())?;
        // Receive handshake ack
        let _ = self.stream.read(handshake_msg.as_mut_slice())?;
        // Verify handshake
        assert_eq!(torrent_info_hash, handshake_msg[28..48]);
        // Send unchoke message.
        let _ = self.stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 1_u8])?;
        // Send interested message.
        let _ = self.stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 2_u8])?;
        let peer_state = models::PeerState {
            handshake: true,
            choked: true,
            interested: true,
            bitfield: None,
        };
        let peer_active_conn = PeerActiveConnection {
            peer_conn: self,
            peer_state: Arc::new(Mutex::new(peer_state)),
            piece_count,
        };
        peer_active_conn.send_state_change_event();
        Ok(peer_active_conn)
    }
}

impl PeerActiveConnection {
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
        self.log("listener started");
        loop {
            let read_bytes = stream.read(&mut len_buf)?;
            if read_bytes > 0 {
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > 0 && len < max_size {
                    let mut data_buf: Vec<u8> = vec![0_u8; len];
                    let rb = stream.read(data_buf.as_mut_slice()).unwrap();
                    if rb == 0 {
                        continue;
                    }
                    self.log(format!("Message type - {}", data_buf[0]).as_str());
                    match data_buf[0] {
                        0_u8 => self.mark_choked(true),
                        1_u8 => self.mark_choked(false),
                        4_u8 => {
                            let mut index_buf = [0_u8; 4];
                            index_buf.copy_from_slice(&data_buf[1..5]);
                            let index = u32::from_be_bytes(index_buf) as usize;
                            self.set_peer_bitfield_index(index);
                        }
                        5_u8 => {
                            let bitfield_bytes = (self.piece_count + 7) / 8;
                            assert_eq!(len - 1, bitfield_bytes);
                            let mut bitfield = vec![false; self.piece_count];
                            let mut index = 0;
                            for byte in &data_buf[1..bitfield_bytes] {
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
                        7_u8 => {
                            let mut index_buf = [0_u8; 4];
                            index_buf.copy_from_slice(&data_buf[1..5]);
                            let index = u32::from_be_bytes(index_buf) as usize;
                            index_buf.copy_from_slice(&data_buf[5..9]);
                            let offset = u32::from_be_bytes(index_buf) as usize;
                            self.peer_conn
                                .event_tx
                                .lock()
                                .unwrap()
                                .send(models::TorrentEvent::Block(
                                    index,
                                    offset,
                                    data_buf[9..].to_owned(),
                                ))
                                .unwrap();
                        }
                        _ => {}
                    };
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
                    self.request(&mut stream, index, begin, length)?
                }
                models::PeerControlCommand::PieceBlockCancel(index, begin, length) => {
                    self.cancel(&mut stream, index, begin, length)?
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

    fn send_state_change_event(&self) {
        self.peer_conn
            .event_tx
            .lock()
            .unwrap()
            .send(models::TorrentEvent::PeerStateChange(
                self.peer_conn.ip.clone(),
                self.peer_conn.port,
                Some(self.peer_state.lock().unwrap().clone()),
            ))
            .unwrap();
    }

    fn mark_choked(&self, choked: bool) {
        self.peer_state.lock().unwrap().choked = choked;
        self.send_state_change_event();
    }

    fn set_peer_bitfield_index(&self, index: usize) {
        if self.peer_state.lock().unwrap().bitfield.is_none() {
            self.peer_state.lock().unwrap().bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state.lock().unwrap().bitfield.as_mut().unwrap()[index] = true;
        self.send_state_change_event();
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
        self.send_state_change_event();
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
        let _wb = stream.write(&msg)?;
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
        let _wb = stream.write(&msg)?;
        Ok(())
    }
}
