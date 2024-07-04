use rand::{Rng as _, RngCore};
use std::fmt::Write;
use std::time::SystemTime;
use std::{
    collections::HashMap,
    fs,
    io::{self, Read, Write as _},
    sync::mpsc::{channel, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    bencode,
    torrent::{self, ControllerEvent},
};
// Piece hash byte length
const INFO_HASH_BYTE_LEN: usize = 20;
const PEER_ID_BYTE_LEN: usize = 20;
const MAX_EVENTS_PER_CYCLE: usize = 500;

#[derive(Debug)]
struct TorrentState {
    files_total: usize,
    files_completed: usize,
    blocks_total: usize,
    blocks_completed: usize,
    peers_total: usize,
    peers_connected: usize,
    downloaded_window_second: (u64, usize),
}

#[derive(Debug)]
pub enum ClientControlCommand {
    AddTorrent(String, String),
}

#[derive(Debug)]
struct Torrent {
    name: String,
    control_tx: Option<Sender<torrent::ControlCommand>>,
    handle: Option<JoinHandle<()>>,
    state: Option<TorrentState>,
}

pub struct Client {
    peer_id: [u8; PEER_ID_BYTE_LEN],
    torrents: HashMap<[u8; INFO_HASH_BYTE_LEN], Torrent>,
    event_tx: Sender<ControllerEvent>,
    event_rx: Receiver<ControllerEvent>,
    control_rx: Receiver<ClientControlCommand>,
}

impl Client {
    pub fn new() -> (Self, Sender<ClientControlCommand>) {
        let mut peer_id = [0_u8; PEER_ID_BYTE_LEN];
        let (clinet_info, rest) = peer_id.split_at_mut(8);
        clinet_info.copy_from_slice("-rS0001-".as_bytes());
        rand::thread_rng().fill_bytes(rest);
        let (event_tx, event_rx) = channel::<torrent::ControllerEvent>();
        let (control_tx, control_rx) = channel::<ClientControlCommand>();
        (
            Self {
                peer_id,
                torrents: HashMap::new(),
                event_tx,
                event_rx,
                control_rx,
            },
            control_tx,
        )
    }

    pub fn start(&mut self) -> ! {
        loop {
            {
                // Process commands
                loop {
                    match self.control_rx.try_recv() {
                        Ok(cmd) => match cmd {
                            ClientControlCommand::AddTorrent(file_path, dest_path) => {
                                let res = self.add_torrent(file_path.as_str(), dest_path.as_str());
                                println!("Add - {file_path} -> {dest_path} = {:?}", res);
                            }
                        },
                        Err(e) => match e {
                            std::sync::mpsc::TryRecvError::Empty => {
                                break;
                            }
                            std::sync::mpsc::TryRecvError::Disconnected => todo!(),
                        },
                    }
                }

                // Process some events
                let mut processed_events = 0;
                loop {
                    match self.event_rx.try_recv() {
                        Ok(event) => {
                            self.process_event(event);
                            processed_events += 1;
                            if processed_events > MAX_EVENTS_PER_CYCLE {
                                break;
                            }
                        }
                        Err(e) => match e {
                            std::sync::mpsc::TryRecvError::Empty => {
                                break;
                            }
                            std::sync::mpsc::TryRecvError::Disconnected => todo!(),
                        },
                    }
                }

                // Update torrent threads
                for (_, torrent_obj) in self
                    .torrents
                    .iter_mut()
                    .filter(|(_, torrent_obj)| torrent_obj.handle.is_some())
                    .filter(|(_, torrent_obj)| {
                        let mut files_complete = false;
                        if let Some(state) = torrent_obj.state.as_ref() {
                            files_complete = state.files_completed == state.files_total;
                        }
                        files_complete
                    })
                {
                    let _ = torrent_obj.handle.take().unwrap().join();
                }
            }
            thread::sleep(Duration::from_millis(1000));
        }
    }

    fn add_torrent(&mut self, file_path: &str, dest_path: &str) -> Result<(), io::Error> {
        // Read file
        let mut file = fs::OpenOptions::new().read(true).open(file_path)?;
        let mut buf: Vec<u8> = vec![];
        file.read_to_end(&mut buf)?;
        // Parse metadata_info
        let metainfo = bencode::decode_metainfo(buf.as_slice());
        let (mut torrent_controller, control_tx) =
            torrent::Controller::new(&metainfo, self.peer_id, dest_path, self.event_tx.clone());
        self.torrents.insert(
            metainfo.info_hash,
            Torrent {
                name: match metainfo.directory {
                    Some(dir) => dir.to_str().unwrap().to_string(),
                    None => metainfo.files[0]
                        .relative_path
                        .to_str()
                        .unwrap()
                        .to_string(),
                },
                control_tx: Some(control_tx),
                handle: Some(thread::spawn(move || torrent_controller.start())),
                state: None,
            },
        );
        // Add torrent;
        Ok(())
    }

    fn process_event(&mut self, event: ControllerEvent) {
        let torrent = self.torrents.get_mut(&event.hash).unwrap();
        match (event.event, torrent.state.as_mut()) {
            (torrent::Event::State(state), _) => {
                torrent.state = Some(TorrentState {
                    files_total: state.files_total,
                    files_completed: state.files_completed,
                    blocks_total: state.blocks_total,
                    blocks_completed: state.blocks_completed,
                    peers_total: state.peers_total,
                    peers_connected: state.peers_connected,
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
        clear_screen();
        println!(
            "{}",
            self.torrents.values().fold(String::new(), |mut out, ts| {
                match ts.state.as_ref() {
                    Some(tsu) => {
                        let mut download_rate = String::from(" - ");
                        if tsu.downloaded_window_second.0
                            == SystemTime::now()
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .unwrap()
                                .as_secs()
                        {
                            download_rate =
                                format!("{} Kbps", tsu.downloaded_window_second.1 / 1024);
                        }
                        let _ = writeln!(
                            out,
                            "\n[{}] [{}]\n - Blocks - {}/{} ({}), Files - {}/{}, Peers - {}/{}\n",
                            if tsu.files_completed == tsu.files_total {
                                "Completed"
                            } else {
                                "Downloading"
                            },
                            ts.name,
                            tsu.blocks_completed,
                            tsu.blocks_total,
                            download_rate,
                            tsu.files_completed,
                            tsu.files_total,
                            tsu.peers_connected,
                            tsu.peers_total
                        );
                    }
                    None => {
                        let _ = writeln!(out, "\n[Fetching] [{}]\n -------------", ts.name,);
                    }
                }
                out
            })
        );
    }
}

fn clear_screen() {
    print!("{}[2J", 27 as char); // ANSI escape code to clear the screen
    print!("{}[1;1H", 27 as char); // ANSI escape code to move the cursor to the top-left corner
    io::stdout().flush().unwrap(); // Flush stdout to ensure screen is cleared immediately
}
