use std::cmp::min;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::SystemTime;

use crate::bencode;
use crate::models;
use crate::models::MetaInfo;
use crate::peer_copy;
use crate::utils;

const MAX_BLOCK_LENGTH: usize = 1 << 14;
const MAX_CONCURRENT_BLOCKS: usize = 300;
const MIN_PEERS_FOR_DOWNLOAD: usize = 1;
const BLOCK_REQUEST_TIMEOUT_MS: u64 = 60000;
const PEER_REQUEST_TIMEOUT_MS: u64 = 60000;
const BLOCK_SCHEDULER_FREQUENCY_MS: u64 = 10000;

#[derive(Debug)]
pub enum TorrentError {
    Unknown,
}

pub fn add(
    meta: models::MetaInfo,
    client_config: &models::ClientConfig,
) -> Result<models::TorrentInfo, TorrentError> {
    let peers = get_announce_response(&meta, client_config);
    Ok(models::TorrentInfo { meta, peers })
}

pub fn start(
    meta: models::MetaInfo,
    client_config: &models::ClientConfig,
    dest_path: &str,
) -> Result<(), TorrentError> {
    let peers_info = get_announce_response(&meta, client_config);
    println!("Peers {:?}", peers_info);
    let torrent_state = build_torrent_state(meta, peers_info, client_config.peer_id, dest_path);
    let torrent_state = Arc::new(torrent_state);
    let torrent_state_block_scheduler = torrent_state.clone();
    let torrent_state_event_processor = torrent_state.clone();
    let (event_tx, event_rx) = channel::<models::TorrentEvent>();
    let event_tx = Arc::new(Mutex::new(event_tx));
    let block_scheduler_handle = thread::spawn(move || {
        start_block_scheduler(torrent_state_block_scheduler, event_tx);
    });
    let event_processor_handle = thread::spawn(move || {
        start_event_processor(torrent_state_event_processor, event_rx);
    });
    block_scheduler_handle.join().unwrap();
    event_processor_handle.join().unwrap();
    Ok(())
}

fn get_announce_response(
    meta: &models::MetaInfo,
    client_config: &models::ClientConfig,
) -> Vec<models::PeerInfo> {
    let client = reqwest::blocking::Client::new();
    // Workaround for issue with binary data - https://github.com/servo/rust-url/issues/219
    let mut url = reqwest::Url::parse(meta.tracker.as_str()).unwrap();
    url.set_query(Some(
        format!(
            "info_hash={}&peer_id={}",
            utils::bytes_to_hex_encoding(&meta.info_hash),
            utils::bytes_to_hex_encoding(&client_config.peer_id),
        )
        .as_str(),
    ));
    let req = client
        .get(url)
        .query(&[("port", 12457)])
        .query(&[("uploaded", 0)])
        .query(&[("downloaded", 0)])
        .query(&[("left", meta.pieces.iter().map(|p| p.length).sum::<usize>())])
        .build()
        .unwrap();
    let res = client.execute(req).unwrap().bytes().unwrap();
    bencode::decode_peers(res.as_ref())
}

