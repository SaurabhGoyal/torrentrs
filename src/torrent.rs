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

use crate::bencode;
use crate::models;
use crate::peer_copy;
use crate::utils;

const MAX_BLOCK_LENGTH: usize = 1 << 14;
const MIN_PEERS_FOR_DOWNLOAD: usize = 1;

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
    let mut blocks: HashMap<String, models::Block> = HashMap::new();
    for (piece_index, piece_info) in meta.pieces.iter().enumerate() {
        let mut begin = 0;
        while begin < piece_info.length {
            let block_length = min(MAX_BLOCK_LENGTH, piece_info.length - begin);
            blocks.insert(
                get_block_id(piece_index, begin),
                models::Block {
                    // !TODO
                    file_index: 0,
                    piece_index,
                    begin,
                    length: block_length,
                    path: None,
                },
            );
            begin += block_length;
        }
    }
    let torrent = models::Torrent {
        meta,
        dest_path: PathBuf::from(dest_path),
        blocks,
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
    let writer_handle = thread::spawn(move || loop {
        {
            let mut torrent = torrent_arc_writer.lock().unwrap();
            let mut pending = false;
            let mut initiated_peers: HashSet<String> = HashSet::new();
            let mut unreachable_peers: HashSet<String> = HashSet::new();
            for (_block_id, block) in torrent
                .blocks
                .iter()
                .filter(|(_block_id, block)| block.path.is_none())
                .take(5)
            {
                pending = true;
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
                    torrent.peers.iter().for_each(|(peer_id, peer)| {
                        if peer.control_rx.is_some()
                            || initiated_peers.contains(peer_id)
                            || peer.ip != "116.88.97.233"
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
                        initiated_peers.insert(peer_id.to_string());
                    });
                } else {
                    for (peer_id, peer) in candidate_peers {
                        if peer
                            .control_rx
                            .as_ref()
                            .unwrap()
                            .send(models::PeerControlCommand::PieceBlockRequest(
                                block.piece_index,
                                block.begin,
                                block.length,
                            ))
                            .is_err()
                        {
                            // Peer has become unreachabnle,
                            unreachable_peers.insert(peer_id.to_string());
                        }
                    }
                }
            }
            for peer_id in unreachable_peers {
                let peer = torrent.peers.get_mut(&peer_id).unwrap();
                peer.control_rx = None;
                peer.state = None;
            }
            if pending {
                let completed_blocks = torrent
                    .blocks
                    .iter()
                    .filter(|(_block_id, block)| block.path.is_some())
                    .map(|(block_id, _block)| block_id)
                    .collect::<Vec<&String>>();
                let pending_blocks = torrent
                    .blocks
                    .iter()
                    .filter(|(_block_id, block)| block.path.is_none())
                    .map(|(block_id, _block)| block_id)
                    .collect::<Vec<&String>>();
                println!(
                    "Blocks - {} / {}",
                    completed_blocks.len(),
                    completed_blocks.len() + pending_blocks.len()
                );
                if completed_blocks.len() < 100 {
                    println!("{:?} are complete", completed_blocks);
                }
                if pending_blocks.len() < 100 {
                    println!("{:?} are pending", pending_blocks);
                }
            } else {
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
            }
        }
        thread::sleep(Duration::from_millis(9000));
    });
    let torrent_arc_reader = torrent_arc.clone();
    let reader_handle = thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            match event {
                models::TorrentEvent::PeerControlChange(ip, port, control_rx) => {
                    let peer_id = get_peer_id(ip.as_str(), port);
                    let mut torrent = torrent_arc_reader.lock().unwrap();
                    let peer = torrent.peers.get_mut(peer_id.as_str()).unwrap();
                    peer.control_rx = control_rx;
                    if peer.control_rx.is_none() {
                        peer.state = None;
                    }
                }
                models::TorrentEvent::PeerStateChange(ip, port, peer_state) => {
                    let peer_id = get_peer_id(ip.as_str(), port);
                    let mut torrent = torrent_arc_reader.lock().unwrap();
                    let peer = torrent.peers.get_mut(peer_id.as_str()).unwrap();
                    peer.state = peer_state;
                }
                models::TorrentEvent::Block(piece_index, begin, data) => {
                    let block_id = get_block_id(piece_index, begin);
                    let mut torrent = torrent_arc_reader.lock().unwrap();
                    let block = torrent.blocks.get_mut(block_id.as_str()).unwrap();
                    assert_eq!(block.path, None);
                    assert_eq!(block.length, data.len());
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
                    let block = torrent.blocks.get_mut(block_id.as_str()).unwrap();
                    println!(
                        "Written block ({}, {}, {}) at {:?}",
                        block.piece_index, block.begin, block.length, block_path
                    );
                    block.path = Some(block_path);
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
