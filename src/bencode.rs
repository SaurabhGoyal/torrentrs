use crate::models;
use crate::utils;
use std::path::PathBuf;
use std::str;
use std::str::FromStr;
use std::vec;

use bendy::decoding::{self, Decoder, Object};

// "announce" key
const ANNOUNCE_KEY: &[u8] = "announce".as_bytes();
// "announce-list" key
const ANNOUNCE_LIST_KEY: &[u8] = "announce-list".as_bytes();
// "info" key
const INFO_KEY: &[u8] = "info".as_bytes();
// "name" key
const NAME_KEY: &[u8] = "name".as_bytes();
// "path" key
const PATH_KEY: &[u8] = "path".as_bytes();
// "length" key
const LENGTH_KEY: &[u8] = "length".as_bytes();
// "files" key
const FILES_KEY: &[u8] = "files".as_bytes();
// "piece length" key
const PIECE_LENGTH_KEY: &[u8] = "piece length".as_bytes();
// "pieces" key
const PIECES_KEY: &[u8] = "pieces".as_bytes();
// "peers" key
const PEERS_KEY: &[u8] = "peers".as_bytes();
// "peer_id" key
const PEER_ID_KEY: &[u8] = "peer_id".as_bytes();
// "ip" key
const IP_KEY: &[u8] = "ip".as_bytes();
// "port" key
const PORT_KEY: &[u8] = "port".as_bytes();

pub fn decode_metainfo(metainfo: &[u8]) -> models::MetaInfo {
    let mut decoder = Decoder::new(metainfo);
    let mut info_hash: Option<[u8; models::INFO_HASH_BYTE_LEN]> = None;
    let mut tracker: Option<&str> = None;
    let mut primary_file_name: Option<PathBuf> = None;
    let mut primary_file_length: Option<u64> = None;
    let mut files = vec![];
    let mut piece_length: Option<u32> = None;
    let mut piece_hashes: Vec<[u8; models::PIECE_HASH_BYTE_LEN]> = vec![];
    if let Ok(Some(decoding::Object::Dict(mut metainfo_object))) = decoder.next_object() {
        while let Ok(Some((key, value))) = metainfo_object.next_pair() {
            match (key, value) {
                (ANNOUNCE_KEY, Object::Bytes(value)) => {
                    tracker = Some(str::from_utf8(value).unwrap());
                }
                (INFO_KEY, Object::Dict(mut value)) => {
                    while let Ok(Some((key, mut value))) = value.next_pair() {
                        match (key, value) {
                            (NAME_KEY, Object::Bytes(value)) => {
                                primary_file_name = Some(
                                    PathBuf::from_str(str::from_utf8(value).unwrap()).unwrap(),
                                );
                            }
                            (LENGTH_KEY, Object::Integer(value)) => {
                                primary_file_length = Some(value.parse::<u64>().unwrap());
                            }
                            (FILES_KEY, Object::List(mut files_object)) => {
                                while let Ok(Some(Object::Dict(mut file_object))) =
                                    files_object.next_object()
                                {
                                    let mut file_path = None;
                                    let mut file_length = None;
                                    while let Ok(Some((key, value))) = file_object.next_pair() {
                                        match (key, value) {
                                            (PATH_KEY, Object::List(mut path_components_obj)) => {
                                                let mut path_components = PathBuf::new();
                                                while let Ok(Some(Object::Bytes(path_component))) =
                                                    path_components_obj.next_object()
                                                {
                                                    path_components.push(
                                                        str::from_utf8(path_component).unwrap(),
                                                    );
                                                }
                                                file_path = Some(path_components);
                                            }
                                            (LENGTH_KEY, Object::Integer(value)) => {
                                                file_length = Some(value.parse::<u64>().unwrap());
                                            }
                                            _ => {}
                                        }
                                    }
                                    if let (Some(file_path), Some(file_length)) =
                                        (file_path, file_length)
                                    {
                                        files.push(models::FileInfo {
                                            relative_path: file_path,
                                            length: file_length,
                                        });
                                    }
                                }
                            }
                            (PIECE_LENGTH_KEY, Object::Integer(value)) => {
                                piece_length = Some(value.parse::<u32>().unwrap());
                            }
                            (PIECES_KEY, Object::Bytes(value)) => {
                                let mut offset = 0;
                                while offset < value.len() {
                                    let mut hash = [0; models::PIECE_HASH_BYTE_LEN];
                                    hash.clone_from_slice(
                                        &value[offset..offset + models::PIECE_HASH_BYTE_LEN],
                                    );
                                    piece_hashes.push(hash);
                                    offset += models::PIECE_HASH_BYTE_LEN;
                                }
                            }
                            _ => {}
                        }
                    }
                    info_hash = Some(utils::sha1_hash(value.into_raw().unwrap()));
                }
                _ => {}
            }
        }
    }
    let mut directory = None;
    match (primary_file_name, primary_file_length, files.is_empty()) {
        (Some(file_name), Some(file_length), false) => {
            files.push(models::FileInfo {
                relative_path: file_name,
                length: file_length,
            });
        }
        (Some(file_name), _, _) => {
            directory = Some(file_name);
        }
        _ => {}
    }
    models::MetaInfo {
        tracker: tracker.unwrap().to_string(),
        files,
        pieces: piece_hashes
            .into_iter()
            .map(|hash| models::PieceInfo {
                hash,
                length: piece_length.unwrap() as usize,
            })
            .collect(),
        info_hash: info_hash.unwrap(),
        directory,
    }
}

pub fn decode_peers(announce_response: &[u8]) -> Vec<models::PeerInfo> {
    let mut decoder = Decoder::new(announce_response);
    let mut peers: Vec<models::PeerInfo> = vec![];
    if let Ok(Some(decoding::Object::Dict(mut announce_object))) = decoder.next_object() {
        while let Ok(Some((key, peers_object))) = announce_object.next_pair() {
            if key == PEERS_KEY {
                match peers_object {
                    Object::List(mut peers_list_object) => {
                        while let Ok(Some(Object::Dict(mut peer_object))) =
                            peers_list_object.next_object()
                        {
                            let mut ip = None;
                            let mut port = None;
                            while let Ok(Some((key, value))) = peer_object.next_pair() {
                                if key == IP_KEY {
                                    if let Object::Bytes(value) = value {
                                        ip = Some(str::from_utf8(value).unwrap().to_string());
                                    }
                                }
                                if key == PORT_KEY {
                                    if let Object::Integer(value) = value {
                                        port = Some(value.parse::<u16>().unwrap());
                                    }
                                }
                            }
                            peers.push(models::PeerInfo {
                                ip: ip.unwrap(),
                                port: port.unwrap(),
                            });
                        }
                    }
                    Object::Bytes(peers_binary_object) => {
                        let mut offset = 0;
                        while offset < peers_binary_object.len() {
                            peers.push(models::PeerInfo {
                                ip: peers_binary_object[offset..offset + 4]
                                    .iter()
                                    .map(|b| format!("{b}"))
                                    .collect::<Vec<String>>()
                                    .join("."),
                                port: (peers_binary_object[offset + 4] as u16) * 256
                                    + (peers_binary_object[offset + 5] as u16),
                            });
                            offset += 6;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    peers
}
