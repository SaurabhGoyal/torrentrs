use std::cmp::min;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::mpsc::Sender;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::SystemTime;

use crate::bencode;
use crate::peer;
use crate::utils;

const INFO_HASH_BYTE_LEN: usize = 20;
const PEER_ID_BYTE_LEN: usize = 20;

const MAX_BLOCK_LENGTH: usize = 1 << 14;
const MAX_CONCURRENT_BLOCKS: usize = 500;
const MIN_PEERS_FOR_DOWNLOAD: usize = 1;
const BLOCK_REQUEST_TIMEOUT_MS: u64 = 6000;
const PEER_REQUEST_TIMEOUT_MS: u64 = 6000;
const BLOCK_SCHEDULER_FREQUENCY_MS: u64 = 500;
const MAX_EVENTS_PER_CYCLE: i32 = 10000;

#[derive(Debug)]
pub struct TorrentController {
    torrent: Torrent,
    event_tx: Sender<TorrentControllerEvent>,
}

#[derive(Debug, Clone)]
pub struct TorrentState {
    pub files_total: usize,
    pub files_completed: usize,
    pub blocks_total: usize,
    pub blocks_completed: usize,
    pub peers_total: usize,
    pub peers_connected: usize,
}

#[derive(Debug)]
pub enum TorrentControlCommand {
    Start,
    Pause,
    Stop,
}

#[derive(Debug)]
pub enum TorrentEvent {
    State(TorrentState),
}

#[derive(Debug)]
pub struct TorrentControllerEvent {
    pub hash: [u8; PEER_ID_BYTE_LEN],
    pub event: TorrentEvent,
}

#[derive(Debug)]
struct File {
    index: usize,
    relative_path: PathBuf,
    length: usize,
    block_ids: Vec<String>,
    path: Option<PathBuf>,
}

#[derive(Debug)]
struct Piece {
    index: usize,
    length: usize,
    have: usize,
}

#[derive(Debug)]
struct Block {
    file_index: usize,
    piece_index: usize,
    begin: usize,
    length: usize,
    path: Option<PathBuf>,
    last_requested_at: Option<SystemTime>,
}

#[derive(Debug)]
struct Peer {
    ip: String,
    port: u16,
    control_rx: Option<Sender<peer::PeerControlCommand>>,
    state: Option<peer::PeerState>,
    handle: Option<JoinHandle<()>>,
    last_initiated_at: Option<SystemTime>,
}

#[derive(Debug)]
struct Torrent {
    client_id: [u8; PEER_ID_BYTE_LEN],
    dest_path: PathBuf,
    hash: [u8; INFO_HASH_BYTE_LEN],
    tracker: String,
    directory: Option<PathBuf>,
    files: Vec<File>,
    pieces: Vec<Piece>,
    blocks: HashMap<String, Block>,
    peers: Option<HashMap<String, Peer>>,
}

impl TorrentController {
    pub fn new(
        meta: &bencode::MetaInfo,
        client_id: [u8; PEER_ID_BYTE_LEN],
        dest_path: &str,
        event_tx: Sender<TorrentControllerEvent>,
    ) -> (Self, Sender<TorrentControlCommand>) {
        let piece_standard_length = meta.pieces[0].length;
        let mut piece_budget = piece_standard_length;
        let mut file_budget = meta.files[0].length as usize;
        let mut begin = 0_usize;
        let mut piece_index = 0_usize;
        let mut piece_length = 0_usize;
        let mut block_ids = vec![];
        let mut file_index = 0_usize;
        let mut files: Vec<File> = vec![];
        let mut pieces: Vec<Piece> = vec![];
        let mut blocks: HashMap<String, Block> = HashMap::new();

        while file_index < meta.files.len() {
            // Find the length that can be added
            let block_length = min(min(MAX_BLOCK_LENGTH, piece_budget), file_budget);
            let block_id = get_block_id(piece_index, begin);
            // We would always hit the block boundary, add block and move block cursor.
            blocks.insert(
                block_id.clone(),
                Block {
                    file_index,
                    piece_index,
                    begin,
                    length: block_length,
                    path: None,
                    last_requested_at: None,
                },
            );
            begin += block_length;
            piece_length += block_length;
            block_ids.push(block_id.clone());
            piece_budget -= block_length;
            file_budget -= block_length;
            // If we have hit piece boundary or this is the last piece for last file, add piece and move piece cursor.
            if piece_budget == 0 || (file_budget == 0 && file_index == meta.files.len() - 1) {
                pieces.push(Piece {
                    index: piece_index,
                    length: piece_length,
                    have: 0,
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
                });
                block_ids.clear();
                if file_index + 1 >= meta.files.len() {
                    break;
                }
                file_index += 1;
                file_budget = meta.files[file_index].length as usize;
            }
        }
        let mut dest_path = PathBuf::from(dest_path);
        if let Some(dir_path) = meta.directory.as_ref() {
            dest_path = dest_path.join(dir_path);
        }
        let (controller_tx, controller_rx) = channel::<TorrentControlCommand>();
        let torrent = Torrent {
            client_id,
            dest_path,
            hash: meta.info_hash,
            tracker: meta.tracker.clone(),
            directory: meta.directory.clone(),
            files,
            pieces,
            blocks,
            peers: None,
        };

        (Self { torrent, event_tx }, controller_tx)
    }

