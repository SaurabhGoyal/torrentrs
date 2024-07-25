use core::str;
use std::fs;
use std::path::PathBuf;
use std::{cmp::min, collections::HashMap};

use walkdir::WalkDir;

use crate::{bencode, utils};

use super::models::{Block, BlockStatus, File, Piece, Torrent};
use super::{formatter, writer};

const MAX_BLOCK_LENGTH: usize = 1 << 14;

const PEER_ID_BYTE_LEN: usize = 20;

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
            block_ids_pos.insert(block_id.clone(), (file_offset, block_length));
            block_ids.push(block_id.clone());
            begin += block_length;
            piece_length += block_length;
            file_offset += block_length;
            piece_budget -= block_length;
            file_budget -= block_length;
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
                    verified: false,
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
            peers: HashMap::new(),
            downloaded_window_second: (0, 0),
        }
    }

    pub(super) fn sync_with_disk(&mut self) -> anyhow::Result<()> {
        let temp_dir_path = self.get_temp_dir_path();
        for entry in fs::read_dir(temp_dir_path.as_path())?.filter_map(|e| e.ok()) {
            let entry = entry.path();
            let file_name = entry.file_name().unwrap().to_str().unwrap();
            if let Some(block) = self.blocks.get_mut(file_name) {
                block.data_status = BlockStatus::PersistedSeparately(entry);
                block.verified = false;
            }
        }

        let mut block_ids_persisted_in_file = vec![];
        let mut files_persisted = vec![];
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
            let file = self.files.get(file_index).unwrap();
            if file_len == file.length as u64 {
                for block_id in file.block_ids.iter() {
                    block_ids_persisted_in_file.push(block_id.to_string());
                }
                files_persisted.push((file_index, dir_entry.into_path()));
            }
        }
        for block_id in block_ids_persisted_in_file {
            let block = self.blocks.get_mut(&block_id).unwrap();
            block.data_status = BlockStatus::PersistedInFile;
        }
        for (file_index, path) in files_persisted {
            let file = self.files.get_mut(file_index).unwrap();
            file.path = Some(path);
        }
        Ok(())
    }

    pub(super) fn verify_blocks_and_files(&mut self, max_count: usize) -> anyhow::Result<()> {
        let mut piece_results = vec![];
        for piece_index in self
            .blocks
            .iter()
            .filter(|(_block_id, block)| !block.verified)
            .map(|(_block_id, block)| block.piece_index)
            .take(max_count)
        {
            if let Ok(Some(verified)) = writer::is_piece_valid(self, piece_index) {
                piece_results.push((piece_index, verified));
            }
        }
        for (piece_index, verified) in piece_results {
            for (_block_id, block) in self
                .blocks
                .iter_mut()
                .filter(|(_block_id, block)| block.piece_index == piece_index)
            {
                // If piece verified, mark all blocks verified, else mark blocks for fresh download.
                if verified {
                    block.verified = true;
                } else {
                    block.data_status = BlockStatus::Pending;
                }
            }
        }
        for file in self
            .files
            .iter_mut()
            .filter(|f| f.path.is_some() && !f.verified)
        {
            if self
                .blocks
                .iter_mut()
                .filter(|(_block_id, block)| block.file_index == file.index)
                .all(|(_block_id, block)| block.verified)
            {
                file.verified = true;
            }
        }
        Ok(())
    }

    pub(super) fn get_temp_dir_path(&self) -> PathBuf {
        self.dest_path
            .join(format!(".tmp_{}", utils::bytes_to_hex_encoding(&self.hash)))
    }
}
