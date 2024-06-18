use crate::bencode;
use crate::models;
use crate::utils;

#[derive(Debug)]
pub enum TorrentError {
    Unknown,
}

pub async fn add(
    meta: models::MetaInfo,
    client_config: &models::ClientConfig,
) -> Result<models::TorrentInfo, TorrentError> {
    let peers = get_announce_response(&meta, client_config).await;
    Ok(models::TorrentInfo { meta, peers })
}

async fn get_announce_response(
    meta: &models::MetaInfo,
    client_config: &models::ClientConfig,
) -> Vec<models::PeerInfo> {
    let client = reqwest::Client::new();
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
        .query(&[("left", meta.pieces.iter().map(|p| p.length).sum::<u32>())])
        .build()
        .unwrap();
    let res = client.execute(req).await.unwrap().bytes().await.unwrap();
    bencode::decode_peers(res.as_ref())
}
