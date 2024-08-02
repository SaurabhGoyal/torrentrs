mod models;

pub(crate) use models::{ClientControlCommand, ClientState as _, TorrentState};
use rand::{Rng as _, RngCore as _};
use std::path::PathBuf;
use std::{collections::HashMap, fs, io::Read};

use crate::utils;
use crate::{bencode, torrent_async};

pub struct Client {
    state: models::ClientState,
    controllers: HashMap<String, torrent_async::Controller>,
    pending_commands: Vec<models::ClientControlCommand>,
}

impl Client {
    pub fn new(data_dir: &str) -> anyhow::Result<Self> {
        Ok(Self {
            state: init_state(data_dir)?,
            pending_commands: vec![],
            controllers: HashMap::new(),
        })
    }

    pub fn add_command(&mut self, cmd: ClientControlCommand) {
        self.pending_commands.push(cmd);
    }

    pub async fn listen(&mut self) -> anyhow::Result<Option<models::ClientState>> {
        Ok(self
            .process_commands()
            .await?
            .or(self.process_pending_torrents().await?))
    }

    async fn process_commands(&mut self) -> anyhow::Result<Option<models::ClientState>> {
        if let Some(cmd) = self.pending_commands.pop() {
            match cmd {
                models::ClientControlCommand::AddTorrent(file_path, dest_path) => {
                    self.add_torrent(file_path.as_str(), dest_path.as_str())
                        .await?;
                }
            }
            return Ok(Some(self.state.clone()));
        }
        Ok(None)
    }

    async fn process_pending_torrents(&mut self) -> anyhow::Result<Option<models::ClientState>> {
        let mut torrent_update_info = None;
        let pending_torrents = self
            .controllers
            .keys()
            .filter(|torrent_hash| {
                let torrent = self.state.torrents.get(*torrent_hash).unwrap();
                torrent.state.is_none()
                    || !matches!(
                        torrent.state.as_ref().unwrap().status,
                        models::TorrentStatus::Completed
                    )
            })
            .map(|h| h.to_string())
            .collect::<Vec<String>>();
        for torrent_hash in pending_torrents.iter() {
            if let Some(torrent_state) = self.resume_torrent(torrent_hash.to_string()).await? {
                torrent_update_info = Some((torrent_hash, torrent_state));
                break;
            }
        }
        Ok(torrent_update_info.map(|(hash, state)| {
            self.state.torrents.get_mut(hash).unwrap().state = Some(state);
            self.state.clone()
        }))
    }

    async fn add_torrent(&mut self, file_path: &str, dest_path: &str) -> anyhow::Result<()> {
        // Read torrent file and parse metadata
        let mut file = fs::OpenOptions::new().read(true).open(file_path)?;
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf)?;
        let metainfo = bencode::decode_metainfo(buf.as_slice())?;
        let hash = utils::bytes_to_hex_encoding(&metainfo.info_hash);
        if !self.state.torrents.contains_key(&hash) {
            // If meta has been fetched, torrent is valid, persist it into client state.
            let torrent = models::Torrent {
                file_path: file_path.to_string(),
                dest_path: dest_path.to_string(),
                name: match metainfo.directory {
                    Some(ref dir) => dir.to_str().unwrap().to_string(),
                    None => metainfo.files[0]
                        .relative_path
                        .to_str()
                        .unwrap()
                        .to_string(),
                },
                metainfo: metainfo.clone(),
                state: None,
            };
            self.state
                .torrents
                .insert(utils::bytes_to_hex_encoding(&metainfo.info_hash), torrent);
        }
        Ok(())
    }

    async fn resume_torrent(
        &mut self,
        torrent_hash: String,
    ) -> anyhow::Result<Option<TorrentState>> {
        let torrent = self.state.torrents.get_mut(torrent_hash.as_str()).unwrap();
        if !self.controllers.contains_key(torrent_hash.as_str()) {
            self.controllers.insert(
                torrent_hash.clone(),
                torrent_async::Controller::new(
                    self.state.peer_id,
                    &torrent.metainfo,
                    torrent.dest_path.as_str(),
                )?,
            );
        }
        let mut controller = self.controllers.remove(torrent_hash.as_str()).unwrap();
        let handle = tokio::spawn(async move {
            let torrent_event = controller.listen().await?;
            let mut torrent_state = None;
            if let Some(torrent_event) = torrent_event {
                match torrent_event {
                    torrent_async::Event::State(state) => {
                        torrent_state = Some(models::TorrentState {
                            files: state.files,
                            peers: state.peers,
                            blocks: state.blocks,
                            downloaded_window_second: (0, 0),
                            status: models::TorrentStatus::Downloading,
                        });
                    }
                    torrent_async::Event::Metric(_) => {}
                }
            }
            Ok::<(torrent_async::Controller, Option<TorrentState>), anyhow::Error>((
                controller,
                torrent_state,
            ))
        });
        let (controller, event) = handle.await??;
        self.controllers.insert(torrent_hash, controller);
        Ok(event)
    }
}

fn create_peer_id() -> [u8; models::PEER_ID_BYTE_LEN] {
    let mut peer_id = [0_u8; models::PEER_ID_BYTE_LEN];
    let (clinet_info, rest) = peer_id.split_at_mut(8);
    clinet_info.copy_from_slice("-rS0001-".as_bytes());
    rand::thread_rng().fill_bytes(rest);
    peer_id
}

fn init_state(data_dir: &str) -> anyhow::Result<models::ClientState> {
    // Create data directory for holding client data if it doesn't already exist.
    let data_dir = PathBuf::from(data_dir);
    fs::create_dir_all(data_dir.as_path())?;

    // Initialise state from data db if present.
    let state = match fs::OpenOptions::new()
        .read(true)
        .open(data_dir.join(models::DB_FILE).as_path())
    {
        Ok(mut db_file) => {
            let mut buf: Vec<u8> = vec![];
            db_file.read_to_end(&mut buf)?;
            serde_json::from_slice::<models::ClientState>(&buf)?
        }
        Err(_) => {
            let peer_id = create_peer_id();
            models::ClientState {
                data_dir,
                peer_id,
                torrents: HashMap::new(),
            }
        }
    };
    Ok(state)
}
