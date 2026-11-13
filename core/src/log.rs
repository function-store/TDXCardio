//! One log file per day (`logs/cardio-YYYY-MM-DD.log`) plus a ring the UI shows.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const RING: usize = 500;

#[derive(Clone)]
pub struct Log {
    dir: PathBuf,
    ring: Arc<Mutex<VecDeque<String>>>,
}

impl Log {
    pub fn new(data_dir: &Path) -> Log {
        let dir = data_dir.join("logs");
        let _ = std::fs::create_dir_all(&dir);
        Log { dir, ring: Arc::default() }
    }

    pub fn line(&self, msg: &str) {
        let now = chrono::Local::now();
        let text = format!("{} {msg}", now.format("%Y-%m-%d %H:%M:%S"));
        // One locked write per line, so threads never interleave inside a line.
        let mut r = self.ring.lock().unwrap();
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.today()) {
            let _ = f.write_all(format!("{text}\n").as_bytes());
        }
        if r.len() >= RING {
            r.pop_front();
        }
        r.push_back(text);
    }

    pub fn today(&self) -> PathBuf {
        self.dir.join(format!("cardio-{}.log", chrono::Local::now().format("%Y-%m-%d")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn tail(&self, n: usize) -> Vec<String> {
        let r = self.ring.lock().unwrap();
        r.iter().rev().take(n).rev().cloned().collect()
    }
}
