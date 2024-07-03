use std::{env, thread};

mod bencode;
mod client;
mod peer;
mod torrent;
mod utils;

fn main() {
    let args = env::args().collect::<Vec<String>>();
    let (mut client, control_tx) = client::Client::new();
    let handle = thread::spawn(move || client.start());
    let mut index = 1;
    while index + 1 < args.len() {
        control_tx
            .send(client::ClientControlCommand::AddTorrent(
                args[index].clone(),
                args[index + 1].clone(),
            ))
            .unwrap();
        index += 2;
    }
    handle.join().unwrap();
}
