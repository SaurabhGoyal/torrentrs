use crate::torrent;
use std::str;

use bendy::decoding::{self, Decoder, Object};

// "announce" key
const TRACKER_KEY: [u8; 8] = [97, 110, 110, 111, 117, 110, 99, 101];

pub fn decode(metainfo_buf: &[u8]) -> torrent::MetaInfo {
    let mut decoder = Decoder::new(metainfo_buf);
    let mut tracker: Option<&str> = None;
    if let Ok(Some(decoding::Object::Dict(mut metainfo_object))) = decoder.next_object() {
        while let Ok(Some((key, value))) = metainfo_object.next_pair() {
            if key == TRACKER_KEY {
                if let Object::Bytes(value) = value {
                    tracker = Some(str::from_utf8(value).unwrap());
                }
            }
        }
    }
    torrent::MetaInfo {
        tracker: tracker.unwrap().to_string(),
        files: vec![],
        pieces: vec![],
    }
}
