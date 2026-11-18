//! egui window + tray. Closing the window hides it to the tray; the watchdog
//! threads never depend on the window being open.

use crate::config::{self, Config, Entry, Rules};
use tdxcardio_core::watch::Ctl;
use crate::Core;
use tdxcardio_core::{notify, protocol};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub fn run(core: Arc<Core>, start_hidden: bool, load_error: Option<String>, open_settings: bool) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("TDXCardio")
            .with_inner_size([1040.0, 720.0])
            .with_min_inner_size([520.0, 300.0])
            .with_visible(!start_hidden)
            .with_icon(egui::IconData { rgba: crate::icon::heart_rgba(64), width: 64, height: 64 }),
        ..Default::default()
    };
    eframe::run_native(
        "TDXCardio",
        options,
        Box::new(move |cc| {
            let tray = build_tray(&cc.egui_ctx);
            let draft = open_settings.then(|| draft_from_live(&core));
            let view = if open_settings { View::Settings } else { View::Status };
            Ok(Box::new(App { core, tray, load_error, quitting: false, view, draft, logo: None }))
        }),
    )
    .map_err(|e| e.to_string())
}

struct App {
    core: Arc<Core>,
    #[allow(dead_code)]
    tray: Option<TrayIcon>,
    load_error: Option<String>,
    quitting: bool,
    view: View,
    draft: Option<Draft>,
    /// The icon drawn large for the About page (made on first view).
    logo: Option<egui::TextureHandle>,
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Status,
    Settings,
    About,
}

#[derive(PartialEq, Clone, Copy)]
enum Sel {
    App,
    Project(u64),
}

/// The settings being edited: the machine config (cardio.json) and, per
/// project uid, its rules sidecar. `base` is what is saved and running.
struct Draft {
    base: Config,
    edit: Config,
    rules_base: HashMap<u64, Rules>,
    rules_edit: HashMap<u64, Rules>,
    rules_err: HashMap<u64, String>,
    /// The .toe path each project's rules were read for.
    rules_for: HashMap<u64, String>,
    args_text: HashMap<u64, String>,
    /// Projects seen before but not watched here: their rules stay editable.
    known: Vec<(u64, String)>,
    sel: Sel,
    note: Option<(bool, String)>,
}

impl Draft {
    fn new(base: Config, edit: Config) -> Draft {
        let mut d = Draft {
            base,
            edit,
            rules_base: HashMap::new(),
            rules_edit: HashMap::new(),
            rules_err: HashMap::new(),
            rules_for: HashMap::new(),
            args_text: HashMap::new(),
            known: Vec::new(),
            sel: Sel::App,
            note: None,
        };
        for e in d.edit.projects.clone() {
            d.load_rules(&e);
        }
        if let Some(e) = d.edit.projects.first() {
            d.sel = Sel::Project(e.uid);
        }
        d
    }

    /// Add known projects that are neither watched nor listed yet.
    fn merge_known(&mut self, known: Vec<String>) {
        for path in known {
            let fam = family(&path);
            let listed = self.edit.projects.iter().any(|e| family(&e.path) == fam) || self.known.iter().any(|(_, p)| family(p) == fam);
            if !listed {
                let uid = config::next_uid();
                self.load_rules_at(uid, &path);
                self.known.push((uid, path));
            }
        }
    }

    fn load_rules(&mut self, e: &Entry) {
        self.load_rules_at(e.uid, &e.path.clone());
    }

    fn load_rules_at(&mut self, uid: u64, path: &str) {
        self.rules_err.remove(&uid);
        self.rules_for.insert(uid, path.to_string());
        let r = if path.is_empty() { Ok(Rules::default()) } else { config::load_rules(path) };
        match r {
            Ok(r) => {
                self.args_text.insert(uid, r.args.join("\n"));
                self.rules_base.insert(uid, r.clone());
                self.rules_edit.insert(uid, r);
            }
            Err(err) => {
                self.rules_base.remove(&uid);
                self.rules_edit.remove(&uid);
                self.rules_err.insert(uid, err);
            }
        }
    }

    /// Start watching a known project on this machine (applies on Save).
    fn watch(&mut self, uid: u64) {
        let Some(i) = self.known.iter().position(|(u, _)| *u == uid) else { return };
        let (_, path) = self.known.remove(i);
        self.edit.projects.push(Entry { path, mode: "attach".into(), uid, ..Default::default() });
    }

    fn rules_dirty(&self, uid: u64) -> bool {
        self.rules_edit.get(&uid).is_some_and(|r| self.rules_base.get(&uid) != Some(r))
    }

    fn dirty(&self) -> bool {
        self.edit != self.base || self.rules_edit.keys().any(|k| self.rules_dirty(*k))
    }

    /// Stop watching on this machine; the project stays listed (Not watched)
    /// so its rules remain editable.
    fn unwatch(&mut self, uid: u64) {
        let Some(i) = self.edit.projects.iter().position(|e| e.uid == uid) else { return };
        let e = self.edit.projects.remove(i);
        if e.path.is_empty() {
            self.sel = Sel::App;
        } else {
            self.known.push((uid, e.path));
        }
    }

    fn forget(&mut self, uid: u64) {
        self.known.retain(|(u, _)| *u != uid);
        for m in [&mut self.rules_base, &mut self.rules_edit] {
            m.remove(&uid);
        }
        self.rules_err.remove(&uid);
        self.rules_for.remove(&uid);
        self.sel = Sel::App;
    }

