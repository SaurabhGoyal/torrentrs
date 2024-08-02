mod disk_checker;
mod event_processor;
mod formatter;
mod models;
mod state;

use std::{collections::HashMap, path::PathBuf};

use models::{BlockStatus, Torrent, INFO_HASH_BYTE_LEN, PEER_ID_BYTE_LEN};

use crate::{bencode, peer_async};

#[derive(Debug)]
pub struct Controller {
    torrent: Torrent,
    peer_controllers: HashMap<String, peer_async::Controller>,
    disk_checker: disk_checker::DiskChecker,
}

#[derive(Debug, Clone)]
pub struct State {
    pub files: Vec<(String, bool, bool)>,
    pub blocks: Vec<(String, bool, bool)>,
    pub peers: Vec<(String, bool)>,
}

#[derive(Debug)]
pub enum ControlCommand {}

#[derive(Debug)]
pub enum Metric {
    DownloadWindowSecond(u64, usize),
}

#[derive(Debug)]
pub enum Event {
    State(State),
    Metric(Metric),
}

#[derive(Debug)]
pub struct ControllerEvent {
    pub hash: [u8; INFO_HASH_BYTE_LEN],
    pub event: Event,
}

impl Controller {
    pub fn new(
        client_id: [u8; PEER_ID_BYTE_LEN],
        meta: &bencode::MetaInfo,
        dest_path: &str,
    ) -> anyhow::Result<Self> {
        let torrent = models::Torrent::new(client_id, dest_path, meta);
        let disk_checker =
            disk_checker::DiskChecker::new(torrent.get_temp_dir_path(), PathBuf::from(dest_path))?;
        Ok(Self {
            torrent,
            disk_checker,
            peer_controllers: HashMap::new(),
        })
    }

    pub async fn listen(&mut self) -> anyhow::Result<Option<Event>> {
        Ok(self
            .disk_checker
            .sync_with_disk(&self.torrent)
            .await?
            .map(|disk_checker_event| {
                match disk_checker_event {
                    disk_checker::Event::BlockPersistedSeparately(block_id, path) => {
                        let block = self.torrent.blocks.get_mut(&block_id).unwrap();
                        block.data_status = BlockStatus::PersistedSeparately(path);
                        block.verified = false;
                    }
                    disk_checker::Event::FilePersistedPermanently(file_index, path) => {
                        let file = self.torrent.files.get_mut(file_index).unwrap();
                        file.path = Some(path);
                        for block_id in file.block_ids.iter() {
                            let block = self.torrent.blocks.get_mut(block_id).unwrap();
                            block.data_status = BlockStatus::PersistedInFile;
                        }
                    }
                };
                self.get_event_from_state()
            })
            .or(self.process_pending_peer_messages().await?))
    }

    pub async fn process_pending_peer_messages(&mut self) -> anyhow::Result<Option<Event>> {
        let mut peer_handles = vec![];
        let peer_ids = self
            .peer_controllers
            .keys()
            .map(|k| k.to_string())
            .collect::<Vec<String>>();
        for peer_id in peer_ids {
            let mut controller = self.peer_controllers.remove(&peer_id).unwrap();
            peer_handles.push(tokio::spawn(async move {
                Ok::<Option<peer_async::Event>, anyhow::Error>(controller.listen().await?)
            }));
        }
        let mut any_peer_event = false;
        while let Some(peer_handle) = peer_handles.pop() {
            any_peer_event = any_peer_event || peer_handle.await??.is_some();
        }
        Ok(if any_peer_event {
            Some(self.get_event_from_state())
        } else {
            None
        })
    }

    fn get_event_from_state(&self) -> Event {
        let mut blocks = self
            .torrent
            .blocks
            .iter()
            .map(|(block_id, block)| {
                (
                    block_id.to_string(),
                    match block.data_status {
                        BlockStatus::Pending | BlockStatus::Requested(_) => false,
                        BlockStatus::PersistedSeparately(_) | BlockStatus::PersistedInFile => true,
                    },
                    block.verified,
                )
            })
            .collect::<Vec<(String, bool, bool)>>();
        blocks.sort_by_key(|(block_id, _, _)| block_id.clone());
        let files = self
            .torrent
            .files
            .iter()
            .map(|file| {
                (
                    file.relative_path.to_str().unwrap().to_string(),
                    file.path.is_some(),
                    file.verified,
                )
            })
            .collect::<Vec<(String, bool, bool)>>();
        let peers = self
            .torrent
            .peers
            .iter()
            .map(|(peer_id, _peer)| {
                (
                    peer_id.to_string(),
                    self.peer_controllers.contains_key(peer_id),
                )
            })
            .collect::<Vec<(String, bool)>>();
        Event::State(State {
            files,
            blocks,
            peers,
        })
    }
}
