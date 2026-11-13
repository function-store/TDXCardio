# TDXCardio wire protocol (cardio v1)

TDXCardio is a superset of the TDX Launcher Ultra (TDXLU) utility bus
(`TDXLPP/utility/heartbeat/PROTOCOL.md`). Anything a TDXLauncherUtility companion
sends is accepted as is. The Cardio COMP adds richer heartbeats and a command
channel that runs over the same socket.

## Transport

Localhost TCP only (`127.0.0.1`). Messages are newline-delimited JSON (UTF-8), one
object per line, and every message carries `"v": 1`.

| Listener | Port | Who connects | Commands back to TD |
|---|---|---|---|
| Cardio | **11998** (`cardio_port`) | the Cardio COMP | on the **same connection**: no second port, no port span |
| TDXLU compat | **11999** (`compat_port`) | TDXLauncherUtility (`tcpip_out`) | to the peer's advertised `cmd_port` (12000-12099), the TDXLU way |

Cardio binds the compat port only while it is free. When TDXLU is running, TDXLU
keeps 11999 and Cardio retries the bind every 10 s, so the two apps never fight
over it. A project watched through TDXLauncherUtility while TDXLU is up must
therefore run the Cardio COMP as well, or be watched by TDXLU instead.

## Identity

`id` is the normalized absolute `.toe` path: forward slashes, no `\\?\` prefix,
lowercased on Windows. It is the same function as TDXLU's `watch_project_id`.
Cardio matches a message to a watched project by `td_pid` first and by `id` second.
It then falls back to the Increment-and-Save family (`Show.12.toe` belongs to
`Show.toe`).

## Companion -> Cardio

| type | When | Fields |
|---|---|---|
| `hello` | on connect, then every 2 s | `id`, `td_pid`, `cardio` (COMP version), `td_build`, `caps` (list of verbs it answers); TDXLU adds `cmd_port`, `utility: true`, `utility_version` (and `caps` since it learnt the verbs below) |
| `heartbeat` | every `Interval` s (default 1) | `id`, `fps`, `frame` (absTime.frame), `cook_ms` (last frame cook time), `dropped` (dropped frames since start), `perform` (bool), `gpu_mb` (optional) |
| `hold` | before a known long stall (a big load, a save, a render) | `id`, `seconds` (at most 600), `reason`: Cardio suspends stall detection for that long |
| `release` | when the long stall is over | `id` |
| `bye` | on COMP teardown or project quit | `id`, `reason` (`quit`, `reinit`, `quit_requested`): a `quit` bye is a clean exit, see `relaunch_on_clean_exit`. A bye without a reason (older TDXLauncherUtility) counts as `quit` |
| `result` | reply to a `cmd` | `req`, `ok`, `error` when `ok` is false, plus the verb's fields |

`dropped` is the Perform CHOP's `dropped_frames` reading at the beat.

Legacy plain text `HB id=<id> fps=<fps>` is still accepted as a heartbeat, in
any case, with `path=` or `project=` in place of `id=`.

## Cardio -> companion (Cardio COMP only, same connection)

```json
{"type":"cmd","v":1,"id":"c:/shows/a.toe","action":"perform","req":"7","on":true}
```

| action | Fields | Effect |
|---|---|---|
| `ping` | none | replies `{ok:true}` |
| `save` | none | `project.save()` |
| `quit` | `save` (bool) | graceful quit: an optional save, a `bye` with `reason: quit_requested`, then `project.quit(force=True)` |
| `perform` | `on` (bool), `topmost` (bool, optional) | enters or leaves perform mode; `topmost` sets the perform Window COMP's Always On Top. This is the cross-platform fullscreen. |

Cardio also pushes `{"type":"watch","v":1,"id":...,"phase":"watching","restarts":2,"since":<unix s>}`
on every phase change, so the COMP can show that it is being watched and by what.

Against a TDXLauncherUtility peer, Cardio uses the TDXLU `cmd` envelope on
`cmd_port`, one connection per command, and sends a verb only when the peer's
`caps` list it. A current TDXLauncherUtility lists `ping`, `save`, `quit`,
`perform` and `watch`; the `watch` push then arrives as a `watch` command with
the same fields. A peer without `caps` (an older one) gets only `ping` and
`save`, so a graceful quit is not possible there and Cardio goes straight to a
hard kill. TDXLauncherUtility sends `hold`, `release` and `bye` on its 11999
connection, like the Cardio COMP.

An app that owns the TDXLU port itself (TDX Launcher Ultra) feeds each line it
reads to the same bus code (`tdxcardio-core`, `Bus::ingest_line`), so the
rules above hold there unchanged.

## Cardio's state machine (per watched project)

```
idle -> launching -> grace -> watching <-> unhealthy
                       ^         |  stall / exit / health
                       |         v
                       +--- restarting (graceful quit -> kill -> backoff)
                                 |  restart cap reached within window
                                 v
                              gave_up (-> optional reboot)
```

* **Start**: a `launch` project waits up to 5 s after the app starts for a
  hello from a copy that is already running, and adopts it instead of opening
  the show twice.
* **Increment and Save**: when the companion starts reporting a newer file of
  the same family, relaunches open that file.
* **Exit**: the PID is gone. Cardio relaunches right away and does not wait out the
  heartbeat timeout (the TDXLU watch waited).
* **Stall**: no heartbeat for `timeout_secs` and no active `hold`. Cardio takes the
  screenshot first, then kills, because a hung main thread cannot quit gracefully.
* **Health** (optional): `fps < min_fps` or `cook_ms > max_cook_ms` for
  `bad_secs`. The action is `log` or `restart`, and a restart is graceful first.
