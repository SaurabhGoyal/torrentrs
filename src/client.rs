use rand::RngCore;
use std::{fs, io::Read};

use crate::{bencode, models, torrent};

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
        rand::thread_rng().fill_bytes(&mut peer_id);
        Client {
            config: models::ClientConfig { peer_id },
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
        let torrent =
            torrent::add(metainfo, &self.config).expect("error in adding torrent from metainfo");
        Ok(torrent)
    }
}
