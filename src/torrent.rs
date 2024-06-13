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

    // Workaround for issue with binary data - https://github.com/servo/rust-url/issues/219
    let mut url = reqwest::Url::parse(meta.tracker.as_str()).unwrap();
    url.set_query(Some(
        format!(
            "info_hash={}&peer_id={}",
            meta.info_hash,
            utils::bytes_to_hex_encoding(&peer_id)
        )
        .as_str(),
    ));
    let req = client
        .get(url)
        .query(&[("port", 6883)])
        .query(&[("uploaded", 0)])
        .query(&[("downloaded", 0)])
        .query(&[("left", meta.files[0].length)])
        .build()
        .unwrap();
    let res = client.execute(req).unwrap().text().unwrap();
    Err(TorrentError::Unknown)
}
