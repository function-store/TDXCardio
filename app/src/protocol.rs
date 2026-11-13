//! Wire messages (docs/PROTOCOL.md) and project identity.

use serde::Deserialize;
use std::path::Path;

/// Normalized project id: absolute, forward slashes, no `\\?\`, lowercase on
/// Windows. Same function as TDXLU's `watch_project_id`.
pub fn project_id(path: &str) -> String {
    let p = Path::new(path);
    let canon = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let s = canon.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Increment-and-Save family: `.../Show.28.toe` -> `.../Show.toe`.
pub fn family_key(id: &str) -> String {
    if let Some(base) = id.strip_suffix(".toe") {
        if let Some(i) = base.rfind('.') {
            let suffix = &base[i + 1..];
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return format!("{}.toe", &base[..i]);
            }
        }
    }
    id.to_string()
}

/// Every field any companion may send; absent fields are None.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Incoming {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: Option<String>,
    pub path: Option<String>,
    pub project: Option<String>,
    pub td_pid: Option<u32>,
    // hello
    pub cmd_port: Option<u16>,
    pub utility: Option<bool>,
    pub utility_version: Option<String>,
    pub cardio: Option<String>,
    pub td_build: Option<String>,
    pub caps: Option<Vec<String>>,
    // heartbeat
    pub fps: Option<f64>,
    pub frame: Option<i64>,
    pub cook_ms: Option<f64>,
    pub dropped: Option<i64>,
    pub perform: Option<bool>,
    pub gpu_mb: Option<f64>,
    // hold / bye
    pub seconds: Option<f64>,
    pub reason: Option<String>,
    // result
    pub req: Option<String>,
}

impl Incoming {
    /// The project id this message speaks for, normalized.
    pub fn resolved_id(&self) -> Option<String> {
        [&self.id, &self.path, &self.project]
            .into_iter()
            .flatten()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(|s| if s.contains('/') || s.contains('\\') || s.ends_with(".toe") { project_id(s) } else { s.to_string() })
    }

    /// Parse one line: JSON, or the legacy `HB id=<id> fps=<n>` text heartbeat.
    pub fn parse(line: &str) -> Option<(Incoming, serde_json::Value)> {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            return None;
        }
        if line.starts_with('{') {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            let msg: Incoming = serde_json::from_value(value.clone()).ok()?;
            return Some((msg, value));
        }
        let rest = line.strip_prefix("HB ")?;
        let mut msg = Incoming { kind: "heartbeat".into(), ..Default::default() };
        for part in rest.split_whitespace() {
            if let Some(v) = part.strip_prefix("id=") {
                msg.id = Some(v.to_string());
            } else if let Some(v) = part.strip_prefix("fps=") {
                msg.fps = v.parse().ok();
            }
        }
        Some((msg, serde_json::Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_strips_increment_suffix() {
        assert_eq!(family_key("c:/a/show.28.toe"), "c:/a/show.toe");
        assert_eq!(family_key("c:/a/show.toe"), "c:/a/show.toe");
        assert_eq!(family_key("c:/a/v1.2b.toe"), "c:/a/v1.2b.toe");
    }

    #[test]
    fn parses_tdxlu_hello_and_legacy_hb() {
        let (m, _) = Incoming::parse(r#"{"type":"hello","v":1,"id":"c:/x/a.toe","cmd_port":12001,"utility":true,"td_pid":42}"#).unwrap();
        assert_eq!(m.kind, "hello");
        assert_eq!(m.cmd_port, Some(12001));
        assert_eq!(m.td_pid, Some(42));
        let (m, _) = Incoming::parse("HB id=c:/x/a.toe fps=59.9").unwrap();
        assert_eq!(m.kind, "heartbeat");
        assert_eq!(m.fps, Some(59.9));
    }
}
