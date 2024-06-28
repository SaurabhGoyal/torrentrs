use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{mpsc::Sender, Arc, RwLock},
    time::SystemTime,
};

// Piece hash byte length
pub const INFO_HASH_BYTE_LEN: usize = 20;
pub const PIECE_HASH_BYTE_LEN: usize = 20;
pub const PEER_ID_BYTE_LEN: usize = 20;

#[derive(Debug)]
pub struct FileInfo {
    pub relative_path: PathBuf,
    pub length: u64,
}

#[derive(Debug)]
pub struct PieceInfo {
    pub hash: [u8; PIECE_HASH_BYTE_LEN],
    pub length: usize,
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

#[derive(Debug)]
pub struct File {
    pub index: usize,
    pub relative_path: PathBuf,
    pub length: usize,
    pub block_ids: Vec<String>,
    pub path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Piece {
    pub index: usize,
    pub length: usize,
    pub have: usize,
}

#[derive(Debug)]
pub struct Block {
    pub file_index: usize,
    pub piece_index: usize,
    pub begin: usize,
    pub length: usize,
    pub path: Option<PathBuf>,
    pub last_requested_at: Option<SystemTime>,
}

#[derive(Debug)]
pub enum PeerControlCommand {
    PieceBlockRequest(usize, usize, usize),
    PieceBlockCancel(usize, usize, usize),
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct PeerState {
    pub handshake: bool,
    pub choked: bool,
    pub interested: bool,
    pub bitfield: Option<Vec<bool>>,
}

#[derive(Debug)]
pub struct Peer {
    pub ip: String,
    pub port: u16,
    pub control_rx: Option<Sender<PeerControlCommand>>,
    pub state: Option<PeerState>,
    pub last_connected_at: Option<SystemTime>,
}

#[derive(Debug)]
pub struct TorrentState {
    pub meta: MetaInfo,
    pub client_id: [u8; PEER_ID_BYTE_LEN],
    pub dest_path: PathBuf,
    pub temp_prefix_path: PathBuf,
    pub files: Vec<Arc<RwLock<File>>>,
    pub pieces: Vec<Arc<RwLock<Piece>>>,
    pub blocks: HashMap<String, Arc<RwLock<Block>>>,
    pub peers: HashMap<String, Arc<RwLock<Peer>>>,
}

pub enum PeerStateEvent {
    Init(PeerState),
    FieldChoked(bool),
    FieldHave(usize),
    FieldBitfield(Vec<bool>),
}

pub enum PeerEvent {
    Control(Sender<PeerControlCommand>),
    State(PeerStateEvent),
}

pub enum TorrentEvent {
    Peer(String, u16, PeerEvent),
    Block(usize, usize, Vec<u8>),
}
