//! End to end through the path TDX Launcher Ultra uses: the app owns the
//! TDXLU port and hands each line to `Bus::ingest_line`, commands go to the
//! companion's `cmd_port`, and the app supplies its own launcher. The
//! "TouchDesigner" is `app/tools/fake_td.py` speaking TDXLauncherUtility.
//!
//! A second test covers the standalone app's own path (the Cardio COMP on
//! the cardio port).
//!
//! Needs `python` on PATH and takes about a minute:
//! `cargo test --test tdxlu_path -- --ignored --nocapture`

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tdxcardio_core::bus::Bus;
use tdxcardio_core::config::{self, Entry, Machine, Rules};
use tdxcardio_core::log::Log;
use tdxcardio_core::watch::{self, Ctx, Launched, Launcher, Status, Watcher};

fn fake_td() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/tools/fake_td.py")
}

/// The app's own listener: every line goes to the bus, as TDXLU's hub does.
fn tdxlu_listener(bus: Bus) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        for s in l.incoming().flatten() {
            let bus = bus.clone();
            thread::spawn(move || {
                for line in BufReader::new(s).lines().map_while(Result::ok) {
                    bus.ingest_line(&line);
                }
            });
        }
    });
    port
}

struct Show {
    toe: PathBuf,
    watcher: Watcher,
}

