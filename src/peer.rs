use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{
        mpsc::{Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use crate::{models, writer};

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0_u8, 0_u8, 0_u8, 0_u8, 0_u8, 16_u8, 0_u8, 0_u8];

#[derive(Debug)]
pub struct Peer {
    id: [u8; models::PEER_ID_BYTE_LEN],
    ip: String,
    port: u16,
}

#[derive(Debug)]
pub struct PeerConnection {
    peer: Peer,
    stream: TcpStream,
}

#[derive(Debug)]
pub struct PeerClientState {
    choked: bool,
    interested: bool,
    bitfield: Option<Vec<bool>>,
}

#[derive(Debug)]
pub enum PeerCommand {
    PeerChoke(bool),
    HostInterest(bool),
    PieceBlockRequest(usize, usize, usize),
    PieceBlockCancel(usize, usize, usize),
}

#[derive(Debug)]
pub struct PeerActiveConnection {
    peer_conn: PeerConnection,
    host_state: Arc<Mutex<PeerClientState>>,
    peer_state: Arc<Mutex<PeerClientState>>,
    torrent_info_hash: [u8; models::INFO_HASH_BYTE_LEN],
    client_peer_id: [u8; models::PEER_ID_BYTE_LEN],
    piece_count: usize,
}

impl Peer {
    pub fn new(id: [u8; models::PEER_ID_BYTE_LEN], ip: String, port: u16) -> Self {
        Self { id, ip, port }
    }

    pub fn connect(self) -> Result<PeerConnection, io::Error> {
        let stream = TcpStream::connect(SocketAddr::new(
            self.ip.parse::<IpAddr>().unwrap(),
            self.port,
        ))?;
        stream.set_nodelay(true)?;
        Ok(PeerConnection { peer: self, stream })
    }
}

impl PeerConnection {
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
        Ok(PeerActiveConnection {
            peer_conn: self,
            torrent_info_hash,
            client_peer_id,
            host_state: Arc::new(Mutex::new(PeerClientState {
                choked: true,
                interested: true,
                bitfield: None,
            })),
            peer_state: Arc::new(Mutex::new(PeerClientState {
                choked: false,
                interested: false,
                bitfield: None,
            })),
            piece_count,
        })
    }
}

impl PeerActiveConnection {
    pub fn start_exchange(
        self,
        cmd_rx: Receiver<PeerCommand>,
        data_tx: Arc<Mutex<Sender<writer::Data>>>,
    ) -> Result<Arc<Self>, io::Error> {
        let listener_stream = self.peer_conn.stream.try_clone().unwrap();
        let writer_stream = self.peer_conn.stream.try_clone().unwrap();
        let self_arc = Arc::new(self);
        let listener = self_arc.clone();
        let listener_handle = thread::spawn(move || {
            listener.start_listener(listener_stream, data_tx)?;
            Ok::<(), io::Error>(())
        });
        let writer = self_arc.clone();
        let writer_handle = thread::spawn(move || {
            writer.start_writer(writer_stream, cmd_rx)?;
            Ok::<(), io::Error>(())
        });
        listener_handle.join().unwrap();
        writer_handle.join().unwrap();
        Ok(self_arc)
    }

    fn start_listener(
        &self,
        mut stream: TcpStream,
        data_tx: Arc<Mutex<Sender<writer::Data>>>,
    ) -> Result<(), io::Error> {
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
                        0_u8 => self.mark_host_choked(true),
                        1_u8 => self.mark_host_choked(false),
                        2_u8 => self.mark_peer_interested(true),
                        3_u8 => self.mark_peer_interested(false),
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
                            data_tx
                                .lock()
                                .unwrap()
                                .send(writer::Data::PieceBlock(
                                    index,
                                    offset,
                                    len - 9,
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

    fn start_writer(
        &self,
        mut stream: TcpStream,
        cmd_rx: Receiver<PeerCommand>,
    ) -> Result<(), io::Error> {
        self.log("writer started");
        while let Ok(cmd) = cmd_rx.recv() {
            self.log(format!("received cmd: [{:?}]", cmd).as_str());
            match cmd {
                PeerCommand::PeerChoke(choked) => self.mark_peer_choked(&mut stream, choked)?,
                PeerCommand::HostInterest(interested) => {
                    self.mark_host_interested(&mut stream, interested)?
                }
                PeerCommand::PieceBlockRequest(index, begin, length) => {
                    self.request(&mut stream, index, begin, length)?
                }
                PeerCommand::PieceBlockCancel(index, begin, length) => {
                    self.cancel(&mut stream, index, begin, length)?
                }
            }
            self.log(format!("executed cmd: [{:?}]", cmd).as_str());
        }
        Ok(())
    }

    fn log(&self, msg: &str) {
        println!(
            "{}:{}: {msg}",
            self.peer_conn.peer.ip, self.peer_conn.peer.port
        );
    }

    fn mark_host_choked(&self, choked: bool) {
        self.host_state.lock().unwrap().choked = choked;
    }

    fn mark_peer_interested(&self, interested: bool) {
        self.peer_state.lock().unwrap().interested = interested;
    }

    fn set_peer_bitfield_index(&self, index: usize) {
        if self.peer_state.lock().unwrap().bitfield.is_none() {
            self.peer_state.lock().unwrap().bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state.lock().unwrap().bitfield.as_mut().unwrap()[index] = true;
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
    }

    fn mark_host_interested(
        &self,
        mut stream: &TcpStream,
        interested: bool,
    ) -> Result<(), io::Error> {
        let _wb = stream.write(&[0, 0, 0, 1, if interested { 2 } else { 3 }])?;
        self.host_state.lock().unwrap().interested = interested;
        Ok(())
    }

    fn mark_peer_choked(&self, mut stream: &TcpStream, choked: bool) -> Result<(), io::Error> {
        let _wb = stream.write(&[0, 0, 0, 1, if choked { 0 } else { 1 }])?;
        self.peer_state.lock().unwrap().choked = choked;
        Ok(())
    }

    fn request(
        &self,
        mut stream: &TcpStream,
        index: usize,
        begin: usize,
        length: usize,
    ) -> Result<(), io::Error> {
        if self.host_state.lock().unwrap().choked {
            self.log(
                format!("host is choked from this peer, ignoring request for {index}").as_str(),
            );
            self.mark_host_interested(stream, true)?;
            return Ok(());
        }
        // if let Some(bitfield) = self.peer_state.lock().unwrap().bitfield.as_mut() {
        //     if !bitfield[index] {
        //         self.log(
        //             format!("piece {index} not available with peer, ignoring request").as_str(),
        //         );
        //         return Ok(());
        //     }
        // }
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
        mut stream: &TcpStream,
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
