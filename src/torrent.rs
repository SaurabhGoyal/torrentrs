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
use std::time::Duration;
use std::time::SystemTime;

use crate::bencode;
use crate::models;
use crate::models::MetaInfo;
use crate::peer_copy;
use crate::utils;

const MAX_BLOCK_LENGTH: usize = 1 << 14;
const MAX_CONCURRENT_BLOCKS: usize = 500;
const MIN_PEERS_FOR_DOWNLOAD: usize = 1;
const BLOCK_REQUEST_TIMEOUT_MS: u64 = 6000;
const PEER_REQUEST_TIMEOUT_MS: u64 = 6000;
const BLOCK_SCHEDULER_FREQUENCY_MS: u64 = 500;
const MAX_EVENTS_PER_CYCLE: i32 = 10000;

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
    start_torrent_manager(torrent_state);
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
    let mut files: Vec<models::File> = vec![];
    let mut pieces: Vec<models::Piece> = vec![];
    let mut blocks: HashMap<String, models::Block> = HashMap::new();

    while file_index < meta.files.len() {
        // Find the length that can be added
        let block_length = min(min(MAX_BLOCK_LENGTH, piece_budget), file_budget);
        let block_id = get_block_id(piece_index, begin);
        // We would always hit the block boundary, add block and move block cursor.
        blocks.insert(
            block_id.clone(),
            models::Block {
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
            pieces.push(models::Piece {
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
            files.push(models::File {
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
    let peers = peers_info
        .into_iter()
        .map(|peer_info| {
            (
                get_peer_id(peer_info.ip.as_str(), peer_info.port),
                models::Peer {
                    ip: peer_info.ip,
                    port: peer_info.port,
                    control_rx: None,
                    state: None,
                    last_initiated_at: None,
                    handle: None,
                },
            )
        })
        .collect::<HashMap<String, models::Peer>>();
    let mut dest_path = PathBuf::from(dest_path);
    if let Some(dir_path) = meta.directory.as_ref() {
        dest_path = dest_path.join(dir_path);
    }
    models::TorrentState {
        meta,
        client_id,
        dest_path,
        files,
        pieces,
        blocks,
        peers,
    }
}

fn start_torrent_manager(mut torrent_state: models::TorrentState) {
    let mut end_game_mode = false;
    let mut concurrent_peers = 3;
    let (event_tx, event_rx) = channel::<models::TorrentEvent>();
    let temp_prefix_path = torrent_state.dest_path.join(format!(
        ".tmp_{}",
        utils::bytes_to_hex_encoding(&torrent_state.meta.info_hash)
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
                        process_event(&mut torrent_state, event, temp_prefix_path.as_path());
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
            let pending_blocks_count = torrent_state
                .blocks
                .iter()
                .filter(|(_block_id, block)| block.path.is_none())
                .count();
            let total_block_count = torrent_state.blocks.len();

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
                enqueue_pending_blocks_to_peers(
                    &mut torrent_state,
                    event_tx.clone(),
                    MAX_CONCURRENT_BLOCKS,
                    end_game_mode,
                    concurrent_peers,
                );
            } else {
                println!("all blocks downloaded, closing writer");
                if torrent_state.files.iter().all(|file| file.path.is_some()) {
                    // Delete temp directory
                    fs::remove_dir_all(temp_prefix_path.as_path()).unwrap();
                }
                // Close all peer connections
                for (peer_id, peer) in torrent_state
                    .peers
                    .iter_mut()
                    .filter(|(_peer_id, peer)| peer.handle.is_some())
                {
                    if let Some(control_rx) = peer.control_rx.as_ref() {
                        //  This will close the peer stream, which will close peer listener.
                        let _ = control_rx.send(models::PeerControlCommand::Shutdown);
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

fn process_event(
    torrent_state: &mut models::TorrentState,
    event: models::TorrentEvent,
    temp_prefix_path: &Path,
) {
    let piece_count = torrent_state.pieces.len();
    match event {
        models::TorrentEvent::Peer(ip, port, event) => {
            let peer_id = get_peer_id(ip.as_str(), port);
            let peer = torrent_state.peers.get_mut(peer_id.as_str()).unwrap();
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
                        let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
                        let had = state_bitfield[index];
                        state_bitfield[index] = true;
                        if !had {
                            torrent_state.pieces[index].have += 1;
                        }
                    }
                    (models::PeerStateEvent::FieldBitfield(bitfield), Some(state)) => {
                        let state_bitfield = state.bitfield.get_or_insert(vec![false; piece_count]);
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
                            .iter_mut()
                            .filter(|piece| impacted_piece_indices.contains(&piece.index))
                            .for_each(|piece| {
                                piece.have += 1;
                            });
                    }
                    _ => {}
                },
            }
        }
        models::TorrentEvent::Block(piece_index, begin, data) => {
            let block_id = get_block_id(piece_index, begin);
            let block = torrent_state.blocks.get_mut(&block_id).unwrap();
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
            let file = torrent_state.files.get_mut(file_index).unwrap();
            let file_path = torrent_state.dest_path.join(file.relative_path.as_path());
            let mut file_completed_blocks = torrent_state
                .blocks
                .iter_mut()
                .filter(|(_block_id, block)| block.file_index == file_index && block.path.is_some())
                .collect::<Vec<(&String, &mut models::Block)>>();
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
        }
    }
}

fn enqueue_pending_blocks_to_peers(
    torrent_state: &mut models::TorrentState,
    event_tx: Sender<models::TorrentEvent>,
    count: usize,
    end_game_mode: bool,
    concurrent_peers: usize,
) {
    let mut pending_blocks = torrent_state
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
        .collect::<Vec<(&String, &mut models::Block)>>();
    pending_blocks.sort_by_key(|(_block_id, block)| torrent_state.pieces[block.piece_index].have);

    for (_block_id, block) in pending_blocks.into_iter().take(count) {
        let peers_with_current_block = torrent_state
            .peers
            .iter_mut()
            .filter(|(_peer_id, peer)| {
                peer.control_rx.is_some()
                    && peer.state.is_some()
                    && !peer.state.as_ref().unwrap().choked
                    && peer.state.as_ref().unwrap().bitfield.is_some()
                    && peer.state.as_ref().unwrap().bitfield.as_ref().unwrap()[block.piece_index]
            })
            .collect::<Vec<(&String, &mut models::Peer)>>();
        if peers_with_current_block.len() < MIN_PEERS_FOR_DOWNLOAD {
            torrent_state
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
                .take(concurrent_peers)
                .for_each(|(_peer_id, peer)| {
                    let event_tx = event_tx.clone();
                    let ip = peer.ip.clone();
                    let port = peer.port;
                    let torrent_info_hash = torrent_state.meta.info_hash;
                    let client_peer_id = torrent_state.client_id;
                    let pieces_count = torrent_state.meta.pieces.len();
                    peer.handle = Some(thread::spawn(move || {
                        peer_copy::PeerConnection::new(ip, port, event_tx)
                            .activate(torrent_info_hash, client_peer_id, pieces_count)
                            .unwrap()
                            .start_exchange()
                            .unwrap();
                    }));
                    peer.last_initiated_at = Some(SystemTime::now());
                });
        } else {
            for (_peer_id, peer) in peers_with_current_block {
                // Request a block with only one peer unless we are in end-game
                match peer.control_rx.as_ref().unwrap().send(
                    models::PeerControlCommand::PieceBlockRequest(
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

fn get_peer_id(ip: &str, port: u16) -> String {
    format!("{}:{}", ip, port)
}

fn get_block_id(index: usize, begin: usize) -> String {
    format!("{:05}_{:08}", index, begin)
}
