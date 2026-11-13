//! `cardio.json`: every option, with defaults. Unknown keys are ignored and
//! missing keys take the default, so an old file keeps loading.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Port the Cardio COMP connects to.
    pub cardio_port: u16,
    /// TDXLU-compatible port; bound only while free (TDXLU owns it when running). 0 disables.
    pub compat_port: u16,
    /// Folder for logs/, captures/ and status.json. Empty = next to the config file.
    pub data_dir: String,
    /// Status snapshot written every second. Empty = `<data_dir>/status.json`. "off" disables.
    pub status_file: String,
    /// TouchDesigner executable used when a project names none. Empty = newest install found.
    pub td_executable: String,
    /// Projects this machine watches. Their rules live in each `<Name>.cardio.json`.
    pub projects: Vec<Entry>,
    pub email: Email,
    /// Reboot the machine after this many give-ups (0 = never).
    pub reboot_after_gave_up: u32,
    /// Start with the window hidden (tray only).
    pub start_hidden: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cardio_port: 11998,
            compat_port: 11999,
            data_dir: String::new(),
            status_file: String::new(),
            td_executable: String::new(),
            projects: Vec::new(),
            email: Email::default(),
            reboot_after_gave_up: 0,
            start_hidden: false,
        }
    }
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
}

impl Default for Entry {
    fn default() -> Self {
        Self { path: String::new(), enabled: true, mode: "launch".into(), td_executable: String::new(), uid: next_uid() }
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

pub fn save_rules(toe: &str, rules: &Rules) -> Result<(), String> {
    let path = sidecar_path(toe);
    let text = serde_json::to_string_pretty(rules).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
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

/// Where the config lives: `--config <path>`, else `cardio.json` beside the
/// executable if present, else the per-user config dir.
pub fn resolve_path(cli: Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli {
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        let beside = exe.with_file_name("cardio.json");
        if beside.exists() {
            return beside;
        }
    }
    user_dir().join("cardio.json")
}

fn user_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"));
    base.unwrap_or_else(|| PathBuf::from(".")).join("TDXCardio")
}

/// Load, or write a commented-by-example default when the file is missing.
pub fn load(path: &Path) -> Result<Config, String> {
    if !path.exists() {
        let cfg = Config::default();
        save(path, &cfg)?;
        return Ok(cfg);
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Projects in cardio.json that still carry watch rules (the pre-sidecar
/// layout). Those keys are ignored now, so say which file to move them to.
pub fn misplaced_rules(path: &Path) -> Vec<String> {
    let Ok(v) = std::fs::read_to_string(path).map(|t| serde_json::from_str::<serde_json::Value>(&t)) else { return Vec::new() };
    let Ok(v) = v else { return Vec::new() };
    let entry_keys = ["path", "enabled", "mode", "td_executable"];
    v["projects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let o = p.as_object()?;
            let extra: Vec<&str> = o.keys().map(String::as_str).filter(|k| !entry_keys.contains(k)).collect();
            let toe = o.get("path")?.as_str()?;
            (!extra.is_empty()).then(|| format!("{toe}: {} ignored here; set them in {} (Settings > project)", extra.join(", "), sidecar_path(toe).display()))
        })
        .collect()
}

pub fn save(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

impl Config {
    pub fn data_dir(&self, config_path: &Path) -> PathBuf {
        if self.data_dir.is_empty() {
            config_path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
        } else {
            PathBuf::from(&self.data_dir)
        }
    }
    pub fn status_path(&self, data_dir: &Path) -> Option<PathBuf> {
        match self.status_file.as_str() {
            "off" => None,
            "" => Some(data_dir.join("status.json")),
            p => Some(PathBuf::from(p)),
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

    #[test]
    fn rules_and_entries_split_cleanly() {
        let cfg: Config = serde_json::from_str(r#"{"projects":[{"path":"a.toe","timeout_secs":5}]}"#).unwrap();
        assert_eq!(cfg.projects[0].path, "a.toe");
        assert!(serde_json::to_string(&cfg).unwrap().find("timeout_secs").is_none());
    }
}
