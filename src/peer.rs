use std::{
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent Protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0; 8];

pub trait Peer {
    fn mark_host_interested(&mut self, interested: bool) -> Result<(), io::Error>;
    fn mark_peer_choked(&mut self, choked: bool) -> Result<(), io::Error>;
}

#[derive(Debug)]
pub struct PeerClientState {
    handshake_confirmed: bool,
    choked: bool,
    interested: bool,
}

#[derive(Debug)]
pub struct PeerConnection {
    conn: TcpStream,
    host: PeerClientState,
    peer: PeerClientState,
}

impl PeerConnection {
    pub fn new(
        ip: &str,
        port: u16,
        torrent_info_hash: &str,
        client_peer_id: &str,
    ) -> Result<Self, io::Error> {
        let mut conn = TcpStream::connect_timeout(
            &SocketAddr::new(ip.parse::<IpAddr>().unwrap(), port),
            Duration::from_secs(10),
        )?;
        let handshake_msg = [
            HANSHAKE_PSTR_LEN,
            HANSHAKE_PSTR,
            HANSHAKE_RESTRICTED,
            torrent_info_hash.as_bytes(),
            client_peer_id.as_bytes(),
        ]
        .concat();
        let _ = conn.write(handshake_msg.as_slice())?;
        Ok(PeerConnection {
            conn,
            host: PeerClientState {
                handshake_confirmed: true,
                choked: true,
                interested: false,
            },
            peer: PeerClientState {
                handshake_confirmed: false,
                choked: false,
                interested: false,
            },
        })
    }
}

impl Peer for PeerConnection {
    fn mark_host_interested(&mut self, interested: bool) -> Result<(), io::Error> {
        let _ = self
            .conn
            .write(&[0, 0, 0, 1, if interested { 2 } else { 3 }])?;
        self.host.interested = interested;
        println!("Peer interest acked: {}", self.conn.peer_addr().unwrap());
        Ok(())
    }

    fn mark_peer_choked(&mut self, choked: bool) -> Result<(), io::Error> {
        let _ = self.conn.write(&[0, 0, 0, 1, if choked { 0 } else { 1 }])?;
        self.peer.choked = choked;
        Ok(())
    }
}
