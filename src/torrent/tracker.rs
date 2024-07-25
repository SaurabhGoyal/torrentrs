use rand::{Rng as _, RngCore};

use std::{
    net::{IpAddr, ToSocketAddrs as _, UdpSocket},
    time::Duration,
};

use url::Url;

use crate::{bencode, utils};

use super::models::{BlockStatus, Torrent};

const ANNOUNCE_TIMEOUT_SECS: u64 = 60;
const CLIENT_PORT: u16 = 12457;

struct AnnounceRequest {
    url: Url,
    downloaded: u64,
    uploaded: u64,
    left: u64,
    event: u32,
    ip_address: u32,
    key: u32,
    num_want: i32,
    port: u16,
}

pub(super) fn sync_with_tracker(torrent: &Torrent) -> anyhow::Result<Vec<(String, u16)>> {
    let announce_request = build_announce_request(&torrent)?;
    let tracker_peers = match announce_request.url.scheme() {
        "http" | "https" => http_tracker(torrent, &announce_request)?,
        "udp" => udp_tracker(torrent, &announce_request)?,
        s => {
            panic!("URL scheme {s} not supported yet");
        }
    };
    Ok(tracker_peers)
}

fn http_tracker(torrent: &Torrent, req: &AnnounceRequest) -> anyhow::Result<Vec<(String, u16)>> {
    let client = reqwest::blocking::Client::new();
    let mut url = req.url.clone();
    // Workaround for issue with binary data - https://github.com/servo/rust-url/issues/219
    url.set_query(Some(
        format!(
            "info_hash={}&peer_id={}",
            utils::bytes_to_hex_encoding(&torrent.hash),
            utils::bytes_to_hex_encoding(&torrent.client_id),
        )
        .as_str(),
    ));
    let req = client
        .get(url)
        .timeout(Duration::from_secs(ANNOUNCE_TIMEOUT_SECS))
        .query(&[("port", req.port)])
        .query(&[("uploaded", req.uploaded)])
        .query(&[("downloaded", req.downloaded)])
        .query(&[("left", req.left)])
        .build()?;
    let res = client.execute(req)?.bytes()?;
    let peers_info = bencode::decode_peers(res.as_ref())?;
    Ok(peers_info
        .into_iter()
        .map(|peer_info| (peer_info.ip, peer_info.port))
        .collect::<Vec<(String, u16)>>())
}

fn udp_tracker(torrent: &Torrent, req: &AnnounceRequest) -> anyhow::Result<Vec<(String, u16)>> {
    // Bind a socket for remote server connection.
    let socket = UdpSocket::bind(format!("0.0.0.0:{}", CLIENT_PORT))?;
    // Connect to the remote server.
    let url_socket_str = format!(
        "{}:{}",
        req.url.host_str().unwrap(),
        req.url.port().unwrap()
    );
    let url_socket_addr = url_socket_str.to_socket_addrs()?.next().unwrap();
    socket.connect(url_socket_addr)?;
    // Send connection request.
    let mut buf16 = [0_u8; 16];
    let protocol_id: i64 = 0x41727101980;
    let action: i32 = 0;
    let transaction_id: i32 = rand::thread_rng().gen();
    buf16[0..8].copy_from_slice(&protocol_id.to_be_bytes());
    buf16[8..12].copy_from_slice(&action.to_be_bytes());
    buf16[12..16].copy_from_slice(&transaction_id.to_be_bytes());
    socket.send(&buf16)?;
    buf16.fill(0_u8);
    // Parse and validate the response.
    let recved_bytes_count = socket.recv(&mut buf16)?;
    assert_eq!(recved_bytes_count, 16);
    let mut buf4 = [0_u8; 4];
    buf4.copy_from_slice(&buf16[0..4]);
    let res_action = i32::from_be_bytes(buf4);
    assert_eq!(action, res_action);

    buf4.copy_from_slice(&buf16[4..8]);
    let res_transaction_id = i32::from_be_bytes(buf4);
    assert_eq!(transaction_id, res_transaction_id);

    let mut buf8 = [0_u8; 8];
    buf8.copy_from_slice(&buf16[8..16]);
    let connection_id = i64::from_be_bytes(buf8);

    // Make announce call.
    let transaction_id: i32 = rand::thread_rng().gen();
    let action: i32 = 1;
    let mut buf98 = [0_u8; 98];
    buf98[0..8].copy_from_slice(&connection_id.to_be_bytes());
    buf98[8..12].copy_from_slice(&action.to_be_bytes());
    buf98[12..16].copy_from_slice(&transaction_id.to_be_bytes());
    buf98[16..36].copy_from_slice(&torrent.hash);
    buf98[36..56].copy_from_slice(&torrent.client_id);
    buf98[56..64].copy_from_slice(&req.downloaded.to_be_bytes());
    buf98[64..72].copy_from_slice(&req.left.to_be_bytes());
    buf98[72..80].copy_from_slice(&req.uploaded.to_be_bytes());
    buf98[80..84].copy_from_slice(&req.event.to_be_bytes());
    buf98[84..88].copy_from_slice(&req.ip_address.to_be_bytes());
    buf98[88..92].copy_from_slice(&req.key.to_be_bytes());
    buf98[92..96].copy_from_slice(&req.num_want.to_be_bytes());
    buf98[96..98].copy_from_slice(&req.port.to_be_bytes());
    socket.send(&buf98)?;
    let max_seeders_count = 10;
    let mut buf_announce = vec![0_u8; 20 + max_seeders_count * 6];
    // Parse and validate the response.
    let recved_bytes_count = socket.recv(buf_announce.as_mut_slice())?;
    let mut peers = vec![];
    let mut offset = 20;
    let mut buf2 = [0_u8; 2];
    while offset < recved_bytes_count - 6 {
        buf4.copy_from_slice(&buf_announce[offset..offset + 4]);
        let ip = IpAddr::from(buf4).to_string();
        offset += 4;
        buf2.fill(0_u8);
        buf2.copy_from_slice(&buf_announce[offset + 4..offset + 6]);
        let port = u16::from_be_bytes(buf2);
        offset += 2;
        peers.push((ip, port));
    }
    Ok(peers)
}

fn build_announce_request(torrent: &Torrent) -> anyhow::Result<AnnounceRequest> {
    let url = Url::parse(&torrent.tracker)?;
    let (downloaded, left) = torrent
        .blocks
        .iter()
        .fold((0, 0), |(mut d, mut l), (_, block)| {
            if matches!(
                block.data_status,
                BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile
            ) {
                d += 1;
            } else {
                l += 1;
            }
            (d, l)
        });
    Ok(AnnounceRequest {
        url,
        downloaded,
        uploaded: 0,
        left,
        event: 0,
        ip_address: 0,
        key: 0,
        num_want: -1,
        port: CLIENT_PORT,
    })
}
