//! TDXCardio: standalone heartbeat watchdog for TouchDesigner projects.
//! Wire contract: docs/PROTOCOL.md. Config: cardio.json (see README).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod bus;
mod config;
mod icon;
mod log;
mod notify;
mod platform;
mod protocol;
mod ui;
mod watch;

use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Core {
    pub config_path: PathBuf,
    /// Ports the listeners were started on (a port change needs a restart).
    pub ports: (u16, u16),
    /// Folder settings this run started with (a change needs a restart).
    pub started_dirs: (String, String),
    pub ctx: Arc<watch::Ctx>,
    pub watchers: Mutex<Vec<watch::Watcher>>,
    /// Every project this machine has seen (on the bus or in the config), so
    /// its settings stay reachable when TD is not running. `known_projects.json`.
    pub known: Mutex<Vec<String>>,
    pub known_path: PathBuf,
}

impl Core {
    /// Add paths to the known list (deduplicated by project family) and persist it.
    pub fn remember(&self, paths: impl IntoIterator<Item = String>) {
        let mut k = self.known.lock().unwrap();
        let mut changed = false;
        for p in paths {
            if p.is_empty() {
                continue;
            }
            let fam = protocol::family_key(&protocol::project_id(&p));
            if !k.iter().any(|q| protocol::family_key(&protocol::project_id(q)) == fam) {
                k.push(p);
                changed = true;
            }
        }
        if changed {
            let _ = std::fs::write(&self.known_path, serde_json::to_vec_pretty(&*k).unwrap_or_default());
        }
    }

    pub fn forget(&self, path: &str) {
        let fam = protocol::family_key(&protocol::project_id(path));
        let mut k = self.known.lock().unwrap();
        k.retain(|q| protocol::family_key(&protocol::project_id(q)) != fam);
        let _ = std::fs::write(&self.known_path, serde_json::to_vec_pretty(&*k).unwrap_or_default());
    }

    pub fn add_project(&self, entry: config::Entry) {
        self.remember([entry.path.clone()]);
        let w = watch::spawn(entry, self.ctx.clone());
        self.watchers.lock().unwrap().push(w);
    }

    /// Make the running watchers match a saved config: same uid and path get
    /// the new row (and re-read their rules), new rows spawn, dropped rows stop
    /// watching. TouchDesigner itself is never touched.
    pub fn apply(&self, cfg: config::Config) {
        let mut old: Vec<watch::Watcher> = std::mem::take(&mut *self.watchers.lock().unwrap());
        let mut next = Vec::with_capacity(cfg.projects.len());
        for e in &cfg.projects {
            match old.iter().position(|w| w.uid == e.uid && w.path == e.path) {
                Some(i) => {
                    let w = old.remove(i);
                    let _ = w.ctl.send(watch::Ctl::Update(e.clone()));
                    next.push(w);
                }
                None => {
                    self.remember([e.path.clone()]);
                    next.push(watch::spawn(e.clone(), self.ctx.clone()))
                }
            }
        }
        for w in &old {
            self.ctx.log.line(&format!("stopped watching {} (removed from settings)", w.path));
        }
        drop(old); // dropping the sender ends each removed watcher's thread
        *self.ctx.cfg.lock().unwrap() = Arc::new(cfg);
        *self.watchers.lock().unwrap() = next;
    }

