use std::{env, time::Duration};

mod bencode;
mod client;
mod client_async;
mod peer;
mod peer_async;
mod torrent;
mod torrent_async;
mod utils;

#[tokio::main]
async fn main() {
    let args = env::args().collect::<Vec<String>>();
    let mut client = client_async::Client::new(args[1].as_str()).unwrap();
    let mut index = 2;
    while index + 1 < args.len() {
        client.add_command(client_async::ClientControlCommand::AddTorrent(
            args[index].clone(),
            args[index + 1].clone(),
        ));
        index += 2;
    }
    while let Some(state) = client.listen().await.unwrap() {
        client_async::render::clear_screen();
        println!("{}", client_async::render::view_state(state));
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
