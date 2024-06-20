use rand::{Rng, RngCore};
use std::{
    fs,
    io::{self, Read},
    sync::{mpsc::channel, Arc, Mutex},
    thread,
    time::Duration,
};

use crate::{bencode, models, peer, torrent, writer};

pub struct Client {
    config: models::ClientConfig,
}

impl Client {
    pub fn new() -> Self {
        let mut peer_id = [0_u8; 20];
        let (clinet_info, rest) = peer_id.split_at_mut(8);
        clinet_info.copy_from_slice("-rS0010-".as_bytes());
        rand::thread_rng().fill_bytes(rest);
        Client {
            config: models::ClientConfig { peer_id },
        }
    }

    pub async fn add_torrent(&mut self, file_path: &str) -> Result<models::TorrentInfo, io::Error> {
        // Read file
        let mut file = fs::OpenOptions::new().read(true).open(file_path)?;
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf)?;
        // Parse metadata_info
        let metainfo = bencode::decode_metainfo(buf.as_slice());
        // Add torrent;
        let tor = torrent::add(metainfo, &self.config)
            .await
            .expect("error in adding torrent from metainfo");
        Ok(tor)
    }

    pub async fn start_torrent(
        &mut self,
        tor: models::TorrentInfo,
        dest_path: &str,
    ) -> Result<models::TorrentInfo, io::Error> {
        let mut handles = vec![];
        let (mut data_writer, data_tx) = writer::DataWriter::new(
            &tor.meta.info_hash,
            dest_path,
            &tor.meta.files,
            &tor.meta.pieces,
        )?;
        let data_tx = Arc::new(Mutex::new(data_tx));
        handles.push(tokio::spawn(async move {
            data_writer.start()?;
            Ok::<(), io::Error>(())
        }));
        let pieces_count = tor.meta.pieces.len();
        let (cmd_tx, cmd_rx) = channel::<peer::PeerCommand>();
        let cmd_rx = Arc::new(Mutex::new(cmd_rx));
        for p in tor.peers.iter() {
            let ip = p.ip.clone();
            let port = p.port;
            let torrent_info_hash = tor.meta.info_hash;
            let client_peer_id = self.config.peer_id;
            let data_tx = data_tx.clone();
            let cmd_rx = cmd_rx.clone();
            handles.push(tokio::spawn(async move {
                peer::Peer::new([0; 20], ip, port)
                    .connect()?
                    .activate(torrent_info_hash, client_peer_id, pieces_count)?
                    .start_exchange(cmd_rx, data_tx)
                    .await?;
                Ok::<(), io::Error>(())
            }));
        }
        let block_length = tor.meta.pieces[0].length >> 1;
        handles.push(tokio::spawn(async move {
            let piece_count = pieces_count;
            for index in 0..piece_count {
                cmd_tx
                    .send(peer::PeerCommand::PeerRequest(index, 0, block_length))
                    .unwrap();
                cmd_tx
                    .send(peer::PeerCommand::PeerRequest(
                        index,
                        block_length,
                        block_length,
                    ))
                    .unwrap();
                if index % 2 == 0 {
                    thread::sleep(Duration::from_secs(30));
                }
                if index % 20 == 0 {
                    break;
                }
            }
            drop(cmd_tx);
            Ok::<(), io::Error>(())
        }));

        for handle in handles {
            if let Err(e) = handle.await {
                println!("Error in handle - {e}");
            }
        }
        Ok(tor)
    }
}
