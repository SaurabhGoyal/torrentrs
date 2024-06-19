use std::env;

mod bencode;
mod client;
mod models;
mod peer;
mod torrent;
mod utils;
mod writer;

#[tokio::main]
async fn main() {
    let args = env::args().collect::<Vec<String>>();
    let mut client = client::Client::new();
    let handle = tokio::spawn(async move {
        let tor = client
            .add_torrent(&args[1])
            .await
            .expect("error in adding torrent file path");
        println!("{:?}", tor.peers);
        client.start_torrent(tor, &args[2]).await.unwrap();
    });
    handle.await.unwrap();
}