    /// Write what changed (sidecars first, then cardio.json) and apply it live.
    fn save(&mut self, core: &Core) -> Result<String, String> {
        let mut written = Vec::new();
        let dirty: Vec<u64> = self.rules_edit.keys().copied().filter(|k| self.rules_dirty(*k)).collect();
        for uid in dirty {
            let path = self.rules_for.get(&uid).cloned().unwrap_or_default();
            if path.is_empty() {
                return Err("pick a .toe file before saving its rules".into());
            }
            let r = self.rules_edit.get_mut(&uid).unwrap();
            r.args.retain(|a| !a.trim().is_empty());
            config::save_rules(&path, r)?;
            written.push(file_name(&config::sidecar_path(&path)));
        }
        if self.edit != *core.cfg() {
            config::save(&core.config_path, &self.edit)?;
            written.push(file_name(&core.config_path));
        }
        core.apply(self.edit.clone());
        self.base = self.edit.clone();
        self.rules_base = self.rules_edit.clone();
        let mut msg = if written.is_empty() { "Nothing to save.".to_string() } else { format!("Saved {}.", written.join(", ")) };
        let restart = core.needs_restart(&self.edit);
        if !restart.is_empty() {
            msg.push_str(&format!(" Restart TDXCardio to apply: {}.", restart.join(", ")));
        }
        Ok(msg)
    }
}

fn family(path: &str) -> String {
    protocol::family_key(&protocol::project_id(path))
}

/// The settings as they run now; rules re-read from each sidecar.
fn draft_from_live(core: &Core) -> Draft {
    let live = (*core.cfg()).clone();
    let mut d = Draft::new(live.clone(), live);
    d.merge_known(core.known.lock().unwrap().clone());
    d
}

/// cardio.json as it is on disk, matched to the running watchers by path so
/// saving it updates them instead of starting new ones.
fn draft_from_disk(core: &Core) -> Result<Draft, String> {
    let mut disk = config::load(&core.config_path)?;
    let live = core.cfg();
    let mut used = Vec::new();
    for e in &mut disk.projects {
        if let Some(l) = live.projects.iter().find(|l| l.path == e.path && !used.contains(&l.uid)) {
            e.uid = l.uid;
            used.push(l.uid);
        }
    }
    let mut d = Draft::new((*live).clone(), disk);
    d.merge_known(core.known.lock().unwrap().clone());
    Ok(d)
}

fn norm(p: PathBuf) -> String {
    p.display().to_string().replace('\\', "/")
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
}

fn pick_toe() -> Option<String> {
    rfd::FileDialog::new().add_filter("TouchDesigner project", &["toe"]).pick_file().map(norm)
}

fn pick_exe() -> Option<String> {
    let d = rfd::FileDialog::new();
    #[cfg(windows)]
    let d = d.add_filter("Program", &["exe"]);
    d.pick_file().map(norm)
}

const SHOW: &str = "show";
const QUIT: &str = "quit";

/// Tray menu events arrive on the OS thread even while the window is hidden
/// (when `update` is not being called), so they act on the context directly.
fn build_tray(ctx: &egui::Context) -> Option<TrayIcon> {
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &MenuItem::with_id(SHOW, "Show TDXCardio", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(QUIT, "Quit (TouchDesigner keeps running)", true, None),
    ]);
    let c = ctx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| match e.id.0.as_str() {
        SHOW => show(&c),
        QUIT => std::process::exit(0),
        _ => {}
    }));
    let c = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if matches!(e, TrayIconEvent::DoubleClick { .. }) {
            show(&c);
        }
    }));
    let icon = Icon::from_rgba(crate::icon::heart_rgba(32), 32, 32).ok()?;
    TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("TDXCardio").with_icon(icon).build().ok()
}

fn show(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
}

fn phase_color(phase: &str) -> Color32 {
    match phase {
        "watching" => Color32::from_rgb(80, 200, 120),
        "grace" | "launching" | "waiting" | "held" => Color32::from_rgb(230, 190, 70),
        "unhealthy" | "restarting" => Color32::from_rgb(240, 140, 60),
        "gave_up" | "error" => Color32::from_rgb(235, 80, 80),
        _ => Color32::GRAY,
    }
}

fn open(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
}

/// Open a web link in the default browser.
fn open_url(url: &str) {
    open(std::path::Path::new(url));
}

