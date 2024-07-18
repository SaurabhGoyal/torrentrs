pub(crate) fn get_peer_id(ip: &str, port: u16) -> String {
    format!("{}:{}", ip, port)
}

pub(crate) fn get_block_id(index: usize, begin: usize) -> String {
    format!("{:05}_{:08}", index, begin)
}
