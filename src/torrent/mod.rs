mod event_processor;
mod formatter;
mod models;
mod scheduler;
mod state;
mod tracker;
mod writer;

use std::fs;
use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

use event_processor::process_event;
use models::Peer;
use models::{BlockStatus, Torrent, INFO_HASH_BYTE_LEN, PEER_ID_BYTE_LEN};
use tracker::TrackerManager;

use crate::bencode;
use crate::peer;

const MAX_EVENTS_PER_CYCLE: usize = 10000;
const BLOCK_SCHEDULER_FREQUENCY_MS: u64 = 500;
const BLOCK_SCHEDULING_BATCH_SIZE: usize = 500;
const PIECE_VERIFICATION_BATCH_SIZE: usize = 100;
const FILE_VERIFICATION_AND_WRITE_BATCH_SIZE: usize = 3;
const MAX_PEERS_FROM_TRACKERS: usize = 20;

#[derive(Debug)]
pub struct Controller {
    torrent: Torrent,
    tracker_manager: tracker::TrackerManager,
    scheduler: scheduler::Scheduler,
    event_tx: Sender<ControllerEvent>,
}

#[derive(Debug, Clone)]
pub struct State {
    pub files: Vec<(String, bool, bool)>,
    pub blocks: Vec<(String, bool, bool)>,
    pub peers: Vec<(String, bool)>,
}

#[derive(Debug)]
pub enum ControlCommand {}

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
    pub hash: [u8; INFO_HASH_BYTE_LEN],
    pub event: Event,
}

impl Controller {
    pub fn new(
        client_id: [u8; PEER_ID_BYTE_LEN],
        meta: &bencode::MetaInfo,
        dest_path: &str,
        event_tx: Sender<ControllerEvent>,
    ) -> anyhow::Result<(Self, Sender<ControlCommand>)> {
        let torrent = models::Torrent::new(client_id, dest_path, meta);
        let tracker_manager = tracker::TrackerManager::new(torrent.trackers.as_slice())?;
        let (controller_tx, _controller_rx) = channel::<ControlCommand>();
        Ok((
            Self {
                torrent,
                event_tx,
                tracker_manager,
                scheduler: scheduler::Scheduler::new(),
            },
            controller_tx,
        ))
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        self.setup()?;
        let (event_tx, event_rx) = channel::<peer::ControllerEvent>();

        loop {
            // Process events
            self.process_bounded_events(&event_rx, MAX_EVENTS_PER_CYCLE)?;
            // Refresh peers
            self.refresh_peers()?;
            // Download pending blocks
            self.scheduler.enqueue_pending_blocks(
                &mut self.torrent,
                event_tx.clone(),
                BLOCK_SCHEDULING_BATCH_SIZE,
            )?;
            // Verify downloaded blocks
            self.torrent
                .verify_blocks_and_files(PIECE_VERIFICATION_BATCH_SIZE)?;
            // Verify and write pending files
            let pending_file_indices = writer::verify_and_write_pendinfg_files(
                &mut self.torrent,
                FILE_VERIFICATION_AND_WRITE_BATCH_SIZE,
            )?;
            // Send update to client
            self.send_torrent_controller_event();
            // End controller if no files pending.
            if pending_file_indices == 0 {
                break;
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

    fn refresh_peers(&mut self) -> anyhow::Result<()> {
        // Reset finished peers
        for (_peer_id, peer) in self
            .torrent
            .peers
            .iter_mut()
            .filter(|(_peer_id, peer)| peer.handle.is_some())
        {
            if peer.handle.as_ref().unwrap().is_finished() {
                peer.control_rx = None;
                peer.state = None;
                peer.handle = None;
            }
        }
        if self
            .torrent
            .peers
            .iter()
            .filter(|(_peer_id, peer)| {
                peer.last_initiated_at.is_none()
                    || SystemTime::now()
                        .duration_since(peer.last_initiated_at.unwrap())
                        .unwrap()
                        < Duration::from_secs(240)
            })
            .count()
            == 0
        {
            let trackers_peers = self
                .tracker_manager
                .sync_with_trackers(&self.torrent, MAX_PEERS_FROM_TRACKERS)?;
            trackers_peers.into_iter().for_each(|(ip, port)| {
                self.torrent.peers.insert(
                    formatter::get_peer_id(ip.as_str(), port),
                    Peer {
                        ip,
                        port,
                        control_rx: None,
                        state: None,
                        last_initiated_at: None,
                        handle: None,
                    },
                );
            });
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
                    block.verified,
                )
            })
            .collect::<Vec<(String, bool, bool)>>();
        blocks.sort_by_key(|(block_id, _, _)| block_id.clone());
        let files = self
            .torrent
            .files
            .iter()
            .map(|file| {
                (
                    file.relative_path.to_str().unwrap().to_string(),
                    file.path.is_some(),
                    file.verified,
                )
            })
            .collect::<Vec<(String, bool, bool)>>();
        let peers = self
            .torrent
            .peers
            .iter()
            .map(|(peer_id, peer)| (peer_id.to_string(), peer.control_rx.is_some()))
            .collect::<Vec<(String, bool)>>();

        self.event_tx
            .send(ControllerEvent {
                hash: self.torrent.hash,
                event: Event::State(State {
                    files,
                    blocks,
                    peers,
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

    fn setup(&mut self) -> anyhow::Result<()> {
        fs::create_dir_all(self.torrent.dest_path.as_path()).unwrap();
        fs::create_dir_all(self.torrent.get_temp_dir_path()).unwrap();
        self.torrent.sync_with_disk()?;
        Ok(())
    }

    fn cleanup(&mut self) -> anyhow::Result<()> {
        // Send a final controller event to notify client of the state.
        self.send_torrent_controller_event();
        // Remove temp dir.
        fs::remove_dir_all(self.torrent.get_temp_dir_path()).unwrap();
        // Wait for all files to be written.
        assert!(self.torrent.files.iter().all(|file| file.path.is_some()));
        // Close all peer connections
        for (_peer_id, peer) in self
            .torrent
            .peers
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