const LINKS: [(&str, &str, &str); 4] = [
    ("Source code", "https://github.com/function-store/TDXCardio", "The TDXCardio repository on GitHub: code, releases and documentation."),
    ("Report an issue", "https://github.com/function-store/TDXCardio/issues", "Found a bug or want a feature? Open an issue on GitHub."),
    ("Function Store", "https://functionstore.xyz/link-in-bio", "More tools, links and contact."),
    ("Support on Patreon", "https://patreon.com/function_store", "Support Function Store on Patreon and help keep tools like this coming."),
];

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
        let core = self.core.clone();

        egui::Panel::top("top").show(root, |ui| {
            ui.horizontal(|ui| {
                ui.heading("TDXCardio");
                let (cardio, compat) = core.ctx.bus.listening();
                let (cp, xp) = core.ports;
                lamp(ui, cardio, &format!("cardio :{cp}"));
                if xp != 0 {
                    let label = if compat { format!("TDXLU compat :{xp}") } else { format!(":{xp} held by TDXLU?") };
                    lamp(ui, compat, &label);
                }
                ui.separator();
                if ui.selectable_label(self.view == View::Status, "Status").on_hover_text("Live state of every watched project").clicked() {
                    self.view = View::Status;
                }
                let dirty = self.draft.as_ref().is_some_and(Draft::dirty);
                let label = if dirty { "Settings •" } else { "Settings" };
                if ui.selectable_label(self.view == View::Settings, label).on_hover_text("This machine's settings and each project's rules (• = unsaved changes)").clicked() {
                    self.view = View::Settings;
                    if !dirty {
                        let sel = self.draft.as_ref().map(|d| d.sel);
                        let mut d = draft_from_live(&core);
                        if let Some(sel) = sel {
                            d.sel = sel;
                        }
                        self.draft = Some(d);
                    }
                }
                if ui.selectable_label(self.view == View::About, "About").on_hover_text("Version, links and support").clicked() {
                    self.view = View::About;
                }
                if ui.button("Logs").on_hover_text("Open the folder with the daily logs, captures and status.json").clicked() {
                    open(core.ctx.log.dir());
                }
                if ui.button("Quit").on_hover_text("Stop watching. TouchDesigner keeps running.").clicked() {
                    self.quitting = true;
                    std::process::exit(0);
                }
            });
            if let Some(e) = &self.load_error {
                ui.colored_label(Color32::from_rgb(235, 80, 80), format!("Config error, nothing is watched: {e}"));
            }
            if self.view == View::Settings {
                self.save_bar(ui);
            }
        });

        egui::Panel::bottom("log").resizable(true).default_size(140.0).show(root, |ui| {
            egui::ScrollArea::both().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in core.ctx.log.tail(200) {
                    ui.add(egui::Label::new(RichText::new(line).monospace()).extend());
                }
            });
        });

        if self.view == View::About {
            self.about(root);
            return;
        }
        if self.view == View::Settings && self.draft.is_some() {
            self.settings(root);
            return;
        }

        egui::CentralPanel::default().show(root, |ui| {
            let statuses = core.statuses();
            if statuses.is_empty() {
                ui.label("No projects yet. Add one under Settings, or click Watch on a session below.");
            }
            egui::Grid::new("projects").striped(true).num_columns(9).spacing([12.0, 6.0]).show(ui, |ui| {
                for (h, tip) in [
                    ("Project", "Display name; hover a row for the .toe path"),
                    ("Phase", "What TDXCardio is doing for this project; hover a phase for its meaning"),
                    ("PID", "Process ID of the watched TouchDesigner"),
                    ("Companion", "What sends the heartbeats: cardio = the Cardio COMP (full control), tdxlu = TDXLauncherUtility (liveness, ping, save), none = nothing heard yet"),
                    ("FPS", "Frames per second reported in the last heartbeat"),
                    ("Cook ms", "Time the last frame took to cook, in milliseconds"),
                    ("Beat", "Time since the last heartbeat"),
                    ("Restarts", "Restarts since TDXCardio started (and within the current restart window)"),
                    ("", ""),
                ] {
                    let r = ui.label(RichText::new(h).strong());
                    if !tip.is_empty() {
                        r.on_hover_text(tip);
                    }
                }
                ui.end_row();
                for (i, s) in statuses.iter().enumerate() {
                    let name = if s.path.is_empty() { "(no .toe set)" } else { s.name.as_str() };
                    ui.label(name).on_hover_text(&s.path);
                    ui.label(RichText::new(&s.phase).color(phase_color(&s.phase))).on_hover_text(format!("{}\n\n{}", phase_help(&s.phase), s.message));
                    ui.label(s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()));
                    ui.label(match &s.companion_version {
                        Some(v) => format!("{} {v}", s.companion),
                        None => s.companion.clone(),
                    });
                    ui.label(s.fps.map(|f| format!("{f:.1}")).unwrap_or_else(|| "-".into()));
                    ui.label(s.cook_ms.map(|f| format!("{f:.1}")).unwrap_or_else(|| "-".into()));
                    ui.label(s.last_heartbeat_secs.map(|f| format!("{f:.1}s ago")).unwrap_or_else(|| "-".into()));
                    ui.label(format!("{} ({} recent)", s.restarts_total, s.restarts_recent));
                    ui.horizontal(|ui| {
                        let w = core.watchers.lock().unwrap();
                        let Some(w) = w.get(i) else { return };
                        if w.path.is_empty() {
                            return;
                        }
                        let running = !matches!(s.phase.as_str(), "idle" | "stopped" | "gave_up" | "error");
                        if !running && ui.button("Start").clicked() {
                            let _ = w.ctl.send(Ctl::Start);
                        }
                        if running && ui.button("Stop").on_hover_text("Stop watching; TouchDesigner keeps running").clicked() {
                            let _ = w.ctl.send(Ctl::Stop);
                        }
                        if ui.button("Restart").on_hover_text("Graceful quit (if enabled), then relaunch").clicked() {
                            let _ = w.ctl.send(Ctl::Restart);
                        }
                    });
                    ui.end_row();
                }
            });

            let unwatched = core.unwatched();
            if !unwatched.is_empty() {
                ui.separator();
                ui.label(RichText::new("Sessions on the bus, not watched").strong());
                for p in unwatched {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}  pid {}  ({})", p.id, p.pid.map(|x| x.to_string()).unwrap_or("-".into()), p.kind()));
                        if ui.button("Watch").on_hover_text("Add to cardio.json in attach mode and start watching").clicked() {
                            self.watch_session(&p.id);
                        }
                    });
                }
            }
        });
    }
}