fn build_torrent_state(
    meta: MetaInfo,
    peers_info: Vec<models::PeerInfo>,
    client_id: [u8; models::PEER_ID_BYTE_LEN],
    dest_path: &str,
) -> models::TorrentState {
    let piece_standard_length = meta.pieces[0].length;
    let mut piece_budget = piece_standard_length;
    let mut file_budget = meta.files[0].length as usize;
    let mut begin = 0_usize;
    let mut piece_index = 0_usize;
    let mut piece_length = 0_usize;
    let mut block_ids = vec![];
    let mut file_index = 0_usize;
    let mut files: Vec<Arc<RwLock<models::File>>> = vec![];
    let mut pieces: Vec<Arc<RwLock<models::Piece>>> = vec![];
    let mut blocks: HashMap<String, Arc<RwLock<models::Block>>> = HashMap::new();

    while file_index < meta.files.len() {
        // Find the length that can be added
        let block_length = min(min(MAX_BLOCK_LENGTH, piece_budget), file_budget);
        let block_id = get_block_id(piece_index, begin);
        // We would always hit the block boundary, add block and move block cursor.
        blocks.insert(
            block_id.clone(),
            Arc::new(RwLock::new(models::Block {
                file_index,
                piece_index,
                begin,
                length: block_length,
                path: None,
                last_requested_at: None,
            })),
        );
        begin += block_length;
        piece_length += block_length;
        block_ids.push(block_id.clone());
        piece_budget -= block_length;
        file_budget -= block_length;
        // If we have hit piece boundary or this is the last piece for last file, add piece and move piece cursor.
        if piece_budget == 0 || (file_budget == 0 && file_index == meta.files.len() - 1) {
            pieces.push(Arc::new(RwLock::new(models::Piece {
                index: piece_index,
                length: piece_length,
                have: 0,
            })));
            begin = 0;
            piece_index += 1;
            piece_length = 0;
            piece_budget = piece_standard_length;
        }

        // If we have hit file boundary, add file and move file cursor.
        if file_budget == 0 {
            block_ids.sort();
            let mut file_path = meta.files[file_index].relative_path.clone();
            if let Some(dir_path) = meta.directory.as_ref() {
                file_path = dir_path.join(file_path);
            }
            files.push(Arc::new(RwLock::new(models::File {
                index: file_index,
                relative_path: file_path,
                length: meta.files[file_index].length as usize,
                block_ids: block_ids.clone(),
                path: None,
            })));
            block_ids.clear();
            if file_index + 1 >= meta.files.len() {
                break;
            }
            file_index += 1;
            file_budget = meta.files[file_index].length as usize;
        }
    }
    let peers = peers_info
        .into_iter()
        .map(|peer_info| {
            (
                get_peer_id(peer_info.ip.as_str(), peer_info.port),
                Arc::new(RwLock::new(models::Peer {
                    ip: peer_info.ip,
                    port: peer_info.port,
                    control_rx: None,
                    state: None,
                    last_initiated_at: None,
                    handle: None,
                })),
            )
        })
        .collect::<HashMap<String, Arc<RwLock<models::Peer>>>>();
    let dest_path = PathBuf::from(dest_path);
    let temp_prefix_path = dest_path.join(format!(
        ".tmp_{}",
        utils::bytes_to_hex_encoding(&meta.info_hash)
    ));
    models::TorrentState {
        meta,
        client_id,
        dest_path,
        temp_prefix_path,
        files,
        pieces,
        blocks,
        peers,
    }
}

