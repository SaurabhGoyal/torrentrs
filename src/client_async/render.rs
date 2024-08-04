use std::io::{self, Write as _};

use super::models::ClientState;

pub(crate) fn view_state(state: ClientState) -> String {
    let mut torrent_views = vec![];
    for torrent in state.torrents.values() {
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
    torrent_views.join("\n")
}

pub(crate) fn clear_screen() {
    print!("{}[2J", 27 as char); // ANSI escape code to clear the screen
    print!("{}[1;1H", 27 as char); // ANSI escape code to move the cursor to the top-left corner
    io::stdout().flush().unwrap(); // Flush stdout to ensure screen is cleared immediately
}
