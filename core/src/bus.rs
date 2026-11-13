//! TCP bus: the Cardio listener (commands ride the same connection) and the
//! TDXLU-compatible listener (commands go to the peer's `cmd_port`).
//!
//! An app that already owns the TDXLU port (TDX Launcher Ultra) builds a bus
//! with [`Bus::new`] and hands it every line it reads, via [`Bus::ingest_line`].

use crate::protocol::{family_key, Incoming};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct Beat {
    pub fps: Option<f64>,
    pub frame: Option<i64>,
    pub cook_ms: Option<f64>,
    pub dropped: Option<i64>,
    pub perform: Option<bool>,
    pub gpu_mb: Option<f64>,
}

#[derive(Clone)]
pub enum Channel {
    /// Cardio COMP: write commands on the connection it opened.
    Cardio { conn: u64, writer: Arc<Mutex<TcpStream>> },
    /// TDXLauncherUtility: connect to its command server.
    Tdxlu { cmd_port: u16 },
}

#[derive(Clone)]
pub struct Peer {
    pub id: String,
    pub pid: Option<u32>,
    pub channel: Option<Channel>,
    pub version: Option<String>,
    pub caps: Vec<String>,
    pub last_seen: Instant,
    pub last_beat: Option<Instant>,
    pub beat: Beat,
    pub hold_until: Option<Instant>,
    pub hold_reason: String,
    pub bye: Option<(String, Instant)>,
}

impl Peer {
    pub fn kind(&self) -> &'static str {
        match self.channel {
            Some(Channel::Cardio { .. }) => "cardio",
            Some(Channel::Tdxlu { .. }) => "tdxlu",
            None => "unknown",
        }
    }
    /// Whether the companion answers `verb`. A TDXLauncherUtility that sends
    /// no `caps` (older companions) answers only `ping` and `save` of the verbs
    /// the watcher uses.
    pub fn can(&self, verb: &str) -> bool {
        match &self.channel {
            Some(Channel::Cardio { .. }) => self.caps.iter().any(|c| c == verb),
            Some(Channel::Tdxlu { .. }) if self.caps.is_empty() => matches!(verb, "ping" | "save"),
            Some(Channel::Tdxlu { .. }) => self.caps.iter().any(|c| c == verb),
            None => false,
        }
    }
}

#[derive(Default)]
struct State {
    peers: HashMap<String, Peer>,
    pending: HashMap<String, Sender<Value>>,
    cardio_listening: bool,
    compat_listening: bool,
}

#[derive(Clone)]
pub struct Bus {
    state: Arc<Mutex<State>>,
    next: Arc<AtomicU64>,
    created: Instant,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    /// A bus with no listeners: feed it with [`Bus::ingest_line`].
    pub fn new() -> Bus {
        Bus { state: Arc::default(), next: Arc::new(AtomicU64::new(1)), created: Instant::now() }
    }

    /// A bus serving the Cardio port and, while it is free, the compat port.
    pub fn start(cardio_port: u16, compat_port: u16, log: crate::log::Log) -> Bus {
        let bus = Bus::new();
        bus.spawn_listener(cardio_port, true, log.clone());
        if compat_port != 0 {
            bus.spawn_listener(compat_port, false, log);
        }
        bus
    }

    /// Also serve the Cardio port on a bus built with [`Bus::new`], so an app
    /// that owns the TDXLU port (TDX Launcher Ultra) hears Cardio COMPs too.
    /// Waits for the port while another app (the TDXCardio app) holds it.
    pub fn listen_cardio(&self, port: u16, log: crate::log::Log) {
        self.spawn_listener(port, true, log);
    }

    /// How long this bus has been up. Companions hello every 2 s, so until a
    /// few seconds have passed a missing peer proves nothing.
    pub fn age(&self) -> Duration {
        self.created.elapsed()
    }

    pub fn listening(&self) -> (bool, bool) {
        let g = self.state.lock().unwrap();
        (g.cardio_listening, g.compat_listening)
    }

