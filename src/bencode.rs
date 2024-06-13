use crate::models;
use crate::utils;
use std::str;

use bendy::decoding::{self, Decoder, Object};

// "announce" key
const ANNOUNCE_KEY: [u8; 8] = [97, 110, 110, 111, 117, 110, 99, 101];
// "info" key
const INFO_KEY: [u8; 4] = [105, 110, 102, 111];
// "name" key
const NAME_KEY: [u8; 4] = [110, 97, 109, 101];
// "length" key
const LENGTH_KEY: [u8; 6] = [108, 101, 110, 103, 116, 104];
// "piece length" key
const PIECE_LENGTH_KEY: [u8; 12] = [112, 105, 101, 99, 101, 32, 108, 101, 110, 103, 116, 104];
// "pieces" key
const PIECES_KEY: [u8; 6] = [112, 105, 101, 99, 101, 115];
// "peers" key
const PEERS_KEY: [u8; 5] = [112, 101, 101, 114, 115];
// "peer id" key
const PEER_ID_KEY: [u8; 7] = [112, 101, 101, 114, 32, 105, 100];
// "ip" key
const IP_KEY: [u8; 2] = [105, 112];
// "port" key
const PORT_KEY: [u8; 4] = [112, 111, 114, 116];

pub fn decode_metainfo(metainfo: &[u8]) -> models::MetaInfo {
    let mut decoder = Decoder::new(metainfo);
    let mut info_hash: Option<String> = None;
    let mut tracker: Option<&str> = None;
    let mut file_name: Option<&str> = None;
    let mut file_length: Option<u64> = None;
    let mut piece_length: Option<u64> = None;
    let mut piece_hashes: Vec<[u8; models::PIECE_HASH_BYTE_LEN]> = vec![];
    if let Ok(Some(decoding::Object::Dict(mut metainfo_object))) = decoder.next_object() {
        while let Ok(Some((key, value))) = metainfo_object.next_pair() {
            if key == ANNOUNCE_KEY {
                if let Object::Bytes(value) = value {
                    tracker = Some(str::from_utf8(value).unwrap());
                }
            }
            if key == INFO_KEY {
                if let Object::Dict(mut value) = value {
                    while let Ok(Some((key, value))) = value.next_pair() {
                        if key == NAME_KEY {
                            if let Object::Bytes(value) = value {
                                file_name = Some(str::from_utf8(value).unwrap());
                            }
                        }
                        if key == LENGTH_KEY {
                            if let Object::Integer(value) = value {
                                file_length = Some(value.parse::<u64>().unwrap());
                            }
                        }
                        if key == PIECE_LENGTH_KEY {
                            if let Object::Integer(value) = value {
                                piece_length = Some(value.parse::<u64>().unwrap());
                            }
                        }
                        if key == PIECES_KEY {
                            if let Object::Bytes(value) = value {
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
                        }
                    }
                    info_hash = Some(utils::bytes_to_hex_encoding(&utils::sha1_hash(
                        value.into_raw().unwrap(),
                    )));
                }
            }
        }
    }
    models::MetaInfo {
        tracker: tracker.unwrap().to_string(),
        files: vec![models::File {
            name: file_name.unwrap().to_string(),
            length: file_length.unwrap(),
        }],
        pieces: piece_hashes
            .into_iter()
            .map(|hash| models::Piece {
                hash,
                length: piece_length.unwrap(),
            })
            .collect(),
        info_hash: info_hash.unwrap(),
    }
}

pub fn decode_peers(announce_response: &[u8]) -> Vec<models::Peer> {
    let mut decoder = Decoder::new(announce_response);
    let mut peers: Vec<models::Peer> = vec![];
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
                            peers.push(models::Peer {
                                ip: ip.unwrap(),
                                port: port.unwrap(),
                            });
                        }
                    }
                    Object::Bytes(peers_binary_object) => {
                        let mut offset = 0;
                        while offset < peers_binary_object.len() {
                            peers.push(models::Peer {
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
