use rand::{Rng as _, RngCore as _};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinHandle;
use std::path::PathBuf;
use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Write as _},
    sync::mpsc::Sender,
};

use crate::utils;
use crate::{
    bencode,
    torrent,
};

const PEER_ID_BYTE_LEN: usize = 20;
const DB_FILE: &str = "client.json";
const CLIENT_CONTROL_CHANNEL_BUFFER_SIZE: usize = 10;
const CLIENT_INTERNAL_EVENT_CHANNEL_BUFFER_SIZE: usize = 1000;

#[derive(Debug, Deserialize, Serialize)]
enum TorrentStatus {
    Fetching,
    Downloading,
    Completed
}

#[derive(Debug, Deserialize, Serialize)]
struct TorrentState {
    files: Vec<(String, bool, bool)>,
    blocks: Vec<(String, bool, bool)>,
    peers: Vec<(String, bool)>,
    downloaded_window_second: (u64, usize),
}

#[derive(Debug)]
pub enum ClientControlCommand {
    AddTorrent(String, String),
}

#[derive(Debug)]
enum ClientInternalEvent {
    Command(ClientControlCommand),
    TorrentEvent(torrent::ControllerEvent),
}

#[derive(Debug, Deserialize, Serialize)]
struct Torrent {
    file_path: String,
    dest_path: String,
    name: String,
    metainfo: bencode::MetaInfo,
    #[serde(skip)]
    control_tx: Option<Sender<torrent::ControlCommand>>,
    #[serde(skip)]
    handle: Option<JoinHandle<anyhow::Result<()>>>,
    #[serde(skip)]
    state: Option<TorrentState>,
}

#[derive(Deserialize, Serialize)]
pub struct ClientState {
    data_dir: PathBuf,
    peer_id: [u8; PEER_ID_BYTE_LEN],
    torrents: HashMap<String, Torrent>,
}

pub struct Client {
    state: ClientState,
    event_tx: tokio_mpsc::Sender<torrent::ControllerEvent>,
    event_rx: Option<tokio_mpsc::Receiver<torrent::ControllerEvent>>,
    control_rx: Option<tokio_mpsc::Receiver<ClientControlCommand>>,
}

impl Client {
    pub fn new(data_dir: &str) -> anyhow::Result<(Self, tokio_mpsc::Sender<ClientControlCommand>)> {
        let (event_tx, event_rx) = tokio_mpsc::channel::<torrent::ControllerEvent>(CLIENT_INTERNAL_EVENT_CHANNEL_BUFFER_SIZE);
        let (control_tx, control_rx) = tokio_mpsc::channel::<ClientControlCommand>(CLIENT_CONTROL_CHANNEL_BUFFER_SIZE);
        Ok((
            Self {
                state: init_state(data_dir)?,
                event_tx,
                event_rx: Some(event_rx),
                control_rx: Some(control_rx),
            },
            control_tx,
        ))
    }

    pub async fn start(&mut self) -> anyhow::Result<()> {
        // Create channel for handling all activity on client.
        let (internal_event_tx, mut internal_event_rx) = tokio_mpsc::channel::<ClientInternalEvent>(CLIENT_INTERNAL_EVENT_CHANNEL_BUFFER_SIZE);
        
        // Trigger task to redirect commands to internal event channel.
        let internal_event_tx_clone = internal_event_tx.clone();
        let rx = self.control_rx.take().unwrap();
        let command_pipe_handle = tokio::spawn(async move {
            redirect_commands(rx, internal_event_tx_clone).await?;
            Ok::<(), anyhow::Error>(())
        });

        // Trigger task to redirect torrent events to internal event channel.
        let internal_event_tx_clone = internal_event_tx.clone();
        let rx = self.event_rx.take().unwrap();
        let torrent_event_pipe_handle = tokio::spawn(async move {
            redirect_torrent_events(rx, internal_event_tx_clone).await?;
            Ok::<(), anyhow::Error>(())
        });

        // Process events from all tasks
        while let Some(internal_event) =  internal_event_rx.recv().await {
            match internal_event {
                ClientInternalEvent::Command(cmd) => self.process_command(cmd)?,
                ClientInternalEvent::TorrentEvent(controller_event) => self.process_event(controller_event)?,
            }
            self.save_state()?;
            clear_screen();
            println!("{}", self.render_view()?);
        }

        // Wait on all tasks to finish.
        command_pipe_handle.await??;
        torrent_event_pipe_handle.await??;
        Ok(())
    }

