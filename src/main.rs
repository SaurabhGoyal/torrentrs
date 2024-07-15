use std::{env, thread};

mod bencode;
mod client;
mod peer;
mod torrent;
mod utils;

fn main() {
    let args = env::args().collect::<Vec<String>>();
    let (mut client, control_tx) = client::Client::new(args[1].as_str()).unwrap();
    let handle = thread::spawn(move || -> anyhow::Result<()> { client.start() });
    let mut index = 2;
    while index + 1 < args.len() {
        control_tx
            .send(client::ClientControlCommand::AddTorrent(
                args[index].clone(),
                args[index + 1].clone(),
            ))
            .unwrap();
        index += 2;
    }
    // drop(control_tx);
    handle.join().unwrap().unwrap();
}
