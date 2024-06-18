use std::{
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    sync::{mpsc::Receiver, Arc, Mutex},
    thread,
    time::Duration,
};

use rand::seq::index;

use crate::models;

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0; 8];

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
    bitfield: Vec<bool>,
}

#[derive(Debug)]
pub enum PeerCommand {
    PeerChoke(bool),
    PieceRequest(u32, u32, u32),
    PieceCancel(u32, u32, u32),
}

#[derive(Debug)]
pub struct PeerActiveConnection {
    peer: Peer,
    stream: Arc<Mutex<TcpStream>>,
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
        let stream = TcpStream::connect_timeout(
            &SocketAddr::new(self.ip.parse::<IpAddr>().unwrap(), self.port),
            Duration::from_secs(180),
        )?;
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
        let _ = self.stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 2_u8])?;
        Ok(PeerActiveConnection {
            peer: self.peer,
            torrent_info_hash,
            client_peer_id,
            stream: Arc::new(Mutex::new(self.stream)),
            host_state: Arc::new(Mutex::new(PeerClientState {
                choked: true,
                interested: true,
                bitfield: vec![false; piece_count],
            })),
            peer_state: Arc::new(Mutex::new(PeerClientState {
                choked: false,
                interested: false,
                bitfield: vec![false; piece_count],
            })),
            piece_count,
        })
    }
}

impl PeerActiveConnection {
    pub fn start_listener(&self) -> Result<(), io::Error> {
        let mut len_buf = [0_u8; 4];
        let max_size = 1 << 16;
        self.log("listener started");
        loop {
            self.read(&mut len_buf)?;
            let len = u32::from_be_bytes(len_buf) as usize;
            self.log(format!("Read - {:?} ({:?})", len_buf, len).as_str());
            if len > 0 && len < max_size {
                let mut data_buf: Vec<u8> = vec![0_u8; len];
                self.read(data_buf.as_mut_slice()).unwrap();
                println!("Message type - {}", data_buf[0]);
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
                    _ => {}
                };
            }
            thread::sleep(Duration::from_millis(1000));
        }
        Ok(())
    }

    fn log(&self, msg: &str) {
        println!(
            "{}: {msg}",
            self.stream.lock().unwrap().peer_addr().unwrap()
        );
    }

    fn read(&self, buf: &mut [u8]) -> Result<(), io::Error> {
        let _ = self.stream.lock().unwrap().read(buf)?;
        Ok(())
    }

    fn mark_host_choked(&self, choked: bool) {
        self.host_state.lock().unwrap().choked = choked;
    }

    fn mark_peer_interested(&self, interested: bool) {
        self.peer_state.lock().unwrap().interested = interested;
    }

    fn set_peer_bitfield_index(&self, index: usize) {
        self.peer_state.lock().unwrap().bitfield[index] = true;
    }

    fn set_peer_bitfield(&self, bitfield: Vec<bool>) {
        self.peer_state.lock().unwrap().bitfield[..self.piece_count]
            .copy_from_slice(&bitfield[..self.piece_count]);
    }
}