    fn process_command(&mut self, cmd: ClientControlCommand) -> anyhow::Result<()> {
        match cmd {
            ClientControlCommand::AddTorrent(file_path, dest_path) => {
                let res =
                    self.add_torrent(file_path.as_str(), dest_path.as_str());
                println!("Add - {file_path} -> {dest_path} = {:?}", res);
            }
        }
        Ok(())
    }

    fn add_torrent(&mut self, file_path: &str, dest_path: &str) -> anyhow::Result<()> {
        // Read torrent file and parse metadata
        let mut file = fs::OpenOptions::new().read(true).open(file_path)?;
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf)?;
        let metainfo = bencode::decode_metainfo(buf.as_slice())?;
        let hash = utils::bytes_to_hex_encoding(&metainfo.info_hash);
        if !self.state.torrents.contains_key(&hash) {
            // If meta has been fetched, torrent is valid, persist it into client state.
            let torrent = Torrent {
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
                control_tx: None,
                handle: None,
                state: None,
            };
            self.state
                .torrents
                .insert(utils::bytes_to_hex_encoding(&metainfo.info_hash), torrent);
        }
        self.resume_torrent(&hash)?;
        Ok(())
    }

    fn resume_torrent(&mut self, torrent_hash: &str) -> anyhow::Result<()> {
        let torrent = self.state.torrents.get_mut(torrent_hash).unwrap();
        let (mut torrent_controller, control_tx) = torrent::Controller::new(
            self.state.peer_id,
            &torrent.metainfo,
            torrent.dest_path.as_str(),
            self.event_tx.clone(),
        )?;
        torrent.control_tx = Some(control_tx);
        torrent.handle = Some(tokio::spawn(async move {torrent_controller.start().await?; Ok::<(), anyhow::Error>(())}));
        Ok(())
    }

    fn process_event(&mut self, event: torrent::ControllerEvent) -> anyhow::Result<()> {
        let torrent = self
            .state
            .torrents
            .get_mut(&utils::bytes_to_hex_encoding(&event.hash))
            .unwrap();
        match (event.event, torrent.state.as_mut()) {
            (torrent::Event::State(state), _) => {
                torrent.state = Some(TorrentState {
                    files: state.files,
                    peers: state.peers,
                    blocks: state.blocks,
                    downloaded_window_second: (0, 0),
                });
            }
            (torrent::Event::Metric(metric), Some(state)) => match metric {
                torrent::Metric::DownloadWindowSecond(second_window, downloaded_bytes) => {
                    state.downloaded_window_second = (second_window, downloaded_bytes);
                }
            },
            _ => {}
        }
        Ok(())
    }

    fn save_state(&self) -> anyhow::Result<()> {
        let data = serde_json::to_vec(&self.state)?;
        let _wb = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.state.data_dir.join(DB_FILE).as_path())
            .unwrap()
            .write(&data)
            .unwrap();
        Ok(())
    }

    fn render_view(&self) -> anyhow::Result<String> {
        let mut torrent_views = vec![];
        for torrent in self.state.torrents.values() {
            let mut status = "Fetching";
            let mut stats = String::from("---");
            if let Some(state) = torrent.state.as_ref() {
                status = "Downloading";
                if state.files.len() == state
                .files
                .iter()
                .filter(|(_, downloaded, verified)| *downloaded && !*verified)
                .count() {
                    status = "Downloaded, verifying files";
                }
                if state.files.len() == state
                .files
                .iter()
                .filter(|(_, downloaded, verified)| *downloaded && *verified)
                .count() {
                    status = "Completed";
                }
                // let mut download_rate = String::from(" - ");
                // if state.downloaded_window_second.0
                //     == SystemTime::now()
                //         .duration_since(SystemTime::UNIX_EPOCH)?
                //         .as_secs()
                // {
                //     download_rate = format!("{} Kbps", state.downloaded_window_second.1 / 1024);
                // }
                // let block_bar = state
                //     .blocks
                //     .iter()
                //     .map(|b| if *b { "\u{007c}" } else { "\u{2506}" })
                //     .collect::<Vec<&str>>()
                //     .join("");

                stats = format!(
                    " - Blocks - total {}, downloaded - {}, verified {}\n - Files - total {}, downloaded - {}, verified {}\n - Peers - total - {}, connected - {}\n",
                    state.blocks.len(),
                    state
                        .blocks
                        .iter()
                        .filter(|(_, downloaded, _)| *downloaded)
                        .count(),
                    state
                        .blocks
                        .iter()
                        .filter(|(_, _, verified)| *verified)
                        .count(),
                    state.files.len(),
                    state
                        .files
                        .iter()
                        .filter(|(_, downloaded, _)| *downloaded)
                        .count(),
                    state
                        .files
                        .iter()
                        .filter(|(_, _, verified)| *verified)
                        .count(),
                    state.peers.len(),
                    state
                        .peers
                        .iter()
                        .filter(|(_, connected)| *connected)
                        .count(),
                                                
                );
            }
            torrent_views.push(format!("[{}] [{}]\n{}\n", status, torrent.name, stats));
        }
        Ok(torrent_views.join("\n"))
    }
}

