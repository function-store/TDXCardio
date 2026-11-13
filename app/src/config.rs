//! `cardio.json`, the machine config: every option, with defaults. Unknown
//! keys are ignored and missing keys take the default, so an old file keeps
//! loading. Project rules and the row types live in `tdxcardio_core::config`.

pub use tdxcardio_core::config::*;
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
    /// What a watcher reads from the machine config.
    pub fn machine(&self) -> Machine {
        Machine { td_executable: self.td_executable.clone(), email: self.email.clone(), reboot_after_gave_up: self.reboot_after_gave_up }
    }
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
    fn rules_and_entries_split_cleanly() {
        let cfg: Config = serde_json::from_str(r#"{"projects":[{"path":"a.toe","timeout_secs":5}]}"#).unwrap();
        assert_eq!(cfg.projects[0].path, "a.toe");
        assert!(serde_json::to_string(&cfg).unwrap().find("timeout_secs").is_none());
    }
}
