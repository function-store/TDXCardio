//! TDXCardio's watchdog engine, shared by the TDXCardio app and TDX Launcher
//! Ultra so both watch a project the same way and read the same
//! `<Name>.cardio.json` rules. Wire contract: docs/PROTOCOL.md.
//!
//! - [`protocol`]: project identity and the messages a companion sends.
//! - [`config`]: project rules (the sidecar) and the machine row.
//! - [`bus`]: peer registry; can run its own listeners or be fed lines by an
//!   app that owns the socket.
//! - [`watch`]: one watcher thread per project (the state machine).
//! - [`platform`]: launch, kill, focus, screenshot, reboot.

pub mod bus;
pub mod config;
pub mod log;
#[cfg(feature = "smtp")]
pub mod notify;
pub mod platform;
pub mod protocol;
pub mod watch;