fn create_peer_id() -> [u8; PEER_ID_BYTE_LEN] {
    let mut peer_id = [0_u8; PEER_ID_BYTE_LEN];
    let (clinet_info, rest) = peer_id.split_at_mut(8);
    clinet_info.copy_from_slice("-rS0001-".as_bytes());
    rand::thread_rng().fill_bytes(rest);
    peer_id
}

fn init_state(data_dir: &str) -> anyhow::Result<ClientState> {
    // Create data directory for holding client data if it doesn't already exist.
    let data_dir = PathBuf::from(data_dir);
    fs::create_dir_all(data_dir.as_path())?;

    // Initialise state from data db if present.
    let state = match fs::OpenOptions::new()
        .read(true)
        .open(data_dir.join(DB_FILE).as_path())
    {
        Ok(mut db_file) => {
            let mut buf: Vec<u8> = vec![];
            db_file.read_to_end(&mut buf)?;
            serde_json::from_slice::<ClientState>(&buf)?
        }
        Err(_) => {
            let peer_id = create_peer_id();
            ClientState {
                data_dir,
                peer_id,
                torrents: HashMap::new(),
            }
        }
    };
    Ok(state)
}

async fn redirect_commands(mut rx: tokio_mpsc::Receiver<ClientControlCommand>, internal_event_tx: tokio_mpsc::Sender<ClientInternalEvent>) -> anyhow::Result<()> {
    while let Some(msg) = rx.recv().await {
        internal_event_tx.send(ClientInternalEvent::Command(msg)).await.unwrap();
    }
    Ok(())
}

async fn redirect_torrent_events(mut rx: tokio_mpsc::Receiver<torrent::ControllerEvent>, internal_event_tx: tokio_mpsc::Sender<ClientInternalEvent>) -> anyhow::Result<()> {
    while let Some(msg) = rx.recv().await {
        internal_event_tx.send(ClientInternalEvent::TorrentEvent(msg)).await.unwrap();
    }
    Ok(())
}

fn clear_screen() {
    print!("{}[2J", 27 as char); // ANSI escape code to clear the screen
    print!("{}[1;1H", 27 as char); // ANSI escape code to move the cursor to the top-left corner
    io::stdout().flush().unwrap(); // Flush stdout to ensure screen is cleared immediately
}
