// Piece hash byte length
pub const INFO_HASH_BYTE_LEN: usize = 20;
pub const PIECE_HASH_BYTE_LEN: usize = 20;
pub const PEER_ID_BYTE_LEN: usize = 20;

type Id = u64;

#[derive(Debug)]
pub struct FileInfo {
    pub name: String,
    pub length: u64,
}

#[derive(Debug)]
pub struct PieceInfo {
    pub hash: [u8; PIECE_HASH_BYTE_LEN],
    pub length: u32,
}

#[derive(Debug)]
pub struct PeerInfo {
    pub ip: String,
    pub port: u16,
}

#[derive(Debug)]
pub struct MetaInfo {
    pub info_hash: [u8; INFO_HASH_BYTE_LEN],
    pub tracker: String,
    pub files: Vec<FileInfo>,
    pub pieces: Vec<PieceInfo>,
}

#[derive(Debug)]
pub struct TorrentInfo {
    pub meta: MetaInfo,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug)]
pub struct ClientConfig {
    pub peer_id: [u8; PEER_ID_BYTE_LEN],
}
