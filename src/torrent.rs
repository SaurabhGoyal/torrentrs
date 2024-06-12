// Piece hash byte length
pub const PIECE_HASH_BYTE_LEN: usize = 20;

#[derive(Debug)]
enum TorrentError {}

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

pub fn add(meta: MetaInfo) -> Torrent {
    todo!()
}
