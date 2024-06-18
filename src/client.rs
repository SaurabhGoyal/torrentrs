use rand::RngCore;
use std::{
    fs,
    io::{self, Read},
    sync::Arc,
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
            config: models::ClientConfig { peer_id },
        }
    }

    pub async fn add_torrent(
        &mut self,
        file_path: &str,
    ) -> Result<models::TorrentInfo, ClientError> {
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
        tor: models::TorrentInfo,
    ) -> Result<models::TorrentInfo, io::Error> {
        let mut handles = vec![];
        let pieces: Vec<(u32, u32)> = tor
            .meta
            .pieces
            .iter()
            .enumerate()
            .map(|(i, p)| (i as u32, p.length))
            .collect();
        for p in tor.peers.iter() {
            let ip = p.ip.clone();
            let port = p.port;
            let torrent_info_hash = tor.meta.info_hash;
            let client_peer_id = self.config.peer_id;
            if ip == "116.88.97.233" {
                handles.push(tokio::spawn(async move {
                    let peer_obj = peer::Peer::new([0; 20], ip, port);
                    let peer_conn = peer_obj.connect()?;
                    let peer_active_conn =
                        peer_conn.activate(torrent_info_hash, client_peer_id, pieces.len())?;
                    let peer_active_conn = Arc::new(peer_active_conn);
                    let peer_active_conn_listener = peer_active_conn.clone();
                    let listener_handle = tokio::spawn(async move {
                        peer_active_conn_listener.start_listener()?;
                        Ok::<(), io::Error>(())
                    });
                    listener_handle.await??;
                    Ok::<(), io::Error>(())
                }));
                break;
            }
        }
        for handle in handles {
            if let Err(e) = handle.await {
                println!("Err - {e}");
            }
        }
        Ok(tor)
    }
}
