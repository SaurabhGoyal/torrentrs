use crate::torrent;
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

pub fn decode(metainfo_buf: &[u8]) -> torrent::MetaInfo {
    let mut decoder = Decoder::new(metainfo_buf);
    let mut tracker: Option<&str> = None;
    let mut file_name: Option<&str> = None;
    let mut file_length: Option<u64> = None;
    let mut piece_length: Option<u64> = None;
    let mut piece_hashes: Vec<[u8; torrent::PIECE_HASH_BYTE_LEN]> = vec![];
    let mut pieces: Vec<torrent::Piece> = vec![];
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
                                    let mut hash = [0; torrent::PIECE_HASH_BYTE_LEN];
                                    hash.clone_from_slice(
                                        &value[offset..offset + torrent::PIECE_HASH_BYTE_LEN],
                                    );
                                    piece_hashes.push(hash);
                                    offset += torrent::PIECE_HASH_BYTE_LEN;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    torrent::MetaInfo {
        tracker: tracker.unwrap().to_string(),
        files: vec![torrent::File {
            name: file_name.unwrap().to_string(),
            length: file_length.unwrap(),
        }],
        pieces: piece_hashes
            .into_iter()
            .map(|hash| torrent::Piece {
                hash,
                length: piece_length.unwrap(),
            })
            .collect(),
    }
}
