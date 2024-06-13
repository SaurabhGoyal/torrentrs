use std::env;

mod bencode;
mod client;
mod models;
mod torrent;
mod utils;

fn main() {
    let args = env::args().collect::<Vec<String>>();
    let mut client = client::Client::new();
    let tor = client
        .add_torrent(&args[1])
        .expect("error in adding torrent file path");
    println!("{:?}", tor.peers)
}
