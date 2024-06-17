use rand::RngCore;
use std::{
    fs,
    io::{self, Read},
};

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

    pub async fn add_torrent(&mut self, file_path: &str) -> Result<models::Torrent, ClientError> {
        // Read file
        let mut file = fs::OpenOptions::new().read(true).open(file_path).unwrap();
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf).unwrap();
        // Parse metadata_info
        let metainfo = bencode::decode_metainfo(buf.as_slice());
        // Add torrent;
        let tor = torrent::add(metainfo, &self.config)
            .await
            .expect("error in adding torrent from metainfo");
        Ok(tor)
    }

    pub async fn start_torrent(
        &mut self,
        tor: models::Torrent,
    ) -> Result<models::Torrent, io::Error> {
        let mut handles = vec![];
        for p in tor.peers.iter() {
            let ip = p.ip.clone();
            let port = p.port;
            let info_hash = tor.meta.info_hash.clone();
            let peer_id = self.config.peer_id.clone();
            // let pieces: Vec<(usize, u32)> = tor
            //     .meta
            //     .pieces
            //     .iter()
            //     .enumerate()
            //     .map(|(i, p)| (i, p.length))
            //     .collect();
            handles.push(tokio::spawn(async move {
                if let Ok(mut pc) = peer::PeerConnection::new(ip.as_str(), port).await {
                    pc.handshake(&info_hash, &peer_id).await.unwrap();
                    pc.mark_host_interested(true).await.unwrap();
                }
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        Ok(tor)
    }
}
