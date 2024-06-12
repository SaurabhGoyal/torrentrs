#[derive(Debug)]
enum TorrentError {}

type Id = u64;

#[derive(Debug)]
pub struct File {}

#[derive(Debug)]
pub struct Piece {}

#[derive(Debug)]
pub struct MetaInfo {
    pub tracker: String,
    pub files: Vec<File>,
    pub pieces: Vec<Piece>,
}

#[derive(Debug)]
pub struct Torrent {
    id: Id,
    meta: MetaInfo,
    peers: Vec<String>,
}

pub fn add(meta: MetaInfo) -> Torrent {
    todo!()
}