impl App {
    fn about(&mut self, root: &mut egui::Ui) {
        let core = self.core.clone();
        let logo = self
            .logo
            .get_or_insert_with(|| {
                let n = 160;
                let img = egui::ColorImage::from_rgba_unmultiplied([n, n], &crate::icon::heart_rgba(n as u32));
                root.ctx().load_texture("tdxcardio-logo", img, egui::TextureOptions::LINEAR)
            })
            .clone();
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new(&logo).fit_to_exact_size(egui::vec2(120.0, 120.0)));
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.add_space(14.0);
                        ui.label(RichText::new("TDXCardio").size(30.0).strong());
                        ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).weak());
                        ui.add_space(6.0);
                        ui.label("A heartbeat watchdog for TouchDesigner projects. It launches or attaches to a show,");
                        ui.label("watches its heartbeat, and restarts it when it crashes, freezes or runs unhealthy.");
                        ui.add_space(4.0);
                        ui.label(RichText::new("Made by Function Store.").weak());
                    });
                });

                section(ui, "Links", "");
                for (title, url, tip) in LINKS {
                    ui.horizontal(|ui| {
                        let b = ui.add_sized([170.0, 26.0], egui::Button::new(RichText::new(title).strong()));
                        if b.on_hover_text(format!("{tip}\n{url}")).clicked() {
                            open_url(url);
                        }
                        ui.add_space(6.0);
                        if ui.link(RichText::new(url).weak()).on_hover_text("Open in your browser").clicked() {
                            open_url(url);
                        }
                    });
                    ui.add_space(2.0);
                }

                section(ui, "For support requests", "Attach the day's log and your settings files when you report a problem.");
                grid(ui, "about_files", |ui| {
                    label(ui, "Settings (this machine)", "cardio.json: ports, email, folders and the projects this machine watches.");
                    ui.horizontal(|ui| {
                        ui.label(norm(core.config_path.clone()));
                        if ui.small_button("Open").on_hover_text("Open cardio.json").clicked() {
                            open(&core.config_path);
                        }
                    });
                    ui.end_row();
                    label(ui, "Logs", "One log file per day, plus captures/ (screenshots) and status.json.");
                    ui.horizontal(|ui| {
                        ui.label(norm(core.ctx.log.dir().to_path_buf()));
                        if ui.small_button("Open").on_hover_text("Open the logs folder").clicked() {
                            open(core.ctx.log.dir());
                        }
                    });
                    ui.end_row();
                    label(ui, "Project rules", "Each project's rules live beside its .toe as <Name>.cardio.json.");
                    ui.label("<Name>.cardio.json beside each .toe");
                    ui.end_row();
                });
            });
        });
    }

    /// Add a bus session to the machine config (attach mode), save, apply.
    fn watch_session(&mut self, id: &str) {
        let core = self.core.clone();
        if self.load_error.is_some() {
            core.ctx.log.line("not saving: cardio.json failed to load; fix it, then Settings > App > Reload from disk");
            return;
        }
        let entry = Entry { path: id.to_string(), mode: "attach".into(), ..Default::default() };
        let mut cfg = (*core.cfg()).clone();
        cfg.projects.push(entry.clone());
        if let Err(e) = config::save(&core.config_path, &cfg) {
            core.ctx.log.line(&format!("could not save config: {e}"));
            return;
        }
        core.apply(cfg);
        if let Some(d) = &mut self.draft {
            let fam = family(&entry.path);
            d.known.retain(|(_, p)| family(p) != fam);
            d.base.projects.push(entry.clone());
            d.edit.projects.push(entry.clone());
            d.load_rules(&entry);
        }
    }

    fn save_bar(&mut self, ui: &mut egui::Ui) {
        let core = self.core.clone();
        let locked = self.load_error.is_some();
        let Some(d) = &mut self.draft else { return };
        ui.horizontal(|ui| {
            let dirty = d.dirty();
            let save = ui.add_enabled(dirty && !locked, egui::Button::new(RichText::new("Save").strong()));
            let save = if locked { save.on_disabled_hover_text("cardio.json failed to load. Fix it, then App > Reload from disk.") } else { save };
            if save.clicked() {
                d.note = Some(match d.save(&core) {
                    Ok(m) => (true, m),
                    Err(e) => (false, format!("Not saved: {e}")),
                });
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                let sel = d.sel;
                *d = draft_from_live(&core);
                d.sel = sel;
            }
            match &d.note {
                Some((true, m)) if !dirty => ui.colored_label(Color32::from_rgb(80, 200, 120), m),
                Some((false, m)) => ui.colored_label(Color32::from_rgb(235, 80, 80), m),
                _ if dirty => ui.label("Unsaved changes. Save applies them right away; a save never restarts TouchDesigner."),
                _ => ui.label("Changes apply when you click Save."),
            };
        });
    }

    fn settings(&mut self, root: &mut egui::Ui) {
        let core = self.core.clone();
        let statuses = core.statuses();
        let mut reloaded = false;
        let Some(d) = &mut self.draft else { return };

        egui::Panel::left("settings_nav").resizable(true).default_size(210.0).show(root, |ui| {
            ui.add_space(4.0);
            if ui.selectable_label(d.sel == Sel::App, RichText::new("App (this machine)").strong()).clicked() {
                d.sel = Sel::App;
            }
            ui.separator();
            d.merge_known(core.known.lock().unwrap().clone());
            ui.label(RichText::new("Projects").weak()).on_hover_text(
                "Every project TDXCardio has seen: in cardio.json, or talking on the bus at some point.\n\
                 Tick a project to watch it on this machine; untick to stop (applies on Save).\n\
                 Rules can be edited either way.",
            );
            if d.edit.projects.is_empty() && d.known.is_empty() {
                ui.label(RichText::new("none yet").weak().italics());
            }
            let rows: Vec<(u64, String, bool)> = d
                .edit
                .projects
                .iter()
                .map(|e| (e.uid, e.path.clone(), true))
                .chain(d.known.iter().map(|(u, p)| (*u, p.clone(), false)))
                .collect();
            for (uid, path, watched) in rows {
                let name = d.rules_edit.get(&uid).map(|r| r.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| stem(&path));
                let entry_changed = d.edit.projects.iter().find(|e| e.uid == uid) != d.base.projects.iter().find(|e| e.uid == uid);
                let changed = d.rules_dirty(uid) || entry_changed;
                let phase = if watched {
                    statuses.iter().find(|s| s.path == path && !path.is_empty()).map(|s| s.phase.clone()).unwrap_or_else(|| "not started".into())
                } else {
                    "not watched".into()
                };
                ui.horizontal(|ui| {
                    let mut on = watched;
                    let tip = if watched {
                        "Watched on this machine. Untick to stop watching (applies on Save; TouchDesigner keeps running)."
                    } else {
                        "Not watched on this machine. Tick to watch it (applies on Save)."
                    };
                    if ui.checkbox(&mut on, "").on_hover_text(tip).changed() {
                        if on { d.watch(uid) } else { d.unwatch(uid) }
                    }
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    if watched {
                        ui.painter().circle_filled(rect.center(), 4.0, phase_color(&phase));
                    } else {
                        ui.painter().circle_stroke(rect.center(), 3.5, egui::Stroke::new(1.0, Color32::GRAY));
                    }
                    let text = if changed { format!("{name} •") } else { name };
                    let text = if watched { RichText::new(text) } else { RichText::new(text).weak() };
                    let tip = format!("{path}\n{phase}: {}{}", phase_help(&phase), if changed { "\n• unsaved changes" } else { "" });
                    if ui.selectable_label(d.sel == Sel::Project(uid), text).on_hover_text(tip).clicked() {
                        d.sel = Sel::Project(uid);
                    }
                });
            }
            ui.add_space(6.0);
            if ui.button("+ Add project…").on_hover_text("Pick a .toe file to watch on this machine").clicked() {
                if let Some(path) = pick_toe() {
                    let e = Entry { path, ..Default::default() };
                    d.edit.projects.push(e.clone());
                    d.load_rules(&e);
                    d.sel = Sel::Project(e.uid);
                }
            }
        });

        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match d.sel {
                Sel::App => {
                    if app_page(ui, d, &core) {
                        match draft_from_disk(&core) {
                            Ok(mut nd) => {
                                nd.note = Some((true, "Loaded cardio.json from disk; Save applies it.".into()));
                                nd.sel = Sel::App;
                                *d = nd;
                                reloaded = true;
                            }
                            Err(e) => d.note = Some((false, e)),
                        }
                    }
                }
                Sel::Project(uid) => project_page(ui, d, uid, &core),
            });
        });
        if reloaded {
            self.load_error = None;
        }
    }
}

