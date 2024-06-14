// Piece hash byte length
pub const PIECE_HASH_BYTE_LEN: usize = 20;
pub const PEER_ID_BYTE_LEN: usize = 20;

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
pub struct Peer {
    pub ip: String,
    pub port: u16,
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
    pub meta: MetaInfo,
    pub peers: Vec<Peer>,
}

#[derive(Debug)]
pub struct ClientConfig {
    pub peer_id: String,
}
