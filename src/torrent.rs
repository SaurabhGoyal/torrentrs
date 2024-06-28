use std::cmp::min;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

use crate::bencode;
use crate::models;
use crate::peer_copy;
use crate::utils;

const MAX_BLOCK_LENGTH: usize = 1 << 14;
const MAX_CONCURRENT_BLOCKS: usize = 50;
const MIN_PEERS_FOR_DOWNLOAD: usize = 1;
const BLOCK_REQUEST_TIMEOUT_MS: u64 = 5000;
const PEER_REQUEST_TIMEOUT_MS: u64 = 10000;

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
    let peers = get_announce_response(&meta, client_config);
    println!("Peers {:?}", peers);
    let client_id = client_config.peer_id;
    let piece_count = meta.pieces.len();

    let piece_standard_length = meta.pieces[0].length;
    let mut piece_budget = piece_standard_length;
    let mut file_budget = meta.files[0].length as usize;
    let mut begin = 0_usize;
    let mut piece_index = 0_usize;
    let mut file_index = 0_usize;
    let mut files: Vec<models::File> = vec![];
    let mut pieces: Option<Vec<models::Piece>> = Some(vec![]);
    let mut blocks: Option<HashMap<String, models::Block>> = Some(HashMap::new());
    let mut block_id_file_index_map: HashMap<String, usize> = HashMap::new();

    while file_index < meta.files.len() {
        // Find the length that can be added
        let length = min(min(MAX_BLOCK_LENGTH, piece_budget), file_budget);
        let block_id = get_block_id(piece_index, begin);
        // We would always hit the block boundary, add block and move block cursor.
        blocks.as_mut().unwrap().insert(
            block_id.clone(),
            models::Block {
                file_index,
                piece_index,
                begin,
                length,
                path: None,
                last_requested_at: None,
            },
        );
        block_id_file_index_map.insert(block_id, file_index);
        begin += length;
        piece_budget -= length;
        file_budget -= length;
        // If we have hit piece or file boundary, add piece and move piece cursor.
        if piece_budget == 0 || file_budget == 0 {
            pieces.as_mut().unwrap().push(models::Piece {
                index: piece_index,
                length: blocks
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|(_, block)| block.length)
                    .sum(),
                have: 0,
                blocks: blocks.unwrap(),
            });
            begin = 0;
            piece_index += 1;
            piece_budget = piece_standard_length;
            blocks = Some(HashMap::new());

            // If we have hit file boundary, add file and move file cursor.
            if file_budget == 0 {
                files.push(models::File {
                    index: file_index,
                    relative_path: meta.files[file_index].relative_path.clone(),
                    length: meta.files[file_index].length as usize,
                    pieces: pieces.unwrap(),
                    path: None,
                });
                if file_index + 1 >= meta.files.len() {
                    break;
                }
                file_index += 1;
                file_budget = meta.files[file_index].length as usize;
                pieces = Some(vec![]);
            }
        }
    }
    let block_id_file_index_map = Arc::new(block_id_file_index_map);
    let block_id_file_index_map_clone_reader = block_id_file_index_map.clone();
    let block_id_file_index_map_clone_writer = block_id_file_index_map.clone();

    let torrent = models::Torrent {
        meta,
        dest_path: PathBuf::from(dest_path),
        files,
        peers: peers
            .into_iter()
            .map(|peer_info| {
                (
                    get_peer_id(peer_info.ip.as_str(), peer_info.port),
                    models::Peer {
                        ip: peer_info.ip,
                        port: peer_info.port,
                        control_rx: None,
                        state: None,
                        last_connected_at: None,
                    },
                )
            })
            .collect::<HashMap<String, models::Peer>>(),
    };
    let prefix_path = torrent.dest_path.join(format!(
        ".tmp_{}",
        utils::bytes_to_hex_encoding(&torrent.meta.info_hash)
    ));
    fs::create_dir_all(prefix_path.as_path()).unwrap();
    let (event_tx, event_rx) = channel::<models::TorrentEvent>();
    let event_tx = Arc::new(Mutex::new(event_tx));
    let torrent_arc = Arc::new(Mutex::new(torrent));
    let torrent_arc_writer = torrent_arc.clone();
    let writer_handle = thread::spawn(move || {
        let mut end_game_mode = false;
        let mut concurrent_peers = 3;
        loop {
            {
                let mut torrent = torrent_arc_writer.lock().unwrap();
                let mut requested_blocks: HashMap<String, (usize, SystemTime)> = HashMap::new();
                let mut initiated_peers: HashMap<String, SystemTime> = HashMap::new();
                let mut unreachable_peers: HashSet<String> = HashSet::new();

                let mut candidate_pieces = torrent
                    .files
                    .iter()
                    .flat_map(|file| &file.pieces)
                    .collect::<Vec<&models::Piece>>();
                candidate_pieces.sort_by_key(|piece| -(piece.have as i32));
                for (block_id, block) in candidate_pieces
                    .iter()
                    .flat_map(|piece| piece.blocks.iter())
                    .filter(|(_block_id, block)| {
                        block.path.is_none()
                            && (block.last_requested_at.is_none()
                                || SystemTime::now()
                                    .duration_since(*block.last_requested_at.as_ref().unwrap())
                                    .unwrap()
                                    > Duration::from_millis(BLOCK_REQUEST_TIMEOUT_MS))
                    })
                    .take(MAX_CONCURRENT_BLOCKS)
                {
                    let candidate_peers = torrent
                        .peers
                        .iter()
                        .filter(|(_peer_id, peer)| {
                            peer.control_rx.is_some()
                                && peer.state.is_some()
                                && !peer.state.as_ref().unwrap().choked
                                && peer.state.as_ref().unwrap().bitfield.is_some()
                                && peer.state.as_ref().unwrap().bitfield.as_ref().unwrap()
                                    [block.piece_index]
                        })
                        .collect::<Vec<(&String, &models::Peer)>>();
                    if candidate_peers.len() < MIN_PEERS_FOR_DOWNLOAD {
                        torrent
                            .peers
                            .iter()
                            .filter(|(_peer_id, peer)| {
                                peer.control_rx.is_none()
                                    && (peer.last_connected_at.is_none()
                                        || SystemTime::now()
                                            .duration_since(
                                                *peer.last_connected_at.as_ref().unwrap(),
                                            )
                                            .unwrap()
                                            > Duration::from_millis(PEER_REQUEST_TIMEOUT_MS))
                            })
                            .take(concurrent_peers)
                            .for_each(|(peer_id, peer)| {
                                if peer.control_rx.is_some()
                                    || initiated_peers.contains_key(peer_id)
                                {
                                    return;
                                }
                                let event_tx = event_tx.clone();
                                let ip = peer.ip.clone();
                                let port = peer.port;
                                let torrent_info_hash = torrent.meta.info_hash;
                                let client_peer_id = client_id;
                                let pieces_count = torrent.meta.pieces.len();
                                thread::spawn(move || {
                                    peer_copy::PeerConnection::new(ip, port, event_tx)
                                        .activate(torrent_info_hash, client_peer_id, pieces_count)
                                        .unwrap()
                                        .start_exchange()
                                        .unwrap();
                                });
                                initiated_peers.insert(peer_id.to_string(), SystemTime::now());
                            });
                    } else {
                        for (peer_id, peer) in candidate_peers {
                            // Request a block with only one peer unless we are in end-game
                            if !requested_blocks.contains_key(block_id) || end_game_mode {
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
                    let file_index = block_id_file_index_map_clone_reader.get(&block_id).unwrap();
                    let block = torrent.files[*file_index].pieces[piece_index]
                        .blocks
                        .get_mut(&block_id)
                        .unwrap();
                    block.last_requested_at = Some(requested_at);
                }
                for (peer_id, connected_at) in initiated_peers {
                    let peer = torrent.peers.get_mut(&peer_id).unwrap();
                    peer.last_connected_at = Some(connected_at);
                }
                for peer_id in unreachable_peers {
                    let peer = torrent.peers.get_mut(&peer_id).unwrap();
                    peer.control_rx = None;
                    peer.state = None;
                }
                let completed_blocks = torrent
                    .files
                    .iter()
                    .flat_map(|file| file.pieces.iter())
                    .flat_map(|piece| piece.blocks.iter())
                    .filter(|(_block_id, block)| block.path.is_some())
                    .map(|(block_id, _block)| block_id)
                    .collect::<Vec<&String>>();
                let pending_blocks = torrent
                    .files
                    .iter()
                    .flat_map(|file| file.pieces.iter())
                    .flat_map(|piece| piece.blocks.iter())
                    .filter(|(_block_id, block)| block.path.is_none())
                    .map(|(block_id, _block)| block_id)
                    .collect::<Vec<&String>>();
                if pending_blocks.is_empty() {
                    println!("all blocks downloaded, closing writer");
                    // Close all peer connections
                    for (_peer_id, peer) in torrent
                        .peers
                        .iter_mut()
                        .filter(|(_peer_id, peer)| peer.state.is_some())
                    {
                        //  This will close the peer stream, which will close peer listener.
                        peer.control_rx
                            .as_ref()
                            .unwrap()
                            .send(models::PeerControlCommand::Shutdown)
                            .unwrap();
                        //  This will close the peer writer.
                        peer.control_rx = None;
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
            thread::sleep(Duration::from_millis(1000));
        }
    });
    let torrent_arc_reader = torrent_arc.clone();
    let reader_handle = thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            match event {
                models::TorrentEvent::Peer(ip, port, event) => {
                    let peer_id = get_peer_id(ip.as_str(), port);
                    let mut torrent = torrent_arc_reader.lock().unwrap();
                    let peer = torrent.peers.get_mut(peer_id.as_str()).unwrap();
                    match event {
                        models::PeerEvent::Control(control_rx) => {
                            peer.control_rx = Some(control_rx)
                        }
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
                                    torrent
                                        .files
                                        .iter_mut()
                                        .flat_map(|file| file.pieces.as_mut_slice())
                                        .filter(|piece| piece.index == index)
                                        .for_each(|piece| {
                                            piece.have += 1;
                                        });
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
                                torrent
                                    .files
                                    .iter_mut()
                                    .flat_map(|file| file.pieces.as_mut_slice())
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
                    let file_index = block_id_file_index_map_clone_writer.get(&block_id).unwrap();
                    let mut torrent = torrent_arc_reader.lock().unwrap();
                    let block = torrent.files[*file_index].pieces[piece_index]
                        .blocks
                        .get_mut(&block_id)
                        .unwrap();
                    if block.path.is_some() {
                        continue;
                    }
                    drop(torrent);
                    let block_path = prefix_path.join(block_id.as_str());
                    let _wb = fs::OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(block_path.as_path())
                        .unwrap()
                        .write(data.as_slice())
                        .unwrap();
                    let mut torrent = torrent_arc_reader.lock().unwrap();
                    let block = torrent.files[*file_index].pieces[piece_index]
                        .blocks
                        .get_mut(&block_id)
                        .unwrap();
                    println!(
                        "Written block ({}, {}, {}) at {:?}",
                        block.piece_index, block.begin, block.length, block_path
                    );
                    block.path = Some(block_path);
                    // if complete
                }
            }
        }
    });
    writer_handle.join().unwrap();
    reader_handle.join().unwrap();
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

fn get_peer_id(ip: &str, port: u16) -> String {
    format!("{}:{}", ip, port)
}

fn get_block_id(index: usize, begin: usize) -> String {
    format!("{}_{}", index, begin)
}