fn stem(path: &str) -> String {
    if path.is_empty() {
        return "(no .toe set)".into();
    }
    std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.into())
}

/// The big "Watch on this machine" switch at the top of a project page.
/// Returns true when it was flipped.
fn watch_toggle(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let text = RichText::new("Watch on this machine").strong().size(15.0);
    ui.checkbox(on, text)
        .on_hover_text(
            "On: this machine watches the project (it is listed in cardio.json).\n\
             Off: not watched here; its rules can still be edited.\n\
             Applies on Save. TouchDesigner is never started or stopped by this switch alone.",
        )
        .changed()
}

/// A setting's name with a small "?" marker; hovering either shows the help.
fn label(ui: &mut egui::Ui, text: &str, help: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.label(text).on_hover_text(help);
        help_mark(ui).on_hover_text(help);
    });
}

/// A small circled "?" that says "hover me for help".
fn help_mark(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(15.0, 15.0), egui::Sense::hover());
    let c = if resp.hovered() { ui.visuals().strong_text_color() } else { Color32::from_gray(130) };
    ui.painter().circle_stroke(rect.center(), 6.5, egui::Stroke::new(1.0, c));
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "?", egui::FontId::proportional(10.5), c);
    resp
}

fn phase_help(phase: &str) -> &'static str {
    match phase {
        "idle" => "not started yet",
        "not started" => "watched here, starts on Save or with Start",
        "not watched" => "this machine does not watch it",
        "waiting" => "attach mode: waiting for this project's TouchDesigner to say hello",
        "launching" | "grace" => "TouchDesigner is starting; heartbeats are not required yet",
        "watching" => "heartbeats arriving, all good",
        "held" => "TD asked for a pause (Hold) during a long load; stall detection is off until it ends",
        "unhealthy" => "fps or cook time outside the health limits",
        "restarting" => "collecting evidence, stopping and relaunching TouchDesigner",
        "gave_up" => "too many restarts in the window; not relaunching until you press Start",
        "stopped" => "watching stopped by hand; TouchDesigner left running",
        "error" => "cannot watch; see the message",
        _ => "",
    }
}

fn section(ui: &mut egui::Ui, title: &str, sub: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).heading());
    if !sub.is_empty() {
        ui.label(RichText::new(sub).weak());
    }
    ui.add_space(4.0);
}

fn grid(ui: &mut egui::Ui, id: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).min_col_width(170.0).show(ui, body);
}

