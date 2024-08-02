use std::{
    fs,
    io::{Read as _, Seek as _, SeekFrom},
};

use sha1::{Digest, Sha1};

use crate::peer_async;

use super::{
    formatter,
    models::{Block, BlockStatus, Torrent},
    // writer,
};

pub(super) fn process_event(
    torrent: &mut Torrent,
    event: peer_async::ControllerEvent,
) -> anyhow::Result<()> {
    let piece_count = torrent.pieces.len();
    let peer_id = formatter::get_peer_id(event.ip.as_str(), event.port);
    let peer = torrent.peers.get_mut(peer_id.as_str()).unwrap();
    match event.event {
        peer::Event::Control(control_rx) => peer.control_rx = Some(control_rx),
        peer::Event::State(event) => match (event, peer.state.as_mut()) {
            (peer::StateEvent::Init(state), None) => {
                peer.state = Some(state);
            }
            (peer::StateEvent::FieldChoked(choked), Some(state)) => {
                state.choked = choked;
            }
            (peer::StateEvent::FieldHave(index), Some(state)) => {
                let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
                let had = state_bitfield[index];
                state_bitfield[index] = true;
                if !had {
                    torrent.pieces[index].have += 1;
                }
            }
            (peer::StateEvent::FieldBitfield(bitfield), Some(state)) => {
                let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
                let mut impacted_piece_indices = vec![];
                for index in 0..piece_count {
                    let had = state_bitfield[index];
                    state_bitfield[index] = bitfield[index];
                    if !had {
                        impacted_piece_indices.push(index);
                    }
                }
                torrent
                    .pieces
                    .iter_mut()
                    .filter(|piece| impacted_piece_indices.contains(&piece.index))
                    .for_each(|piece| {
                        piece.have += 1;
                    });
            }
            _ => {}
        },
        peer::Event::Block(piece_index, begin, data) => {
            // writer::write_block(torrent, piece_index, begin, data)?;
            let mut piece_blocks = torrent
                .blocks
                .iter()
                .filter(|(_block_id, block)| block.piece_index == piece_index)
                .collect::<Vec<(&String, &Block)>>();
            piece_blocks.sort_by_key(|(_block_id, block)| block.begin);
            if piece_blocks.iter().all(|(_block_id, block)| {
                matches!(
                    block.data_status,
                    BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
                )
            }) {
                let mut sha1_hasher = Sha1::new();
                let piece = torrent.pieces.get(piece_index).unwrap();
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
                let verified = piece.hash == sha1_hasher.finalize().as_slice();
                for (_block_id, block) in torrent
                    .blocks
                    .iter_mut()
                    .filter(|(_block_id, block)| block.piece_index == piece_index)
                {
                    // If piece verified, mark all blocks verified, else mark blocks for fresh download.
                    block.verified = verified;
                }
                if verified {
                    let file_index = torrent
                        .blocks
                        .get(&formatter::get_block_id(piece_index, begin))
                        .unwrap()
                        .file_index;
                    writer::verify_and_write_file(torrent, file_index)?;
                }
            }
        }
        peer::Event::Metric(event) => match event {
            peer::Metric::DownloadWindowSecond(second_window, downloaded_bytes) => {
                if second_window == torrent.downloaded_window_second.0 {
                    torrent.downloaded_window_second.1 += downloaded_bytes;
                } else if second_window > torrent.downloaded_window_second.0 {
                    torrent.downloaded_window_second.0 = second_window;
                    torrent.downloaded_window_second.1 = downloaded_bytes;
                }
            }
        },
    }
    Ok(())
}
