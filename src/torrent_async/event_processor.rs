use crate::peer_async;

use super::{
    models::{Peer, Torrent},
    // writer,
};

pub(super) fn process_event(
    torrent: &mut Torrent,
    peer_id: &str,
    event: peer_async::Event,
) -> anyhow::Result<()> {
    let piece_count = torrent.pieces.len();
    let peer = torrent.peers.get_mut(peer_id).unwrap();
    match event {
        peer_async::Event::State(event) => match (event, peer.state.as_mut()) {
            (peer_async::StateEvent::Init(state), None) => {
                peer.state = Some(state);
            }
            (peer_async::StateEvent::FieldChoked(choked), Some(state)) => {
                state.choked = choked;
            }
            (peer_async::StateEvent::FieldHave(index), Some(state)) => {
                let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
                let had = state_bitfield[index];
                state_bitfield[index] = true;
                if !had {
                    torrent.pieces[index].have += 1;
                }
            }
            (peer_async::StateEvent::FieldBitfield(bitfield), Some(state)) => {
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
        peer_async::Event::Block(piece_index, begin, data) => {
            // writer::write_block(torrent, piece_index, begin, data)?;
        }
        peer_async::Event::Metric(event) => match event {
            peer_async::Metric::DownloadWindowSecond(second_window, downloaded_bytes) => {
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