fn secs(ui: &mut egui::Ui, v: &mut u64, max: u64) {
    ui.add(egui::DragValue::new(v).range(0..=max).suffix(" s"));
}

fn path_edit(ui: &mut egui::Ui, v: &mut String, hint: &str, pick: fn() -> Option<String>) -> egui::Response {
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(v).hint_text(hint).desired_width(420.0)).on_hover_text(v.as_str());
        if ui.button("Browse…").clicked() {
            if let Some(p) = pick() {
                *v = p;
            }
        }
        r
    })
    .inner
}

fn combo(ui: &mut egui::Ui, id: &str, v: &mut String, options: &[(&str, &str)]) {
    let current = options.iter().find(|(k, _)| *k == v.as_str()).map(|(_, l)| *l).unwrap_or(v.as_str()).to_string();
    egui::ComboBox::from_id_salt(id).selected_text(current).show_ui(ui, |ui| {
        for (k, l) in options {
            ui.selectable_value(v, k.to_string(), *l);
        }
    });
}

fn pick_folder() -> Option<String> {
    rfd::FileDialog::new().pick_folder().map(norm)
}

/// The machine page. Returns true when "Reload from disk" was clicked.
fn app_page(ui: &mut egui::Ui, d: &mut Draft, core: &Core) -> bool {
    let mut reload = false;
    let c = &mut d.edit;
    ui.label(RichText::new(format!("Machine settings, saved to {}", core.config_path.display().to_string().replace('\\', "/"))).weak());
    ui.horizontal(|ui| {
        if ui.button("Open cardio.json").clicked() {
            open(&core.config_path);
        }
        if ui.button("Reload from disk").on_hover_text("Re-read cardio.json after editing it by hand").clicked() {
            reload = true;
        }
    });

    section(ui, "TouchDesigner", "");
    grid(ui, "app_td", |ui| {
        label(ui, "TD executable", "TouchDesigner used when a project names none on this machine. Empty = the newest install found.");
        path_edit(ui, &mut c.td_executable, "newest install", pick_exe);
        ui.end_row();
    });

    section(ui, "Ports", "Port changes apply after restarting TDXCardio.");
    grid(ui, "app_ports", |ui| {
        label(ui, "Cardio port", "Port the Cardio COMP connects to; must match the COMP's Port parameter. Default 11998 (11997 is TDXLU's control server).");
        ui.add(egui::DragValue::new(&mut c.cardio_port).range(1024..=65535));
        ui.end_row();
        label(ui, "TDXLU compat port", "Accepts TDXLauncherUtility heartbeats. Bound only while TDX Launcher Ultra is not running. 0 disables.");
        ui.add(egui::DragValue::new(&mut c.compat_port).range(0..=65535));
        ui.end_row();
    });

    section(ui, "Files", "Folder changes apply after restarting TDXCardio.");
    grid(ui, "app_files", |ui| {
        label(ui, "Data folder", "Holds logs/, captures/ and status.json. Empty = beside cardio.json.");
        path_edit(ui, &mut c.data_dir, "beside cardio.json", pick_folder);
        ui.end_row();
        label(ui, "Status file", "Snapshot of every watched project, rewritten every second for other tools. Empty = <data folder>/status.json; \"off\" disables it.");
        ui.add(egui::TextEdit::singleline(&mut c.status_file).hint_text("<data folder>/status.json").desired_width(380.0));
        ui.end_row();
    });

    section(ui, "Behaviour", "");
    grid(ui, "app_behaviour", |ui| {
        label(ui, "Start hidden", "Start in the tray without showing this window.");
        ui.checkbox(&mut c.start_hidden, "");
        ui.end_row();
        label(ui, "Reboot after give-ups", "Reboot the machine after this many give-ups (crash loops). 0 = never.");
        ui.add(egui::DragValue::new(&mut c.reboot_after_gave_up).range(0..=20));
        ui.end_row();
    });

    section(ui, "Email alerts", "Which events send mail is set per project (Evidence).");
    let m = &mut c.email;
    grid(ui, "app_email", |ui| {
        label(ui, "Enabled", "Send alert emails at all.");
        ui.checkbox(&mut m.enabled, "");
        ui.end_row();
        label(ui, "SMTP host", "For example smtp.gmail.com.");
        ui.text_edit_singleline(&mut m.smtp_host);
        ui.end_row();
        label(ui, "Port / security", "587 with STARTTLS, 465 with TLS, 25 unencrypted.");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut m.smtp_port).range(1..=65535));
            combo(ui, "email_sec", &mut m.security, &[("starttls", "STARTTLS"), ("tls", "TLS"), ("none", "None")]);
        });
        ui.end_row();
        label(ui, "Username", "SMTP login. Empty = no authentication.");
        ui.text_edit_singleline(&mut m.username);
        ui.end_row();
        label(ui, "Password variable", "Name of an environment variable holding the password. Preferred: keeps the password out of cardio.json.");
        ui.text_edit_singleline(&mut m.password_env);
        ui.end_row();
        label(ui, "Password", "Used only when the variable above is empty. Stored as plain text in cardio.json.");
        ui.add(egui::TextEdit::singleline(&mut m.password).password(true));
        ui.end_row();
        label(ui, "From", "Sender address. Empty = the username.");
        ui.text_edit_singleline(&mut m.from);
        ui.end_row();
        label(ui, "To", "Recipients, comma separated.");
        let mut to = m.to.join(", ");
        if ui.text_edit_singleline(&mut to).changed() {
            m.to = to.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        ui.end_row();
        label(ui, "Attach evidence", "Attach the day's log and the screenshot.");
        ui.checkbox(&mut m.attach, "");
        ui.end_row();
    });
    if ui.button("Send test email").on_hover_text("Uses the fields above, saved or not. The result appears in the log.").clicked() {
        let mut m = c.email.clone();
        m.enabled = true;
        core.ctx.log.line(&format!("sending a test email to {}", m.to.join(", ")));
        notify::send(&m, "[TDXCardio] test email".into(), "TDXCardio can send alerts from this machine.".into(), Vec::new(), core.ctx.log.clone());
    }
    reload
}

