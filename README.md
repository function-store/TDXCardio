# TDXCardio

A standalone heartbeat watchdog for TouchDesigner projects, for Windows and macOS.
It keeps a show running: it launches the `.toe` file (or attaches to one that is
already open), watches its heartbeat, and restarts it when it crashes, freezes or
runs unhealthy. Each restart collects evidence (a screenshot, a log and an
optional email).

It speaks the **TDX Launcher Ultra (TDXLU)** heartbeat protocol. It runs on its own, it can run beside TDXLU, and it adds a
command channel back into TD.

| Piece | Where | What |
|---|---|---|
| `app/` | Rust crate `tdxcardio` | the watchdog: egui window + tray, or `--headless` |
| `/Cardio` COMP | `TDXCardio.toe`, externalized as TDXN; `release/Cardio.tox` to drop into any project | the TD companion that sends heartbeats and answers commands |
| `docs/PROTOCOL.md` | | the wire contract (cardio v1, a superset of the TDXLU bus) |
| `app/tools/fake_td.py` | | a fake TD session for testing without TouchDesigner |

## What it adds over the TDXLU watch

| | TDXLU watch | TDXCardio |
|---|---|---|
| Crash detection | heartbeat timeout | **checks the process ID**: relaunch in about 1 s, not 60 s |
| Hung TD | timeout, then kill | screenshot first, then kill |
| Low fps or slow frames | no | optional `health` rule: `log` or `restart` |
| Known long stalls (load, render) | trip the watchdog | TD sends `hold`, and Cardio waits |
| Restart | kill + relaunch | **graceful** `quit` (optional save) when TD can still answer; exponential backoff; give up on a crash loop |
| Back channel into TD | separate `cmd_port` span | same socket: `ping`, `save`, `quit`, `perform`, plus the watch state shown in TD |
| Focus / fullscreen | no | **optional**: `keep_focus` (OS-level), `perform_mode` + `topmost` (TD perform window) |
| Multi-project | yes | yes; attaches to sessions that are already running |
| Status for other tools | UI | `status.json` rewritten every second |

## Running it

```bash
cd app && cargo build --release
```

`target/release/tdxcardio(.exe) [--config cardio.json] [--headless | --settings]`

`--settings` opens the window on the Settings page.

The icon (a heart with a banana) is drawn in code, in `app/src/icon.rs`. On
Windows, `app/build.rs` turns it into a multi-size `.ico` and embeds it in the
`.exe`; this needs `rc.exe` from the Windows SDK, which comes with the Visual
Studio C++ build tools that Rust uses anyway.

The config file is `--config`, else `cardio.json` beside the executable, else
`%APPDATA%/TDXCardio/cardio.json` (or `~/Library/Application Support/TDXCardio`
on macOS). A default file is written if none exists. A config that fails to
parse is reported in the log and the window, and it is never overwritten.

Closing the window hides it to the tray. The tray's **Quit** stops watching and
leaves TouchDesigner running. Sessions that talk to Cardio but are not
watched are listed under "Sessions on the bus". Their **Watch** button adds them
to `cardio.json` in attach mode.

## The TD side

Drop `release/Cardio.tox` anywhere in a project. Its parameters:

- **Cardio page:** Active, Port (11998), Heartbeat Interval, Watch ID Override, and Hold / Hold Seconds / Release.
- **Status page:** Connected, Watch Phase and Restarts. TDXCardio drives these.

From Python:

```python
op('Cardio').Hold(300, 'loading 40 GB of media')
op('Cardio').Release()
```

Without the COMP, a project that already has **TDXLauncherUtility** with
Heartbeat Active is still watched. That needs the compat port (11999), which
Cardio holds only while TDXLU is not running. A TDXLU-only project gets
liveness, fps, `ping` and `save`, but no graceful quit, hold or perform.

## Ports

| Port | Owner | Notes |
|---|---|---|
| 11997 | TDXLU control HTTP server | not ours: Cardio moved off it after a live collision |
| **11998** | TDXCardio `cardio_port` | the Cardio COMP connects here |
| 11999 | TDXLU bus / Cardio `compat_port` | Cardio binds it only when nothing else serves it, and retries every 10 s |
| 12000-12099 | TDXLauncherUtility `cmd_port` | Cardio uses these only to command TDXLU-only peers |

Before binding a port, Cardio checks whether something already answers there.
Windows lets `127.0.0.1:N` bind beside another app's `0.0.0.0:N`, which caused a
silent split-brain during testing.

## Settings

Click **Settings** in the window. **App** holds this machine's settings.

The **Projects** list shows every project TDXCardio has seen: those in
`cardio.json`, plus any that have ever talked on the bus. The list is kept in
`known_projects.json` in the data folder, so a project stays listed when its
TouchDesigner session is closed. Each row has a checkbox that sets whether this
machine watches the project; the switch at the top of the project page does the
same. Rules can be edited whether or not the project is watched. **Forget**
removes an unwatched project from the list. Every setting has a circled **?**
that shows its tooltip on hover, and so do the Status table headers, phases and
port lamps.

Each project page has two parts, and each part says which file it saves to:

| Scope | File | Holds |
|---|---|---|
| This machine | `cardio.json` (beside the exe, else `%APPDATA%/TDXCardio`) | ports, default TD executable, data folder, status file, start hidden, reboot after give-ups, email (SMTP); and which projects this machine watches: `.toe` path, watch on start, launch or attach, TD executable override |
| The project | `<Name>.cardio.json` beside the `.toe` | display name, TD arguments, timeouts, restart limits, graceful restart, health, window (focus, perform, topmost), evidence |

A project's rules travel with it: commit `Show.cardio.json` next to `Show.toe`,
and every machine that watches the show uses the same rules.
Increment-and-Save copies (`Show.28.toe`) share `Show.cardio.json`. A missing
rules file means defaults.

**Save** writes only what changed and applies it immediately. Watchers pick up
their new rules, and removed projects stop being watched. A save never restarts
TouchDesigner. Port and data-folder changes are the only ones that need a
TDXCardio restart, and the save message says so.

Watchers also re-read their rules file within 2 s when it changes on disk, for
example after a `git pull`. A rules file that fails to parse keeps the previous
rules and logs the error. When a project starts with a broken rules file, it is
shown as `error` and not watched; the app never falls back to defaults.

`cardio.json`:

```json
{
  "cardio_port": 11998,
  "compat_port": 11999,
  "data_dir": "",
  "status_file": "",
  "td_executable": "",
  "reboot_after_gave_up": 0,
  "start_hidden": false,
  "email": { "enabled": false, "smtp_host": "smtp.gmail.com", "smtp_port": 587, "security": "starttls",
             "username": "", "password_env": "TDXCARDIO_SMTP_PASSWORD", "from": "", "to": [], "attach": true },
  "projects": [
    { "path": "D:/Shows/Main.toe", "enabled": true, "mode": "launch", "td_executable": "" }
  ]
}
```

`D:/Shows/Main.cardio.json`:

```json
{
  "name": "Main show",
  "args": [],
  "launch_grace_secs": 60,
  "timeout_secs": 30,
  "relaunch_delay_secs": 3,
  "max_restarts": 5,
  "restart_window_secs": 600,
  "relaunch_on_clean_exit": false,
  "graceful": { "enabled": true, "save": false, "timeout_secs": 10 },
  "health":   { "enabled": false, "min_fps": 20, "max_cook_ms": 0, "bad_secs": 30, "action": "log" },
  "window":   { "keep_focus": false, "focus_interval_secs": 5, "perform_mode": false, "topmost": false },
  "evidence": { "screenshot": true, "email_on_restart": false, "email_on_gave_up": true }
}
```

- **`mode`**: `launch` starts TD, or adopts a running session of the same project
  when its companion is already talking. `attach` never starts TD itself; it
  relaunches only after the session dies or stalls.
- **`td_executable`**: empty means the newest install under
  `C:/Program Files/Derivative` or `/Applications/TouchDesigner*.app`.
- **Relaunch delay**: it doubles per restart inside the window, capped at 60 s.
  Reaching `max_restarts` within `restart_window_secs` sets the phase to
  `gave_up`, sends the email, and counts toward `reboot_after_gave_up`.
- **Data folders**: `data_dir` (default: beside the config) holds `logs/`
  (one file per day), `captures/` and `status.json`. Set `status_file` to
  `"off"` to disable the status file.
- **Older files**: rule keys still inside a `cardio.json` project row are
  ignored, and the log names the rules file they belong in.

## Single sources of truth

| Data | Lives in | Reaches TD as | Reaches the app as |
|---|---|---|---|
| Watch rules (timeouts, health, window, evidence) | `watch` messages (phase, restarts) → Status page parameters | re-read within 2 s of any change |
| Which projects this machine watches, ports, email | | loaded at start; Settings saves apply live |
| Port, interval, hold length | `/Cardio` custom parameters (`.tdxn`) | n/a | `hello` / `heartbeat` / `hold` |
| Project identity | | `id` on every message |
| Watch state for other tools | | `status.json` |
| Wire format | `CardioExt.py` | `protocol.rs`, `bus.rs` |

## Testing

`app/tools/fake_td.py` acts as a TD session (Cardio or TDXLU dialect). Its
`<file>.mode` sidecar switches it between `ok`, `lowfps`, `hang`, `exit`, `hold`
and `tdxlu`. The scenario covers every phase transition, plus live reload of a
project's rules file and a broken rules file keeping the previous rules. It ran
green on 2026-11-13 (17/17), and against the live TDXCardio.toe (watching, hold,
reconnect after an extension reinit).

## About and support

TDXCardio is made by Function Store. The window's **About** page has these
links, plus the locations of the settings and log files to attach to a
support request.

- Source code and issues: https://github.com/function-store/TDXCardio
- Function Store: https://functionstore.xyz/link-in-bio
- Support on Patreon: https://patreon.com/function_store

## Acknowledgements

TDXCardio was inspired by [Stalker](https://github.com/interactiveimmersivehq/Stalker/)
by The Interactive & Immersive HQ, a TouchDesigner watchdog.

## License

MIT, see [LICENSE](LICENSE).
