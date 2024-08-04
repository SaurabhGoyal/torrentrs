use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::bencode;

pub(super) const PEER_ID_BYTE_LEN: usize = 20;
pub(super) const DB_FILE: &str = "client.json";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) enum TorrentStatus {
    Fetching,
    Downloading,
    Completed,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct TorrentState {
    pub(crate) status: TorrentStatus,
    pub(crate) files: Vec<(String, bool, bool)>,
    pub(crate) blocks: Vec<(String, bool, bool)>,
    pub(crate) peers: Vec<(String, bool)>,
    pub(crate) downloaded_window_second: (u64, usize),
}

#[derive(Debug)]
pub(crate) enum ClientControlCommand {
    AddTorrent(String, String),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct Torrent {
    pub(crate) file_path: String,
    pub(crate) dest_path: String,
    pub(crate) name: String,
    pub(crate) metainfo: bencode::MetaInfo,
    #[serde(skip)]
    pub(crate) state: Option<TorrentState>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub(crate) struct ClientState {
    pub(crate) data_dir: PathBuf,
    pub(crate) peer_id: [u8; PEER_ID_BYTE_LEN],
    pub(crate) torrents: HashMap<String, Torrent>,
}