fn start_block_scheduler(
    torrent_state: Arc<models::TorrentState>,
    event_tx: Arc<Mutex<Sender<models::TorrentEvent>>>,
) {
    let mut end_game_mode = false;
    let mut concurrent_peers = 3;
    loop {
        {
            let mut requested_blocks: HashMap<String, (usize, SystemTime)> = HashMap::new();
            let mut initiated_peers: HashMap<String, (JoinHandle<()>, SystemTime)> = HashMap::new();
            let mut unreachable_peers: HashSet<String> = HashSet::new();

            let mut candidate_blocks = torrent_state
                .blocks
                .iter()
                .filter(|(_block_id, block)| {
                    let block = block.read().unwrap();
                    block.path.is_none()
                        && (block.last_requested_at.is_none()
                            || SystemTime::now()
                                .duration_since(*block.last_requested_at.as_ref().unwrap())
                                .unwrap()
                                > Duration::from_millis(BLOCK_REQUEST_TIMEOUT_MS))
                })
                .collect::<Vec<(&String, &Arc<RwLock<models::Block>>)>>();

            candidate_blocks.sort_by_key(|(_block_id, block)| {
                torrent_state.pieces[block.read().unwrap().piece_index]
                    .read()
                    .unwrap()
                    .have
            });

            for (block_id, block) in candidate_blocks.into_iter().take(MAX_CONCURRENT_BLOCKS) {
                let block = block.read().unwrap();
                let candidate_peers = torrent_state
                    .peers
                    .iter()
                    .filter(|(_peer_id, peer)| {
                        let peer = peer.read().unwrap();
                        peer.control_rx.is_some()
                            && peer.state.is_some()
                            && !peer.state.as_ref().unwrap().choked
                            && peer.state.as_ref().unwrap().bitfield.is_some()
                            && peer.state.as_ref().unwrap().bitfield.as_ref().unwrap()
                                [block.piece_index]
                    })
                    .collect::<Vec<(&String, &Arc<RwLock<models::Peer>>)>>();
                if candidate_peers.len() < MIN_PEERS_FOR_DOWNLOAD {
                    torrent_state
                        .peers
                        .iter()
                        .filter(|(_peer_id, peer)| {
                            let peer = peer.read().unwrap();
                            peer.control_rx.is_none()
                                && (peer.last_initiated_at.is_none()
                                    || SystemTime::now()
                                        .duration_since(*peer.last_initiated_at.as_ref().unwrap())
                                        .unwrap()
                                        > Duration::from_millis(PEER_REQUEST_TIMEOUT_MS))
                        })
                        .take(concurrent_peers)
                        .for_each(|(peer_id, peer)| {
                            let peer = peer.read().unwrap();
                            if peer.control_rx.is_some() || initiated_peers.contains_key(peer_id) {
                                return;
                            }
                            let event_tx = event_tx.clone();
                            let ip = peer.ip.clone();
                            let port = peer.port;
                            let torrent_info_hash = torrent_state.meta.info_hash;
                            let client_peer_id = torrent_state.client_id;
                            let pieces_count = torrent_state.meta.pieces.len();
                            let peer_handle = thread::spawn(move || {
                                peer_copy::PeerConnection::new(ip, port, event_tx)
                                    .activate(torrent_info_hash, client_peer_id, pieces_count)
                                    .unwrap()
                                    .start_exchange()
                                    .unwrap();
                            });
                            initiated_peers
                                .insert(peer_id.to_string(), (peer_handle, SystemTime::now()));
                        });
                } else {
                    for (peer_id, peer) in candidate_peers {
                        // Request a block with only one peer unless we are in end-game
                        if !requested_blocks.contains_key(block_id) || end_game_mode {
                            let peer = peer.read().unwrap();
                            match peer.control_rx.as_ref().unwrap().send(
                                models::PeerControlCommand::PieceBlockRequest(
                                    block.piece_index,
                                    block.begin,
                                    block.length,
                                ),
                            ) {
                                // Peer has become unreachabnle,
                                Ok(_) => {
                                    requested_blocks.insert(
                                        block_id.to_string(),
                                        (block.piece_index, SystemTime::now()),
                                    );
                                }
                                Err(_) => {
                                    unreachable_peers.insert(peer_id.to_string());
                                }
                            }
                        }
                    }
                }
            }
            for (block_id, (piece_index, requested_at)) in requested_blocks {
                let mut block = torrent_state
                    .blocks
                    .get(&block_id)
                    .unwrap()
                    .write()
                    .unwrap();
                block.last_requested_at = Some(requested_at);
            }
            for (peer_id, (handle, initiated_at)) in initiated_peers {
                let mut peer = torrent_state.peers.get(&peer_id).unwrap().write().unwrap();
                peer.handle = Some(handle);
                peer.last_initiated_at = Some(initiated_at);
            }
            for peer_id in unreachable_peers {
                let mut peer = torrent_state.peers.get(&peer_id).unwrap().write().unwrap();
                peer.control_rx = None;
                peer.state = None;
            }
            let completed_blocks = torrent_state
                .blocks
                .iter()
                .filter(|(_block_id, block)| block.read().unwrap().path.is_some())
                .map(|(block_id, _block)| block_id)
                .collect::<Vec<&String>>();
            let pending_blocks = torrent_state
                .blocks
                .iter()
                .filter(|(_block_id, block)| block.read().unwrap().path.is_none())
                .map(|(block_id, _block)| block_id)
                .collect::<Vec<&String>>();

            if pending_blocks.is_empty() {
                println!("all blocks downloaded, closing writer");
                // Delete tmp data
                fs::remove_dir_all(torrent_state.temp_prefix_path.as_path()).unwrap();
                // Close all peer connections
                for (_peer_id, peer) in torrent_state
                    .peers
                    .iter()
                    .filter(|(_peer_id, peer)| peer.read().unwrap().control_rx.is_some())
                {
                    let mut peer = peer.write().unwrap();
                    //  This will close the peer stream, which will close peer listener.
                    peer.control_rx
                        .as_ref()
                        .unwrap()
                        .send(models::PeerControlCommand::Shutdown)
                        .unwrap();
                    //  This will close the peer writer.
                    peer.control_rx = None;
                    peer.state = None;
                }
                println!("Waiting for peer threads to close");
                for (_peer_id, peer) in torrent_state
                    .peers
                    .iter()
                    .filter(|(_peer_id, peer)| peer.read().unwrap().handle.is_some())
                {
                    let mut peer = peer.write().unwrap();
                    let handle = peer.handle.take().unwrap();
                    //  This will close the peer stream, which will close peer listener.
                    handle.join().unwrap();
                }
                return;
            } else {
                println!(
                    "Blocks - {} / {}",
                    completed_blocks.len(),
                    completed_blocks.len() + pending_blocks.len()
                );
                if completed_blocks.len() < 50 {
                    println!("{:?} are complete", completed_blocks);
                }
                if pending_blocks.len() < 50 {
                    println!("{:?} are pending", pending_blocks);
                }
                if pending_blocks.len() < 3 {
                    end_game_mode = true;
                    concurrent_peers = 10;
                }
            }
        }
        thread::sleep(Duration::from_millis(BLOCK_SCHEDULER_FREQUENCY_MS));
    }
}

