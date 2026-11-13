//! One watchdog thread per project: launch/attach, detect exit/stall/ill
//! health, collect evidence, restart with backoff, give up on a crash loop.

use crate::bus::{Bus, Peer};
use crate::config::{self, Config, Entry, Project};
use crate::log::Log;
use crate::{notify, platform, protocol};
use serde::Serialize;
use serde_json::json;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TICK: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub enum Ctl {
    Start,
    Stop,
    Restart,
    /// The machine row changed (enabled, mode, executable); rules are re-read too.
    Update(Entry),
}

/// How often a watcher checks its rules sidecar for edits (UI save, git pull).
const RULES_POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, Default)]
pub struct Status {
    pub name: String,
    pub id: String,
    pub path: String,
    pub mode: String,
    /// idle | waiting | launching | grace | watching | unhealthy | held | restarting | stopped | gave_up | error
    pub phase: String,
    pub message: String,
    pub pid: Option<u32>,
    /// cardio | tdxlu | unknown | none
    pub companion: String,
    pub companion_version: Option<String>,
    pub last_heartbeat_secs: Option<f64>,
    pub fps: Option<f64>,
    pub cook_ms: Option<f64>,
    pub dropped: Option<i64>,
    pub perform: Option<bool>,
    pub hold_secs: Option<f64>,
    pub restarts_total: u32,
    pub restarts_recent: u32,
    pub phase_since: u64,
    pub last_restart_reason: Option<String>,
}

pub struct Watcher {
    pub uid: u64,
    pub path: String,
    pub status: Arc<Mutex<Status>>,
    pub ctl: Sender<Ctl>,
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    Exited,
    Stalled,
    Unhealthy,
    Manual,
}
impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::Exited => "exited",
            Reason::Stalled => "stalled",
            Reason::Unhealthy => "unhealthy",
            Reason::Manual => "manual",
        }
    }
}

pub struct Ctx {
    pub cfg: Mutex<Arc<Config>>,
    pub bus: Bus,
    pub log: Log,
    pub data_dir: PathBuf,
    pub gave_ups: Arc<AtomicU32>,
}

impl Ctx {
    /// The current machine config (swapped whole when the settings are saved).
    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.lock().unwrap().clone()
    }
}

fn rules_mtime(toe: &str) -> Option<SystemTime> {
    std::fs::metadata(config::sidecar_path(toe)).and_then(|m| m.modified()).ok()
}

/// Spawn a watcher for one machine row; its rules come from the sidecar.
/// A sidecar that fails to parse leaves the project in `error`, never on defaults.
pub fn spawn(entry: Entry, ctx: Arc<Ctx>) -> Watcher {
    let (rules, rules_error) = match config::load_rules(&entry.path) {
        Ok(r) => (r, None),
        Err(e) => (config::Rules::default(), Some(e)),
    };
    let mut project = Project::resolve(&entry, &rules);
    if entry.path.is_empty() {
        project.enabled = false;
    }
    if rules_error.is_some() {
        project.enabled = false;
    }
    let (uid, path) = (project.uid, project.path.clone());
    let id = protocol::project_id(&project.path);
    let status = Arc::new(Mutex::new(Status {
        name: project.display_name(),
        id: id.clone(),
        path: project.path.clone(),
        mode: project.mode.clone(),
        phase: if rules_error.is_some() { "error".into() } else { "idle".into() },
        message: rules_error.clone().map(|e| format!("rules file: {e}")).unwrap_or_default(),
        companion: "none".into(),
        phase_since: now_unix(),
        ..Default::default()
    }));
    let (tx, rx) = mpsc::channel();
    if project.enabled {
        let _ = tx.send(Ctl::Start);
    }
    let st = status.clone();
    if let Some(e) = &rules_error {
        ctx.log.line(&format!("[{}] RULES ERROR, not watching: {e}", project.display_name()));
    }
    thread::spawn(move || Run::new(project, id, ctx, st).run(rx));
    Watcher { uid, path, status, ctl: tx }
}

