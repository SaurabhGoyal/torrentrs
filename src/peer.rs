use std::{
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent Protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0; 8];

pub trait Peer {
    async fn handshake(
        &mut self,
        torrent_info_hash: &str,
        client_peer_id: &str,
    ) -> Result<(), io::Error>;
    async fn mark_host_interested(&mut self, interested: bool) -> Result<(), io::Error>;
    async fn mark_peer_choked(&mut self, choked: bool) -> Result<(), io::Error>;
    async fn request(&mut self, index: u32, begin: u32, length: u32) -> Result<(), io::Error>;
    async fn cancel(&mut self, index: u32, begin: u32, length: u32) -> Result<(), io::Error>;
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
    pub async fn new(ip: &str, port: u16) -> Result<PeerConnection, io::Error> {
        let conn = TcpStream::connect_timeout(
            &SocketAddr::new(ip.parse::<IpAddr>().unwrap(), port),
            Duration::from_secs(10),
        )?;
        Ok(PeerConnection {
            conn,
            host: PeerClientState {
                handshake_confirmed: false,
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
    async fn mark_host_interested(&mut self, interested: bool) -> Result<(), io::Error> {
        let _ = self
            .conn
            .write(&[0, 0, 0, 1, if interested { 2 } else { 3 }])?;
        self.host.interested = interested;
        println!("Peer interest acked: {}", self.conn.peer_addr().unwrap());
        Ok(())
    }

    async fn mark_peer_choked(&mut self, choked: bool) -> Result<(), io::Error> {
        let _ = self.conn.write(&[0, 0, 0, 1, if choked { 0 } else { 1 }])?;
        self.peer.choked = choked;
        Ok(())
    }

    async fn request(&mut self, index: u32, begin: u32, length: u32) -> Result<(), io::Error> {
        let msg = [
            &[0, 0, 1, 3],
            &6_u32.to_be_bytes()[..],
            &index.to_be_bytes()[..],
            &begin.to_be_bytes()[..],
            &length.to_be_bytes()[..],
        ]
        .concat();
        let _ = self.conn.write(msg.as_slice())?;
        println!("Piece with ({}, {}, {}) requested", index, begin, length);
        Ok(())
    }

    async fn cancel(&mut self, index: u32, begin: u32, length: u32) -> Result<(), io::Error> {
        let msg = [
            &[0, 0, 1, 3],
            &8_u32.to_be_bytes()[..],
            &index.to_be_bytes()[..],
            &begin.to_be_bytes()[..],
            &length.to_be_bytes()[..],
        ]
        .concat();
        let _ = self.conn.write(msg.as_slice())?;
        println!(
            "Request for piece with ({}, {}, {}) cancelled",
            index, begin, length
        );
        Ok(())
    }

    async fn handshake(
        &mut self,
        torrent_info_hash: &str,
        client_peer_id: &str,
    ) -> Result<(), io::Error> {
        let handshake_msg = [
            HANSHAKE_PSTR_LEN,
            HANSHAKE_PSTR,
            HANSHAKE_RESTRICTED,
            torrent_info_hash.as_bytes(),
            client_peer_id.as_bytes(),
        ]
        .concat();
        let _ = self.conn.write(handshake_msg.as_slice())?;
        Ok(())
    }
}
