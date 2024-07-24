use std::{
    sync::mpsc::Sender,
    thread,
    time::{Duration, SystemTime},
};

use crate::peer;

use super::models::{Block, BlockStatus, Peer, Torrent};

const MIN_PEERS_FOR_DOWNLOAD: usize = 1;
const BLOCK_REQUEST_TIMEOUT_MS: u64 = 6000;
const PEER_REQUEST_TIMEOUT_MS: u64 = 6000;

#[derive(Debug)]
pub(super) struct Scheduler {
    end_game_mode: bool,
    concurrent_peers: usize,
}

impl Scheduler {
    pub(super) fn new() -> Self {
        Self {
            end_game_mode: false,
            concurrent_peers: 3,
        }
    }

    pub(super) fn enqueue_pending_blocks(
        &mut self,
        torrent: &mut Torrent,
        event_tx: Sender<peer::ControllerEvent>,
        max_count: usize,
    ) -> anyhow::Result<usize> {
        // Check updated status
        let pending_blocks_count = torrent
            .blocks
            .iter()
            .filter(|(_block_id, block)| {
                matches!(
                    block.data_status,
                    BlockStatus::Pending | BlockStatus::Requested(_)
                )
            })
            .count();
        // Enqueue download requests for pending blocks.
        if pending_blocks_count > 0 {
            if pending_blocks_count < 3 {
                self.end_game_mode = true;
                self.concurrent_peers = 10;
            }
            self.reset_finished_peers(torrent);
            self.schedule(torrent, event_tx, max_count);
        }
        Ok(pending_blocks_count)
    }

    fn schedule(
        &self,
        torrent: &mut Torrent,
        event_tx: Sender<peer::ControllerEvent>,
        max_count: usize,
    ) {
        let mut pending_blocks = torrent
            .blocks
            .iter_mut()
            .filter(|(_block_id, block)| match block.data_status {
                BlockStatus::Pending => true,
                BlockStatus::Requested(last_requested_at) => {
                    SystemTime::now().duration_since(last_requested_at).unwrap()
                        > Duration::from_millis(BLOCK_REQUEST_TIMEOUT_MS)
                }
                _ => false,
            })
            .collect::<Vec<(&String, &mut Block)>>();
        pending_blocks.sort_by_key(|(_block_id, block)| torrent.pieces[block.piece_index].have);

        for (_block_id, block) in pending_blocks.into_iter().take(max_count) {
            let peers_with_current_block = torrent
                .peers
                .iter_mut()
                .filter(|(_peer_id, peer)| {
                    peer.control_rx.is_some()
                        && peer.state.is_some()
                        && !peer.state.as_ref().unwrap().choked
                        && peer.state.as_ref().unwrap().bitfield.is_some()
                        && peer.state.as_ref().unwrap().bitfield.as_ref().unwrap()
                            [block.piece_index]
                })
                .collect::<Vec<(&String, &mut Peer)>>();
            if peers_with_current_block.len() < MIN_PEERS_FOR_DOWNLOAD {
                torrent
                    .peers
                    .iter_mut()
                    .filter(|(_peer_id, peer)| {
                        peer.control_rx.is_none()
                            && (peer.last_initiated_at.is_none()
                                || SystemTime::now()
                                    .duration_since(*peer.last_initiated_at.as_ref().unwrap())
                                    .unwrap()
                                    > Duration::from_millis(PEER_REQUEST_TIMEOUT_MS))
                    })
                    .take(self.concurrent_peers)
                    .for_each(|(_peer_id, peer)| {
                        let event_tx = event_tx.clone();
                        let ip = peer.ip.clone();
                        let port = peer.port;
                        let torrent_info_hash = torrent.hash;
                        let client_peer_id = torrent.client_id;
                        let pieces_count = torrent.pieces.len();
                        peer.handle = Some(thread::spawn(move || {
                            if let Ok(peer_controller) = peer::Controller::new(
                                ip,
                                port,
                                event_tx,
                                torrent_info_hash,
                                client_peer_id,
                                pieces_count,
                            ) {
                                let _ = peer_controller.start();
                            }
                        }));
                        peer.last_initiated_at = Some(SystemTime::now());
                    });
            } else {
                for (_peer_id, peer) in peers_with_current_block {
                    // Request a block with only one peer unless we are in end-game
                    match peer.control_rx.as_ref().unwrap().send(
                        peer::ControlCommand::PieceBlockRequest(
                            block.piece_index,
                            block.begin,
                            block.length,
                        ),
                    ) {
                        // Peer has become unreachabnle,
                        Ok(_) => {
                            block.data_status = BlockStatus::Requested(SystemTime::now());
                            if !self.end_game_mode {
                                break;
                            }
                        }
                        Err(_) => {
                            peer.control_rx = None;
                            peer.state = None;
                        }
                    }
                }
            }
        }
    }

    fn reset_finished_peers(&self, torrent: &mut Torrent) {
        // Update peers
        for (_peer_id, peer) in torrent
            .peers
            .iter_mut()
            .filter(|(_peer_id, peer)| peer.handle.is_some())
        {
            if peer.handle.as_ref().unwrap().is_finished() {
                peer.control_rx = None;
                peer.state = None;
                peer.state = None;
                peer.last_initiated_at = None;
            }
        }
    }
}