struct Run {
    p: Project,
    id: String,
    ctx: Arc<Ctx>,
    status: Arc<Mutex<Status>>,
    phase: &'static str,
    child: Option<Child>,
    pid: Option<u32>,
    started_at: Instant,
    restarts: VecDeque<Instant>,
    restarts_total: u32,
    relaunch_at: Option<Instant>,
    bad_since: Option<Instant>,
    bad_logged: bool,
    last_focus: Instant,
    perform_sent_for: Option<u32>,
    pushed_phase: Option<(&'static str, u32)>,
    pushed_at: Instant,
    last_reason: Option<String>,
    rules_mtime: Option<SystemTime>,
    rules_checked: Instant,
}

impl Run {
    fn new(p: Project, id: String, ctx: Arc<Ctx>, status: Arc<Mutex<Status>>) -> Run {
        Run {
            id,
            ctx,
            status,
            phase: "idle",
            child: None,
            pid: None,
            started_at: Instant::now(),
            restarts: VecDeque::new(),
            restarts_total: 0,
            relaunch_at: None,
            bad_since: None,
            bad_logged: false,
            last_focus: Instant::now(),
            perform_sent_for: None,
            pushed_phase: None,
            pushed_at: Instant::now(),
            rules_mtime: rules_mtime(&p.path),
            rules_checked: Instant::now(),
            last_reason: None,
            p,
        }
    }

    fn log(&self, msg: &str) {
        self.ctx.log.line(&format!("[{}] {msg}", self.p.display_name()));
    }

    fn set_phase(&mut self, phase: &'static str, msg: &str) {
        if self.phase != phase {
            self.log(&format!("{} -> {phase}: {msg}", self.phase));
            self.phase = phase;
            let mut s = self.status.lock().unwrap();
            s.phase_since = now_unix();
        }
        let mut s = self.status.lock().unwrap();
        s.phase = phase.into();
        s.message = msg.into();
    }

    fn run(mut self, rx: Receiver<Ctl>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Ctl::Start) => self.start(),
                Ok(Ctl::Update(entry)) => self.update(entry),
                Ok(Ctl::Stop) => {
                    self.relaunch_at = None;
                    self.set_phase("stopped", "stopped by user (TouchDesigner left running)");
                }
                Ok(Ctl::Restart) => {
                    if self.pid.is_some() || self.phase == "gave_up" || self.phase == "stopped" {
                        self.restarts.clear();
                        self.restart(Reason::Manual);
                    } else {
                        self.start();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if self.rules_checked.elapsed() >= RULES_POLL {
                self.rules_checked = Instant::now();
                if rules_mtime(&self.p.path) != self.rules_mtime {
                    self.reload_rules("rules file changed");
                }
            }
            self.tick();
        }
    }

    /// New machine row (same project path): apply it and re-read the rules.
    fn update(&mut self, e: Entry) {
        let was_enabled = self.p.enabled;
        self.p.enabled = e.enabled;
        self.p.mode = e.mode.clone();
        self.p.td_executable = e.td_executable.clone();
        self.status.lock().unwrap().mode = e.mode;
        self.reload_rules("settings saved");
        let running = !matches!(self.phase, "idle" | "stopped" | "gave_up" | "error");
        if was_enabled && !self.p.enabled && running {
            self.relaunch_at = None;
            self.set_phase("stopped", "disabled in settings (TouchDesigner left running)");
        } else if self.p.enabled && !running && self.phase != "gave_up" {
            self.start();
        }
    }

    /// Swap in the sidecar's rules; a broken file keeps the old rules and says so.
    fn reload_rules(&mut self, why: &str) {
        self.rules_mtime = rules_mtime(&self.p.path);
        match config::load_rules(&self.p.path) {
            Ok(r) => {
                let e = Entry { path: self.p.path.clone(), enabled: self.p.enabled, mode: self.p.mode.clone(), td_executable: self.p.td_executable.clone(), uid: self.p.uid };
                let next = Project::resolve(&e, &r);
                if next == self.p {
                    return;
                }
                let window_changed = next.window != self.p.window;
                self.p = next;
                if window_changed {
                    self.perform_sent_for = None;
                }
                self.status.lock().unwrap().name = self.p.display_name();
                self.log(&format!("{why}: rules reloaded"));
                if self.phase == "error" && self.status.lock().unwrap().message.starts_with("rules file") {
                    self.set_phase("idle", "rules file fixed");
                }
            }
            Err(e) => self.log(&format!("{why}: RULES ERROR, keeping the previous rules: {e}")),
        }
    }

