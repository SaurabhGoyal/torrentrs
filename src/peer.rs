use std::{
    io::{self, Error, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream},
    sync::{
        mpsc::{channel, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime},
};

const HANSHAKE_PSTR_LEN: &[u8] = &[19];
const HANSHAKE_PSTR: &[u8] = "BitTorrent protocol".as_bytes();
const HANSHAKE_RESTRICTED: &[u8] = &[0_u8; 8];

#[derive(Debug, Clone)]
pub struct State {
    pub handshake: bool,
    pub choked: bool,
    pub interested: bool,
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
    Control(Sender<ControlCommand>),
    State(StateEvent),
    Block(usize, usize, Vec<u8>),
    Metric(Metric),
}

pub struct ControllerEvent {
    pub ip: String,
    pub port: u16,
    pub event: Event,
}

#[derive(Debug)]
pub struct Controller {
    ip: String,
    port: u16,
    stream: TcpStream,
    event_tx: Sender<ControllerEvent>,
    peer_state: Arc<Mutex<State>>,
    piece_count: usize,
}

impl Controller {
    pub fn new(
        ip: String,
        port: u16,
        event_tx: Sender<ControllerEvent>,
        torrent_info_hash: [u8; 20],
        client_peer_id: [u8; 20],
        piece_count: usize,
    ) -> Result<Self, io::Error> {
        let mut stream = TcpStream::connect_timeout(
            &SocketAddr::new(ip.parse::<IpAddr>().unwrap(), port),
            Duration::from_millis(5000),
        )?;
        let mut handshake_msg = [
            HANSHAKE_PSTR_LEN,
            HANSHAKE_PSTR,
            HANSHAKE_RESTRICTED,
            torrent_info_hash.as_slice(),
            client_peer_id.as_slice(),
        ]
        .concat();
        // Send handshake
        let _ = stream.write(handshake_msg.as_slice())?;
        // Receive handshake ack
        let _ = stream.read(handshake_msg.as_mut_slice())?;
        // Verify handshake
        assert_eq!(torrent_info_hash, handshake_msg[28..48]);
        // Send unchoke message.
        let _ = stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 1_u8])?;
        // Send interested message.
        let _ = stream.write(&[0_u8, 0_u8, 0_u8, 1_u8, 2_u8])?;
        let peer_controller = Controller {
            ip,
            port,
            stream,
            event_tx,
            peer_state: Arc::new(Mutex::new(State {
                handshake: true,
                choked: true,
                interested: true,
                bitfield: None,
                total_downloaded: 0,
                downloaded_window_second: (0, 0),
            })),
            piece_count,
        };
        peer_controller.send_event(Event::State(StateEvent::Init(
            peer_controller.peer_state.lock().unwrap().clone(),
        )));
        Ok(peer_controller)
    }

    pub fn start(self) -> Result<(), io::Error> {
        let listener_stream = self.stream.try_clone().unwrap();
        let writer_stream = self.stream.try_clone().unwrap();
        let self_arc = Arc::new(self);
        let listener = self_arc.clone();
        let listener_handle = thread::spawn(move || {
            listener.start_listener(listener_stream)?;
            Ok::<(), io::Error>(())
        });
        let writer = self_arc.clone();
        let writer_handle = thread::spawn(move || {
            writer.start_writer(writer_stream)?;
            Ok::<(), io::Error>(())
        });
        listener_handle.join().unwrap()?;
        writer_handle.join().unwrap()?;
        Ok(())
    }

    fn start_listener(&self, mut stream: TcpStream) -> Result<(), io::Error> {
        let mut len_buf = [0_u8; 4];
        let max_size = 1 << 17;
        let bitfield_byte_count = (self.piece_count + 7) / 8;
        self.log("listener started");
        loop {
            stream.read_exact(&mut len_buf)?;
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > 0 && len < max_size {
                match len {
                    0 => {
                        // keep-alive message
                    }
                    1.. => {
                        let mut msg_type = [0_u8];
                        stream.read_exact(&mut msg_type)?;
                        match (len - 1, msg_type[0]) {
                            (0, 0_u8) => self.mark_choked(true),
                            (0, 1_u8) => self.mark_choked(false),
                            (4, 4_u8) => {
                                let mut index_buf = [0_u8; 4];
                                stream.read_exact(&mut index_buf)?;
                                let index = u32::from_be_bytes(index_buf) as usize;
                                self.set_peer_bitfield_index(index);
                            }
                            (_, 5_u8) => {
                                if len - 1 != bitfield_byte_count {
                                    self.log(
                                        format!(
                                            "extra bf msg size - ex - {} actu - {}, reading only till required bit-count",
                                            bitfield_byte_count,
                                            len - 1
                                        )
                                        .as_str(),
                                    );
                                }
                                let mut bitfield_buf = vec![0_u8; len - 1];
                                stream.read_exact(&mut bitfield_buf)?;
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
                                self.set_peer_bitfield(bitfield);
                            }
                            (8.., 7_u8) => {
                                let msg_len = len - 1;
                                let mut msg_buf = vec![0_u8; msg_len];
                                stream.read_exact(&mut msg_buf)?;
                                let mut index_buf = [0_u8; 4];
                                let mut begin_buf = [0_u8; 4];
                                index_buf.copy_from_slice(&msg_buf[0..4]);
                                begin_buf.copy_from_slice(&msg_buf[4..8]);
                                let piece_index = u32::from_be_bytes(index_buf) as usize;
                                let begin = u32::from_be_bytes(begin_buf) as usize;
                                let data = msg_buf[8..msg_len].to_owned();
                                self.send_event(Event::Block(piece_index, begin, data));
                                let second_window = SystemTime::now()
                                    .duration_since(SystemTime::UNIX_EPOCH)
                                    .unwrap()
                                    .as_secs();
                                {
                                    let mut state = self.peer_state.lock().unwrap();
                                    if state.downloaded_window_second.0 == second_window {
                                        state.downloaded_window_second.1 += msg_len - 8;
                                    } else if state.downloaded_window_second.0 < second_window {
                                        state.downloaded_window_second =
                                            (second_window, msg_len - 8);
                                    }
                                    self.send_event(Event::Metric(Metric::DownloadWindowSecond(
                                        state.downloaded_window_second.0,
                                        state.downloaded_window_second.1,
                                    )));
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn start_writer(&self, mut stream: TcpStream) -> Result<(), io::Error> {
        self.log("writer initialising");
        let (control_tx, control_rx) = channel::<ControlCommand>();
        self.send_event(Event::Control(control_tx));
        self.log("writer started");
        while let Ok(cmd) = control_rx.recv() {
            self.log(format!("received cmd: [{:?}]", cmd).as_str());
            match cmd {
                ControlCommand::PieceBlockRequest(index, begin, length) => {
                    self.request(&mut stream, index, begin, length)?;
                }
                ControlCommand::Shutdown => {
                    stream.shutdown(std::net::Shutdown::Both)?;
                }
            }
            self.log(format!("executed cmd: [{:?}]", cmd).as_str());
        }
        Ok(())
    }

    fn log(&self, msg: &str) {
        // println!("{}:{}: {msg}", self.ip, self.port);
    }

    fn send_event(&self, event: Event) {
        self.event_tx
            .send(ControllerEvent {
                ip: self.ip.clone(),
                port: self.port,
                event,
            })
            .unwrap();
    }

    fn mark_choked(&self, choked: bool) {
        self.peer_state.lock().unwrap().choked = choked;
        self.send_event(Event::State(StateEvent::FieldChoked(choked)));
    }

    fn set_peer_bitfield_index(&self, index: usize) {
        if self.peer_state.lock().unwrap().bitfield.is_none() {
            self.peer_state.lock().unwrap().bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state.lock().unwrap().bitfield.as_mut().unwrap()[index] = true;
        self.send_event(Event::State(StateEvent::FieldHave(index)));
    }

    fn set_peer_bitfield(&self, bitfield: Vec<bool>) {
        if self.peer_state.lock().unwrap().bitfield.is_none() {
            self.peer_state.lock().unwrap().bitfield = Some(vec![false; self.piece_count]);
        }
        self.peer_state
            .lock()
            .unwrap()
            .bitfield
            .as_mut()
            .unwrap()
            .copy_from_slice(&bitfield[..self.piece_count]);
        self.send_event(Event::State(StateEvent::FieldBitfield(bitfield.clone())));
    }

    fn request(
        &self,
        stream: &mut TcpStream,
        index: usize,
        begin: usize,
        length: usize,
    ) -> Result<(), io::Error> {
        let mut msg = [0_u8; 17];
        msg[0..4].copy_from_slice(&[0_u8, 0_u8, 0_u8, 0xd_u8]);
        msg[4..5].copy_from_slice(&[6_u8]);
        msg[5..9].copy_from_slice(&(index as u32).to_be_bytes());
        msg[9..13].copy_from_slice(&(begin as u32).to_be_bytes());
        msg[13..17].copy_from_slice(&(length as u32).to_be_bytes());
        let _wb = stream.write(&msg)?;
        Ok(())
    }
}