    pub fn start(&mut self) {
        if self.torrent.peers.is_none() {
            self.torrent.peers = Some(self.get_announce_response());
        }
        let mut end_game_mode = false;
        let mut concurrent_peers = 3;
        let (event_tx, event_rx) = channel::<peer::PeerControllerEvent>();
        let temp_prefix_path = self.torrent.dest_path.join(format!(
            ".tmp_{}",
            utils::bytes_to_hex_encoding(&self.torrent.hash)
        ));
        // Create destination directory and a temporary directory inside it for holding the blocks.
        fs::create_dir_all(temp_prefix_path.as_path()).unwrap();

        loop {
            {
                // Process some events
                let mut processed_events = 0;
                loop {
                    match event_rx.try_recv() {
                        Ok(event) => {
                            self.process_event(event, temp_prefix_path.as_path());
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

                // Check updated status
                let pending_blocks_count = self
                    .torrent
                    .blocks
                    .iter()
                    .filter(|(_block_id, block)| block.path.is_none())
                    .count();
                let total_block_count = self.torrent.blocks.len();

                // Enqueue download requests for pending blocks.
                if pending_blocks_count > 0 {
                    println!(
                        "Blocks - {} / {}",
                        total_block_count - pending_blocks_count,
                        total_block_count
                    );
                    if pending_blocks_count < 3 {
                        end_game_mode = true;
                        concurrent_peers = 10;
                    }
                    self.enqueue_pending_blocks_to_peers(
                        event_tx.clone(),
                        MAX_CONCURRENT_BLOCKS,
                        end_game_mode,
                        concurrent_peers,
                    );
                } else {
                    println!("all blocks downloaded, closing writer");
                    if self.torrent.files.iter().all(|file| file.path.is_some()) {
                        // Delete temp directory
                        fs::remove_dir_all(temp_prefix_path.as_path()).unwrap();
                    }
                    // Close all peer connections
                    for (peer_id, peer) in self
                        .torrent
                        .peers
                        .as_mut()
                        .unwrap()
                        .iter_mut()
                        .filter(|(_peer_id, peer)| peer.handle.is_some())
                    {
                        if let Some(control_rx) = peer.control_rx.as_ref() {
                            //  This will close the peer stream, which will close peer listener.
                            let _ = control_rx.send(peer::PeerControlCommand::Shutdown);
                        }
                        //  This will close the peer writer.
                        peer.control_rx = None;
                        peer.state = None;
                        println!("Waiting for peer {peer_id} to close");
                        // We don't care if peer thread faced any issue.
                        let _ = peer.handle.take().unwrap().join();
                    }
                    return;
                }
            }
            thread::sleep(Duration::from_millis(BLOCK_SCHEDULER_FREQUENCY_MS));
        }
    }

    fn get_announce_response(&self) -> HashMap<String, Peer> {
        let client = reqwest::blocking::Client::new();
        // Workaround for issue with binary data - https://github.com/servo/rust-url/issues/219
        let mut url = reqwest::Url::parse(self.torrent.tracker.as_str()).unwrap();
        url.set_query(Some(
            format!(
                "info_hash={}&peer_id={}",
                utils::bytes_to_hex_encoding(&self.torrent.hash),
                utils::bytes_to_hex_encoding(&self.torrent.client_id),
            )
            .as_str(),
        ));
        let req = client
            .get(url)
            .query(&[("port", 12457)])
            .query(&[("uploaded", 0)])
            .query(&[("downloaded", 0)])
            .query(&[(
                "left",
                self.torrent.pieces.iter().map(|p| p.length).sum::<usize>(),
            )])
            .build()
            .unwrap();
        let res = client.execute(req).unwrap().bytes().unwrap();
        let peers_info = bencode::decode_peers(res.as_ref());
        peers_info
            .into_iter()
            .map(|peer_info| {
                (
                    get_peer_id(peer_info.ip.as_str(), peer_info.port),
                    Peer {
                        ip: peer_info.ip,
                        port: peer_info.port,
                        control_rx: None,
                        state: None,
                        last_initiated_at: None,
                        handle: None,
                    },
                )
            })
            .collect::<HashMap<String, Peer>>()
    }

    fn process_event(&mut self, event: peer::PeerControllerEvent, temp_prefix_path: &Path) {
        let piece_count = self.torrent.pieces.len();
        let peer_id = get_peer_id(event.ip.as_str(), event.port);
        let peer = self
            .torrent
            .peers
            .as_mut()
            .unwrap()
            .get_mut(peer_id.as_str())
            .unwrap();
        match event.event {
            peer::PeerEvent::Control(control_rx) => peer.control_rx = Some(control_rx),
            peer::PeerEvent::State(event) => match (event, peer.state.as_mut()) {
                (peer::PeerStateEvent::Init(state), None) => {
                    peer.state = Some(state);
                }
                (peer::PeerStateEvent::FieldChoked(choked), Some(state)) => {
                    state.choked = choked;
                }
                (peer::PeerStateEvent::FieldHave(index), Some(state)) => {
                    let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
                    let had = state_bitfield[index];
                    state_bitfield[index] = true;
                    if !had {
                        self.torrent.pieces[index].have += 1;
                    }
                }
                (peer::PeerStateEvent::FieldBitfield(bitfield), Some(state)) => {
                    let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
                    let mut impacted_piece_indices = vec![];
                    for index in 0..piece_count {
                        let had = state_bitfield[index];
                        state_bitfield[index] = bitfield[index];
                        if !had {
                            impacted_piece_indices.push(index);
                        }
                    }
                    self.torrent
                        .pieces
                        .iter_mut()
                        .filter(|piece| impacted_piece_indices.contains(&piece.index))
                        .for_each(|piece| {
                            piece.have += 1;
                        });
                }
                _ => {}
            },
            peer::PeerEvent::Block(piece_index, begin, data) => {
                let block_id = get_block_id(piece_index, begin);
                let block = self.torrent.blocks.get_mut(&block_id).unwrap();
                let file_index = block.file_index;
                if block.path.is_some() {
                    return;
                }
                let block_path = temp_prefix_path.join(block_id.as_str());
                let _wb = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(block_path.as_path())
                    .unwrap()
                    .write(&data)
                    .unwrap();
                println!(
                    "Written block ({}, {}, {}) at {:?}",
                    piece_index,
                    begin,
                    data.len(),
                    block_path
                );
                block.path = Some(block_path);

                // Check if file is complete.
                let file = self.torrent.files.get_mut(file_index).unwrap();
                let file_path = self.torrent.dest_path.join(file.relative_path.as_path());
                let mut file_completed_blocks = self
                    .torrent
                    .blocks
                    .iter_mut()
                    .filter(|(_block_id, block)| {
                        block.file_index == file_index && block.path.is_some()
                    })
                    .collect::<Vec<(&String, &mut Block)>>();
                file_completed_blocks.sort_by_key(|(_, block)| (block.piece_index, block.begin));
                // If all blocks of the file are done, write them to file.
                if file_completed_blocks.len() == file.block_ids.len() {
                    let mut file_object = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(file_path.as_path())
                        .unwrap();
                    for (block_id, block) in file_completed_blocks {
                        let mut block_file = fs::OpenOptions::new()
                            .read(true)
                            .open(block.path.as_ref().unwrap().as_path())
                            .unwrap();
                        let bytes_copied = io::copy(&mut block_file, &mut file_object).unwrap();
                        assert_eq!(bytes_copied, (block.length) as u64);
                        fs::remove_file(block.path.as_ref().unwrap().as_path()).unwrap();
                        println!("Written block {block_id} to file {:?}", file_path);
                    }
                    println!("Written file {} to {:?}", file_index, file_path);
                    file.path = Some(file_path);
                }
                self.send_torrent_controller_event();
            }
        }
    }

    fn enqueue_pending_blocks_to_peers(
        &mut self,
        event_tx: Sender<peer::PeerControllerEvent>,
        count: usize,
        end_game_mode: bool,
        concurrent_peers: usize,
    ) {
        let mut pending_blocks = self
            .torrent
            .blocks
            .iter_mut()
            .filter(|(_block_id, block)| {
                block.path.is_none()
                    && (block.last_requested_at.is_none()
                        || SystemTime::now()
                            .duration_since(*block.last_requested_at.as_ref().unwrap())
                            .unwrap()
                            > Duration::from_millis(BLOCK_REQUEST_TIMEOUT_MS))
            })
            .collect::<Vec<(&String, &mut Block)>>();
        pending_blocks
            .sort_by_key(|(_block_id, block)| self.torrent.pieces[block.piece_index].have);

        for (_block_id, block) in pending_blocks.into_iter().take(count) {
            let peers_with_current_block = self
                .torrent
                .peers
                .as_mut()
                .unwrap()
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
                self.torrent
                    .peers
                    .as_mut()
                    .unwrap()
                    .iter_mut()
                    .filter(|(_peer_id, peer)| {
                        peer.control_rx.is_none()
                            && (peer.last_initiated_at.is_none()
                                || SystemTime::now()
                                    .duration_since(*peer.last_initiated_at.as_ref().unwrap())
                                    .unwrap()
                                    > Duration::from_millis(PEER_REQUEST_TIMEOUT_MS))
                    })
                    .take(concurrent_peers)
                    .for_each(|(_peer_id, peer)| {
                        let event_tx = event_tx.clone();
                        let ip = peer.ip.clone();
                        let port = peer.port;
                        let torrent_info_hash = self.torrent.hash;
                        let client_peer_id = self.torrent.client_id;
                        let pieces_count = self.torrent.pieces.len();
                        peer.handle = Some(thread::spawn(move || {
                            peer::PeerController::new(
                                ip,
                                port,
                                event_tx,
                                torrent_info_hash,
                                client_peer_id,
                                pieces_count,
                            )
                            .start_exchange()
                            .unwrap();
                        }));
                        peer.last_initiated_at = Some(SystemTime::now());
                    });
            } else {
                for (_peer_id, peer) in peers_with_current_block {
                    // Request a block with only one peer unless we are in end-game
                    match peer.control_rx.as_ref().unwrap().send(
                        peer::PeerControlCommand::PieceBlockRequest(
                            block.piece_index,
                            block.begin,
                            block.length,
                        ),
                    ) {
                        // Peer has become unreachabnle,
                        Ok(_) => {
                            block.last_requested_at = Some(SystemTime::now());
                            if !end_game_mode {
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

    fn send_torrent_controller_event(&self) {
        self.event_tx
            .send(TorrentControllerEvent {
                hash: self.torrent.hash,
                event: TorrentEvent::State(TorrentState {
                    files_total: self.torrent.files.len(),
                    files_completed: self
                        .torrent
                        .files
                        .iter()
                        .filter(|f| f.path.is_some())
                        .count(),
                    blocks_total: self.torrent.blocks.len(),
                    blocks_completed: self
                        .torrent
                        .blocks
                        .iter()
                        .filter(|(_, block)| block.path.is_some())
                        .count(),
                    peers_total: self.torrent.peers.as_ref().unwrap().len(),
                    peers_connected: self
                        .torrent
                        .peers
                        .as_ref()
                        .unwrap()
                        .iter()
                        .filter(|(_, p)| p.control_rx.is_some())
                        .count(),
                }),
            })
            .unwrap();
    }
}

fn get_peer_id(ip: &str, port: u16) -> String {
    format!("{}:{}", ip, port)
}

fn get_block_id(index: usize, begin: usize) -> String {
    format!("{:05}_{:08}", index, begin)
}
