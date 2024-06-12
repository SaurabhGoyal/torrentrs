use std::{fs, io::Read};

use crate::{bencode, torrent};

#[derive(Debug)]
pub enum ClientError {
    Unknown,
}

pub struct Client {}

impl Client {
    pub fn new() -> Self {
        Client {}
    }

    pub fn add_torrent(&mut self, file_path: &str) -> Result<torrent::Torrent, ClientError> {
        let mut file = fs::OpenOptions::new().read(true).open(file_path).unwrap();
        let mut buf: Vec<u8> = vec![];
        let _ = file.read_to_end(&mut buf).unwrap();
        let metainfo = bencode::decode(buf.as_slice());
        println!("{:?}", metainfo);
        Err(ClientError::Unknown)
    }
}
