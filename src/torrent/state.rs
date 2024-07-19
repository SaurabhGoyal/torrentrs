use std::collections::HashSet;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::{cmp::min, collections::HashMap};
use std::{path::PathBuf, sync::mpsc::Sender, thread::JoinHandle, time::SystemTime};

use sha1::{Digest as _, Sha1};
use walkdir::{DirEntry, WalkDir};

use crate::{bencode, utils};

use super::models::{Block, BlockStatus, File, Peer, Piece, Torrent};
use super::{formatter, writer};

pub(super) const MAX_BLOCK_LENGTH: usize = 1 << 14;

use crate::peer;

pub(super) const CLIENT_PORT: usize = 12457;
pub(super) const INFO_HASH_BYTE_LEN: usize = 20;
pub(super) const PEER_ID_BYTE_LEN: usize = 20;
pub(super) const PIECE_HASH_BYTE_LEN: usize = 20;

pub(super) const MAX_CONCURRENT_BLOCKS: usize = 500;
pub(super) const MIN_PEERS_FOR_DOWNLOAD: usize = 1;
pub(super) const BLOCK_REQUEST_TIMEOUT_MS: u64 = 6000;
pub(super) const PEER_REQUEST_TIMEOUT_MS: u64 = 6000;
pub(super) const BLOCK_SCHEDULER_FREQUENCY_MS: u64 = 500;
pub(super) const MAX_EVENTS_PER_CYCLE: i32 = 10000;

impl Torrent {
    pub(super) fn new(
        client_id: [u8; PEER_ID_BYTE_LEN],
        dest_path: &str,
        meta: &bencode::MetaInfo,
    ) -> Self {
        let piece_standard_length = meta.pieces[0].length;
        let mut piece_budget = piece_standard_length;
        let mut file_offset = 0;
        let mut file_budget = meta.files[0].length as usize;
        let mut begin = 0_usize;
        let mut piece_index = 0_usize;
        let mut piece_length = 0_usize;
        let mut block_ids = vec![];
        let mut block_ids_pos = HashMap::new();
        let mut file_index = 0_usize;
        let mut files: Vec<File> = vec![];
        let mut pieces: Vec<Piece> = vec![];
        let mut blocks: HashMap<String, Block> = HashMap::new();

        while file_index < meta.files.len() {
            // Find the length that can be added
            let block_length = min(min(MAX_BLOCK_LENGTH, piece_budget), file_budget);
            let block_id = formatter::get_block_id(piece_index, begin);
            // We would always hit the block boundary, add block and move block cursor.
            blocks.insert(
                block_id.clone(),
                Block {
                    file_index,
                    piece_index,
                    begin,
                    length: block_length,
                    data_status: BlockStatus::Pending,
                    verified: false,
                },
            );
            begin += block_length;
            piece_length += block_length;
            file_offset += block_length;
            piece_budget -= block_length;
            file_budget -= block_length;
            block_ids.push(block_id.clone());
            block_ids_pos.insert(block_id.clone(), (file_offset, block_length));
            // If we have hit piece boundary or this is the last piece for last file, add piece and move piece cursor.
            if piece_budget == 0 || (file_budget == 0 && file_index == meta.files.len() - 1) {
                pieces.push(Piece {
                    index: piece_index,
                    length: piece_length,
                    have: 0,
                    hash: meta.pieces[piece_index].hash,
                });
                begin = 0;
                piece_index += 1;
                piece_length = 0;
                piece_budget = piece_standard_length;
            }

            // If we have hit file boundary, add file and move file cursor.
            if file_budget == 0 {
                block_ids.sort();
                files.push(File {
                    index: file_index,
                    relative_path: meta.files[file_index].relative_path.clone(),
                    length: meta.files[file_index].length as usize,
                    block_ids: block_ids.clone(),
                    path: None,
                    block_ids_pos: block_ids_pos.clone(),
                });
                block_ids.clear();
                block_ids_pos.clear();
                if file_index + 1 >= meta.files.len() {
                    break;
                }
                file_index += 1;
                file_budget = meta.files[file_index].length as usize;
                file_offset = 0;
            }
        }
        let mut dest_path = PathBuf::from(dest_path);
        if let Some(dir_path) = meta.directory.as_ref() {
            dest_path = dest_path.join(dir_path);
        }
        Self {
            client_id,
            dest_path,
            hash: meta.info_hash,
            tracker: meta.tracker.clone(),
            directory: meta.directory.clone(),
            files,
            pieces,
            blocks,
            peers: None,
            downloaded_window_second: (0, 0),
        }
    }

    pub(super) fn sync_with_tracker(&mut self) -> anyhow::Result<()> {
        self.peers = Some(self.get_announce_response()?);
        Ok(())
    }

    pub(super) fn sync_with_disk(&mut self) -> anyhow::Result<()> {
        // Mark the blocks which are already present.
        self.sync_blocks()?;
        // Mark the files which are already present.
        self.sync_files()?;
        Ok(())
    }