impl Show {
    fn mode(&self, m: &str) {
        std::fs::write(format!("{}.mode", self.toe.display()), m).unwrap();
    }
    fn status(&self) -> Status {
        self.watcher.status.lock().unwrap().clone()
    }
    /// Wait until `f` holds, printing the phase as it changes.
    fn wait(&self, what: &str, secs: u64, f: impl Fn(&Status) -> bool) -> Status {
        let until = Instant::now() + Duration::from_secs(secs);
        let mut last = String::new();
        loop {
            let s = self.status();
            if s.phase != last {
                println!("  [{}] {} : {}", self.toe.file_name().unwrap().to_string_lossy(), s.phase, s.message);
                last = s.phase.clone();
            }
            if f(&s) {
                return s;
            }
            assert!(Instant::now() < until, "timed out waiting for {what}; status {s:#?}");
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn kill(&self) {
        let _ = self.watcher.ctl.send(watch::Ctl::Stop);
        if let Some(pid) = self.status().pid {
            tdxcardio_core::platform::kill(pid, Duration::from_secs(5));
        }
    }
}

fn show(dir: &Path, name: &str, ctx: &Arc<Ctx>, port: u16, caps: bool, rules: Rules) -> Show {
    let env = vec![
        ("COMPAT_PORT", port.to_string()),
        ("FAKE_COMPANION", "tdxlu".to_string()),
        ("FAKE_CAPS", if caps { "1" } else { "0" }.to_string()),
    ];
    show_with(dir, name, ctx, env, rules)
}

fn show_with(dir: &Path, name: &str, ctx: &Arc<Ctx>, env: Vec<(&'static str, String)>, rules: Rules) -> Show {
    let toe = dir.join(format!("{name}.toe"));
    std::fs::copy(fake_td(), &toe).unwrap();
    std::fs::write(format!("{}.mode", toe.display()), "ok").unwrap();
    config::save_rules(&toe.to_string_lossy(), &rules).unwrap();
    let launcher: Launcher = Arc::new(move |p| {
        let child = Command::new("python")
            .arg(&p.path)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(Launched { pid: Some(child.id()), child: Some(child) })
    });
    let entry = Entry { path: toe.to_string_lossy().into_owned(), ..Default::default() };
    Show { toe, watcher: watch::spawn(entry, ctx.clone(), Some(launcher)) }
}

fn quick_rules() -> Rules {
    Rules {
        launch_grace_secs: 15,
        timeout_secs: 4,
        relaunch_delay_secs: 1,
        max_restarts: 10,
        evidence: config::Evidence { screenshot: false, email_on_restart: false, email_on_gave_up: false },
        ..Rules::default()
    }
}

#[test]
#[ignore = "spawns python fake TouchDesigner sessions; run with --ignored"]
fn tdxlu_companion_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tdxcardio-tdxlu-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bus = Bus::new();
    let port = tdxlu_listener(bus.clone());
    let ctx = Arc::new(Ctx::new(Machine::default(), bus, Log::new(&dir), dir.clone()));

    // 1. A current companion: launched (after the first-hello wait), watched,
    //    with the heartbeat fields the health rules read.
    let a = show(&dir, "Alpha", &ctx, port, true, quick_rules());
    let s = a.wait("watching", 30, |s| s.phase == "watching");
    assert_eq!(s.companion, "tdxlu");
    assert_eq!(s.fps, Some(60.0));
    assert!(s.cook_ms.is_some(), "cook_ms reaches the watcher");
    let first_pid = s.pid.unwrap();
    // The watcher tells the companion how it is watched (`watch` verb).
    let watch_file = format!("{}.watch", a.toe.display());
    let until = Instant::now() + Duration::from_secs(10);
    let pushed = loop {
        let v: Option<serde_json::Value> = std::fs::read_to_string(&watch_file).ok().and_then(|t| serde_json::from_str(&t).ok());
        if let Some(v) = v.filter(|v| v["phase"] == "watching") {
            break v;
        }
        assert!(Instant::now() < until, "no `watch` push reached the companion");
        thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(pushed["action"], "watch");

    // 2. Crash: relaunched by pid, without waiting out the heartbeat timeout.
    a.mode("exit");
    let t0 = Instant::now();
    let s = a.wait("restart after exit", 10, |s| s.restarts_total == 1);
    assert_eq!(s.last_restart_reason.as_deref(), Some("exited"));
    let detect = t0.elapsed();
    println!("  exit detected after {detect:?} (fake checks its mode file every 1 s)");
    assert!(detect < Duration::from_secs(3), "exit took {detect:?}; the timeout is 4 s");
    a.mode("ok");
    let s = a.wait("watching again", 30, |s| s.phase == "watching" && s.pid != Some(first_pid));
    let second_pid = s.pid.unwrap();

    // 3. Hold: silence during a hold is not a stall; after it, it is.
    a.mode("hold");
    a.wait("held", 10, |s| s.phase == "held");
    let s = a.wait("stall after the hold", 30, |s| s.restarts_total == 2);
    assert_eq!(s.last_restart_reason.as_deref(), Some("stalled"));
    a.mode("ok");
    a.wait("watching after stall", 30, |s| s.phase == "watching" && s.pid != Some(second_pid));

    // 4. Health rule, hot-reloaded from the sidecar, restarts through a
    //    graceful quit (the companion lists `quit` in its caps).
    let mut r = quick_rules();
    r.health.enabled = true;
    r.health.min_fps = 20.0;
    r.health.bad_secs = 2;
    r.health.action = "restart".into();
    config::save_rules(&a.toe.to_string_lossy(), &r).unwrap();
    a.wait("rules reloaded", 10, |s| s.recent.iter().any(|l| l.contains("rules reloaded")));
    a.mode("lowfps");
    a.wait("unhealthy", 15, |s| s.phase == "unhealthy");
    let s = a.wait("health restart", 20, |s| s.restarts_total == 3);
    assert_eq!(s.last_restart_reason.as_deref(), Some("unhealthy"));
    assert!(s.recent.iter().any(|l| l.contains("quit gracefully")), "graceful quit: {:#?}", s.recent);
    a.mode("ok");
    a.wait("watching after health restart", 30, |s| s.phase == "watching");
    a.kill();

    // 5. An older companion (no caps): the same health restart falls back to
    //    a kill, because it cannot quit gracefully.
    let mut r = quick_rules();
    r.health.enabled = true;
    r.health.min_fps = 20.0;
    r.health.bad_secs = 2;
    r.health.action = "restart".into();
    let b = show(&dir, "Bravo", &ctx, port, false, r);
    b.wait("watching", 30, |s| s.phase == "watching");
    b.mode("lowfps");
    let s = b.wait("killed", 30, |s| s.recent.iter().any(|l| l.contains("killed pid")));
    assert!(!s.recent.iter().any(|l| l.contains("asking pid")), "no quit sent to a companion without caps");
    b.mode("ok");
    b.wait("watching after kill", 30, |s| s.phase == "watching");
    b.kill();

    // 6. Increment and Save: the companion now reports Charlie.2.toe, so a
    //    restart relaunches that file; both share Charlie.cardio.json.
    let c = show(&dir, "Charlie", &ctx, port, true, quick_rules());
    c.wait("watching", 30, |s| s.phase == "watching");
    let saved = dir.join("Charlie.2.toe");
    std::fs::copy(fake_td(), &saved).unwrap();
    std::fs::write(format!("{}.mode", saved.display()), "ok").unwrap();
    assert_eq!(config::sidecar_path(&saved.to_string_lossy()), config::sidecar_path(&c.toe.to_string_lossy()));
    let _ = c.watcher.ctl.send(watch::Ctl::Retarget(saved.to_string_lossy().into_owned()));
    c.mode("exit");
    let s = c.wait("relaunched the saved file", 30, |s| s.phase == "watching" && s.restarts_total == 1);
    assert!(s.path.ends_with("Charlie.2.toe"), "relaunched {}", s.path);
    c.kill();

    let _ = std::fs::remove_dir_all(&dir);
}

/// The standalone app's own path: the Cardio COMP on the cardio port, with
/// commands and the `watch` push on its connection.
#[test]
#[ignore = "spawns a python fake TouchDesigner session; run with --ignored"]
fn cardio_companion_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tdxcardio-cardio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let log = Log::new(&dir);
    let bus = Bus::start(port, 0, log.clone());
    let ctx = Arc::new(Ctx::new(Machine::default(), bus, log, dir.clone()));
    let mut r = quick_rules();
    r.health.enabled = true;
    r.health.min_fps = 20.0;
    r.health.bad_secs = 2;
    r.health.action = "restart".into();
    let d = show_with(&dir, "Delta", &ctx, vec![("CARDIO_PORT", port.to_string())], r);
    let s = d.wait("watching", 30, |s| s.phase == "watching");
    assert_eq!(s.companion, "cardio");
    d.mode("exit");
    let s = d.wait("restart after exit", 10, |s| s.restarts_total == 1);
    assert_eq!(s.last_restart_reason.as_deref(), Some("exited"));
    d.mode("ok");
    d.wait("watching again", 30, |s| s.phase == "watching");
    d.mode("lowfps");
    let s = d.wait("health restart", 30, |s| s.restarts_total == 2);
    assert!(s.recent.iter().any(|l| l.contains("quit gracefully")), "graceful quit: {:#?}", s.recent);
    d.mode("ok");
    d.wait("watching after health restart", 30, |s| s.phase == "watching");
    d.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

/// TDXLU's own shape: one bus fed by its TDXLU-port listener AND serving the
/// Cardio port (`listen_cardio`). A TDXLauncherUtility project and a Cardio
/// COMP project are watched side by side, and one crashing leaves the other be.
#[test]
#[ignore = "spawns python fake TouchDesigner sessions; run with --ignored"]
fn both_companions_on_one_bus() {
    let dir = std::env::temp_dir().join(format!("tdxcardio-both-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = Log::new(&dir);
    let bus = Bus::new();
    let compat = tdxlu_listener(bus.clone());
    let cardio_port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    bus.listen_cardio(cardio_port, log.clone());
    let t0 = Instant::now();
    while !bus.listening().0 {
        assert!(t0.elapsed() < Duration::from_secs(5), "cardio port never bound");
        thread::sleep(Duration::from_millis(50));
    }
    let ctx = Arc::new(Ctx::new(Machine::default(), bus, log, dir.clone()));
    let e = show(&dir, "Echo", &ctx, compat, true, quick_rules());
    let f = show_with(&dir, "Foxtrot", &ctx, vec![("CARDIO_PORT", cardio_port.to_string())], quick_rules());
    assert_eq!(e.wait("utility watching", 30, |s| s.phase == "watching").companion, "tdxlu");
    assert_eq!(f.wait("cardio watching", 30, |s| s.phase == "watching").companion, "cardio");
    f.mode("exit");
    let s = f.wait("cardio restart after exit", 10, |s| s.restarts_total == 1);
    assert_eq!(s.last_restart_reason.as_deref(), Some("exited"));
    f.mode("ok");
    f.wait("cardio watching again", 30, |s| s.phase == "watching");
    let s = e.status();
    assert_eq!((s.phase.as_str(), s.restarts_total), ("watching", 0), "utility project untouched");
    e.kill();
    f.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