fn project_page(ui: &mut egui::Ui, d: &mut Draft, uid: u64, core: &Core) {
    let Some(i) = d.edit.projects.iter().position(|e| e.uid == uid) else {
        let Some(path) = d.known.iter().find(|(u, _)| *u == uid).map(|(_, p)| p.clone()) else { return };
        let mut on = false;
        let (mut watch, mut forget) = (false, false);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            watch = watch_toggle(ui, &mut on);
            forget = ui.button("Forget").on_hover_text("Remove it from the Projects list. Its rules file beside the .toe is kept, and it comes back if it talks on the bus again.").clicked();
        });
        section(ui, "This machine", "Not watched here. Tick Watch on this machine to add it to cardio.json; its rules can be edited either way.");
        ui.label(&path).on_hover_text("The project's .toe file");
        if watch {
            d.watch(uid);
            return;
        }
        if forget {
            core.forget(&path);
            d.forget(uid);
            return;
        }
        rules_section(ui, d, uid, &path);
        return;
    };

    let mut path_focused = false;
    let mut remove = false;
    let mut on = true;
    ui.add_space(6.0);
    if watch_toggle(ui, &mut on) {
        remove = true;
    }
    section(ui, "This machine", "Saved to cardio.json: how this machine watches the project.");
    {
        let e = &mut d.edit.projects[i];
        grid(ui, "proj_machine", |ui| {
            label(ui, ".toe file", "The project to watch. Increment-and-Save copies (Show.12.toe) count as the same project.");
            path_focused = path_edit(ui, &mut e.path, "C:/Shows/Main.toe", pick_toe).has_focus();
            ui.end_row();
            label(ui, "Start with TDXCardio", "Begin watching as soon as TDXCardio starts. Off = watched here, but you press Start on the Status page yourself.");
            ui.checkbox(&mut e.enabled, "");
            ui.end_row();
            label(ui, "Mode", "Launch: TDXCardio starts TD, or adopts a running session of this project. Attach: never starts TD itself; relaunches only after it dies or stalls.");
            combo(ui, "proj_mode", &mut e.mode, &[("launch", "Launch"), ("attach", "Attach")]);
            ui.end_row();
            label(ui, "TD executable", "TouchDesigner for this project on this machine. Empty = the App setting.");
            path_edit(ui, &mut e.td_executable, "App default", pick_exe);
            ui.end_row();
        });
    }
    if remove {
        d.unwatch(uid);
        return;
    }
    let path = d.edit.projects[i].path.clone();
    if !path_focused && d.rules_for.get(&uid) != Some(&path) {
        let e = d.edit.projects[i].clone();
        d.load_rules(&e);
    }
    rules_section(ui, d, uid, &path);
}