    /// Settings that only take effect after TDXCardio restarts.
    pub fn needs_restart(&self, cfg: &config::Config) -> Vec<&'static str> {
        let mut out = Vec::new();
        if (cfg.cardio_port, cfg.compat_port) != self.ports {
            out.push("ports");
        }
        if (cfg.data_dir.clone(), cfg.status_file.clone()) != self.started_dirs {
            out.push("data folder / status file");
        }
        out
    }

    pub fn statuses(&self) -> Vec<watch::Status> {
        self.watchers.lock().unwrap().iter().map(|w| w.status.lock().unwrap().clone()).collect()
    }

    /// Bus sessions no watcher has claimed (pid or id match).
    pub fn unwatched(&self) -> Vec<bus::Peer> {
        let st = self.statuses();
        self.ctx
            .bus
            .peers()
            .into_iter()
            .filter(|p| p.last_seen.elapsed() < Duration::from_secs(10))
            .filter(|p| {
                !st.iter().any(|s| {
                    (p.pid.is_some() && s.pid == p.pid) || protocol::family_key(&s.id) == protocol::family_key(&p.id)
                })
            })
            .collect()
    }

    fn write_status(&self, path: &std::path::Path) {
        let (cardio, compat) = self.ctx.bus.listening();
        let sessions: Vec<_> = self
            .unwatched()
            .iter()
            .map(|p| json!({"id": p.id, "pid": p.pid, "companion": p.kind(), "version": p.version, "fps": p.beat.fps}))
            .collect();
        let body = json!({
            "app": "TDXCardio",
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "updated": watch::now_unix(),
            "listeners": {
                "cardio_port": self.ports.0, "cardio": cardio,
                "compat_port": self.ports.1, "compat": compat,
            },
            "projects": self.statuses(),
            "unwatched_sessions": sessions,
        });
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(&body).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut config_arg = None;
    let mut headless = false;
    let mut open_settings = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" | "-c" => config_arg = args.next().map(PathBuf::from),
            "--headless" => headless = true,
            "--settings" => open_settings = true,
            "--help" | "-h" => {
                println!("tdxcardio [--config cardio.json] [--headless | --settings]");
                return;
            }
            _ => {}
        }
    }

    let config_path = config::resolve_path(config_arg);
    let (cfg, load_error) = match config::load(&config_path) {
        Ok(c) => (c, None),
        Err(e) => (config::Config::default(), Some(e)),
    };
    let data_dir = cfg.data_dir(&config_path);
    let log = log::Log::new(&data_dir);
    log.line(&format!("TDXCardio {} starting; config {}", env!("CARGO_PKG_VERSION"), config_path.display()));
    if let Some(e) = &load_error {
        // Fail loud, and never overwrite a broken file with defaults.
        log.line(&format!("CONFIG ERROR, watching nothing: {e}"));
    }
    for w in config::misplaced_rules(&config_path) {
        log.line(&format!("CONFIG WARNING: {w}"));
    }

    let bus = bus::Bus::start(cfg.cardio_port, cfg.compat_port, log.clone());
    let status_path = cfg.status_path(&data_dir);
    let projects = cfg.projects.clone();
    let start_hidden = cfg.start_hidden;
    let ports = (cfg.cardio_port, cfg.compat_port);
    let started_dirs = (cfg.data_dir.clone(), cfg.status_file.clone());
    let ctx = Arc::new(watch::Ctx { cfg: Mutex::new(Arc::new(cfg)), bus, log: log.clone(), data_dir, gave_ups: Arc::new(AtomicU32::new(0)) });
    let known_path = ctx.data_dir.join("known_projects.json");
    let known = std::fs::read(&known_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let core = Arc::new(Core { config_path, ports, started_dirs, ctx, watchers: Mutex::new(Vec::new()), known: Mutex::new(known), known_path });
    // One watcher per row, empty paths included (they stay idle), so rows and
    // watchers line up for the settings editor.
    for e in projects {
        core.add_project(e);
    }

    if let Some(path) = status_path {
        let core = core.clone();
        std::thread::spawn(move || {
            loop {
                let t = Instant::now();
                core.write_status(&path);
                std::thread::sleep(Duration::from_secs(1).saturating_sub(t.elapsed()));
            }
        });
    }

    {
        // Remember every session that talks to the bus, watched or not.
        let core = core.clone();
        std::thread::spawn(move || loop {
            let seen = core.ctx.bus.peers().into_iter().filter(|p| p.last_seen.elapsed() < Duration::from_secs(10)).map(|p| p.id);
            core.remember(seen);
            std::thread::sleep(Duration::from_secs(2));
        });
    }

    if headless {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    if let Err(e) = ui::run(core.clone(), start_hidden && !open_settings, load_error, open_settings) {
        log.line(&format!("UI failed ({e}); continuing headless"));
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}
