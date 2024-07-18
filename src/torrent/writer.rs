use std::{
    fs,
    io::{self, Read as _, Seek as _, SeekFrom, Write as _},
};

use sha1::{Digest, Sha1};

use super::{
    format,
    state::{Block, BlockStatus, Torrent},
};

pub(super) fn write_block(
    torrent: &mut Torrent,
    piece_index: usize,
    begin: usize,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    let block_id = format::get_block_id(piece_index, begin);
    let block_path = torrent.get_temp_dir_path().join(block_id.as_str());
    let block = torrent.blocks.get_mut(&block_id).unwrap();
    match block.data_status {
        BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile => {
            return Ok(());
        }
        _ => {}
    }
    let _wb = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(block_path.as_path())
        .unwrap()
        .write(&data)
        .unwrap();
    block.data_status = BlockStatus::PersistedSeparately(block_path);
    Ok(())
}

pub(super) fn validate_piece(torrent: &mut Torrent, piece_index: usize) -> anyhow::Result<bool> {
    let mut piece_blocks = torrent
        .blocks
        .iter_mut()
        .filter(|(_block_id, block)| block.piece_index == piece_index)
        .collect::<Vec<(&String, &mut Block)>>();
    piece_blocks.sort_by_key(|(_block_id, block)| block.begin);
    if piece_blocks.iter().all(|(_block_id, block)| {
        matches!(
            block.data_status,
            BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
        )
    }) {
        let mut sha1_hasher = Sha1::new();
        let piece = torrent.pieces.get_mut(piece_index).unwrap();
        for (block_id, block) in piece_blocks.iter() {
            let mut buf = vec![0_u8; block.length];
            match &block.data_status {
                BlockStatus::PersistedSeparately(path) => {
                    let mut block_file = fs::OpenOptions::new()
                        .read(true)
                        .open(path.as_path())
                        .unwrap();
                    let _ = block_file.read(&mut buf[..]).unwrap();
                }
                BlockStatus::PersistedInFile => {
                    let file = torrent.files.get(block.file_index).unwrap();
                    let mut block_file = fs::OpenOptions::new()
                        .read(true)
                        .open(file.path.as_ref().unwrap())
                        .unwrap();
                    let (offset, _) = file.block_ids_pos.get(block_id.as_str()).unwrap();
                    let _ = block_file.seek(SeekFrom::Start(*offset as u64));
                    block_file.read_exact(&mut buf[..])?;
                }
                _ => {
                    // this case is not possible
                }
            }
            sha1_hasher.update(&buf);
        }
        return Ok(piece.hash == sha1_hasher.finalize().as_slice());
    }
    Ok(false)
}

pub(super) fn write_file(torrent: &mut Torrent, file_index: usize) -> anyhow::Result<()> {
    let file = torrent.files.get_mut(file_index).unwrap();
    let mut file_completed_and_verified_blocks = torrent
        .blocks
        .iter_mut()
        .filter(|(_block_id, block)| {
            block.file_index == file_index
                && matches!(
                    block.data_status,
                    BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
                )
                && block.verified
        })
        .collect::<Vec<(&String, &mut Block)>>();
    file_completed_and_verified_blocks.sort_by_key(|(_, block)| (block.piece_index, block.begin));
    // If all blocks of the file are done, write them to file.
    if file_completed_and_verified_blocks.len() == file.block_ids.len() {
        let file_path = torrent.dest_path.join(file.relative_path.as_path());
        let mut file_object = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(file_path.as_path())
            .unwrap();
        for (block_id, block) in file_completed_and_verified_blocks {
            match &block.data_status {
                BlockStatus::PersistedSeparately(path) => {
                    let mut block_file = fs::OpenOptions::new()
                        .read(true)
                        .open(path.as_path())
                        .unwrap();
                    let bytes_copied = io::copy(&mut block_file, &mut file_object).unwrap();
                    assert_eq!(bytes_copied, (block.length) as u64);
                    fs::remove_file(path.as_path()).unwrap();
                }
                BlockStatus::PersistedInFile => {
                    let mut block_file = fs::OpenOptions::new()
                        .read(true)
                        .open(file.path.as_ref().unwrap())
                        .unwrap();
                    let (offset, _) = file.block_ids_pos.get(block_id.as_str()).unwrap();
                    let _ = block_file.seek(SeekFrom::Start(*offset as u64));
                    let mut buf = vec![0_u8; block.length];
                    block_file.read_exact(&mut buf[..])?;
                    file_object.write_all(buf.as_slice())?;
                }
                _ => {
                    // this case is not possible
                }
            }
        }
        file.path = Some(file_path);
    }
    Ok(())
}