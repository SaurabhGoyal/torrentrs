use crate::bencode;
use crate::models;

#[derive(Debug)]
pub enum TorrentError {
    Unknown,
}

pub fn add(
    meta: models::MetaInfo,
    client_config: &models::ClientConfig,
) -> Result<models::Torrent, TorrentError> {
    let peers = get_announce_response(&meta, client_config);
    Ok(models::Torrent { meta, peers })
}

fn get_announce_response(
    meta: &models::MetaInfo,
    client_config: &models::ClientConfig,
) -> Vec<models::Peer> {
    let client = reqwest::blocking::Client::new();
    // Workaround for issue with binary data - https://github.com/servo/rust-url/issues/219
    let mut url = reqwest::Url::parse(meta.tracker.as_str()).unwrap();
    url.set_query(Some(
        format!(
            "info_hash={}&peer_id={}",
            meta.info_hash, client_config.peer_id,
        )
        .as_str(),
    ));
    let req = client
        .get(url)
        .query(&[("port", 12457)])
        .query(&[("uploaded", 0)])
        .query(&[("downloaded", 0)])
        .query(&[("left", meta.pieces.iter().map(|p| p.length).sum::<u64>())])
        .build()
        .unwrap();
    let res = client.execute(req).unwrap().bytes().unwrap();
    bencode::decode_peers(res.as_ref())
}