    fn sync_blocks(&mut self) -> anyhow::Result<()> {
        let temp_dir_path = self.get_temp_dir_path();
        for entry in fs::read_dir(temp_dir_path.as_path())?.filter_map(|e| e.ok()) {
            let entry = entry.path();
            let file_name = entry.file_name().unwrap().to_str().unwrap();
            if let Some(block) = self.blocks.get_mut(file_name) {
                block.data_status = BlockStatus::PersistedSeparately(entry);
                block.verified = false;
            }
        }
        Ok(())
    }

    fn sync_files(&mut self) -> anyhow::Result<()> {
        let mut verified_blocks = HashSet::new();
        let mut verified_file_paths = vec![];
        for (file_index, dir_entry) in WalkDir::new(self.dest_path.as_path())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let matching_files = self
                    .files
                    .iter()
                    .filter(|f| self.dest_path.join(f.relative_path.as_path()) == e.path())
                    .collect::<Vec<&File>>();
                if matching_files.len() == 1 {
                    return Some((matching_files[0].index, e));
                }
                None
            })
        {
            let meta = dir_entry.metadata().unwrap();
            let file_len = meta.len();
            let mut file_offset = 0;
            let mut file_obj = fs::OpenOptions::new()
                .read(true)
                .open(dir_entry.path())
                .unwrap();
            let mut piece_option: Option<&Piece> = None;
            let mut piece_offset = 0;
            let mut piece_block_ids = vec![];
            let mut sha1_hasher_option: Option<Sha1> = None;
            let file = self.files.get(file_index).unwrap();
            for block_id in file.block_ids.iter() {
                if file_offset > file_len {
                    break;
                }
                let block = self.blocks.get_mut(block_id).unwrap();
                let mut buf = vec![0_u8; block.length];
                let _ = file_obj.seek(SeekFrom::Start(file_offset));
                if file_obj.read_exact(&mut buf[..]).is_err() {
                    break;
                }
                if piece_option.is_none()
                    || piece_option.as_ref().unwrap().index != block.piece_index
                {
                    piece_option = Some(self.pieces.get(block.piece_index).unwrap());
                    piece_offset = 0;
                    sha1_hasher_option = Some(Sha1::new());
                }
                piece_block_ids.push(block_id);
                piece_offset += block.length;
                sha1_hasher_option.as_mut().unwrap().update(&buf);
                let piece = piece_option.as_ref().unwrap();
                if piece.length == piece_offset {
                    let piece = piece_option.take().unwrap();
                    let sha1_hasher = sha1_hasher_option.take().unwrap();
                    if piece.hash == sha1_hasher.finalize().as_slice() {
                        while let Some(block_id) = piece_block_ids.pop() {
                            verified_blocks.insert(block_id.to_string());
                        }
                    } else {
                        break;
                    }
                }
                file_offset += block.length as u64;
            }
            if file_offset == file.length as u64 {
                verified_file_paths.push((file_index, dir_entry.into_path()));
            }
        }
        for block_id in verified_blocks {
            let block = self.blocks.get_mut(&block_id).unwrap();
            block.data_status = BlockStatus::PersistedInFile;
            block.verified = true;
        }
        for (file_index, path) in verified_file_paths {
            let file = self.files.get_mut(file_index).unwrap();
            file.path = Some(path);
        }
        Ok(())
    }

    fn get_announce_response(&self) -> anyhow::Result<HashMap<String, Peer>> {
        let client = reqwest::blocking::Client::new();
        // Workaround for issue with binary data - https://github.com/servo/rust-url/issues/219
        let mut url = reqwest::Url::parse(self.tracker.as_str())?;
        url.set_query(Some(
            format!(
                "info_hash={}&peer_id={}",
                utils::bytes_to_hex_encoding(&self.hash),
                utils::bytes_to_hex_encoding(&self.client_id),
            )
            .as_str(),
        ));
        let req = client
            .get(url)
            .query(&[("port", CLIENT_PORT)])
            .query(&[("uploaded", 0)])
            .query(&[(
                "downloaded",
                self.blocks
                    .iter()
                    .filter(|(_, block)| {
                        matches!(
                            block.data_status,
                            BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
                        )
                    })
                    .map(|(_, block)| block.length)
                    .sum::<usize>(),
            )])
            .query(&[(
                "left",
                self.blocks
                    .iter()
                    .filter(|(_, block)| {
                        !matches!(
                            block.data_status,
                            BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
                        )
                    })
                    .map(|(_, block)| block.length)
                    .sum::<usize>(),
            )])
            .build()?;
        let res = client.execute(req)?.bytes()?;
        let peers_info = bencode::decode_peers(res.as_ref())?;
        Ok(peers_info
            .into_iter()
            .map(|peer_info| {
                (
                    formatter::get_peer_id(peer_info.ip.as_str(), peer_info.port),
                    Peer {
                        ip: peer_info.ip,
                        port: peer_info.port,
                        control_rx: None,
                        state: None,
                        last_initiated_at: None,
                        handle: None,
                    },
                )
            })
            .collect::<HashMap<String, Peer>>())
    }

    pub(super) fn get_temp_dir_path(&self) -> PathBuf {
        self.dest_path
            .join(format!(".tmp_{}", utils::bytes_to_hex_encoding(&self.hash)))
    }
}