    fn peer(&self) -> Option<Peer> {
        self.ctx.bus.find(&self.id, self.pid)
    }

    fn alive(&mut self) -> bool {
        let Some(pid) = self.pid else { return false };
        if let Some(c) = self.child.as_mut().filter(|c| c.id() == pid) {
            return matches!(c.try_wait(), Ok(None));
        }
        platform::pid_alive(pid)
    }

    /// Adopt a TD that already runs this project (by companion pid), else launch.
    fn start(&mut self) {
        self.relaunch_at = None;
        if let Some(pid) = self.peer().and_then(|p| p.pid).filter(|&pid| platform::pid_alive(pid)) {
            self.pid = Some(pid);
            self.started_at = Instant::now();
            self.set_phase("grace", &format!("attached to running pid {pid}"));
            return;
        }
        if self.p.is_attach() {
            self.pid = None;
            self.set_phase("waiting", "waiting for this project's companion to say hello");
            return;
        }
        self.launch();
    }

    fn launch(&mut self) {
        let exe = [&self.p.td_executable, &self.ctx.cfg().td_executable]
            .into_iter()
            .find(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(platform::find_td_executable);
        let Some(exe) = exe else {
            self.set_phase("error", "no TouchDesigner executable found; set td_executable");
            return;
        };
        self.ctx.bus.forget(&self.id, self.pid);
        match platform::launch(&exe, &self.p.path, &self.p.args) {
            Ok(child) => {
                let pid = child.id();
                self.child = Some(child);
                self.pid = Some(pid);
                self.started_at = Instant::now();
                self.perform_sent_for = None;
                self.set_phase("grace", &format!("launched pid {pid}; grace {}s", self.p.launch_grace_secs));
            }
            Err(e) => self.set_phase("error", &format!("launch failed: {e}")),
        }
    }

    fn tick(&mut self) {
        let peer = self.peer();
        self.publish(peer.as_ref());
        match self.phase {
            "waiting" => {
                if let Some(pid) = peer.as_ref().and_then(|p| p.pid).filter(|&pid| platform::pid_alive(pid)) {
                    self.pid = Some(pid);
                    self.started_at = Instant::now();
                    self.set_phase("grace", &format!("companion appeared, pid {pid}"));
                }
            }
            "restarting" => {
                if self.relaunch_at.is_some_and(|t| Instant::now() >= t) {
                    self.relaunch_at = None;
                    self.launch();
                }
            }
            "grace" | "watching" | "unhealthy" | "held" => self.check(peer.clone()),
            _ => {}
        }
        self.push_watch(peer.as_ref());
    }

    fn check(&mut self, peer: Option<Peer>) {
        if !self.alive() {
            if let Some((reason, _)) = peer.as_ref().and_then(|p| p.bye.clone()) {
                if reason == "quit" && !self.p.relaunch_on_clean_exit {
                    self.pid = None;
                    self.set_phase("stopped", "TouchDesigner quit cleanly (relaunch_on_clean_exit is off)");
                    return;
                }
            }
            self.restart(Reason::Exited);
            return;
        }
        let beat_age = peer
            .as_ref()
            .and_then(|p| p.last_beat)
            .filter(|t| *t >= self.started_at)
            .map(|t| t.elapsed());
        let held = peer.as_ref().and_then(|p| p.hold_until).filter(|t| *t > Instant::now());
        if let Some(until) = held {
            let reason = peer.as_ref().map(|p| p.hold_reason.clone()).unwrap_or_default();
            self.set_phase("held", &format!("hold for {:.0}s more: {reason}", (until - Instant::now()).as_secs_f64()));
            return;
        }
        match beat_age {
            None => {
                if self.started_at.elapsed() >= Duration::from_secs(self.p.launch_grace_secs) {
                    self.restart(Reason::Stalled);
                } else if self.phase != "grace" {
                    self.set_phase("grace", "waiting for the first heartbeat");
                }
                return;
            }
            Some(age) if age >= Duration::from_secs(self.p.timeout_secs) => {
                self.restart(Reason::Stalled);
                return;
            }
            Some(_) => {}
        }
        let peer = peer.expect("beat implies peer");
        if self.check_health(&peer) {
            return;
        }
        if self.p.window.perform_mode && self.perform_sent_for != peer.pid && peer.can("perform") {
            self.perform_sent_for = peer.pid;
            let r = self.ctx.bus.command(&peer, "perform", json!({"on": true, "topmost": self.p.window.topmost}), Duration::from_secs(5));
            self.log(&format!("perform mode requested: {}", r.map(|_| "ok".to_string()).unwrap_or_else(|e| e)));
        }
        if self.p.window.keep_focus && self.last_focus.elapsed() >= Duration::from_secs(self.p.window.focus_interval_secs.max(1)) {
            self.last_focus = Instant::now();
            if let Some(pid) = self.pid {
                platform::focus(pid);
            }
        }
    }

    /// True when it acted (restart) and the tick should stop.
    fn check_health(&mut self, peer: &Peer) -> bool {
        let h = self.p.health.clone();
        if !h.enabled {
            if self.phase != "watching" {
                self.set_phase("watching", "heartbeat ok");
            }
            return false;
        }
        let low_fps = h.min_fps > 0.0 && peer.beat.fps.is_some_and(|f| f < h.min_fps);
        let slow = h.max_cook_ms > 0.0 && peer.beat.cook_ms.is_some_and(|c| c > h.max_cook_ms);
        if !(low_fps || slow) {
            self.bad_since = None;
            self.bad_logged = false;
            if self.phase != "watching" {
                self.set_phase("watching", "heartbeat ok");
            }
            return false;
        }
        let since = *self.bad_since.get_or_insert_with(Instant::now);
        let what = format!(
            "fps {:.1} cook {:.1} ms",
            peer.beat.fps.unwrap_or(0.0),
            peer.beat.cook_ms.unwrap_or(0.0)
        );
        self.set_phase("unhealthy", &format!("{what} for {:.0}s", since.elapsed().as_secs_f64()));
        if since.elapsed() >= Duration::from_secs(h.bad_secs) {
            if h.action.eq_ignore_ascii_case("restart") {
                self.bad_since = None;
                self.restart(Reason::Unhealthy);
                return true;
            } else if !self.bad_logged {
                self.bad_logged = true;
                self.log(&format!("unhealthy for {}s ({what}); action is log", h.bad_secs));
            }
        }
        false
    }

    fn restart(&mut self, reason: Reason) {
        let now = Instant::now();
        let window = Duration::from_secs(self.p.restart_window_secs);
        while self.restarts.front().is_some_and(|t| now.duration_since(*t) > window) {
            self.restarts.pop_front();
        }
        self.last_reason = Some(reason.as_str().into());
        self.set_phase("restarting", &format!("{}: collecting evidence and stopping", reason.as_str()));
        self.publish(self.peer().as_ref());
        let mut evidence = vec![self.ctx.log.today()];
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");

        // Screenshot first: a stalled TD still shows what it froze on.
        if self.p.evidence.screenshot && reason != Reason::Exited {
            let file = self.ctx.data_dir.join("captures").join(format!("{}_{stamp}_{}.png", safe(&self.p.display_name()), reason.as_str()));
            match platform::screenshot(&file) {
                Ok(()) => {
                    self.log(&format!("screenshot {}", file.display()));
                    evidence.push(file);
                }
                Err(e) => self.log(&format!("screenshot failed: {e}")),
            }
        }

        self.stop_process(reason);

        if self.restarts.len() as u32 >= self.p.max_restarts {
            let msg = format!("{} restarts within {}s; giving up ({})", self.restarts.len(), self.p.restart_window_secs, reason.as_str());
            self.set_phase("gave_up", &msg);
            if self.p.evidence.email_on_gave_up {
                self.email("gave up", &msg, evidence);
            }
            let n = self.ctx.gave_ups.fetch_add(1, Ordering::SeqCst) + 1;
            let limit = self.ctx.cfg().reboot_after_gave_up;
            if limit > 0 && n >= limit {
                self.log(&format!("{n} give-ups: rebooting in 30 s"));
                platform::reboot();
            }
            return;
        }
        self.restarts.push_back(now);
        self.restarts_total += 1;
        let n = self.restarts.len() as u32;
        let delay = (self.p.relaunch_delay_secs.max(1) << (n - 1).min(6)).min(60);
        let msg = format!("{}; relaunch {n}/{} in {delay}s", reason.as_str(), self.p.max_restarts);
        self.set_phase("restarting", &msg);
        if self.p.evidence.email_on_restart {
            self.email("restarting", &msg, evidence);
        }
        self.relaunch_at = Some(now + Duration::from_secs(delay));
    }

    /// Graceful quit when TD can still answer, hard kill otherwise.
    fn stop_process(&mut self, reason: Reason) {
        let Some(pid) = self.pid else { return };
        if !self.alive() {
            self.pid = None;
            return;
        }
        let g = self.p.graceful.clone();
        let can_answer = matches!(reason, Reason::Unhealthy | Reason::Manual);
        if g.enabled && can_answer {
            if let Some(peer) = self.peer().filter(|p| p.can("quit")) {
                self.log(&format!("asking pid {pid} to quit (save: {})", g.save));
                let _ = self.ctx.bus.command(&peer, "quit", json!({"save": g.save}), Duration::from_secs(3));
                if platform::wait_gone(pid, Duration::from_secs(g.timeout_secs)) {
                    self.log("quit gracefully");
                    self.pid = None;
                    self.child = None;
                    return;
                }
                self.log("graceful quit timed out");
            }
        }
        let gone = platform::kill(pid, Duration::from_secs(10));
        self.log(&format!("killed pid {pid}{}", if gone { "" } else { " (still present after 10 s)" }));
        if let Some(mut c) = self.child.take() {
            let _ = c.try_wait();
        }
        self.pid = None;
    }

    fn email(&self, what: &str, msg: &str, attachments: Vec<PathBuf>) {
        let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
        notify::send(
            &self.ctx.cfg().email,
            format!("[TDXCardio] {} {what} on {host}", self.p.display_name()),
            format!("{msg}\n\nProject: {}\nRestarts total: {}\n", self.p.path, self.restarts_total),
            attachments,
            self.ctx.log.clone(),
        );
    }

    fn publish(&self, peer: Option<&Peer>) {
        let mut s = self.status.lock().unwrap();
        s.pid = self.pid;
        s.companion = peer.map(|p| p.kind().to_string()).unwrap_or_else(|| "none".into());
        s.companion_version = peer.and_then(|p| p.version.clone());
        s.last_heartbeat_secs = peer.and_then(|p| p.last_beat).map(|t| t.elapsed().as_secs_f64());
        s.fps = peer.and_then(|p| p.beat.fps);
        s.cook_ms = peer.and_then(|p| p.beat.cook_ms);
        s.dropped = peer.and_then(|p| p.beat.dropped);
        s.perform = peer.and_then(|p| p.beat.perform);
        s.hold_secs = peer.and_then(|p| p.hold_until).filter(|t| *t > Instant::now()).map(|t| (t - Instant::now()).as_secs_f64());
        s.restarts_total = self.restarts_total;
        s.restarts_recent = self.restarts.len() as u32;
        s.last_restart_reason = self.last_reason.clone();
    }

    /// Tell the Cardio COMP what state it is watched in, on every change.
    fn push_watch(&mut self, peer: Option<&Peer>) {
        let Some(peer) = peer.filter(|p| p.kind() == "cardio") else { return };
        // On every change, and every 5 s so a reconnected COMP catches up.
        let key = (self.phase, self.restarts_total);
        if self.pushed_phase == Some(key) && self.pushed_at.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.pushed_phase = Some(key);
        self.pushed_at = Instant::now();
        self.ctx.bus.push(
            peer,
            json!({"type": "watch", "v": 1, "id": peer.id, "phase": self.phase, "restarts": self.restarts_total, "since": now_unix()}),
        );
    }
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}