fn start_event_processor(
    torrent_state: Arc<models::TorrentState>,
    event_rx: Receiver<models::TorrentEvent>,
) {
    let piece_count = torrent_state.meta.pieces.len();
    fs::create_dir_all(torrent_state.temp_prefix_path.as_path()).unwrap();
    if let Some(dir_path) = torrent_state.meta.directory.as_ref() {
        fs::create_dir_all(dir_path.as_path()).unwrap();
    }
    while let Ok(event) = event_rx.recv() {
        match event {
            models::TorrentEvent::Peer(ip, port, event) => {
                let peer_id = get_peer_id(ip.as_str(), port);
                let mut peer = torrent_state
                    .peers
                    .get(peer_id.as_str())
                    .unwrap()
                    .write()
                    .unwrap();
                match event {
                    models::PeerEvent::Control(control_rx) => peer.control_rx = Some(control_rx),
                    models::PeerEvent::State(event) => match (event, peer.state.as_mut()) {
                        (models::PeerStateEvent::Init(state), None) => {
                            peer.state = Some(state);
                        }
                        (models::PeerStateEvent::FieldChoked(choked), Some(state)) => {
                            state.choked = choked;
                        }
                        (models::PeerStateEvent::FieldHave(index), Some(state)) => {
                            let state_bitfield =
                                state.bitfield.get_or_insert(vec![false; piece_count]);
                            let had = state_bitfield[index];
                            state_bitfield[index] = true;
                            if !had {
                                torrent_state.pieces[index].write().unwrap().have += 1;
                            }
                        }
                        (models::PeerStateEvent::FieldBitfield(bitfield), Some(state)) => {
                            let state_bitfield =
                                state.bitfield.get_or_insert(vec![false; piece_count]);
                            let mut impacted_piece_indices = vec![];
                            for index in 0..piece_count {
                                let had = state_bitfield[index];
                                state_bitfield[index] = bitfield[index];
                                if !had {
                                    impacted_piece_indices.push(index);
                                }
                            }
                            torrent_state
                                .pieces
                                .iter()
                                .filter(|piece| {
                                    impacted_piece_indices.contains(&piece.read().unwrap().index)
                                })
                                .for_each(|piece| {
                                    piece.write().unwrap().have += 1;
                                });
                        }
                        _ => {}
                    },
                }
            }
            models::TorrentEvent::Block(piece_index, begin, data) => {
                let block_id = get_block_id(piece_index, begin);
                let block = torrent_state.blocks.get(&block_id).unwrap().read().unwrap();
                let file_index = block.file_index;
                if block.path.is_some() {
                    continue;
                }
                drop(block);
                let block_path = torrent_state.temp_prefix_path.join(block_id.as_str());
                let _wb = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(block_path.as_path())
                    .unwrap()
                    .write(data.as_slice())
                    .unwrap();

                println!(
                    "Written block ({}, {}, {}) at {:?}",
                    piece_index,
                    begin,
                    data.len(),
                    block_path
                );
                torrent_state
                    .blocks
                    .get(&block_id)
                    .unwrap()
                    .write()
                    .unwrap()
                    .path = Some(block_path);

                // Check if file is complete.
                let file = torrent_state.files[file_index].read().unwrap();
                let file_path = torrent_state.dest_path.join(file.relative_path.as_path());
                let file_block_ids = file.block_ids.as_slice();
                let file_complete = torrent_state
                    .blocks
                    .iter()
                    .filter(|(block_id, _)| file_block_ids.contains(block_id))
                    .all(|(_block_id, block)| block.read().unwrap().path.is_some());
                // If all blocks of the file are done, write them to file.
                if file_complete {
                    let mut file_object = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(file_path.as_path())
                        .unwrap();
                    for block_id in file_block_ids {
                        let block = torrent_state.blocks.get(block_id).unwrap().read().unwrap();
                        let mut block_file = fs::OpenOptions::new()
                            .read(true)
                            .open(block.path.as_ref().unwrap().as_path())
                            .unwrap();
                        let bytes_copied = io::copy(&mut block_file, &mut file_object).unwrap();
                        assert_eq!(bytes_copied, (block.length) as u64);
                        fs::remove_file(block.path.as_ref().unwrap().as_path()).unwrap();
                        println!("Written block {block_id} to file {:?}", file_path);
                    }
                    drop(file);
                    println!("Written file {} to {:?}", file_index, file_path);
                    torrent_state.files[file_index].write().unwrap().path = Some(file_path);
                }
            }
        }
    }
}

fn get_peer_id(ip: &str, port: u16) -> String {
    format!("{}:{}", ip, port)
}

fn get_block_id(index: usize, begin: usize) -> String {
    format!("{:05}_{:08}", index, begin)
}
