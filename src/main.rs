use std::env;

mod bencode;
mod client;
mod peer;
mod torrent;
mod utils;

#[tokio::main]
async fn main() {
    let args = env::args().collect::<Vec<String>>();
    let (mut client, control_tx) = client::Client::new(args[1].as_str()).unwrap();
    let handle = tokio::spawn(async move {
        client.start().await?;
        Ok::<(), anyhow::Error>(())
    });
    let mut index = 2;
    while index + 1 < args.len() {
        control_tx
            .send(client::ClientControlCommand::AddTorrent(
                args[index].clone(),
                args[index + 1].clone(),
            ))
            .await
            .unwrap();
        index += 2;
    }
    // drop(control_tx);
    handle.await.unwrap().unwrap();
}