/// The project's rules (its sidecar), editable whether or not it is watched.
fn rules_section(ui: &mut egui::Ui, d: &mut Draft, uid: u64, path: &str) {
    let path = path.to_string();
    let sidecar = if path.is_empty() { PathBuf::from("<Name>.cardio.json") } else { config::sidecar_path(&path) };
    let file = sidecar.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    section(ui, "Project rules", "");
    ui.label(RichText::new(format!("Saved in {file} beside the .toe, so they travel with the project (git, other machines).")).weak())
        .on_hover_text(norm(sidecar.clone()));
    if let Some(err) = d.rules_err.get(&uid) {
        ui.colored_label(Color32::from_rgb(235, 80, 80), format!("Cannot read the rules file, so the project is not watched: {err}"));
        let mut again = false;
        ui.horizontal(|ui| {
            if ui.button("Open rules file").clicked() {
                open(&config::sidecar_path(&path));
            }
            again = ui.button("Read it again").clicked();
        });
        if again {
            d.load_rules_at(uid, &path);
        }
        return;
    }
    let Some(r) = d.rules_edit.get_mut(&uid) else { return };
    let args = d.args_text.entry(uid).or_default();

    grid(ui, "rules_general", |ui| {
        label(ui, "Display name", "Name in this window, logs and emails. Empty = the file name.");
        ui.add(egui::TextEdit::singleline(&mut r.name).hint_text(stem(&path)));
        ui.end_row();
        label(ui, "TD arguments", "Extra command-line arguments for TouchDesigner, one per line.");
        if ui.add(egui::TextEdit::multiline(args).desired_rows(2).desired_width(380.0)).changed() {
            r.args = if args.is_empty() { Vec::new() } else { args.split('\n').map(String::from).collect() };
        }
        ui.end_row();
    });

    section(ui, "Heartbeat and restarts", "");
    grid(ui, "rules_timing", |ui| {
        label(ui, "Launch grace", "Time after launch before heartbeats are required (project load).");
        secs(ui, &mut r.launch_grace_secs, 3600);
        ui.end_row();
        label(ui, "Heartbeat timeout", "No heartbeat for this long (and no Hold from TD) counts as a stall: screenshot, then kill and relaunch.");
        secs(ui, &mut r.timeout_secs, 3600);
        ui.end_row();
        label(ui, "Relaunch delay", "Wait before relaunching. Doubles for each restart inside the window, capped at 60 s.");
        secs(ui, &mut r.relaunch_delay_secs, 600);
        ui.end_row();
        label(ui, "Give up after", "This many restarts inside the window counts as a crash loop: stop relaunching and alert.");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut r.max_restarts).range(1..=100).suffix(" restarts"));
            ui.label("within");
            secs(ui, &mut r.restart_window_secs, 86400);
        });
        ui.end_row();
        label(ui, "Relaunch after clean quit", "Relaunch even when TD quit on purpose (File > Quit). Off = a clean quit stops watching.");
        ui.checkbox(&mut r.relaunch_on_clean_exit, "");
        ui.end_row();
    });

    section(ui, "Graceful restart", "For health and manual restarts while TD can still answer (needs the Cardio COMP). Stalls always kill.");
    grid(ui, "rules_graceful", |ui| {
        label(ui, "Ask TD to quit first", "Send a quit command and wait before killing the process.");
        ui.checkbox(&mut r.graceful.enabled, "");
        ui.end_row();
        let on = r.graceful.enabled;
        label(ui, "Save before quitting", "TD saves the project before the graceful quit.");
        ui.add_enabled(on, egui::Checkbox::without_text(&mut r.graceful.save));
        ui.end_row();
        label(ui, "Quit timeout", "How long to wait for TD to exit before killing it.");
        ui.add_enabled_ui(on, |ui| secs(ui, &mut r.graceful.timeout_secs, 600));
        ui.end_row();
    });

    section(ui, "Health", "Acts on slow running, not only on crashes and freezes.");
    grid(ui, "rules_health", |ui| {
        label(ui, "Check health", "Watch the fps and frame cook time reported in heartbeats.");
        ui.checkbox(&mut r.health.enabled, "");
        ui.end_row();
        let on = r.health.enabled;
        label(ui, "Minimum fps", "Below this counts as unhealthy. 0 ignores fps.");
        ui.add_enabled(on, egui::DragValue::new(&mut r.health.min_fps).range(0.0..=240.0).speed(0.5));
        ui.end_row();
        label(ui, "Maximum cook time", "Frame cook time above this counts as unhealthy. 0 ignores it.");
        ui.add_enabled(on, egui::DragValue::new(&mut r.health.max_cook_ms).range(0.0..=1000.0).speed(0.5).suffix(" ms"));
        ui.end_row();
        label(ui, "For at least", "How long it must stay unhealthy before acting.");
        ui.add_enabled_ui(on, |ui| secs(ui, &mut r.health.bad_secs, 3600));
        ui.end_row();
        label(ui, "Then", "Log only, or restart TD (gracefully first).");
        ui.add_enabled_ui(on, |ui| combo(ui, "health_action", &mut r.health.action, &[("log", "Log it"), ("restart", "Restart TD")]));
        ui.end_row();
    });

    section(ui, "Window", "All optional and off by default.");
    grid(ui, "rules_window", |ui| {
        label(ui, "Keep focus", "Bring TD's main window to the front at an interval (OS level).");
        ui.horizontal(|ui| {
            ui.checkbox(&mut r.window.keep_focus, "");
            let on = r.window.keep_focus;
            ui.add_enabled_ui(on, |ui| {
                ui.label("every");
                secs(ui, &mut r.window.focus_interval_secs, 600);
            });
        });
        ui.end_row();
        label(ui, "Perform mode", "Ask TD to open its perform window (fullscreen per the project's Window COMP). Needs the Cardio COMP.");
        ui.checkbox(&mut r.window.perform_mode, "");
        ui.end_row();
        let on = r.window.perform_mode;
        label(ui, "Keep perform mode", "With perform mode: open the perform window again within 5 s when someone leaves it.");
        ui.add_enabled(on, egui::Checkbox::without_text(&mut r.window.keep_perform));
        ui.end_row();
        label(ui, "Always on top", "With perform mode: keep the perform window above other windows.");
        ui.add_enabled(on, egui::Checkbox::without_text(&mut r.window.topmost));
        ui.end_row();
    });

    section(ui, "Evidence", "Collected on every stall or health restart. Email setup is on the App page.");
    grid(ui, "rules_evidence", |ui| {
        label(ui, "Screenshot", "Capture the desktop before killing a stalled TD (saved in captures/).");
        ui.checkbox(&mut r.evidence.screenshot, "");
        ui.end_row();
        label(ui, "Email on restart", "Send an alert for every restart.");
        ui.checkbox(&mut r.evidence.email_on_restart, "");
        ui.end_row();
        label(ui, "Email on give-up", "Send an alert when a crash loop stops relaunching.");
        ui.checkbox(&mut r.evidence.email_on_gave_up, "");
        ui.end_row();
    });
}

fn lamp(ui: &mut egui::Ui, on: bool, label: &str) {
    let tip = if label.starts_with("cardio") {
        "Port the Cardio COMP connects to. Green = listening."
    } else {
        "TDXLU-compatible port for TDXLauncherUtility heartbeats. Grey = another app (usually TDX Launcher Ultra) holds it; retried every 10 s."
    };
    let c = if on { Color32::from_rgb(80, 200, 120) } else { Color32::from_rgb(150, 150, 150) };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, c);
    ui.label(RichText::new(label).color(c)).on_hover_text(tip);
}
