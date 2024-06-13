use crate::utils;
use rand::RngCore;

// Piece hash byte length
pub const PIECE_HASH_BYTE_LEN: usize = 20;

#[derive(Debug)]
pub enum TorrentError {
    Unknown,
}

type Id = u64;

#[derive(Debug)]
pub struct File {
    pub name: String,
    pub length: u64,
}

#[derive(Debug)]
pub struct Piece {
    pub hash: [u8; PIECE_HASH_BYTE_LEN],
    pub length: u64,
}

#[derive(Debug)]
pub struct MetaInfo {
    pub info_hash: String,
    pub tracker: String,
    pub files: Vec<File>,
    pub pieces: Vec<Piece>,
}

#[derive(Debug)]
pub struct Torrent {
    id: Id,
    meta: MetaInfo,
    peers: Vec<String>,
}

pub fn add(meta: MetaInfo) -> Result<Torrent, TorrentError> {
    let mut peer_id = [0_u8; 20];
    rand::thread_rng().fill_bytes(&mut peer_id);
    let client = reqwest::blocking::Client::new();
    let req = client
        .get(meta.tracker)
        .query(&[("info_hash", urlencoding::encode(meta.info_hash.as_str()))])
        .query(&[(
            "peer_id",
            urlencoding::encode(utils::bytes_to_hex_encoding(&peer_id).as_str()),
        )])
        .query(&[("port", 6883)])
        .query(&[("uploaded", 0)])
        .query(&[("downloaded", 0)])
        .query(&[("left", meta.files[0].length)])
        .query(&[("compact", 0)])
        .build()
        .unwrap();
    println!("Req -> {:?}", req);
    let res = client.execute(req).unwrap().text().unwrap();
    println!("Res -> {res}");
    Err(TorrentError::Unknown)
}
