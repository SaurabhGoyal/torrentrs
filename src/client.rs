use rand::{Rng as _, RngCore};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    sync::mpsc::{channel, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    bencode,
    torrent::{self, TorrentControllerEvent},
};
// Piece hash byte length
const INFO_HASH_BYTE_LEN: usize = 20;
const PIECE_HASH_BYTE_LEN: usize = 20;
const PEER_ID_BYTE_LEN: usize = 20;
const MAX_EVENTS_PER_CYCLE: usize = 500;

#[derive(Debug)]
struct TorrentState {
    files_total: usize,
    files_completed: usize,
    blocks_total: usize,
    blocks_completed: usize,
    peers_total: usize,
    peers_connected: usize,
}

#[derive(Debug)]
pub enum ClientControlCommand {
    AddTorrent(String, String),
}

#[derive(Debug)]
struct Torrent {
    hash: [u8; INFO_HASH_BYTE_LEN],
    control_tx: Option<Sender<torrent::TorrentControlCommand>>,
    handle: Option<JoinHandle<()>>,
    state: Option<TorrentState>,
}

pub struct Client {
    peer_id: [u8; PEER_ID_BYTE_LEN],
    torrents: HashMap<[u8; 20], Torrent>,
    event_tx: Sender<TorrentControllerEvent>,
    event_rx: Receiver<TorrentControllerEvent>,
    control_rx: Receiver<ClientControlCommand>,
}

impl Client {
    pub fn new() -> (Self, Sender<ClientControlCommand>) {
        let mut peer_id = [0_u8; 20];
        let (clinet_info, rest) = peer_id.split_at_mut(8);
        clinet_info.copy_from_slice("-rS0001-".as_bytes());
        rand::thread_rng().fill_bytes(rest);
        let (event_tx, event_rx) = channel::<torrent::TorrentControllerEvent>();
        let (control_tx, control_rx) = channel::<ClientControlCommand>();
        (
            Self {
                peer_id,
                torrents: HashMap::new(),
                event_tx,
                event_rx,
                control_rx,
            },
            control_tx,
        )
    }

    pub fn start(&mut self) -> ! {
        loop {
            {
                // Process commands
                loop {
                    match self.control_rx.try_recv() {
                        Ok(cmd) => match cmd {
                            ClientControlCommand::AddTorrent(file_path, dest_path) => {
                                let res = self.add_torrent(file_path.as_str(), dest_path.as_str());
                                println!("Add - {file_path} -> {dest_path} = {:?}", res);
                            }
                        },
                        Err(e) => match e {
                            std::sync::mpsc::TryRecvError::Empty => {
                                break;
                            }
                            std::sync::mpsc::TryRecvError::Disconnected => todo!(),
                        },
                    }
                }

                // Process some events
                let mut processed_events = 0;
                loop {
                    match self.event_rx.try_recv() {
                        Ok(event) => {
                            self.process_event(event);
                            processed_events += 1;
                            if processed_events > MAX_EVENTS_PER_CYCLE {
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
            }
            thread::sleep(Duration::from_millis(1000));
        }
    }

    fn add_torrent(&mut self, file_path: &str, dest_path: &str) -> Result<(), io::Error> {
        // Read file
        let mut file = fs::OpenOptions::new().read(true).open(file_path)?;
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf)?;
        // Parse metadata_info
        let metainfo = bencode::decode_metainfo(buf.as_slice());
        let (mut torrent_controller, control_tx) = torrent::TorrentController::new(
            &metainfo,
            self.peer_id.clone(),
            dest_path,
            self.event_tx.clone(),
        );
        self.torrents.insert(
            metainfo.info_hash.clone(),
            Torrent {
                hash: metainfo.info_hash.clone(),
                control_tx: Some(control_tx),
                handle: None,
                state: None,
            },
        );
        // Add torrent;
        thread::spawn(move || torrent_controller.start());
        Ok(())
    }

    fn process_event(&mut self, event: TorrentControllerEvent) {
        let torrent = self.torrents.get_mut(&event.hash).unwrap();
        match event.event {
            torrent::TorrentEvent::State(state) => {
                torrent.state = Some(TorrentState {
                    files_total: state.files_total,
                    files_completed: state.files_completed,
                    blocks_total: state.blocks_total,
                    blocks_completed: state.blocks_completed,
                    peers_total: state.peers_total,
                    peers_connected: state.peers_connected,
                });
            }
        }
        println!("{:?}", self.torrents);
    }
}
