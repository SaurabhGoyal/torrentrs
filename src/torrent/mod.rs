mod event_processor;
mod format;
mod scheduler;
mod state;
mod writer;

use std::fs;
use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use event_processor::process_event;
use scheduler::schedule;
use state::{BlockStatus, Torrent, PEER_ID_BYTE_LEN};

use crate::bencode;
use crate::peer;

const MAX_EVENTS_PER_CYCLE: usize = 10000;
const BLOCK_SCHEDULER_FREQUENCY_MS: u64 = 500;

#[derive(Debug)]
pub struct Controller {
    torrent: Torrent,
    event_tx: Sender<ControllerEvent>,
}

#[derive(Debug, Clone)]
pub struct State {
    pub files_total: usize,
    pub files_completed: usize,
    pub blocks: Vec<(String, bool)>,
    pub peers_total: usize,
    pub peers_connected: usize,
    pub downloaded_window_second: (u64, usize),
}

#[derive(Debug)]
pub enum ControlCommand {
    Start,
    Pause,
    Stop,
}

#[derive(Debug)]
pub enum Metric {
    DownloadWindowSecond(u64, usize),
}

#[derive(Debug)]
pub enum Event {
    State(State),
    Metric(Metric),
}

#[derive(Debug)]
pub struct ControllerEvent {
    pub hash: [u8; PEER_ID_BYTE_LEN],
    pub event: Event,
}

impl Controller {
    pub fn new(
        client_id: [u8; PEER_ID_BYTE_LEN],
        meta: &bencode::MetaInfo,
        dest_path: &str,
        event_tx: Sender<ControllerEvent>,
    ) -> (Self, Sender<ControlCommand>) {
        let torrent = state::Torrent::new(client_id, dest_path, meta);
        let (controller_tx, controller_rx) = channel::<ControlCommand>();
        (Self { torrent, event_tx }, controller_tx)
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        fs::create_dir_all(self.torrent.dest_path.as_path()).unwrap();
        fs::create_dir_all(self.torrent.get_temp_dir_path()).unwrap();
        self.torrent.sync_with_disk()?;
        self.torrent.sync_with_tracker()?;

        let mut end_game_mode = false;
        let mut concurrent_peers = 3;
        let (event_tx, event_rx) = channel::<peer::ControllerEvent>();

        loop {
            {
                // Process events
                self.process_bounded_events(&event_rx, MAX_EVENTS_PER_CYCLE)?;

                // Update peers
                for (_peer_id, peer) in self
                    .torrent
                    .peers
                    .as_mut()
                    .unwrap()
                    .iter_mut()
                    .filter(|(_peer_id, peer)| peer.handle.is_some())
                {
                    if peer.handle.as_ref().unwrap().is_finished() {
                        peer.handle = None;
                    }
                }

                // Check updated status
                let pending_blocks_count = self
                    .torrent
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
                        end_game_mode = true;
                        concurrent_peers = 10;
                    }
                    schedule(
                        &mut self.torrent,
                        event_tx.clone(),
                        end_game_mode,
                        concurrent_peers,
                    );
                } else {
                    let pending_file_indices = self
                        .torrent
                        .files
                        .iter()
                        .filter(|file| file.path.is_some())
                        .map(|file| file.index)
                        .collect::<Vec<usize>>();
                    for file_index in pending_file_indices {
                        writer::write_file(&mut self.torrent, file_index)?;
                    }
                    break;
                }
            }
            thread::sleep(Duration::from_millis(BLOCK_SCHEDULER_FREQUENCY_MS));
        }
        self.cleanup()
    }

    fn process_bounded_events(
        &mut self,
        event_rx: &Receiver<peer::ControllerEvent>,
        count: usize,
    ) -> anyhow::Result<()> {
        let mut processed_events = 0;
        loop {
            match event_rx.try_recv() {
                Ok(event) => {
                    process_event(&mut self.torrent, event)?;
                    processed_events += 1;
                    self.send_torrent_controller_event();
                    if processed_events > count {
                        break;
                    }
                }
                Err(e) => match e {
                    std::sync::mpsc::TryRecvError::Empty => {
                        break;
                    }
                    std::sync::mpsc::TryRecvError::Disconnected => todo!(),
                },
            }
        }
        Ok(())
    }

    fn send_torrent_controller_event(&self) {
        let mut blocks = self
            .torrent
            .blocks
            .iter()
            .map(|(block_id, block)| {
                (
                    block_id.to_string(),
                    match block.data_status {
                        BlockStatus::Pending | BlockStatus::Requested(_) => false,
                        BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile => true,
                    },
                )
            })
            .collect::<Vec<(String, bool)>>();
        blocks.sort_by_key(|(block_id, _)| block_id.clone());
        self.event_tx
            .send(ControllerEvent {
                hash: self.torrent.hash,
                event: Event::State(State {
                    files_total: self.torrent.files.len(),
                    files_completed: self
                        .torrent
                        .files
                        .iter()
                        .filter(|f| f.path.is_some())
                        .count(),
                    blocks,
                    peers_total: self.torrent.peers.as_ref().unwrap().len(),
                    peers_connected: self
                        .torrent
                        .peers
                        .as_ref()
                        .unwrap()
                        .iter()
                        .filter(|(_, p)| p.control_rx.is_some())
                        .count(),
                    downloaded_window_second: self.torrent.downloaded_window_second,
                }),
            })
            .unwrap();
        if self.torrent.downloaded_window_second.0 > 0 {
            self.event_tx
                .send(ControllerEvent {
                    hash: self.torrent.hash,
                    event: Event::Metric(Metric::DownloadWindowSecond(
                        self.torrent.downloaded_window_second.0,
                        self.torrent.downloaded_window_second.1,
                    )),
                })
                .unwrap();
        }
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        // Wait for all files to be written.
        assert!(self.torrent.files.iter().all(|file| file.path.is_some()));
        // Close all peer connections
        for (_peer_id, peer) in self
            .torrent
            .peers
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|(_peer_id, peer)| peer.handle.is_some())
        {
            if let Some(control_rx) = peer.control_rx.as_ref() {
                //  This will close the peer stream, which will close peer listener.
                let _ = control_rx.send(peer::ControlCommand::Shutdown);
            }
            //  This will close the peer writer.
            peer.control_rx = None;
            peer.state = None;
            // We don't care if peer thread faced any issue.
            let _ = peer.handle.take().unwrap().join();
        }
        Ok(())
    }
}
