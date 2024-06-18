use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

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
            })),
            peer_state: Arc::new(Mutex::new(PeerClientState {
                choked: false,
                interested: false,
            })),
        })
    }
}
