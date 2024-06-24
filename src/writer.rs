use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
};

use crate::{models, utils};

#[derive(Debug)]
pub struct FilePersistenceInfo {
    relative_path: PathBuf,
    length: u64,
    piece_indices: Vec<usize>,
    path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct BlockPersistenceInfo {
    piece_index: usize,
    begin: usize,
    length: usize,
    path: PathBuf,
}

#[derive(Debug)]
pub struct PiecePersistenceInfo {
    hash: [u8; models::PIECE_HASH_BYTE_LEN],
    length: usize,
    file_index: usize,
    blocks: Vec<BlockPersistenceInfo>,
    completed: bool,
}

#[derive(Debug)]
pub struct DataWriterMeta {
    dest_path: PathBuf,
    temp_path: PathBuf,
    files: Vec<FilePersistenceInfo>,
    pieces: Vec<PiecePersistenceInfo>,
}

#[derive(Debug)]
pub enum Data {
    PieceBlock(usize, usize, usize, Vec<u8>),
}

#[derive(Debug)]
pub struct DataWriter {
    meta: DataWriterMeta,
    data_rx: Receiver<Data>,
}

impl DataWriter {
    pub fn new(
        info_hash: &[u8],
        dest_path: &str,
        files_info: &[models::FileInfo],
        pieces_info: &[models::PieceInfo],
    ) -> Result<(Self, Sender<Data>), io::Error> {
        let dest_path = Path::new(dest_path).to_path_buf();
        let dest_path_metadata = dest_path.symlink_metadata()?;
        assert!(dest_path_metadata.is_dir());
        let temp_path = dest_path
            .join(format!(".tmp_{}", utils::bytes_to_hex_encoding(info_hash)))
            .to_path_buf();
        if let Err(e) = fs::create_dir(temp_path.as_path()) {
            println!("Error in creating tmp dir - {e}");
        }

        let piece_count = pieces_info.len();
        let piece_len = pieces_info[0].length as usize;
        let mut pieces = pieces_info
            .iter()
            .enumerate()
            .map(|(_index, pi)| PiecePersistenceInfo {
                hash: pi.hash,
                length: pi.length,
                file_index: 0, // This will be updates when we create file info.
                blocks: vec![],
                completed: false,
            })
            .collect::<Vec<PiecePersistenceInfo>>();

        let mut next_piece_index = 0_usize;
        let files = files_info
            .iter()
            .enumerate()
            .map(|(index, fi)| {
                assert!(next_piece_index < piece_count);
                let file_pieces_count = (fi.length as usize + piece_len - 1) / piece_len;
                let piece_indices = next_piece_index..next_piece_index + file_pieces_count;
                let info = FilePersistenceInfo {
                    relative_path: fi.relative_path.clone(),
                    length: fi.length,
                    piece_indices: piece_indices.clone().collect(),
                    path: None,
                };
                for pi in piece_indices {
                    pieces[pi].file_index = index;
                }
                next_piece_index += file_pieces_count;
                info
            })
            .collect();
        let (data_tx, data_rx) = channel::<Data>();
        Ok((
            Self {
                meta: DataWriterMeta {
                    dest_path,
                    temp_path,
                    files,
                    pieces,
                },
                data_rx,
            },
            data_tx,
        ))
    }

    pub fn start(&mut self) -> Result<(), io::Error> {
        println!("Listening for data write requests.");
        while let Ok(data) = self.data_rx.recv() {
            match data {
                Data::PieceBlock(index, begin, length, data) => {
                    println!(
                        "Data Write for piece block ({}, {}, {}) requested",
                        index, begin, length
                    );
                    self.write_piece_block(index, begin, length, data.as_slice())?;
                    println!(
                        "Data Write for piece block ({}, {}, {}) completed",
                        index, begin, length
                    );
                }
            }
        }
        Ok(())
    }

    fn write_piece_block(
        &mut self,
        index: usize,
        begin: usize,
        length: usize,
        data: &[u8],
    ) -> Result<(), io::Error> {
        assert!(index < self.meta.pieces.len());
        assert!(begin < self.meta.pieces[index].length);
        assert!(begin + length < self.meta.pieces[index].length);
        if self.meta.pieces[index].completed {
            println!("Piece {index} is already complete.");
            return Ok(());
        }
        if self.meta.pieces[index]
            .blocks
            .iter()
            .any(|b| b.begin > begin && b.begin < begin + length)
        {
            println!("Piece block ({index}, {begin}, {length}) already exists.");
            return Ok(());
        }
        let piece_block_path = self.meta.temp_path.join(format!(
            "{}_{}_{}",
            index,
            utils::bytes_to_hex_encoding(&self.meta.pieces[index].hash),
            begin,
        ));
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(piece_block_path.as_path())?
            .write_all(data)?;
        println!(
            "Written piece block ({index}, {begin}, {length}) at {:?}",
            piece_block_path
        );
        self.meta.pieces[index].blocks.push(BlockPersistenceInfo {
            piece_index: index,
            begin,
            length,
            path: piece_block_path,
        });
        self.meta.pieces[index].blocks.sort_by_key(|b| b.begin);
        let blocks_length = self.meta.pieces[index]
            .blocks
            .iter()
            .map(|b| b.length)
            .sum();
        println!("Piece block ({index}, {begin}, {length}) total blocks length - {blocks_length}");
        if self.meta.pieces[index].length == blocks_length {
            self.meta.pieces[index].completed = true;
            self.check_and_write_file(self.meta.pieces[index].file_index)?;
        }
        Ok(())
    }

    fn check_and_write_file(&mut self, index: usize) -> Result<(), io::Error> {
        assert!(index < self.meta.files.len());
        let total_pieces = self.meta.files[index].piece_indices.len();
        let pieces_completed = self.meta.files[index]
            .piece_indices
            .iter()
            .map(|pi| &self.meta.pieces[*pi])
            .filter(|p| p.completed)
            .collect::<Vec<&PiecePersistenceInfo>>();
        println!(
            "File {index} - pieces - {} / {} ",
            pieces_completed.len(),
            total_pieces
        );
        if pieces_completed.len() == total_pieces {
            let file_path = self
                .meta
                .dest_path
                .join(self.meta.files[index].relative_path.as_path());
            println!("File {index} - writing file - {:?}", file_path.as_path());
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(file_path.as_path())?;

            for piece in pieces_completed {
                for block in piece.blocks.iter() {
                    let mut piece_block_file = fs::OpenOptions::new()
                        .read(true)
                        .open(block.path.as_path())?;
                    let bytes_copied = io::copy(&mut piece_block_file, &mut file)?;
                    assert_eq!(bytes_copied, (piece.length >> 1) as u64);
                    println!(
                        "Written piece block ({index}, {}, {}) to file {:?}",
                        block.begin, block.length, file_path
                    );
                }
            }
            println!("Written file {index} to {:?}", file_path);
            self.meta.files[index].path = Some(file_path);
        }
        Ok(())
    }
}
