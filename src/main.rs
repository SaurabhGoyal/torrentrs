use std::{env, thread};

mod bencode;
mod client;
mod models;
mod peer;
mod peer_copy;
mod torrent;
mod utils;
mod writer;

fn main() {
    let args = env::args().collect::<Vec<String>>();
    let mut client = client::Client::new();
    let handle = thread::spawn(move || {
        // let tor = client
        //     .add_torrent(&args[1])
        //     .expect("error in adding torrent file path");
        // println!("{:?}", tor.peers);
        // client.start_torrent(tor, &args[2]).unwrap();
        let tor = client.add_torrent_2(&args[1], &args[2]).unwrap();
    });
    handle.join().unwrap();
}
