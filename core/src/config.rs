//! Watch settings: the project rules in `<Name>.cardio.json` beside the .toe
//! ([`Rules`]), the machine row that says a project is watched here
//! ([`Entry`]) and the machine-wide bits a watcher needs ([`Machine`]).
//! Unknown keys are ignored and missing keys take the default, so an old file
//! keeps loading.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Machine-wide settings a watcher reads (the app owns where they are stored).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Machine {
    /// TouchDesigner executable used when a project names none. Empty = newest install found.
    pub td_executable: String,
    pub email: Email,
    /// Reboot the machine after this many give-ups (0 = never).
    pub reboot_after_gave_up: u32,
}

/// Machine scope: one row per project this machine watches (in `cardio.json`).
/// Everything about HOW to watch lives in the project's sidecar ([`Rules`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Entry {
    /// Path to the .toe.
    pub path: String,
    /// Start watching when Cardio starts.
    pub enabled: bool,
    /// `launch`: Cardio starts TD. `attach`: watch a TD that is already running
    /// (it is relaunched only after it dies or stalls).
    pub mode: String,
    /// TouchDesigner executable for this project on this machine (overrides the global one).
    pub td_executable: String,
    /// Runtime identity, so edits find their watcher even when rows move.
    #[serde(skip)]
    pub uid: u64,
    /// A TD process already running this project, to adopt on start without
    /// waiting for its companion's hello. Runtime only.
    #[serde(skip)]
    pub attach_pid: Option<u32>,
}

impl Default for Entry {
    fn default() -> Self {
        Self { path: String::new(), enabled: true, mode: "launch".into(), td_executable: String::new(), uid: next_uid(), attach_pid: None }
    }
}

pub fn next_uid() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Project scope: the watch rules, stored beside the .toe as `<Name>.cardio.json`
/// so they travel with the project (git, other machines).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Rules {
    /// Display name (logs, emails). Empty = file name.
    pub name: String,
    /// Extra command-line arguments for TD.
    pub args: Vec<String>,
    /// Seconds after launch before heartbeats are required.
    pub launch_grace_secs: u64,
    /// Seconds without a heartbeat before the project counts as stalled.
    pub timeout_secs: u64,
    /// Wait before a relaunch; doubles per consecutive restart, capped at 60 s.
    pub relaunch_delay_secs: u64,
    /// Restarts allowed inside `restart_window_secs` before giving up.
    pub max_restarts: u32,
    pub restart_window_secs: u64,
    /// Relaunch after TD quits cleanly (a `bye` with reason `quit`).
    pub relaunch_on_clean_exit: bool,
    pub graceful: Graceful,
    pub health: Health,
    pub window: Window,
    pub evidence: Evidence,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            name: String::new(),
            args: Vec::new(),
            launch_grace_secs: 60,
            timeout_secs: 30,
            relaunch_delay_secs: 3,
            max_restarts: 5,
            restart_window_secs: 600,
            relaunch_on_clean_exit: false,
            graceful: Graceful::default(),
            health: Health::default(),
            window: Window::default(),
            evidence: Evidence::default(),
        }
    }
}

/// The sidecar for a .toe: `Show.toe` and its Increment-and-Save copies
/// (`Show.28.toe`) all share `Show.cardio.json` in the same folder.
pub fn sidecar_path(toe: &str) -> PathBuf {
    let p = Path::new(toe);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let family = match stem.rsplit_once('.') {
        Some((base, n)) if !base.is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => base.to_string(),
        _ => stem,
    };
    p.with_file_name(format!("{family}.cardio.json"))
}