    /// Bind (retrying every 10 s while the port is taken, e.g. by TDXLU) and accept.
    fn spawn_listener(&self, port: u16, cardio: bool, log: crate::log::Log) {
        let bus = self.clone();
        thread::spawn(move || {
            let label = if cardio { "cardio" } else { "compat" };
            let mut warned = false;
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let listener = loop {
                // Windows lets 127.0.0.1:<port> bind beside another app's
                // 0.0.0.0:<port>, and clients then reach whichever it picks.
                // Anything already answering means the port is taken.
                if TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok() {
                    if !warned {
                        log.line(&format!("{label} port {port} is already served by another app; retrying every 10 s"));
                        warned = true;
                    }
                    thread::sleep(Duration::from_secs(10));
                    continue;
                }
                match TcpListener::bind(addr) {
                    Ok(l) => break l,
                    Err(e) => {
                        if !warned {
                            log.line(&format!("{label} port {port} busy ({e}); retrying every 10 s"));
                            warned = true;
                        }
                        thread::sleep(Duration::from_secs(10));
                    }
                }
            };
            log.line(&format!("{label} listening on 127.0.0.1:{port}"));
            {
                let mut g = bus.state.lock().unwrap();
                if cardio { g.cardio_listening = true } else { g.compat_listening = true }
            }
            for stream in listener.incoming().flatten() {
                let bus = bus.clone();
                thread::spawn(move || bus.serve(stream, cardio));
            }
        });
    }

