use std::{
    collections::HashMap, path::PathBuf, sync::mpsc::Sender, thread::JoinHandle, time::SystemTime,
};

use crate::peer;

pub(super) const MAX_BLOCK_LENGTH: usize = 1 << 14;
pub(super) const CLIENT_PORT: usize = 12457;
pub(super) const INFO_HASH_BYTE_LEN: usize = 20;
pub(super) const PEER_ID_BYTE_LEN: usize = 20;
pub(super) const PIECE_HASH_BYTE_LEN: usize = 20;

#[derive(Debug)]
pub(super) struct File {
    pub(super) index: usize,
    pub(super) relative_path: PathBuf,
    pub(super) length: usize,
    pub(super) block_ids: Vec<String>,
    pub(super) block_ids_pos: HashMap<String, (usize, usize)>,
    pub(super) path: Option<PathBuf>,
    pub(super) verified: bool,
}

#[derive(Debug)]
pub(super) struct Piece {
    pub(super) index: usize,
    pub(super) length: usize,
    pub(super) have: usize,
    pub(super) hash: [u8; PIECE_HASH_BYTE_LEN],
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum BlockStatus {
    Pending,
    Requested(SystemTime),
    PersistedSeparately(PathBuf),
    PersistedInFile,
}

#[derive(Debug)]
pub(super) struct Block {
    pub(super) file_index: usize,
    pub(super) piece_index: usize,
    pub(super) begin: usize,
    pub(super) length: usize,
    pub(super) data_status: BlockStatus,
    pub(super) verified: bool,
}

#[derive(Debug)]
pub(super) struct Peer {
    pub(super) ip: String,
    pub(super) port: u16,
    pub(super) control_rx: Option<Sender<peer::ControlCommand>>,
    pub(super) state: Option<peer::State>,
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) last_initiated_at: Option<SystemTime>,
}

#[derive(Debug)]
pub(super) struct Torrent {
    pub(super) client_id: [u8; PEER_ID_BYTE_LEN],
    pub(super) dest_path: PathBuf,
    pub(super) hash: [u8; INFO_HASH_BYTE_LEN],
    pub(super) tracker: String,
    pub(super) directory: Option<PathBuf>,
    pub(super) files: Vec<File>,
    pub(super) pieces: Vec<Piece>,
    pub(super) blocks: HashMap<String, Block>,
    pub(super) peers: Option<HashMap<String, Peer>>,
    pub(super) downloaded_window_second: (u64, usize),
}