/// Missing sidecar = defaults (nothing is written until someone saves rules).
pub fn load_rules(toe: &str) -> Result<Rules, String> {
    let path = sidecar_path(toe);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Rules::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Written whole through a temp file, so a watcher polling the sidecar never
/// reads it half-written.
pub fn save_rules(toe: &str, rules: &Rules) -> Result<(), String> {
    let path = sidecar_path(toe);
    let text = serde_json::to_string_pretty(rules).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

/// A watched project at runtime: its machine row plus its rules, flattened.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub uid: u64,
    pub path: String,
    pub enabled: bool,
    pub mode: String,
    pub td_executable: String,
    pub name: String,
    pub args: Vec<String>,
    pub launch_grace_secs: u64,
    pub timeout_secs: u64,
    pub relaunch_delay_secs: u64,
    pub max_restarts: u32,
    pub restart_window_secs: u64,
    pub relaunch_on_clean_exit: bool,
    pub graceful: Graceful,
    pub health: Health,
    pub window: Window,
    pub evidence: Evidence,
}

impl Project {
    pub fn resolve(e: &Entry, r: &Rules) -> Project {
        let r = r.clone();
        Project {
            uid: e.uid,
            path: e.path.clone(),
            enabled: e.enabled,
            mode: e.mode.clone(),
            td_executable: e.td_executable.clone(),
            name: r.name,
            args: r.args,
            launch_grace_secs: r.launch_grace_secs,
            timeout_secs: r.timeout_secs,
            relaunch_delay_secs: r.relaunch_delay_secs,
            max_restarts: r.max_restarts,
            restart_window_secs: r.restart_window_secs,
            relaunch_on_clean_exit: r.relaunch_on_clean_exit,
            graceful: r.graceful,
            health: r.health,
            window: r.window,
            evidence: r.evidence,
        }
    }
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        Path::new(&self.path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.clone())
    }
    pub fn is_attach(&self) -> bool {
        self.mode.eq_ignore_ascii_case("attach")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Graceful {
    /// Ask TD to quit (Cardio COMP `quit` verb) before killing it. Not used for stalls.
    pub enabled: bool,
    /// Save before the graceful quit.
    pub save: bool,
    pub timeout_secs: u64,
}
impl Default for Graceful {
    fn default() -> Self {
        Self { enabled: true, save: false, timeout_secs: 10 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Health {
    pub enabled: bool,
    /// Below this fps counts as unhealthy (0 = ignore fps).
    pub min_fps: f64,
    /// Above this frame cook time in ms counts as unhealthy (0 = ignore).
    pub max_cook_ms: f64,
    /// How long it must stay unhealthy before acting.
    pub bad_secs: u64,
    /// `log` or `restart`.
    pub action: String,
}
impl Default for Health {
    fn default() -> Self {
        Self { enabled: false, min_fps: 20.0, max_cook_ms: 0.0, bad_secs: 30, action: "log".into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Window {
    /// Re-focus TD's main window every `focus_interval_secs`. Off by default.
    pub keep_focus: bool,
    pub focus_interval_secs: u64,
    /// Ask the Cardio COMP to enter perform mode (fullscreen per the perform Window COMP).
    pub perform_mode: bool,
    /// With perform_mode: set the perform window Always On Top.
    pub topmost: bool,
}
impl Default for Window {
    fn default() -> Self {
        Self { keep_focus: false, focus_interval_secs: 5, perform_mode: false, topmost: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Evidence {
    /// Full-desktop screenshot on every stall / health restart.
    pub screenshot: bool,
    /// Email on these events (needs the top-level `email` block).
    pub email_on_restart: bool,
    pub email_on_gave_up: bool,
}
impl Default for Evidence {
    fn default() -> Self {
        Self { screenshot: true, email_on_restart: false, email_on_gave_up: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Email {
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// `starttls`, `tls` or `none`.
    pub security: String,
    pub username: String,
    /// Prefer `password_env`: the name of an environment variable holding the password.
    pub password: String,
    pub password_env: String,
    pub from: String,
    pub to: Vec<String>,
    /// Attach the day's log and the crash screenshot.
    pub attach: bool,
}
impl Default for Email {
    fn default() -> Self {
        Self {
            enabled: false,
            smtp_host: String::new(),
            smtp_port: 587,
            security: "starttls".into(),
            username: String::new(),
            password: String::new(),
            password_env: "TDXCARDIO_SMTP_PASSWORD".into(),
            from: String::new(),
            to: Vec::new(),
            attach: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_follows_increment_and_save_family() {
        let name = |p: &str| sidecar_path(p).file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name("C:/Shows/Main.toe"), "Main.cardio.json");
        assert_eq!(name("C:/Shows/Main.28.toe"), "Main.cardio.json");
        assert_eq!(name("C:/Shows/Main.v2.toe"), "Main.v2.cardio.json");
        assert_eq!(name("C:/Shows/2026.toe"), "2026.cardio.json");
    }
}