    fn serve(&self, stream: TcpStream, cardio: bool) {
        // Accepted sockets inherit non-blocking mode on Windows; reads must block.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_nodelay(true);
        let conn = self.next.fetch_add(1, Ordering::Relaxed);
        let writer = match stream.try_clone() {
            Ok(w) => {
                let _ = w.set_write_timeout(Some(Duration::from_secs(2)));
                Arc::new(Mutex::new(w))
            }
            Err(_) => return,
        };
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Some((msg, raw)) = Incoming::parse(&line) {
                self.apply(msg, raw, cardio.then(|| Channel::Cardio { conn, writer: writer.clone() }));
            }
        }
        // Connection closed: forget the channel, keep the peer (it may reconnect).
        let mut g = self.state.lock().unwrap();
        for p in g.peers.values_mut() {
            if matches!(&p.channel, Some(Channel::Cardio { conn: c, .. }) if *c == conn) {
                p.channel = None;
            }
        }
    }

    /// One line a TDXLauncherUtility sent to the app's own TDXLU-port
    /// listener (commands go back to its `cmd_port`). Lines that are not bus
    /// messages are ignored.
    pub fn ingest_line(&self, line: &str) {
        if let Some((msg, raw)) = Incoming::parse(line) {
            self.apply(msg, raw, None);
        }
    }

    fn apply(&self, msg: Incoming, raw: Value, cardio_channel: Option<Channel>) {
        let mut g = self.state.lock().unwrap();
        if msg.kind == "result" {
            if let Some(tx) = msg.req.as_ref().and_then(|r| g.pending.remove(r)) {
                let _ = tx.send(raw);
            }
            return;
        }
        let Some(id) = msg.resolved_id() else { return };
        let now = Instant::now();
        let peer = g.peers.entry(id.clone()).or_insert_with(|| Peer {
            id: id.clone(),
            pid: None,
            channel: None,
            version: None,
            caps: Vec::new(),
            last_seen: now,
            last_beat: None,
            beat: Beat::default(),
            hold_until: None,
            hold_reason: String::new(),
            bye: None,
        });
        peer.last_seen = now;
        if msg.td_pid.is_some() {
            peer.pid = msg.td_pid;
        }
        // A Cardio connection always wins over a TDXLU cmd_port for the same id.
        if let Some(ch) = cardio_channel {
            peer.channel = Some(ch);
        } else if let Some(port) = msg.cmd_port {
            if !matches!(peer.channel, Some(Channel::Cardio { .. })) {
                peer.channel = Some(Channel::Tdxlu { cmd_port: port });
            }
        }
        match msg.kind.as_str() {
            "hello" => {
                peer.bye = None;
                if let Some(v) = msg.cardio.or(msg.utility_version) {
                    peer.version = Some(v);
                }
                if let Some(c) = msg.caps {
                    peer.caps = c;
                }
            }
            "heartbeat" => {
                peer.bye = None;
                peer.last_beat = Some(now);
                peer.beat = Beat {
                    fps: msg.fps,
                    frame: msg.frame,
                    cook_ms: msg.cook_ms,
                    dropped: msg.dropped,
                    perform: msg.perform,
                    gpu_mb: msg.gpu_mb,
                };
            }
            "hold" => {
                let secs = msg.seconds.unwrap_or(60.0).clamp(1.0, 600.0);
                peer.hold_until = Some(now + Duration::from_secs_f64(secs));
                peer.hold_reason = msg.reason.unwrap_or_default();
            }
            "release" => peer.hold_until = None,
            "bye" => peer.bye = Some((msg.reason.unwrap_or_else(|| "quit".into()), now)),
            _ => {}
        }
    }

    /// The peer for a watched project: same pid first, then same id, then same
    /// Increment-and-Save family (the session saved itself under a new number).
    pub fn find(&self, id: &str, pid: Option<u32>) -> Option<Peer> {
        let g = self.state.lock().unwrap();
        if let Some(pid) = pid {
            if let Some(p) = g.peers.values().filter(|p| p.pid == Some(pid)).max_by_key(|p| p.last_seen) {
                return Some(p.clone());
            }
        }
        if let Some(p) = g.peers.get(id) {
            return Some(p.clone());
        }
        let fam = family_key(id);
        g.peers.values().filter(|p| family_key(&p.id) == fam).max_by_key(|p| p.last_seen).cloned()
    }

    pub fn peers(&self) -> Vec<Peer> {
        self.state.lock().unwrap().peers.values().cloned().collect()
    }

    /// Forget a peer's per-run state (used when a fresh process starts).
    pub fn forget(&self, id: &str, pid: Option<u32>) {
        let mut g = self.state.lock().unwrap();
        g.peers.retain(|k, p| k != id && (pid.is_none() || p.pid != pid));
    }

    /// Fire-and-forget push (the `watch` state message). A Cardio peer gets
    /// it on its connection; a TDXLauncherUtility that lists `watch` in its
    /// caps gets it as a `watch` command on its cmd_port, off this thread.
    pub fn push(&self, peer: &Peer, body: Value) {
        match &peer.channel {
            Some(Channel::Cardio { writer, .. }) => {
                let _ = write_line(writer, &body);
            }
            Some(Channel::Tdxlu { cmd_port }) if peer.can("watch") => {
                let port = *cmd_port;
                let req = self.next.fetch_add(1, Ordering::Relaxed).to_string();
                let mut cmd = json!({"type": "cmd", "v": 1, "id": peer.id, "action": "watch", "req": req});
                if let (Some(c), Value::Object(b)) = (cmd.as_object_mut(), body) {
                    for (k, v) in b {
                        if !matches!(k.as_str(), "type" | "v" | "id") {
                            c.insert(k, v);
                        }
                    }
                }
                thread::spawn(move || {
                    let _ = tdxlu_exchange(port, &cmd, Duration::from_secs(3));
                });
            }
            _ => {}
        }
    }

    /// Send a command and wait for its result.
    pub fn command(&self, peer: &Peer, action: &str, extra: Value, wait: Duration) -> Result<Value, String> {
        let req = self.next.fetch_add(1, Ordering::Relaxed).to_string();
        let mut body = json!({"type": "cmd", "v": 1, "id": peer.id, "action": action, "req": req});
        if let (Some(b), Value::Object(e)) = (body.as_object_mut(), extra) {
            b.extend(e);
        }
        match peer.channel.clone() {
            Some(Channel::Cardio { writer, .. }) => {
                let (tx, rx) = mpsc::channel();
                self.state.lock().unwrap().pending.insert(req.clone(), tx);
                let sent = write_line(&writer, &body);
                let out = sent.and_then(|_| rx.recv_timeout(wait).map_err(|_| format!("{action}: no reply in {wait:?}")));
                self.state.lock().unwrap().pending.remove(&req);
                out
            }
            Some(Channel::Tdxlu { cmd_port }) => tdxlu_exchange(cmd_port, &body, wait),
            None => Err("no command channel".into()),
        }
    }
}

fn write_line(writer: &Arc<Mutex<TcpStream>>, body: &Value) -> Result<(), String> {
    let mut line = body.to_string();
    line.push('\n');
    writer.lock().unwrap().write_all(line.as_bytes()).map_err(|e| e.to_string())
}

/// TDXLU style: one connection per command, one reply line.
fn tdxlu_exchange(port: u16, body: &Value, wait: Duration) -> Result<Value, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(wait)).ok();
    s.set_write_timeout(Some(Duration::from_secs(2))).ok();
    let mut line = body.to_string();
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).map_err(|e| e.to_string())?;
    serde_json::from_str(reply.trim()).map_err(|e| format!("bad reply: {e}"))
}
