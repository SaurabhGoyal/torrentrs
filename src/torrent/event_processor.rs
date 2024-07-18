use crate::peer;

use super::{format, state::Torrent, writer};

pub(super) fn process_event(
    torrent: &mut Torrent,
    event: peer::ControllerEvent,
) -> anyhow::Result<()> {
    let piece_count = torrent.pieces.len();
    let peer_id = format::get_peer_id(event.ip.as_str(), event.port);
    let peer = torrent
        .peers
        .as_mut()
        .unwrap()
        .get_mut(peer_id.as_str())
        .unwrap();
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
            writer::write_block(torrent, piece_index, begin, data)?;
            writer::validate_piece(torrent, piece_index)?;
            let file_index = torrent
                .blocks
                .get(&format::get_block_id(piece_index, begin))
                .unwrap()
                .file_index;
            writer::write_file(torrent, file_index)?;
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
