use rand::RngCore;
use std::{fs, io::Read, thread};

use crate::{
    bencode, models,
    peer::{self, Peer},
    torrent, utils,
};

#[derive(Debug)]
pub enum ClientError {
    Unknown,
}

pub struct Client {
    config: models::ClientConfig,
}

impl Client {
    pub fn new() -> Self {
        let mut peer_id = [0_u8; 20];
        let (clinet_info, rest) = peer_id.split_at_mut(6);
        clinet_info.copy_from_slice("RS0010".as_bytes());
        rand::thread_rng().fill_bytes(rest);
        Client {
            config: models::ClientConfig {
                peer_id: utils::bytes_to_hex_encoding(&peer_id),
            },
        }
    }

    pub fn add_torrent(&mut self, file_path: &str) -> Result<models::Torrent, ClientError> {
        // Read file
        let mut file = fs::OpenOptions::new().read(true).open(file_path).unwrap();
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf).unwrap();
        // Parse metadata_info
        let metainfo = bencode::decode_metainfo(buf.as_slice());
        // Add torrent;
        let tor =
            torrent::add(metainfo, &self.config).expect("error in adding torrent from metainfo");
        Ok(tor)
    }

    pub fn start_torrent(&mut self, tor: models::Torrent) -> Result<models::Torrent, ClientError> {
        let mut handles = vec![];
        for p in tor.peers.iter() {
            let ip = p.ip.clone();
            let port = p.port;
            let info_hash = tor.meta.info_hash.clone();
            let peer_id = self.config.peer_id.clone();
            let pieces: Vec<(usize, u32)> = tor
                .meta
                .pieces
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.length))
                .collect();
            handles.push(thread::spawn(move || {
                if let Ok(mut peer_conn) =
                    peer::PeerConnection::new(ip.as_str(), port, &info_hash, &peer_id)
                {
                    peer_conn.mark_host_interested(true).unwrap();
                    for (i, l) in pieces.iter().take(2) {
                        peer_conn.request(*i as u32, 0, *l).unwrap();
                        peer_conn.cancel(*i as u32, 0, *l).unwrap();
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        Ok(tor)
    }
}
