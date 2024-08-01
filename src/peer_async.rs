use std::net::{IpAddr, SocketAddr};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0_u8; 8];

#[derive(Debug, Clone)]
pub struct State {
    pub choked: bool,
    pub bitfield: Option<Vec<bool>>,
    pub total_downloaded: usize,
    pub downloaded_window_second: (u64, usize),
}

pub enum StateEvent {
    Init(State),
    FieldChoked(bool),
    FieldHave(usize),
    FieldBitfield(Vec<bool>),
}

pub enum Metric {
    DownloadWindowSecond(u64, usize),
}

#[derive(Debug)]
pub enum ControlCommand {
    PieceBlockRequest(usize, usize, usize),
    Shutdown,
}

pub enum Event {
    State(StateEvent),
    Block(usize, usize, Vec<u8>),
    Metric(Metric),
}

#[derive(Debug)]
pub struct Controller {
    ip: String,
    port: u16,
    stream: TcpStream,
    peer_state: State,
    piece_count: usize,
    pending_commands: Vec<ControlCommand>,
}

impl Controller {
    pub async fn new(
        ip: String,
        port: u16,
        torrent_info_hash: [u8; 20],
        client_peer_id: [u8; 20],
        piece_count: usize,
    ) -> anyhow::Result<Self> {
        let mut stream = TcpStream::connect(&SocketAddr::new(ip.parse::<IpAddr>()?, port)).await?;
        let mut handshake_msg = [
            HANSHAKE_PSTR_LEN,
            HANSHAKE_PSTR,
            HANSHAKE_RESTRICTED,
            torrent_info_hash.as_slice(),
            client_peer_id.as_slice(),
        ]
        .concat();
        // Send handshake
        let _ = stream.write(handshake_msg.as_slice()).await?;
        // Receive handshake ack
        let _ = stream.read(handshake_msg.as_mut_slice()).await?;
        // Verify handshake
        assert_eq!(torrent_info_hash, handshake_msg[28..48]);
        // Send unchoke message.
        let _ = stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 1_u8]).await?;
        // Send interested message.
        let _ = stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 2_u8]).await?;
        let peer_controller = Controller {
            ip,
            port,
            stream,
            peer_state: State {
                choked: true,
                bitfield: None,
                total_downloaded: 0,
                downloaded_window_second: (0, 0),
            },
            piece_count,
            pending_commands: vec![],
        };
        Ok(peer_controller)
    }

    pub async fn listen(&mut self) -> anyhow::Result<Option<Event>> {
        if self.process_commands().await? {
            return Ok(Some(Event::State(StateEvent::Init(
                self.peer_state.clone(),
            ))));
        }
        Ok(Some(self.process_peer_messages().await?))
    }

    async fn process_commands(&mut self) -> anyhow::Result<bool> {
        if let Some(cmd) = self.pending_commands.pop() {
            match cmd {
                ControlCommand::PieceBlockRequest(index, begin, length) => {
                    self.request(index, begin, length).await?;
                }
                ControlCommand::Shutdown => {
                    self.stream.shutdown().await?;
                }
            }
            return Ok(true);
        }
        Ok(false)
    }

    async fn request(&mut self, index: usize, begin: usize, length: usize) -> anyhow::Result<()> {
        let mut msg = [0_u8; 17];
        msg[0..4].copy_from_slice(&[0_u8, 0_u8, 0_u8, 0xd_u8]);
        msg[4..5].copy_from_slice(&[6_u8]);
        msg[5..9].copy_from_slice(&(index as u32).to_be_bytes());
        msg[9..13].copy_from_slice(&(begin as u32).to_be_bytes());
        msg[13..17].copy_from_slice(&(length as u32).to_be_bytes());
        let _wb = self.stream.write(&msg).await?;
        Ok(())
    }

    async fn process_peer_messages(&mut self) -> anyhow::Result<Event> {
        let mut msg_len_buf = [0_u8; 4];
        let bitfield_byte_count = (self.piece_count + 7) / 8;
        let read_bytes = self.stream.read_exact(&mut msg_len_buf).await?;
        let len = u32::from_be_bytes(msg_len_buf) as usize;
        let mut event = None;
        if read_bytes > 0 && len > 0 && len < 1 << 17 {
            match len {
                0 => {
                    // keep-alive message
                }
                1.. => {
                    let mut msg_type = [0_u8];
                    self.stream.read_exact(&mut msg_type).await?;
                    match (len - 1, msg_type[0]) {
                        (0, 0_u8) => {
                            event = Some(self.mark_choked(true));
                        }
                        (0, 1_u8) => {
                            event = Some(self.mark_choked(false));
                        }
                        (4, 4_u8) => {
                            let mut index_buf = [0_u8; 4];
                            self.stream.read_exact(&mut index_buf).await?;
                            let index = u32::from_be_bytes(index_buf) as usize;
                            event = Some(self.set_peer_bitfield_index(index));
                        }
                        (_, 5_u8) => {
                            let mut bitfield_buf = vec![0_u8; len - 1];
                            self.stream.read_exact(&mut bitfield_buf).await?;
                            let mut bitfield = vec![false; self.piece_count];
                            let mut index = 0;
                            for byte in &bitfield_buf[..bitfield_byte_count] {
                                for i in 0..8 {
                                    let bit = byte >> (7 - i) & 1;
                                    bitfield[index] = bit == 1;
                                    index += 1;
                                    if index >= self.piece_count {
                                        break;
                                    }
                                }
                            }
                            event = Some(self.set_peer_bitfield(bitfield));
                        }
                        (8.., 7_u8) => {
                            let msg_len = len - 1;
                            let mut msg_buf = vec![0_u8; msg_len];
                            self.stream.read_exact(&mut msg_buf).await?;
                            let mut index_buf = [0_u8; 4];
                            let mut begin_buf = [0_u8; 4];
                            index_buf.copy_from_slice(&msg_buf[0..4]);
                            begin_buf.copy_from_slice(&msg_buf[4..8]);
                            let piece_index = u32::from_be_bytes(index_buf) as usize;
                            let begin = u32::from_be_bytes(begin_buf) as usize;
                            let data = msg_buf[8..msg_len].to_owned();
                            event = Some(Event::Block(piece_index, begin, data));
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(event.unwrap_or(Event::State(StateEvent::Init(self.peer_state.clone()))))
    }

    fn mark_choked(&mut self, choked: bool) -> Event {
        self.peer_state.choked = choked;
        Event::State(StateEvent::FieldChoked(choked))
    }

    fn set_peer_bitfield_index(&mut self, index: usize) -> Event {
        if self.peer_state.bitfield.is_none() {
            self.peer_state.bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state.bitfield.as_mut().unwrap()[index] = true;
        Event::State(StateEvent::FieldHave(index))
    }

    fn set_peer_bitfield(&mut self, bitfield: Vec<bool>) -> Event {
        if self.peer_state.bitfield.is_none() {
            self.peer_state.bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state
            .bitfield
            .as_mut()
            .unwrap()
            .copy_from_slice(&bitfield[..self.piece_count]);
        Event::State(StateEvent::FieldBitfield(bitfield.clone()))
    }
}
