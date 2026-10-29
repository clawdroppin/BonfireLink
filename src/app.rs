//! The window: Home, one tab per game (Overview · Configure · Play together · Saves · Versions ·
//! Troubleshoot), Settings and Help. Work runs on one background thread at a time (`RunningJob`).

use crate::config::{self, Settings, State};
use crate::games::{GAMES, GameDef, GameId};
use crate::ini::{Ini, Kind};
use crate::ops::{self, Job, LaunchOptionOutcome, Status};
use crate::remote::{self, Feed, Release};
use crate::share::{self, Session, Severity};
use crate::{nxm, presets, selfupdate, steam, util};
use egui::{Color32, RichText, Ui, vec2};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, SystemTime};

// ---------- palette ----------

const BG: Color32 = Color32::from_rgb(17, 15, 13);
const PANEL: Color32 = Color32::from_rgb(27, 24, 21);
const CARD: Color32 = Color32::from_rgb(36, 32, 28);
const CARD_HI: Color32 = Color32::from_rgb(46, 41, 35);
const TEXT: Color32 = Color32::from_rgb(226, 216, 196);
const DIM: Color32 = Color32::from_rgb(150, 140, 124);
const EMBER: Color32 = Color32::from_rgb(232, 140, 48);
const GOOD: Color32 = Color32::from_rgb(120, 190, 110);
const WARN: Color32 = Color32::from_rgb(230, 180, 70);
const BAD: Color32 = Color32::from_rgb(220, 90, 80);

fn accent(def: &GameDef) -> Color32 {
    Color32::from_rgb(def.accent[0], def.accent[1], def.accent[2])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Home,
    Game(GameId),
    Settings,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Sub {
    #[default]
    Overview,
    Configure,
    Together,
    Saves,
    Versions,
    Troubleshoot,
}

impl Sub {
    const ALL: [(Sub, &'static str); 6] = [
        (Sub::Overview, "Overview"),
        (Sub::Configure, "Configure"),
        (Sub::Together, "Play together"),
        (Sub::Saves, "Saves"),
        (Sub::Versions, "Versions"),
        (Sub::Troubleshoot, "Troubleshoot"),
    ];
}

/// What the UI should do once a job finishes.
enum Follow {
    None,
    OpenPath(PathBuf),
    AskSteamRestart(GameId),
    RestartApp,
}

struct Done {
    message: String,
    follow: Follow,
}

impl Done {
    fn msg(m: impl Into<String>) -> Self {
        Done { message: m.into(), follow: Follow::None }
    }
}

struct RunningJob {
    title: String,
    job: Job,
    rx: Receiver<anyhow::Result<Done>>,
    started: Instant,
}

enum Confirm {
    Uninstall(GameId),
    RestoreBackup(GameId, PathBuf),
    DeleteBackup(PathBuf),
    TransferSave(GameId, usize, bool),
    SteamRestart(GameId),
    SteamRestartAll,
    AddToSteam,
    InstallRelease(GameId, Release),
    Rollback(GameId, PathBuf, String),
    ApplySession(GameId, Session, bool),
    ResetDefaults(GameId),
}

#[derive(Default)]
struct IniEditor {
    path: PathBuf,
    saved_text: String,
    ini: Ini,
    raw: String,
    raw_mode: bool,
    error: String,
    preset_name: String,
    show_password: bool,
    loaded_for: Option<PathBuf>,
}

impl IniEditor {
    fn dirty(&self) -> bool {
        if self.raw_mode { self.raw != self.saved_text } else { self.ini.to_text() != self.saved_text }
    }
}

#[derive(Default)]
struct ShareUi {
    pasted: String,
    decoded: Option<Result<Session, String>>,
    last_parsed: String,
}

#[derive(Clone, Copy, PartialEq)]
enum MsgKind {
    Info,
    Ok,
    Err,
}

pub struct App {
    settings: Settings,
    tab: Tab,
    subs: BTreeMap<GameId, Sub>,
    status: BTreeMap<GameId, Status>,
    last_scan: Instant,
    feeds: BTreeMap<GameId, Feed>,
    feed_rx: Option<Receiver<(GameId, Feed)>>,
    feeds_pending: usize,
    job: Option<RunningJob>,
    messages: Vec<(MsgKind, String, Instant)>,
    confirm: Option<Confirm>,
    helper_update: Option<selfupdate::Available>,
    helper_rx: Option<Receiver<Option<selfupdate::Available>>>,
    nexus_watch: Option<(GameId, SystemTime, Instant)>,
    editors: BTreeMap<GameId, IniEditor>,
    shares: BTreeMap<GameId, ShareUi>,
    nexus_user: Option<Result<remote::NexusUser, String>>,
    nexus_rx: Option<Receiver<Result<remote::NexusUser, String>>>,
    applied_scale: f32,
    log_open: bool,
    diag_cache: BTreeMap<GameId, String>,
    /// Disk-backed data cached between rescans so frames never touch the disk.
    state: State,
    backups: BTreeMap<GameId, Vec<ops::BackupEntry>>,
    rollbacks: BTreeMap<GameId, Vec<ops::RollbackEntry>>,
    languages: BTreeMap<GameId, Vec<String>>,
    storage: Option<(u64, u64)>,
    last_watch_poll: Instant,
    /// Set by background threads (version identification) when the status should be re-read.
    needs_rescan: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Nexus "Mod Manager Download" links waiting for the job slot.
    pending_nxm: std::collections::VecDeque<String>,
    last_inbox_poll: Instant,
    nxm_handler: nxm::Handler,
    nexus_key_draft: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        style(&cc.egui_ctx, settings.dark_theme);
        let tab = match settings.last_tab.as_str() {
            "settings" => Tab::Settings,
            "help" => Tab::Help,
            k => crate::games::by_key(k).map(|g| Tab::Game(g.id)).unwrap_or(Tab::Home),
        };
        let mut app = App {
            settings,
            tab,
            subs: BTreeMap::new(),
            status: BTreeMap::new(),
            last_scan: Instant::now(),
            feeds: BTreeMap::new(),
            feed_rx: None,
            feeds_pending: 0,
            job: None,
            messages: Vec::new(),
            confirm: None,
            helper_update: None,
            helper_rx: None,
            nexus_watch: None,
            editors: BTreeMap::new(),
            shares: BTreeMap::new(),
            nexus_user: None,
            nexus_rx: None,
            applied_scale: 0.0,
            log_open: false,
            diag_cache: BTreeMap::new(),
            state: State::default(),
            backups: BTreeMap::new(),
            rollbacks: BTreeMap::new(),
            languages: BTreeMap::new(),
            storage: None,
            last_watch_poll: Instant::now(),
            needs_rescan: Default::default(),
            pending_nxm: Default::default(),
            last_inbox_poll: Instant::now() - Duration::from_secs(10),
            nxm_handler: nxm::current_handler(),
            nexus_key_draft: String::new(),
        };
        nxm::mark_gui_running();
        app.nexus_key_draft = app.settings.nexus_api_key.clone();
        app.rescan(true);
        if app.settings.check_updates_on_start {
            app.check_feeds();
        }
        if app.settings.check_helper_updates {
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(selfupdate::check().ok().flatten());
            });
            app.helper_rx = Some(rx);
        }
        app
    }

    fn def(id: GameId) -> &'static GameDef {
        crate::games::get(id)
    }

    fn visible_games(&self) -> Vec<&'static GameDef> {
        GAMES.iter().filter(|g| !self.settings.hidden_games.contains(&g.id)).collect()
    }

    fn rescan(&mut self, first: bool) {
        for def in GAMES {
            let st = ops::scan(def, &self.settings);
            if first && st.installed() && !st.managed {
                // Mods installed by hand become manageable (updates keep their settings).
                let _ = ops::adopt_existing(def, &self.settings);
            }
            if first
                && steam::is_linux()
                && let Some(loc) = &st.loc
                && st.wrapper_present
            {
                let _ = ops::refresh_control_files(def, loc, None);
            }
            let st = if first && st.installed() && !st.managed { ops::scan(def, &self.settings) } else { st };
            self.languages.insert(def.id, ops::languages(&st));
            self.status.insert(def.id, st);
            self.backups.insert(def.id, ops::list_backups(def));
            self.rollbacks.insert(def.id, ops::rollbacks(def));
        }
        self.state = State::load();
        self.storage = None;
        self.diag_cache.clear();
        self.last_scan = Instant::now();
    }

    fn check_feeds(&mut self) {
        if self.feeds_pending > 0 {
            return;
        }
        let (tx, rx) = channel();
        let key = self.settings.nexus_api_key.clone();
        let ids: Vec<GameId> = self.visible_games().iter().map(|g| g.id).collect();
        self.feeds_pending = ids.len();
        for id in ids {
            let tx = tx.clone();
            let key = key.clone();
            std::thread::spawn(move || {
                let _ = tx.send((id, remote::fetch_feed(App::def(id), &key)));
            });
        }
        self.feed_rx = Some(rx);
        let mut state = State::load();
        state.last_update_check = config::now_stamp();
        let _ = state.save();
        self.state.last_update_check = state.last_update_check;
    }

    fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            self.msg(MsgKind::Err, format!("Settings not saved: {e:#}"));
        }
    }

    fn msg(&mut self, kind: MsgKind, text: impl Into<String>) {
        let text = text.into();
        ops::log_line(&text);
        self.messages.push((kind, text, Instant::now()));
        if self.messages.len() > 4 {
            self.messages.remove(0);
        }
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn run(&mut self, title: &str, work: impl FnOnce(&Job) -> anyhow::Result<Done> + Send + 'static) {
        if self.busy() {
            self.msg(MsgKind::Info, "Wait for the current task to finish.");
            return;
        }
        let job = Job::default();
        job.status(title);
        let (tx, rx) = channel();
        let j = job.clone();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&j)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("internal error (the task crashed); see the log")));
            let _ = tx.send(r);
        });
        self.job = Some(RunningJob { title: title.to_string(), job, rx, started: Instant::now() });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.feed_rx {
            while let Ok((id, feed)) = rx.try_recv() {
                let unknown = self.status.get(&id).is_some_and(|s| s.installed() && s.mod_version.is_empty());
                if unknown && !feed.releases.is_empty() {
                    let (settings, f, ctx2, flag) = (self.settings.clone(), feed.clone(), ctx.clone(), self.needs_rescan.clone());
                    std::thread::spawn(move || {
                        if let Ok(Some(v)) = ops::identify_version(App::def(id), &settings, &f) {
                            ops::log_line(&format!("{} identified as Seamless Co-op {v}", App::def(id).name));
                            flag.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        ctx2.request_repaint();
                    });
                }
                self.feeds.insert(id, feed);
                self.feeds_pending = self.feeds_pending.saturating_sub(1);
            }
            if self.feeds_pending == 0 {
                self.feed_rx = None;
            }
        }
        if let Some(rx) = &self.helper_rx
            && let Ok(a) = rx.try_recv()
        {
            self.helper_update = a;
            self.helper_rx = None;
        }
        if let Some(rx) = &self.nexus_rx
            && let Ok(r) = rx.try_recv()
        {
            self.nexus_rx = None;
            match &r {
                Ok(u) => {
                    self.settings.nexus_api_key = self.nexus_key_draft.trim().to_string();
                    self.settings.nexus_user = u.name.clone();
                    self.settings.nexus_premium = u.premium;
                    self.save_settings();
                    self.msg(
                        MsgKind::Ok,
                        if u.premium {
                            format!("Connected to Nexus as {} (Premium): installs and updates are fully automatic.", u.name)
                        } else {
                            format!("Connected to Nexus as {}. Free accounts download with one click on Nexus's \"Mod Manager Download\" button.", u.name)
                        },
                    );
                    self.feeds.clear();
                    self.check_feeds();
                }
                Err(e) => self.msg(MsgKind::Err, format!("Nexus key not saved: {e}")),
            }
            self.nexus_user = Some(r);
        }
        if self.last_inbox_poll.elapsed() >= Duration::from_secs(2) {
            self.last_inbox_poll = Instant::now();
            self.pending_nxm.extend(nxm::take_queued());
        }
        if !self.busy()
            && let Some(url) = self.pending_nxm.pop_front()
        {
            let settings = self.settings.clone();
            self.nexus_watch = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.run("Installing from Nexus…", move |job| {
                let msg = nxm::install(&url, &settings, job)?;
                let def = nxm::parse(&url).ok().and_then(|l| nxm::game_for(&l));
                match def {
                    Some(def) => finish_linux_setup(def, &settings, job)
                        .map(|d| Done { message: if d.message.is_empty() { msg } else { d.message }, ..d }),
                    None => Ok(Done::msg(msg)),
                }
            });
        }
        ctx.request_repaint_after(Duration::from_secs(2));
        let finished = self.job.as_ref().and_then(|j| j.rx.try_recv().ok());
        if let Some(result) = finished {
            let job = self.job.take().unwrap();
            match result {
                Ok(done) => {
                    if !done.message.is_empty() {
                        self.msg(MsgKind::Ok, done.message);
                    }
                    match done.follow {
                        Follow::None => {}
                        Follow::OpenPath(p) => util::open_path(&p),
                        Follow::AskSteamRestart(id) => self.confirm = Some(Confirm::SteamRestart(id)),
                        Follow::RestartApp => selfupdate::restart(),
                    }
                }
                Err(e) => self.msg(MsgKind::Err, format!("{}: {e:#}", job.title.trim_end_matches('…'))),
            }
            self.rescan(false);
            self.editors.retain(|_, e| !e.dirty());
        }
        if let Some((id, since, started)) = self.nexus_watch {
            if started.elapsed() > Duration::from_secs(30 * 60) {
                self.nexus_watch = None;
                self.msg(MsgKind::Info, "Stopped watching Downloads (30 minutes passed).");
            } else if !self.busy() && self.last_watch_poll.elapsed() >= Duration::from_secs(2) {
                self.last_watch_poll = Instant::now();
                if let Some(file) = ops::find_new_download(Self::def(id), &self.settings, since) {
                    self.nexus_watch = None;
                    self.install_file(id, file);
                }
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        let flagged = self.needs_rescan.swap(false, std::sync::atomic::Ordering::Relaxed);
        if (flagged || self.last_scan.elapsed() > Duration::from_secs(20)) && !self.busy() {
            self.rescan(false);
        }
        self.messages.retain(|(k, _, t)| t.elapsed() < Duration::from_secs(if *k == MsgKind::Err { 25 } else { 10 }));
        if self.job.is_some() || self.feed_rx.is_some() || !self.messages.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(150));
        }
    }

    // ---------- actions ----------

    fn install_file(&mut self, id: GameId, file: PathBuf) {
        let def = Self::def(id);
        let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let version = {
            let v = ops::version_from_filename(&name, def);
            if v.is_empty() { "unknown (from file)".to_string() } else { v }
        };
        let settings = self.settings.clone();
        self.run(&format!("Installing {name}…"), move |job| {
            ops::install_from(def, &settings, &file, &version, &format!("file {name}"), job)?;
            finish_linux_setup(def, &settings, job)
        });
    }

    fn update_game(&mut self, id: GameId) {
        let def = Self::def(id);
        let settings = self.settings.clone();
        let feed = self.feeds.get(&id).cloned();
        let installed = self.installed_version(id);
        self.run(&format!("Updating {}…", def.name), move |job| {
            let feed = feed.filter(|f| f.latest_version().is_some()).unwrap_or_else(|| remote::fetch_feed(def, &settings.nexus_api_key));
            let (archive, version, origin) = ops::fetch_latest(def, &settings, &feed, &installed, job)?;
            ops::install_from(def, &settings, &archive, &version, &origin, job)?;
            finish_linux_setup(def, &settings, job)
        });
    }

    fn install_release(&mut self, id: GameId, r: Release) {
        let def = Self::def(id);
        let settings = self.settings.clone();
        self.run(&format!("Installing Seamless Co-op {}…", r.version), move |job| {
            let feed = Feed { releases: vec![r.clone()], ..Default::default() };
            let (archive, version, origin) = ops::fetch_latest(def, &settings, &feed, "", job)?;
            ops::install_from(def, &settings, &archive, &version, &origin, job)?;
            finish_linux_setup(def, &settings, job)
        });
    }

    fn play(&mut self, id: GameId) {
        let def = Self::def(id);
        let settings = self.settings.clone();
        let feed = self.feeds.get(&id).cloned();
        let installed = self.installed_version(id);
        self.run(&format!("Starting {}…", def.name), move |job| {
            // Windows: update first if a one-click update exists (the Deck wrapper does this itself).
            if !steam::is_linux()
                && settings.auto_update_on_play
                && let Some(f) = feed.as_ref()
                && let Some(r) = f.one_click(&installed)
                && !installed.is_empty()
            {
                job.status(format!("Updating to {} before playing…", r.version));
                let (a, v, o) = ops::fetch_latest(def, &settings, f, &installed, job)?;
                ops::install_from(def, &settings, &a, &v, &o, job)?;
            }
            ops::launch(def, &settings, job)?;
            Ok(Done::msg(format!("{} is starting. Have fun, and don't go hollow!", def.name)))
        });
    }

    fn do_confirm(&mut self, c: Confirm) {
        let settings = self.settings.clone();
        match c {
            Confirm::Uninstall(id) => {
                let def = Self::def(id);
                self.run(&format!("Removing Seamless Co-op from {}…", def.name), move |job| {
                    ops::uninstall(def, &settings, job)?;
                    Ok(Done::msg(""))
                });
            }
            Confirm::RestoreBackup(id, path) => {
                let def = Self::def(id);
                self.run("Restoring saves…", move |job| {
                    ops::restore_saves(def, &settings, &path, job)?;
                    Ok(Done::msg(""))
                });
            }
            Confirm::DeleteBackup(path) => {
                match std::fs::remove_file(&path) {
                    Ok(()) => self.msg(MsgKind::Ok, "Backup deleted."),
                    Err(e) => self.msg(MsgKind::Err, format!("Could not delete the backup: {e}")),
                }
                self.rescan(false);
            }
            Confirm::TransferSave(id, idx, to_coop) => {
                let def = Self::def(id);
                let Some(slot) = self.status.get(&id).and_then(|s| s.save_slots.get(idx)).cloned() else { return };
                self.run("Copying save…", move |job| {
                    ops::transfer_save(def, &settings, &slot, to_coop, job)?;
                    Ok(Done::msg(""))
                });
            }
            Confirm::SteamRestart(id) => {
                let def = Self::def(id);
                self.run("Setting the Steam launch option…", move |job| {
                    ops::ensure_launch_option(def, &settings, true, job)?;
                    Ok(Done::msg(format!("Steam's Play button now starts {} with Seamless Co-op (also in Gaming Mode).", def.name)))
                });
            }
            Confirm::SteamRestartAll => {
                let defs: Vec<&'static GameDef> = self.status.iter().filter(|(_, s)| s.installed()).map(|(id, _)| Self::def(*id)).collect();
                self.run("Setting Steam launch options…", move |job| {
                    let n = ops::ensure_launch_options_all(&defs, &settings, job)?;
                    Ok(Done::msg(if n == 0 {
                        "All launch options were already set.".to_string()
                    } else {
                        format!("Launch options set for {n} game(s).")
                    }))
                });
            }
            Confirm::AddToSteam => {
                self.run("Adding BonfireLink to Steam…", move |job| {
                    let running = steam::is_steam_running();
                    if running {
                        steam::shutdown_steam()?;
                    }
                    let n = ops::add_helper_to_steam(job)?;
                    if running {
                        steam::start_steam();
                    }
                    Ok(Done::msg(if n == 0 {
                        "BonfireLink was already in your Steam library.".into()
                    } else {
                        "BonfireLink is now in your Steam library (look under Non-Steam).".to_string()
                    }))
                });
            }
            Confirm::InstallRelease(id, r) => self.install_release(id, r),
            Confirm::Rollback(id, path, version) => {
                let def = Self::def(id);
                self.run(&format!("Rolling back to {version}…"), move |job| {
                    ops::install_from(def, &settings, &path, &version, "rollback", job)?;
                    Ok(Done::msg(""))
                });
            }
            Confirm::ApplySession(id, s, with_settings) => self.apply_session(id, &s, with_settings),
            Confirm::ResetDefaults(id) => {
                let pristine = self.state.game(id).pristine_ini;
                if let Some(ed) = self.editors.get_mut(&id) {
                    if pristine.is_empty() {
                        self.msg(MsgKind::Err, "The original settings file isn't known; reinstall the mod to get it.");
                    } else {
                        let pw = ed.ini.get_any("cooppassword").unwrap_or_default();
                        ed.ini = Ini::parse(&pristine);
                        ed.ini.set_id("cooppassword", &pw);
                        ed.raw = ed.ini.to_text();
                        self.msg(MsgKind::Info, "Mod defaults loaded (password kept). Press Save to apply.");
                    }
                }
            }
        }
    }

    fn apply_session(&mut self, id: GameId, s: &Session, with_settings: bool) {
        let st = self.status.get(&id).cloned().unwrap_or_default();
        let result = (|| -> anyhow::Result<usize> {
            let (path, text) = ops::read_ini(&st)?;
            let mut ini = Ini::parse(&text);
            let mut n = 0;
            if ini.set_id("cooppassword", &s.pw) {
                n += 1;
            }
            if with_settings {
                for (k, v) in &s.s {
                    if !share::is_personal(k) && ini.set_id(k, v) {
                        n += 1;
                    }
                }
            }
            ops::write_ini(&path, &ini.to_text())?;
            Ok(n)
        })();
        match result {
            Ok(_) => {
                self.editors.remove(&id);
                self.msg(
                    MsgKind::Ok,
                    if with_settings { "Their password and settings are now yours." } else { "Their password is now yours." },
                );
            }
            Err(e) => self.msg(MsgKind::Err, format!("{e:#}")),
        }
    }

    fn my_session(&self, id: GameId) -> Session {
        let def = Self::def(id);
        let st = self.status.get(&id).cloned().unwrap_or_default();
        let values = ops::read_ini(&st).map(|(_, t)| Ini::parse(&t).values()).unwrap_or_default();
        Session {
            g: def.key.into(),
            mv: st.mod_version.clone(),
            gv: st.game_version.clone(),
            pw: st.password.clone(),
            n: self.settings.player_name.clone(),
            s: share::shared_settings(&values),
        }
    }
}

/// On the Deck, a fresh install also needs Steam's launch option. Returns the follow-up for the UI.
fn finish_linux_setup(def: &GameDef, settings: &Settings, job: &Job) -> anyhow::Result<Done> {
    if !steam::is_linux() {
        return Ok(Done::msg(""));
    }
    match ops::ensure_launch_option(def, settings, false, job) {
        Ok(LaunchOptionOutcome::NeedsSteamRestart) => Ok(Done { message: String::new(), follow: Follow::AskSteamRestart(def.id) }),
        Ok(LaunchOptionOutcome::NeedsDesktopMode) => {
            Ok(Done::msg("Installed. One more step: open BonfireLink once in Desktop Mode so it can set Steam's launch option."))
        }
        Ok(_) => Ok(Done::msg("")),
        Err(e) => Ok(Done::msg(format!("Installed, but the Steam launch option wasn't set: {e:#}"))),
    }
}

/// The window icon: a small flame drawn in code (no image files to ship).
pub fn icon() -> egui::IconData {
    const N: usize = 64;
    let mut rgba = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = ((x as f32 + 0.5) / N as f32 - 0.5, (y as f32 + 0.5) / N as f32);
            // Teardrop: a circle at the bottom narrowing to a point at the top.
            let width = if fy > 0.62 { (0.30f32.powi(2) - (fy - 0.68).powi(2)).max(0.0).sqrt() } else { 0.30 * (fy / 0.62).powf(1.6) };
            let inner = if fy > 0.70 {
                (0.15f32.powi(2) - (fy - 0.78).powi(2)).max(0.0).sqrt()
            } else {
                0.15 * ((fy - 0.30).max(0.0) / 0.40).powf(1.4)
            };
            let i = (y * N + x) * 4;
            let (r, g, b, a) = if fx.abs() < inner {
                (255, 214, 120, 255)
            } else if fx.abs() < width {
                (232, 120 + (fy * 40.0) as u8, 40, 255)
            } else {
                (0, 0, 0, 0)
            };
            rgba[i..i + 4].copy_from_slice(&[r, g, b, a]);
        }
    }
    egui::IconData { rgba, width: N as u32, height: N as u32 }
}

// ---------- styling ----------

fn style(ctx: &egui::Context, dark: bool) {
    if !dark {
        ctx.set_visuals(egui::Visuals::light());
        return;
    }
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(12, 11, 10);
    v.faint_bg_color = PANEL;
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = EMBER;
    v.selection.bg_fill = Color32::from_rgb(120, 70, 30);
    v.selection.stroke.color = EMBER;
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.inactive.weak_bg_fill = CARD_HI;
    v.widgets.inactive.bg_fill = CARD_HI;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(64, 54, 44);
    v.widgets.hovered.bg_fill = Color32::from_rgb(64, 54, 44);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(90, 66, 40);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = 6.into();
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 7.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 26.0;
    });
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(10)
        .inner_margin(14)
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(52, 46, 40)))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn big_button(ui: &mut Ui, text: &str, color: Color32, enabled: bool) -> bool {
    let b = egui::Button::new(RichText::new(text).size(17.0).strong().color(Color32::from_rgb(20, 16, 12)))
        .fill(if enabled { color } else { color.gamma_multiply(0.4) })
        .min_size(vec2(220.0, 44.0))
        .corner_radius(8);
    ui.add_enabled(enabled, b).clicked()
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(19.0).strong());
}

fn dim(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(DIM).size(13.0));
}

fn status_row(ui: &mut Ui, label: &str, ok: Option<bool>, value: &str) {
    ui.horizontal(|ui| {
        let (icon, color) = match ok {
            Some(true) => ("✔", GOOD),
            Some(false) => ("✖", BAD),
            None => ("•", WARN),
        };
        ui.allocate_ui_with_layout(vec2(16.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(16.0);
            ui.label(RichText::new(icon).color(color).strong());
        });
        if !label.is_empty() {
            ui.allocate_ui_with_layout(vec2(170.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(170.0);
                ui.label(RichText::new(label).color(DIM));
            });
        }
        ui.label(value);
    });
}

// ---------- eframe ----------

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if (self.applied_scale - self.settings.ui_scale).abs() > f32::EPSILON {
            ctx.set_zoom_factor(self.settings.ui_scale.clamp(0.7, 2.5));
            self.applied_scale = self.settings.ui_scale;
        }
        self.poll(&ctx);

        egui::Panel::top("tabs").exact_size(54.0).show(ui, |ui| self.tab_bar(ui));
        egui::Panel::bottom("statusbar").show(ui, |ui| self.status_bar(ui));
        egui::CentralPanel::default_margins().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(format!("{:?}", self.tab)).show(ui, |ui| {
                ui.add_space(4.0);
                match self.tab {
                    Tab::Home => self.home(ui),
                    Tab::Game(id) => self.game_page(ui, id),
                    Tab::Settings => self.settings_page(ui),
                    Tab::Help => self.help_page(ui),
                }
                ui.add_space(24.0);
            });
        });
        self.confirm_modal(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        nxm::clear_gui_running();
        self.settings.last_tab = match self.tab {
            Tab::Home => String::new(),
            Tab::Settings => "settings".into(),
            Tab::Help => "help".into(),
            Tab::Game(id) => Self::def(id).key.into(),
        };
        let _ = self.settings.save();
    }
}

impl App {
    fn tab_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("🔥 BonfireLink").size(21.0).strong().color(EMBER));
            ui.add_space(12.0);
            let tab_btn = |ui: &mut Ui, me: &mut Self, t: Tab, text: String, color: Color32| {
                let selected = me.tab == t;
                let rt = RichText::new(text).size(15.0).strong().color(if selected { color } else { TEXT });
                let resp = ui.add(egui::Button::new(rt).fill(if selected { CARD_HI } else { Color32::TRANSPARENT }).corner_radius(6));
                if resp.clicked() && me.tab != t {
                    me.tab = t;
                    me.rescan(false);
                }
            };
            tab_btn(ui, self, Tab::Home, "Home".into(), EMBER);
            for def in self.visible_games() {
                let st = self.status.get(&def.id);
                let mut text = def.name.split(':').next().unwrap_or(def.name).to_string();
                if self.update_available(def.id) && st.is_some_and(|s| s.installed()) {
                    text.push_str(" •");
                }
                tab_btn(ui, self, Tab::Game(def.id), text, accent(def));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                tab_btn(ui, self, Tab::Help, "Help".into(), EMBER);
                tab_btn(ui, self, Tab::Settings, "⚙ Settings".into(), EMBER);
            });
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);
        if let Some(j) = &self.job {
            let inner = j.job.inner.lock().unwrap();
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(&inner.status).strong());
                if let Some(p) = inner.progress {
                    ui.add(egui::ProgressBar::new(p).desired_width(260.0).show_percentage());
                }
                ui.label(RichText::new(format!("{}s", j.started.elapsed().as_secs())).color(DIM));
            });
        }
        for (kind, text, _) in &self.messages {
            let color = match kind {
                MsgKind::Ok => GOOD,
                MsgKind::Err => BAD,
                MsgKind::Info => TEXT,
            };
            ui.label(RichText::new(text).color(color));
        }
        ui.horizontal(|ui| {
            let deck = if steam::is_steam_deck() {
                if steam::in_gaming_mode() { "Steam Deck · Gaming Mode" } else { "Steam Deck · Desktop Mode" }
            } else if steam::is_linux() {
                "Linux"
            } else {
                "Windows"
            };
            dim(ui, format!("v{} · {deck} · data: {}", config::VERSION, util::redact(&config::data_dir().display().to_string())));
            if self.feeds_pending > 0 {
                ui.spinner();
                dim(ui, "checking for updates…");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(if self.log_open { "Hide log" } else { "Log" }).clicked() {
                    self.log_open = !self.log_open;
                }
            });
        });
        if self.log_open {
            let path = config::data_dir().join("logs/bonfirelink.log");
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(60).collect();
            egui::ScrollArea::vertical().max_height(160.0).id_salt("log").stick_to_bottom(true).show(ui, |ui| {
                for l in tail.into_iter().rev() {
                    ui.label(RichText::new(l).monospace().size(11.5).color(DIM));
                }
            });
        }
        ui.add_space(2.0);
    }

    fn installed_version(&self, id: GameId) -> String {
        self.status.get(&id).filter(|s| s.installed()).map(|s| s.mod_version.clone()).unwrap_or_default()
    }

    /// The version a button press installs without the browser: GitHub, or Nexus with a Premium key.
    /// `installed` = "" for a fresh install.
    fn auto_version(&self, id: GameId, installed: &str) -> Option<String> {
        let f = self.feeds.get(&id)?;
        let latest = f.latest_version()?;
        let nexus_premium = self.settings.nexus_premium
            && !self.settings.nexus_api_key.trim().is_empty()
            && f.nexus.as_ref().is_some_and(|n| remote::same_version(&n.version, &latest));
        let v =
            if f.latest_on_github().is_some() || nexus_premium { Some(latest) } else { f.one_click(installed).map(|r| r.version.clone()) };
        v.filter(|v| installed.is_empty() || remote::newer(v, installed))
    }

    /// The version a one-click update would install for this (installed) game right now.
    fn one_click_update(&self, id: GameId) -> Option<String> {
        let st = self.status.get(&id).filter(|s| s.installed())?;
        if st.mod_version.is_empty() {
            return self.auto_version(id, "");
        }
        self.auto_version(id, &st.mod_version)
    }

    fn update_available(&self, id: GameId) -> bool {
        let (Some(st), Some(f)) = (self.status.get(&id), self.feeds.get(&id)) else { return false };
        match f.latest_version() {
            Some(l) if st.installed() => st.mod_version.is_empty() || remote::newer(&l, &st.mod_version),
            _ => false,
        }
    }

    // ---------- Home ----------

    fn home(&mut self, ui: &mut Ui) {
        if let Some(a) = &self.helper_update {
            let v = a.version.clone();
            let notes = a.notes.clone();
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("BonfireLink {v} is available.")).color(EMBER).strong())
                        .on_hover_text(if notes.is_empty() { "No release notes".into() } else { notes });
                    if ui.add_enabled(!self.busy(), egui::Button::new("Update BonfireLink")).clicked()
                        && let Some(a) = self.helper_update.take()
                    {
                        self.run("Updating BonfireLink…", move |job| {
                            selfupdate::apply(&a, job)?;
                            Ok(Done { message: String::new(), follow: Follow::RestartApp })
                        });
                    }
                });
            });
            ui.add_space(6.0);
        }

        ui.label(RichText::new("Seamless Co-op, one button per game.").size(24.0).strong());
        dim(
            ui,
            "BonfireLink installs, updates and configures LukeYui's Seamless Co-op mods, keeps your saves backed up and gets you and your friends on the same version, password and settings.",
        );
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if ui.add_enabled(self.feeds_pending == 0, egui::Button::new("⟳ Check all for updates")).clicked() {
                self.check_feeds();
            }
            let updatable: Vec<GameId> =
                self.visible_games().iter().filter(|g| self.one_click_update(g.id).is_some()).map(|g| g.id).collect();
            if !updatable.is_empty()
                && ui.add_enabled(!self.busy(), egui::Button::new(format!("Update {} game(s)", updatable.len()))).clicked()
            {
                let settings = self.settings.clone();
                let feeds: Vec<(GameId, Feed, String)> =
                    updatable.iter().map(|id| (*id, self.feeds[id].clone(), self.installed_version(*id))).collect();
                self.run("Updating…", move |job| {
                    let mut done = Vec::new();
                    for (id, feed, installed) in feeds {
                        let def = App::def(id);
                        let (a, v, o) = ops::fetch_latest(def, &settings, &feed, &installed, job)?;
                        ops::install_from(def, &settings, &a, &v, &o, job)?;
                        done.push(format!("{} {v}", def.short));
                    }
                    Ok(Done::msg(format!("Updated: {}", done.join(", "))))
                });
            }
            let last = self.state.last_update_check.clone();
            if !last.is_empty() {
                dim(ui, format!("last check {}", last.replace('_', " ")));
            }
        });
        ui.add_space(8.0);

        let games = self.visible_games();
        let cols = if ui.available_width() > 1050.0 {
            3
        } else if ui.available_width() > 640.0 {
            2
        } else {
            1
        };
        for row in games.chunks(cols) {
            ui.columns(cols, |columns| {
                for (col, def) in columns.iter_mut().zip(row.iter()) {
                    self.home_card(col, def);
                }
            });
            ui.add_space(8.0);
        }

        if steam::is_linux() {
            ui.add_space(14.0);
            card(ui, |ui| {
                heading(ui, "Steam Deck setup");
                if steam::in_gaming_mode() {
                    ui.label(RichText::new("You're in Gaming Mode. Installing, updating and settings work here; the one-time Steam launch-option step needs Desktop Mode.").color(WARN));
                }
                dim(
                    ui,
                    "After a one-time setup, Steam's normal Play button starts Seamless Co-op (also in Gaming Mode), and BonfireLink updates the mod right before the game starts.",
                );
                let missing: Vec<&str> =
                    self.status.iter().filter(|(_, s)| s.installed() && !s.launch_option_ok()).map(|(id, _)| App::def(*id).short).collect();
                if missing.is_empty() {
                    status_row(ui, "Launch options", Some(true), "set for every installed game");
                } else {
                    status_row(ui, "Launch options", Some(false), &format!("missing for {}", missing.join(", ")));
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.busy() && !missing.is_empty() && !steam::in_gaming_mode(),
                            egui::Button::new("Set launch options (Steam restarts once)"),
                        )
                        .clicked()
                    {
                        self.confirm = Some(Confirm::SteamRestartAll);
                    }
                    if ui.add_enabled(!self.busy() && !steam::in_gaming_mode(), egui::Button::new("Add BonfireLink to Steam")).clicked() {
                        self.confirm = Some(Confirm::AddToSteam);
                    }
                });
            });
        }
    }

    fn home_card(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        let feed = self.feeds.get(&def.id).cloned();
        let feed = feed.as_ref();
        let latest = feed.and_then(|f| f.latest_version());
        egui::Frame::new()
            .fill(CARD)
            .corner_radius(10)
            .inner_margin(14)
            .stroke(egui::Stroke::new(2.0, accent(def).gamma_multiply(0.55)))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(130.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(def.name).size(18.0).strong().color(accent(def)));
                    if !def.released() {
                        ui.label(RichText::new("coming soon").color(WARN).small());
                    }
                });
                if st.loc.is_none() {
                    dim(ui, "Game not found on this device.");
                } else if !st.installed() {
                    dim(ui, if def.released() { "Ready to install." } else { "Waiting for the first release." });
                } else {
                    let mut line = format!("Mod {}", if st.mod_version.is_empty() { "installed".into() } else { st.mod_version.clone() });
                    if st.mods_off {
                        line.push_str(" · mods off");
                    }
                    ui.label(line);
                    if let Some(l) = &latest {
                        if self.update_available(def.id) {
                            ui.label(RichText::new(format!("Update available: {l}")).color(EMBER));
                        } else {
                            ui.label(RichText::new("Up to date").color(GOOD));
                        }
                    }
                    if st.password.trim().is_empty() {
                        ui.label(RichText::new("No session password yet").color(WARN));
                    }
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if st.installed()
                        && !st.mods_off
                        && ui.add_enabled(!self.busy(), egui::Button::new(RichText::new("▶ Play").strong())).clicked()
                    {
                        self.play(def.id);
                    }
                    if ui.button("Open").clicked() {
                        self.tab = Tab::Game(def.id);
                    }
                    if st.loc.is_some()
                        && def.released()
                        && !st.installed()
                        && self.auto_version(def.id, "").is_some()
                        && ui.add_enabled(!self.busy(), egui::Button::new("Install")).clicked()
                    {
                        self.update_game(def.id);
                    }
                    if st.installed()
                        && self.one_click_update(def.id).is_some()
                        && ui.add_enabled(!self.busy(), egui::Button::new("Update")).clicked()
                    {
                        self.update_game(def.id);
                    }
                });
            });
    }

    // ---------- game pages ----------

    fn game_page(&mut self, ui: &mut Ui, id: GameId) {
        let def = Self::def(id);
        let sub = *self.subs.entry(id).or_default();
        egui::Frame::new().fill(PANEL).corner_radius(10).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(def.name).size(23.0).strong().color(accent(def)));
                ui.add_space(14.0);
                for (s, label) in Sub::ALL {
                    let selected = sub == s;
                    if ui
                        .add(
                            egui::Button::new(RichText::new(label).strong().color(if selected { accent(def) } else { TEXT }))
                                .fill(if selected { CARD_HI } else { Color32::TRANSPARENT }),
                        )
                        .clicked()
                    {
                        self.subs.insert(id, s);
                        if s == Sub::Troubleshoot {
                            self.diag_cache.remove(&id);
                        }
                    }
                }
            });
        });
        ui.add_space(10.0);
        match sub {
            Sub::Overview => self.overview(ui, def),
            Sub::Configure => self.configure(ui, def),
            Sub::Together => self.together(ui, def),
            Sub::Saves => self.saves(ui, def),
            Sub::Versions => self.versions(ui, def),
            Sub::Troubleshoot => self.troubleshoot(ui, def),
        }
    }

    fn overview(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        let feed = self.feeds.get(&def.id).cloned();
        let latest = feed.as_ref().and_then(|f| f.latest_version());
        dim(ui, def.blurb);
        ui.add_space(6.0);

        card(ui, |ui| {
            // Primary action, depending on where the user is.
            if st.loc.is_none() {
                ui.label(RichText::new(format!("{} wasn't found.", def.name)).size(17.0).color(WARN));
                dim(ui, "BonfireLink looks in every Steam library. If the game is installed somewhere else, pick its folder.");
                if ui.button("Pick the game folder…").clicked() {
                    self.pick_game_dir(def);
                }
                return;
            }
            if !st.installed() {
                if !def.released() && self.auto_version(def.id, "").is_none() {
                    ui.label(RichText::new("Not released yet").size(17.0).color(WARN));
                    dim(
                        ui,
                        "BonfireLink checks the author's release page every time it starts and will offer the install as soon as it's out.",
                    );
                } else if let Some(v) = self.auto_version(def.id, "") {
                    if big_button(ui, &format!("Install Seamless Co-op {v}"), accent(def), !self.busy()) {
                        self.update_game(def.id);
                    }
                    dim(ui, "Downloads the official release, checks it, backs up your saves and puts the files in the game folder.");
                    if latest.as_deref().is_some_and(|l| remote::newer(l, &v)) {
                        dim(
                            ui,
                            format!(
                                "{} is listed by the author but only on Nexus Mods so far; BonfireLink offers it here once GitHub has it.",
                                latest.clone().unwrap_or_default()
                            ),
                        );
                    }
                } else if feed.is_none() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Looking up the newest release…");
                    });
                    if self.feeds_pending == 0 && ui.button("Check now").clicked() {
                        self.check_feeds();
                    }
                } else if latest.is_none() {
                    ui.label(RichText::new("Couldn't reach the release page.").size(16.0).color(WARN));
                    for e in feed.as_ref().map(|f| f.errors.clone()).unwrap_or_default() {
                        dim(ui, e);
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(self.feeds_pending == 0, egui::Button::new("Try again")).clicked() {
                            self.check_feeds();
                        }
                        if ui.button("Install from file…").clicked() {
                            self.pick_archive(def);
                        }
                    });
                } else {
                    self.nexus_get_ui(ui, def, latest.as_deref());
                }
                return;
            }
            ui.horizontal(|ui| {
                let can_play = !st.mods_off && !self.busy();
                if big_button(ui, "▶  Play co-op", accent(def), can_play) {
                    self.play(def.id);
                }
                ui.vertical(|ui| {
                    if st.mods_off {
                        ui.label(RichText::new("Mods are off: Steam starts the normal game.").color(WARN));
                    } else if steam::in_gaming_mode() {
                        dim(ui, "In Gaming Mode you can also close BonfireLink and press Play in your library.");
                    } else if steam::is_linux() {
                        dim(ui, "Steam's own Play button works too (also in Gaming Mode).");
                    } else {
                        dim(ui, "Starts Steam if needed, then the Seamless launcher.");
                    }
                    let mut off = st.mods_off;
                    if ui.checkbox(&mut off, "Mods off (play vanilla / online)").changed() {
                        match ops::set_mods_off(def, &self.settings, off) {
                            Ok(()) => self.msg(MsgKind::Ok, if off { "Mods switched off." } else { "Mods switched on." }),
                            Err(e) => self.msg(MsgKind::Err, format!("{e:#}")),
                        }
                        self.rescan(false);
                    }
                });
            });
            if self.update_available(def.id) {
                ui.add_space(6.0);
                let l = latest.clone().unwrap_or_default();
                let one = self.one_click_update(def.id);
                if let Some(v) = &one {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Version {v} is out.")).color(EMBER).strong());
                        if ui.add_enabled(!self.busy(), egui::Button::new(format!("Update to {v}"))).clicked() {
                            self.update_game(def.id);
                        }
                    });
                }
                if one.as_deref().is_none_or(|v| remote::newer(&l, v)) {
                    self.nexus_get_ui(ui, def, Some(&l));
                }
                if feed.as_ref().is_some_and(|f| f.is_blocked(&st.mod_version)) {
                    ui.label(
                        RichText::new(
                            "The author's version list marks your version as outdated; it may refuse to connect until you update.",
                        )
                        .color(WARN),
                    );
                }
            }
        });
        ui.add_space(8.0);

        if st.loc.is_none() {
            return;
        }
        if def.github_releases.is_none() && def.released() && self.settings.nexus_api_key.trim().is_empty() {
            card(ui, |ui| {
                heading(ui, "Connect Nexus Mods");
                dim(
                    ui,
                    format!(
                        "{} Seamless Co-op is published on Nexus Mods only. Connect your account once so BonfireLink can find and install new versions.",
                        def.name
                    ),
                );
                self.nexus_connect_ui(ui);
            });
            ui.add_space(8.0);
        }
        card(ui, |ui| {
            heading(ui, "Status");
            let Some(loc) = st.loc.as_ref() else { return };
            status_row(
                ui,
                "Game",
                Some(true),
                &format!("found{}", if st.game_version.is_empty() { String::new() } else { format!(" · version {}", st.game_version) }),
            );
            status_row(
                ui,
                "Seamless Co-op",
                Some(st.installed()),
                &if st.installed() {
                    format!(
                        "{} {}",
                        if st.mod_version.is_empty() { "installed" } else { &st.mod_version },
                        if st.managed { "" } else { "(installed by hand)" }
                    )
                } else {
                    "not installed".into()
                },
            );
            match (&latest, feed.as_ref()) {
                (Some(l), _) => status_row(ui, "Newest release", if self.update_available(def.id) { None } else { Some(true) }, l),
                (None, Some(f)) if !f.errors.is_empty() => status_row(ui, "Newest release", None, "couldn't check (offline?)"),
                _ => status_row(ui, "Newest release", None, "checking…"),
            }
            if st.installed() {
                status_row(
                    ui,
                    "Session password",
                    Some(!st.password.trim().is_empty()),
                    if st.password.trim().is_empty() { "not set: Configure or Play together" } else { "set" },
                );
            }
            if steam::is_linux() && st.installed() {
                status_row(
                    ui,
                    "Steam launch option",
                    Some(st.launch_option_ok()),
                    if st.launch_option_ok() { "set: Steam's Play button starts co-op" } else { "missing" },
                );
                if !st.launch_option_ok() {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.busy() && !steam::in_gaming_mode(),
                                egui::Button::new("Set launch option (Steam restarts once)"),
                            )
                            .clicked()
                        {
                            self.confirm = Some(Confirm::SteamRestart(def.id));
                        }
                        if steam::in_gaming_mode() {
                            dim(ui, "Needs Desktop Mode once.");
                        }
                    });
                }
                status_row(ui, "Proton", None, st.proton.as_deref().unwrap_or("Steam default (Proton Experimental or 9+ recommended)"));
            }
            if st.game_updated_since_install {
                status_row(ui, "Game patch", None, "the game updated since the mod was installed. If co-op breaks, wait for a mod update.");
            }
            for c in &st.conflicts {
                status_row(ui, "Heads-up", None, c);
            }
            dim(ui, format!("Folder: {}", util::redact(&loc.exe_dir.display().to_string())));
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Good to know");
            for n in def.notes {
                ui.label(format!("• {n}"));
            }
            ui.label("• The mod's own items are added to your inventory at the first bonfire/site of grace: one item hosts a session, another joins one. Everyone needs the same mod version and password.");
            ui.horizontal(|ui| {
                if let Some(u) = def.nexus_url().filter(|_| def.nexus.is_some_and(|(_, id)| id != 0)) {
                    ui.hyperlink_to("Nexus page", u);
                }
                if let Some(u) = def.github_url() {
                    ui.hyperlink_to("Official release repo", u);
                }
            });
        });
    }

    fn nexus_get_ui(&mut self, ui: &mut Ui, def: &'static GameDef, latest: Option<&str>) {
        let v = latest.unwrap_or("the newest version");
        let watching = self.nexus_watch.is_some_and(|(id, _, _)| id == def.id);
        if def.github_releases.is_some() {
            ui.label(
                RichText::new(format!("Seamless Co-op {v} is out, but the author's GitHub mirror doesn't have it yet."))
                    .size(16.0)
                    .strong(),
            );
            dim(
                ui,
                "New versions usually reach Nexus Mods first. Grab it there, or wait: BonfireLink updates with one click as soon as GitHub has it.",
            );
        } else {
            ui.label(RichText::new(format!("Seamless Co-op {v} is published on Nexus Mods.")).size(16.0).strong());
        }
        let handler = self.nxm_handler == nxm::Handler::Us && !self.settings.nexus_api_key.trim().is_empty();
        let file_id = self.feeds.get(&def.id).and_then(|f| f.nexus.as_ref()).map(|n| n.file_id);
        if handler {
            dim(
                ui,
                "Press the button, then \"Slow download\" (or \"Fast download\") on Nexus: the file comes straight to BonfireLink and installs itself.",
            );
        } else if self.settings.nexus_api_key.trim().is_empty() {
            dim(
                ui,
                "Press the button, click \"Manual download\" on the Main file, and BonfireLink installs it the moment it lands in your Downloads folder. Tip: connect your Nexus account in Settings for one-click installs.",
            );
        } else {
            dim(
                ui,
                "Press the button and download the Main file: BonfireLink installs it the moment it lands in your Downloads folder. (Turn on \"Mod Manager Download\" links in Settings to skip the Downloads step.)",
            );
        }
        ui.horizontal(|ui| {
            if big_button(
                ui,
                if watching { "Waiting for the download…" } else { "Get it from Nexus" },
                accent(def),
                !self.busy() && !watching,
            ) {
                let url = if handler { nxm::file_page(def, file_id) } else { def.nexus_files_url() };
                if let Some(url) = url {
                    util::open_url(&url);
                }
                self.nexus_watch = Some((def.id, SystemTime::now() - Duration::from_secs(5), Instant::now()));
            }
            if watching {
                ui.spinner();
                if ui.button("Stop waiting").clicked() {
                    self.nexus_watch = None;
                }
            }
            if ui.button("Install from file…").clicked() {
                self.pick_archive(def);
            }
        });
        if watching {
            dim(
                ui,
                format!(
                    "Watching {} for a .zip/.7z with \"seamless\" or the mod id in its name.",
                    ops::downloads_dir(&self.settings).map(|d| util::redact(&d.display().to_string())).unwrap_or("Downloads".into())
                ),
            );
        }
    }

    /// Guided Nexus setup: open the key page, paste, check, and (optionally) handle "Mod Manager Download" links.
    fn nexus_connect_ui(&mut self, ui: &mut Ui) {
        let connected = !self.settings.nexus_api_key.trim().is_empty() && !self.settings.nexus_user.is_empty();
        if connected {
            let (text, color) = if self.settings.nexus_premium {
                (
                    format!(
                        "✔ Connected as {} (Premium): installs and updates are fully automatic, also from the Steam Deck's Play button.",
                        self.settings.nexus_user
                    ),
                    GOOD,
                )
            } else {
                (
                    format!(
                        "✔ Connected as {} (free account): Nexus only lets apps download after a click on its \"Mod Manager Download\" button.",
                        self.settings.nexus_user
                    ),
                    GOOD,
                )
            };
            ui.label(RichText::new(text).color(color));
        } else {
            ui.horizontal(|ui| {
                ui.label(RichText::new("1").strong().color(EMBER));
                if ui.button("Open my Nexus API key page").clicked() {
                    util::open_url(nxm::API_KEY_PAGE);
                }
                dim(ui, "Log in, find \"Personal API Key\", press Request / Copy.");
            });
        }
        ui.horizontal(|ui| {
            if !connected {
                ui.label(RichText::new("2").strong().color(EMBER));
            }
            let id = egui::Id::unique("nexus-key");
            ui.add(
                egui::TextEdit::singleline(&mut self.nexus_key_draft)
                    .id(id)
                    .password(true)
                    .desired_width(300.0)
                    .hint_text("paste your API key here"),
            );
            if ui.button("Paste").clicked() {
                ui.memory_mut(|m| m.request_focus(id));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
            let draft = self.nexus_key_draft.trim().to_string();
            let label = if self.nexus_rx.is_some() {
                "Checking…"
            } else if connected && draft == self.settings.nexus_api_key {
                "Check again"
            } else {
                "Save & check"
            };
            if ui.add_enabled(!draft.is_empty() && self.nexus_rx.is_none(), egui::Button::new(RichText::new(label).strong())).clicked() {
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    let _ = tx.send(remote::nexus_validate(&draft).map_err(|e| format!("{e:#}")));
                });
                self.nexus_rx = Some(rx);
            }
            if connected && ui.button("Disconnect").clicked() {
                self.settings.nexus_api_key.clear();
                self.settings.nexus_user.clear();
                self.settings.nexus_premium = false;
                self.nexus_key_draft.clear();
                self.nexus_user = None;
                self.save_settings();
            }
        });
        if steam::in_gaming_mode() {
            dim(ui, "Gaming Mode: press Steam + X for the keyboard, or open the key page on your phone and type the key in.");
        }
        if let Some(Err(e)) = &self.nexus_user {
            ui.label(RichText::new(e).color(BAD));
        }
        if !connected || self.settings.nexus_premium {
            return;
        }
        let mut on = self.nxm_handler == nxm::Handler::Us;
        ui.horizontal(|ui| {
            ui.label(RichText::new("3").strong().color(EMBER));
            if ui.checkbox(&mut on, "Let BonfireLink handle Nexus \"Mod Manager Download\" buttons").changed() {
                let result = if on {
                    nxm::register().map(|before| {
                        if let nxm::Handler::Other(cmd) = before {
                            self.settings.nxm_previous_handler = cmd;
                        }
                    })
                } else {
                    nxm::unregister(&self.settings.nxm_previous_handler).map(|_| self.settings.nxm_previous_handler.clear())
                };
                match result {
                    Ok(()) => {
                        self.save_settings();
                        self.msg(
                            MsgKind::Ok,
                            if on { "Nexus download buttons now go to BonfireLink." } else { "Nexus download buttons are handed back." },
                        );
                    }
                    Err(e) => self.msg(MsgKind::Err, format!("{e:#}")),
                }
                self.nxm_handler = nxm::current_handler();
            }
        });
        match &self.nxm_handler {
            nxm::Handler::Other(cmd) => {
                let short = cmd.rsplit(['\\', '/']).next().unwrap_or(cmd).trim_matches(['"', ' ']).to_string();
                dim(
                    ui,
                    format!(
                        "Currently handled by another app ({short}). BonfireLink remembers it and gives the links back when you switch this off."
                    ),
                );
            }
            nxm::Handler::Us => dim(
                ui,
                "Click \"Mod Manager Download\" (or \"Slow download\" after it) on a Seamless Co-op file and it installs by itself, even when BonfireLink is closed.",
            ),
            nxm::Handler::None => dim(ui, "Then one click on Nexus installs the file, no Downloads folder involved."),
        }
    }

    fn pick_archive(&mut self, def: &'static GameDef) {
        let picked = rfd::FileDialog::new()
            .set_title(format!("Pick the Seamless Co-op archive for {}", def.name))
            .add_filter("Archives", &["zip", "7z"])
            .set_directory(ops::downloads_dir(&self.settings).unwrap_or_default())
            .pick_file();
        if let Some(p) = picked {
            self.install_file(def.id, p);
        }
    }

    fn pick_game_dir(&mut self, def: &'static GameDef) {
        if let Some(p) = rfd::FileDialog::new().set_title(format!("Pick the {} folder", def.name)).pick_folder() {
            if steam::location_from_folder(def, &p).is_some() {
                self.settings.game_dirs.insert(def.id, p.to_string_lossy().to_string());
                self.save_settings();
                self.rescan(false);
                self.msg(MsgKind::Ok, format!("{} found.", def.name));
            } else {
                self.msg(MsgKind::Err, format!("{} isn't in that folder (looking for {}).", def.name, def.game_exe));
            }
        }
    }

    // ---------- configure ----------

    fn configure(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        let Some(path) = st.ini.clone() else {
            card(ui, |ui| {
                heading(ui, "Settings");
                dim(ui, "Install Seamless Co-op first; its settings file appears here afterwards.");
            });
            return;
        };
        let ed = self.editors.entry(def.id).or_default();
        if ed.loaded_for.as_ref() != Some(&path) {
            match std::fs::read_to_string(&path) {
                Ok(t) => {
                    *ed = IniEditor {
                        path: path.clone(),
                        ini: Ini::parse(&t),
                        raw: t.clone(),
                        saved_text: t,
                        loaded_for: Some(path.clone()),
                        ..Default::default()
                    };
                }
                Err(e) => ed.error = format!("Could not read the settings file: {e}"),
            }
        }
        let languages = self.languages.get(&def.id).cloned().unwrap_or_default();
        let state = self.state.game(def.id);
        let pristine = Ini::parse(&state.pristine_ini).values();
        let custom: Vec<String> = state.presets.keys().cloned().collect();
        let mut save_now = false;
        let mut revert = false;
        let mut apply_builtin: Option<usize> = None;
        let mut apply_custom: Option<String> = None;
        let mut delete_custom: Option<String> = None;
        let mut save_preset: Option<String> = None;
        let mut reset = false;
        let mut copy_pw: Option<String> = None;
        let busy = self.busy();
        let ed = self.editors.get_mut(&def.id).unwrap();

        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                heading(ui, "Presets");
                egui::ComboBox::from_id_salt(("preset", def.key)).selected_text("Apply a preset…").width(220.0).show_ui(ui, |ui| {
                    for (i, p) in presets::PRESETS.iter().enumerate() {
                        if ui.selectable_label(false, p.name).on_hover_text(p.description).clicked() {
                            apply_builtin = Some(i);
                        }
                    }
                    if !custom.is_empty() {
                        ui.separator();
                        for name in &custom {
                            if ui.selectable_label(false, format!("★ {name}")).clicked() {
                                apply_custom = Some(name.clone());
                            }
                        }
                    }
                });
                if ui.button("Mod defaults").on_hover_text("Settings exactly as the release shipped them (your password stays)").clicked() {
                    reset = true;
                }
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut ed.preset_name).hint_text("my preset name").desired_width(150.0));
                if ui.add_enabled(!ed.preset_name.trim().is_empty(), egui::Button::new("Save as preset")).clicked() {
                    save_preset = Some(ed.preset_name.trim().to_string());
                }
                if !custom.is_empty() {
                    egui::ComboBox::from_id_salt(("del", def.key)).selected_text("Delete preset…").show_ui(ui, |ui| {
                        for name in &custom {
                            if ui.selectable_label(false, name).clicked() {
                                delete_custom = Some(name.clone());
                            }
                        }
                    });
                }
            });
            dim(
                ui,
                "Presets change gameplay and scaling options only; your password and personal options stay. Nothing is written until you press Save.",
            );
        });
        ui.add_space(8.0);

        card(ui, |ui| {
            ui.horizontal(|ui| {
                heading(ui, "Settings");
                ui.add_space(10.0);
                if ui.selectable_label(!ed.raw_mode, "Easy").clicked() && ed.raw_mode {
                    ed.ini = Ini::parse(&ed.raw);
                    ed.raw_mode = false;
                }
                if ui.selectable_label(ed.raw_mode, "Text file").clicked() && !ed.raw_mode {
                    ed.raw = ed.ini.to_text();
                    ed.raw_mode = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dirty = ed.dirty();
                    if ui
                        .add_enabled(
                            dirty && !busy,
                            egui::Button::new(RichText::new("💾 Save").strong()).fill(if dirty {
                                accent(def).gamma_multiply(0.6)
                            } else {
                                CARD_HI
                            }),
                        )
                        .clicked()
                    {
                        save_now = true;
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Undo changes")).clicked() {
                        revert = true;
                    }
                    if dirty {
                        ui.label(RichText::new("unsaved changes").color(WARN));
                    }
                });
            });
            if !ed.error.is_empty() {
                ui.label(RichText::new(&ed.error).color(BAD));
            }
            if ed.raw_mode {
                dim(ui, util::redact(&ed.path.display().to_string()));
                ui.add(egui::TextEdit::multiline(&mut ed.raw).code_editor().desired_rows(24).desired_width(f32::INFINITY));
                return;
            }
            let fields = ed.ini.schema();
            let mut section = String::new();
            for f in fields {
                if f.section != section {
                    section = f.section.clone();
                    ui.add_space(8.0);
                    ui.label(RichText::new(section_title(&section)).size(15.0).strong().color(accent(def)));
                    ui.separator();
                }
                let id = f.id();
                let mut value = ed.ini.get(&f.section, &f.key).unwrap_or_default();
                let before = value.clone();
                let default = pristine.get(&id).cloned();
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(vec2(250.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.set_min_width(250.0);
                        ui.add(egui::Label::new(RichText::new(&f.label).strong()).truncate()).on_hover_text(&f.help);
                    });
                    match &f.kind {
                        Kind::Bool => {
                            let mut b = value.trim() == "1";
                            let label = if b { "On" } else { "Off" };
                            if ui.checkbox(&mut b, label).changed() {
                                value = if b { "1" } else { "0" }.into();
                            }
                        }
                        Kind::Choice(opts) => {
                            let current =
                                opts.iter().find(|(v, _)| *v == value.trim()).map(|(_, l)| l.clone()).unwrap_or_else(|| value.clone());
                            egui::ComboBox::from_id_salt(("c", def.key, id.clone())).selected_text(current).width(260.0).show_ui(
                                ui,
                                |ui| {
                                    for (v, l) in opts {
                                        ui.selectable_value(&mut value, v.clone(), l);
                                    }
                                },
                            );
                        }
                        Kind::Range { min, max, suffix } => {
                            let mut n: i64 = value.trim().parse().unwrap_or(*min);
                            let hi = (*max).max(n);
                            if ui.add(egui::Slider::new(&mut n, *min..=hi).suffix(*suffix).clamping(egui::SliderClamping::Never)).changed()
                            {
                                value = n.to_string();
                            }
                            if *suffix == "%" {
                                let per = n as f32 / 100.0;
                                dim(ui, format!("2P ×{:.2} · 3P ×{:.2} · 4P ×{:.2}", 1.0 + per, 1.0 + 2.0 * per, 1.0 + 3.0 * per));
                            }
                        }
                        Kind::Int => {
                            let mut n: i64 = value.trim().parse().unwrap_or(0);
                            if ui.add(egui::DragValue::new(&mut n)).changed() {
                                value = n.to_string();
                            }
                        }
                        Kind::Password => {
                            ui.add(
                                egui::TextEdit::singleline(&mut value)
                                    .password(!ed.show_password)
                                    .desired_width(220.0)
                                    .hint_text("required to play"),
                            );
                            if ui.small_button(if ed.show_password { "Hide" } else { "Show" }).clicked() {
                                ed.show_password = !ed.show_password;
                            }
                            if ui.small_button("Generate").clicked() {
                                value = share::generate_password();
                                ed.show_password = true;
                            }
                            if ui.small_button("Copy").clicked() {
                                copy_pw = Some(value.clone());
                            }
                        }
                        Kind::Language => {
                            let shown = if value.trim().is_empty() { "Same as the game".to_string() } else { value.clone() };
                            egui::ComboBox::from_id_salt(("lang", def.key)).selected_text(shown).show_ui(ui, |ui| {
                                ui.selectable_value(&mut value, String::new(), "Same as the game");
                                for l in &languages {
                                    ui.selectable_value(&mut value, l.clone(), l);
                                }
                            });
                        }
                        Kind::Text => {
                            ui.add(egui::TextEdit::singleline(&mut value).desired_width(220.0));
                        }
                    }
                    if let Some(d) = &default
                        && d.trim() != value.trim()
                        && !matches!(f.kind, Kind::Password)
                        && ui.small_button("↺").on_hover_text(format!("Back to the mod default ({d})")).clicked()
                    {
                        value = d.clone();
                    }
                });
                if !f.help.is_empty() && !matches!(f.kind, Kind::Choice(_)) {
                    ui.horizontal(|ui| {
                        ui.add_space(258.0);
                        ui.label(RichText::new(first_sentence(&clean_help(&f.help))).color(DIM).size(12.5));
                    });
                }
                if f.key.eq_ignore_ascii_case("save_file_extension") && value.trim() != before.trim() {
                    value = value.chars().filter(|c| c.is_ascii_alphanumeric()).take(120).collect();
                }
                if value != before {
                    ed.ini.set(&f.section, &f.key, &value);
                }
            }
        });

        if let Some(p) = copy_pw {
            ui.ctx().copy_text(p);
            self.msg(MsgKind::Ok, "Password copied.");
        }
        let mut notes: Vec<String> = Vec::new();
        let ed = self.editors.get_mut(&def.id).unwrap();
        if let Some(i) = apply_builtin {
            let p = &presets::PRESETS[i];
            if ed.raw_mode {
                ed.ini = Ini::parse(&ed.raw);
            }
            let n = presets::apply(&mut ed.ini, p.values);
            ed.raw = ed.ini.to_text();
            notes.push(format!("Preset \"{}\" changed {n} setting(s). Press Save to keep it.", p.name));
        }
        if let Some(name) = apply_custom
            && let Some(values) = state.presets.get(&name)
        {
            for (k, v) in values {
                ed.ini.set_id(k, v);
            }
            ed.raw = ed.ini.to_text();
            notes.push(format!("Preset \"{name}\" loaded. Press Save to keep it."));
        }
        if let Some(name) = save_preset {
            let values: BTreeMap<String, String> = share::shared_settings(&ed.ini.values());
            let mut s = State::load();
            s.game_mut(def.id).presets.insert(name.clone(), values);
            let _ = s.save();
            ed.preset_name.clear();
            notes.push(format!("Preset \"{name}\" saved."));
        }
        if let Some(name) = delete_custom {
            let mut s = State::load();
            s.game_mut(def.id).presets.remove(&name);
            let _ = s.save();
            notes.push(format!("Preset \"{name}\" deleted."));
        }
        for n in notes {
            self.msg(MsgKind::Info, n);
        }
        let ed = self.editors.get_mut(&def.id).unwrap();
        if revert {
            ed.ini = Ini::parse(&ed.saved_text);
            ed.raw = ed.saved_text.clone();
        }
        if reset {
            self.confirm = Some(Confirm::ResetDefaults(def.id));
        }
        let mut notes_after_save: Option<&str> = None;
        if save_now {
            let ed = self.editors.get_mut(&def.id).unwrap();
            let mut text = if ed.raw_mode { ed.raw.clone() } else { ed.ini.to_text() };
            // An update may have rewritten the file since it was loaded: apply the edits on top
            // of the new file instead of overwriting its new options.
            let on_disk = std::fs::read_to_string(&ed.path).unwrap_or_default();
            if !on_disk.is_empty() && on_disk != ed.saved_text {
                let mut fresh = Ini::parse(&on_disk);
                fresh.merge_user_values(&Ini::parse(&text));
                text = fresh.to_text();
                notes_after_save = Some("The settings file had changed on disk (mod update?); your edits were applied on top of it.");
            }
            match ops::write_ini(&ed.path, &text) {
                Ok(()) => {
                    ed.saved_text = text.clone();
                    ed.ini = Ini::parse(&text);
                    ed.raw = text;
                    self.msg(MsgKind::Ok, "Settings saved. They apply the next time the game starts.");
                    if let Some(n) = notes_after_save {
                        self.msg(MsgKind::Info, n);
                    }
                    self.rescan(false);
                }
                Err(e) => self.msg(MsgKind::Err, format!("{e:#}")),
            }
        }
    }

    // ---------- play together ----------

    fn together(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        if !st.installed() {
            card(ui, |ui| {
                heading(ui, "Play together");
                dim(ui, "Install Seamless Co-op first.");
            });
            return;
        }
        let mine = self.my_session(def.id);
        let code = share::encode(&mine).unwrap_or_default();

        card(ui, |ui| {
            heading(ui, "Invite friends");
            dim(
                ui,
                "Send your code (or the whole message) to everyone you play with. When they paste it into BonfireLink, it checks versions and copies your password and settings in one click.",
            );
            ui.horizontal(|ui| {
                ui.label("Your name:");
                if ui
                    .add(egui::TextEdit::singleline(&mut self.settings.player_name).desired_width(160.0).hint_text("shown to friends"))
                    .lost_focus()
                {
                    self.save_settings();
                }
            });
            if mine.pw.trim().is_empty() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("You haven't set a session password.").color(WARN));
                    if ui.button("Generate one").clicked() {
                        let pw = share::generate_password();
                        let s = Session { pw, ..Default::default() };
                        self.apply_session(def.id, &s, false);
                        self.rescan(false);
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Mod {} · game {} · password {}",
                        or_unknown(&mine.mv),
                        or_unknown(&mine.gv),
                        if mine.pw.is_empty() { "—" } else { "set" }
                    ))
                    .color(DIM),
                );
            });
            ui.add(egui::TextEdit::singleline(&mut code.clone()).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
            ui.horizontal(|ui| {
                if ui.button("📋 Copy code").clicked() {
                    ui.ctx().copy_text(code.clone());
                    self.msg(MsgKind::Ok, "Code copied. Paste it in Discord / Steam chat.");
                }
                if ui.button("📋 Copy message for friends").clicked() {
                    ui.ctx().copy_text(share::friend_message(def.name, &mine, &code));
                    self.msg(MsgKind::Ok, "Message copied.");
                }
                if ui.button("Copy password only").clicked() {
                    ui.ctx().copy_text(mine.pw.clone());
                    self.msg(MsgKind::Ok, "Password copied.");
                }
                if ui.button("New password").on_hover_text("Generates a new password; send your code again afterwards").clicked() {
                    let s = Session { pw: share::generate_password(), ..Default::default() };
                    self.apply_session(def.id, &s, false);
                    self.rescan(false);
                }
            });
        });
        ui.add_space(8.0);

        let sh = self.shares.entry(def.id).or_default();
        let mut paste_now = false;
        card(ui, |ui| {
            heading(ui, "Join a friend");
            dim(ui, "Paste their code or whole message. On Steam Deck: press Steam + X for the keyboard, or Ctrl+V from Desktop Mode.");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut sh.pasted)
                        .id(egui::Id::unique(("paste-box", def.key)))
                        .desired_rows(3)
                        .desired_width(ui.available_width() - 110.0)
                        .hint_text("BL1.…"),
                );
                ui.vertical(|ui| {
                    if ui.button("Clear").clicked() {
                        sh.pasted.clear();
                    }
                    paste_now = ui.button("Paste").clicked();
                });
            });
        });
        if paste_now {
            // The pasted text goes to the focused widget, so focus the box first.
            ui.memory_mut(|m| m.request_focus(egui::Id::unique(("paste-box", def.key))));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
        let sh = self.shares.get_mut(&def.id).unwrap();
        if sh.pasted != sh.last_parsed {
            sh.last_parsed = sh.pasted.clone();
            sh.decoded = if sh.pasted.trim().is_empty() {
                None
            } else {
                Some(match share::find_code(&sh.pasted) {
                    Some(c) => share::decode(&c).map_err(|e| e.to_string()),
                    None => Err("No BonfireLink code (BL1.…) found in the pasted text.".into()),
                })
            };
        }
        let decoded = sh.decoded.clone();
        match decoded {
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(BAD));
            }
            Some(Ok(theirs)) => {
                ui.add_space(8.0);
                let rows = share::compare(&mine, &theirs);
                let worst = rows.iter().map(|r| r.severity).max().unwrap_or(Severity::Ok);
                card(ui, |ui| {
                    let who = if theirs.n.is_empty() { "your friend".to_string() } else { theirs.n.clone() };
                    let (verdict, color) = match worst {
                        Severity::Ok => (format!("You and {who} are a perfect match. Go play!"), GOOD),
                        Severity::Info => (format!("You can play with {who}; some settings differ."), GOOD),
                        Severity::Warn => (format!("Probably fine with {who}, but check the warnings."), WARN),
                        Severity::Block => (format!("You can't connect to {who} yet; fix the red rows."), BAD),
                    };
                    ui.label(RichText::new(verdict).size(17.0).strong().color(color));
                    egui::Grid::new(("cmp", def.key)).striped(true).num_columns(4).show(ui, |ui| {
                        ui.label(RichText::new("").strong());
                        ui.label(RichText::new("You").strong());
                        ui.label(RichText::new(&who).strong());
                        ui.label("");
                        ui.end_row();
                        for r in rows.iter().filter(|r| {
                            r.severity != Severity::Ok || matches!(r.what.as_str(), "Mod version" | "Game version" | "Password")
                        }) {
                            let c = match r.severity {
                                Severity::Ok => GOOD,
                                Severity::Info => DIM,
                                Severity::Warn => WARN,
                                Severity::Block => BAD,
                            };
                            ui.label(RichText::new(&r.what).color(c));
                            ui.label(&r.mine);
                            ui.label(&r.theirs);
                            ui.label(RichText::new(&r.hint).color(DIM).size(12.0));
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if theirs.g == def.key {
                            if ui
                                .add_enabled(!self.busy(), egui::Button::new(RichText::new("Use their password + settings").strong()))
                                .clicked()
                            {
                                self.confirm = Some(Confirm::ApplySession(def.id, theirs.clone(), true));
                            }
                            if ui.add_enabled(!self.busy(), egui::Button::new("Use their password only")).clicked() {
                                self.apply_session(def.id, &theirs, false);
                                self.rescan(false);
                            }
                            let my_older = !theirs.mv.is_empty() && !mine.mv.is_empty() && remote::newer(&theirs.mv, &mine.mv);
                            if my_older && ui.add_enabled(!self.busy(), egui::Button::new(format!("Update me to {}", theirs.mv))).clicked()
                            {
                                self.update_game(def.id);
                            }
                        } else if let Some(other) = crate::games::by_key(&theirs.g)
                            && ui.button(format!("Open {}", other.name)).clicked()
                        {
                            self.tab = Tab::Game(other.id);
                            self.subs.insert(other.id, Sub::Together);
                            let pasted = self.shares.get(&def.id).map(|s| s.pasted.clone()).unwrap_or_default();
                            self.shares.entry(other.id).or_default().pasted = pasted;
                        }
                    });
                });
            }
            None => {}
        }
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Checklist");
            for (ok, text) in [
                (!mine.pw.trim().is_empty(), "Everyone uses the same password"),
                (!mine.mv.is_empty(), "Everyone has the same Seamless Co-op version (BonfireLink > Update)"),
                (true, "Everyone is on the same game patch (let Steam update)"),
                (!st.mods_off, "Mods are on, and the game was started through Seamless Co-op"),
                (true, "One player hosts with the mod's host item; the others join with the join item"),
            ] {
                status_row(ui, "", Some(ok), text);
            }
        });
    }

    // ---------- saves ----------

    fn saves(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        let backups = std::mem::take(self.backups.entry(def.id).or_default());
        card(ui, |ui| {
            heading(ui, "Your saves");
            match &st.save_root {
                Some(root) if root.is_dir() => {
                    ui.horizontal(|ui| {
                        dim(ui, util::redact(&root.display().to_string()));
                        if ui.small_button("Open folder").clicked() {
                            util::open_path(root);
                        }
                    });
                }
                Some(_) => dim(ui, "No saves on this device yet (start the game and save once)."),
                None => dim(ui, "Start the game once through Steam so its save folder exists."),
            }
            if !st.save_slots.is_empty() {
                egui::Grid::new(("slots", def.key)).striped(true).num_columns(4).show(ui, |ui| {
                    ui.label(RichText::new("Account").strong());
                    ui.label(RichText::new(format!("Vanilla ({}.sl2)", def.save_stem)).strong());
                    ui.label(RichText::new("Co-op save").strong());
                    ui.label("");
                    ui.end_row();
                    for (i, slot) in st.save_slots.iter().enumerate() {
                        ui.label(slot.account_dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                        ui.label(slot.vanilla.as_ref().map(|(_, d)| d.clone()).unwrap_or("—".into()));
                        ui.label(slot.coop.as_ref().map(|(_, d)| d.clone()).unwrap_or("—".into()));
                        ui.horizontal(|ui| {
                            if slot.vanilla.is_some()
                                && ui
                                    .add_enabled(!self.busy(), egui::Button::new("Vanilla > co-op"))
                                    .on_hover_text("Continue your normal character in Seamless Co-op")
                                    .clicked()
                            {
                                self.confirm = Some(Confirm::TransferSave(def.id, i, true));
                            }
                            if slot.coop.is_some()
                                && ui
                                    .add_enabled(!self.busy(), egui::Button::new("Co-op > vanilla"))
                                    .on_hover_text("Take your co-op character back to the normal game")
                                    .clicked()
                            {
                                self.confirm = Some(Confirm::TransferSave(def.id, i, false));
                            }
                        });
                        ui.end_row();
                    }
                });
            }
            dim(
                ui,
                "Seamless Co-op keeps its own save file, so co-op progress never touches your vanilla character unless you copy it here. Steam Cloud may not sync the co-op file: backups below are your undo button.",
            );
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                heading(ui, "Backups");
                if ui
                    .add_enabled(!self.busy() && st.save_root.as_ref().is_some_and(|r| r.is_dir()), egui::Button::new("Back up now"))
                    .clicked()
                {
                    let settings = self.settings.clone();
                    self.run("Backing up saves…", move |job| {
                        ops::backup_saves(def, &settings, "manual", true, job)?;
                        Ok(Done::msg(""))
                    });
                }
                if ui.button("Open backups folder").clicked() {
                    util::open_path(&config::sub("backups").join(def.key));
                }
            });
            dim(
                ui,
                format!(
                    "Automatic backups before every install{}. The newest {} are kept{}.",
                    if self.settings.backup_saves_before_play { " and every launch" } else { "" },
                    self.settings.keep_backups,
                    if self.settings.backup_mirror.is_empty() { String::new() } else { ", plus a copy in your extra backup folder".into() }
                ),
            );
            if backups.is_empty() {
                dim(ui, "No backups yet.");
            }
            egui::Grid::new(("backups", def.key)).striped(true).num_columns(3).show(ui, |ui| {
                for b in &backups {
                    ui.label(b.name.replacen('_', " ", 1));
                    ui.label(RichText::new(util::human_size(b.size)).color(DIM));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.busy(), egui::Button::new("Restore")).clicked() {
                            self.confirm = Some(Confirm::RestoreBackup(def.id, b.path.clone()));
                        }
                        if ui.small_button("Delete").on_hover_text("Delete this backup").clicked() {
                            self.confirm = Some(Confirm::DeleteBackup(b.path.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        });
        self.backups.insert(def.id, backups);
    }

    // ---------- versions ----------

    fn versions(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        let feed = self.feeds.get(&def.id).cloned();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                heading(ui, "Releases");
                if ui.add_enabled(self.feeds_pending == 0, egui::Button::new("⟳ Check")).clicked() {
                    self.check_feeds();
                }
            });
            status_row(ui, "Installed", Some(st.installed()), if st.installed() { or_unknown(&st.mod_version) } else { "nothing" });
            let state = self.state.game(def.id);
            if st.installed() && !state.source.is_empty() {
                let when =
                    if state.installed_at.is_empty() { String::new() } else { format!(" · {}", state.installed_at.replacen('_', " ", 1)) };
                dim(ui, format!("{}{when}", state.source));
            }
            let Some(feed) = feed.clone() else {
                dim(ui, "Not checked yet.");
                return;
            };
            if let Some(cur) = &feed.gate.current {
                status_row(ui, "Author's current version", None, cur);
            }
            if let Some(n) = &feed.nexus {
                status_row(
                    ui,
                    "Nexus newest file",
                    None,
                    &format!("{} {} · {} · {}", n.name, n.version, n.uploaded, util::human_size(n.size)),
                );
                if !n.changelog.is_empty() {
                    egui::CollapsingHeader::new("Nexus changelog").id_salt((def.key, "nxlog")).show(ui, |ui| {
                        ui.label(&n.changelog);
                    });
                }
            }
            for e in &feed.errors {
                ui.label(RichText::new(e).color(WARN).size(12.5));
            }
            ui.add_space(6.0);
            if feed.releases.is_empty() {
                dim(
                    ui,
                    if def.github_releases.is_some() {
                        "No downloadable releases on GitHub yet."
                    } else {
                        "This mod is published on Nexus only; GitHub has just the version list."
                    },
                );
            }
            for (i, r) in feed.releases.iter().enumerate() {
                let installed = remote::same_version(&r.version, &st.mod_version) && st.installed();
                egui::CollapsingHeader::new(
                    RichText::new(format!("{}  ·  {}{}", r.version, r.published, if installed { "  · installed" } else { "" })).strong(),
                )
                .id_salt((def.key, &r.tag))
                .default_open(i == 0)
                .show(ui, |ui| {
                    if r.notes.trim().is_empty() {
                        dim(ui, "No release notes.");
                    } else {
                        ui.label(r.notes.trim());
                    }
                    ui.horizontal(|ui| {
                        dim(ui, format!("{} · {}", r.asset_name, util::human_size(r.size)));
                        if !installed
                            && ui
                                .add_enabled(!self.busy() && st.loc.is_some(), egui::Button::new(format!("Install {}", r.version)))
                                .clicked()
                        {
                            self.confirm = Some(Confirm::InstallRelease(def.id, r.clone()));
                        }
                    });
                });
            }
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Other ways to install");
            if st.loc.is_none() {
                dim(ui, format!("{} wasn't found on this device; install the game first (or pick its folder on Troubleshoot).", def.name));
                return;
            }
            if def.nexus.is_some_and(|(_, id)| id != 0) {
                // Includes "Install from file…".
                self.nexus_get_ui(ui, def, feed.as_ref().and_then(|f| f.latest_version()).as_deref());
            } else if ui.add_enabled(!self.busy(), egui::Button::new("Install from file… (.zip / .7z)")).clicked() {
                self.pick_archive(def);
            }
        });
        ui.add_space(8.0);
        let rbs = std::mem::take(self.rollbacks.entry(def.id).or_default());
        card(ui, |ui| {
            heading(ui, "Previous versions (rollback)");
            dim(
                ui,
                format!(
                    "Every update keeps the version it replaced (newest {}). Your settings carry over either way.",
                    self.settings.keep_rollbacks
                ),
            );
            if rbs.is_empty() {
                dim(ui, "Nothing kept yet.");
            }
            for rb in &rbs {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&rb.version).strong());
                    dim(ui, &rb.when);
                    if ui.add_enabled(!self.busy() && st.loc.is_some(), egui::Button::new("Roll back")).clicked() {
                        self.confirm = Some(Confirm::Rollback(def.id, rb.path.clone(), rb.version.clone()));
                    }
                });
            }
        });
        self.rollbacks.insert(def.id, rbs);
    }

    // ---------- troubleshoot ----------

    fn troubleshoot(&mut self, ui: &mut Ui, def: &'static GameDef) {
        let st = self.status.get(&def.id).cloned().unwrap_or_default();
        card(ui, |ui| {
            heading(ui, "Tools");
            ui.horizontal_wrapped(|ui| {
                if ui.button("⟳ Re-scan").clicked() {
                    self.rescan(false);
                    self.diag_cache.remove(&def.id);
                }
                if let Some(loc) = &st.loc {
                    if ui.button("Open game folder").clicked() {
                        util::open_path(&loc.exe_dir);
                    }
                    if loc.exe_dir.join("SeamlessCoop").is_dir() && ui.button("Open mod folder").clicked() {
                        util::open_path(&loc.exe_dir.join("SeamlessCoop"));
                    }
                }
                if ui.button("Pick the game folder…").clicked() {
                    self.pick_game_dir(def);
                }
                if self.settings.game_dirs.contains_key(&def.id) && ui.button("Forget picked folder").clicked() {
                    self.settings.game_dirs.remove(&def.id);
                    self.save_settings();
                    self.rescan(false);
                }
                if cfg!(windows)
                    && st.installed()
                    && ui.button("Desktop shortcut").on_hover_text("A desktop icon that starts the game with Seamless Co-op").clicked()
                {
                    match ops::create_desktop_shortcut(def, &self.settings) {
                        Ok(_) => self.msg(MsgKind::Ok, "Shortcut created on your desktop."),
                        Err(e) => self.msg(MsgKind::Err, format!("{e:#}")),
                    }
                }
                if steam::is_linux()
                    && st.installed()
                    && ui
                        .add_enabled(!self.busy() && !steam::in_gaming_mode(), egui::Button::new("Set Steam launch option again"))
                        .clicked()
                {
                    self.confirm = Some(Confirm::SteamRestart(def.id));
                }
                if ui.add_enabled(!self.busy(), egui::Button::new("Save a bug report")).clicked() {
                    let settings = self.settings.clone();
                    self.run("Collecting a bug report…", move |job| {
                        let p = ops::bug_report(def, &settings, job)?;
                        Ok(Done {
                            message: "Bug report saved (password removed). Attach the zip when you ask for help.".into(),
                            follow: Follow::OpenPath(p.parent().unwrap().to_path_buf()),
                        })
                    });
                }
                if st.installed() && ui.add_enabled(!self.busy(), egui::Button::new(RichText::new("Uninstall").color(BAD))).clicked() {
                    self.confirm = Some(Confirm::Uninstall(def.id));
                }
            });
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                heading(ui, "Diagnostics");
                if ui.small_button("Copy").clicked() {
                    let d = self.diag_cache.get(&def.id).cloned().unwrap_or_default();
                    ui.ctx().copy_text(d);
                    self.msg(MsgKind::Ok, "Diagnostics copied.");
                }
            });
            let text = self.diag_cache.entry(def.id).or_insert_with(|| ops::diagnostics(def, &self.settings)).clone();
            ui.label(RichText::new(text).monospace().size(12.0));
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Common problems");
            for (q, a) in faq(def) {
                egui::CollapsingHeader::new(RichText::new(q).strong()).id_salt((def.key, q)).show(ui, |ui| {
                    ui.label(a);
                });
            }
        });
    }

    // ---------- settings ----------

    fn settings_page(&mut self, ui: &mut Ui) {
        let mut changed = false;
        card(ui, |ui| {
            heading(ui, "General");
            ui.horizontal(|ui| {
                ui.label("Interface size");
                changed |= ui.add(egui::Slider::new(&mut self.settings.ui_scale, 0.8..=2.2).step_by(0.05)).changed();
            });
            if ui.checkbox(&mut self.settings.dark_theme, "Dark theme").changed() {
                style(ui.ctx(), self.settings.dark_theme);
                changed = true;
            }
            changed |= ui.checkbox(&mut self.settings.check_updates_on_start, "Check for mod updates when BonfireLink starts").changed();
            changed |= ui.checkbox(&mut self.settings.check_helper_updates, "Check for BonfireLink updates").changed();
            let label = if steam::is_linux() {
                "Update the mod automatically when a game starts (Steam's Play button, also in Gaming Mode)"
            } else {
                "Update the mod automatically when I press Play in BonfireLink"
            };
            if ui.checkbox(&mut self.settings.auto_update_on_play, label).changed() {
                changed = true;
                for def in GAMES {
                    if let Some(loc) = self.status.get(&def.id).and_then(|s| s.loc.clone())
                        && loc.exe_dir.join(steam::WRAPPER_NAME).is_file()
                    {
                        let _ = ops::refresh_control_files(def, &loc, Some(self.settings.auto_update_on_play));
                    }
                }
            }
            ui.horizontal(|ui| {
                ui.label("Your name (shown to friends)");
                changed |= ui.add(egui::TextEdit::singleline(&mut self.settings.player_name).desired_width(180.0)).changed();
            });
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Saves & backups");
            changed |= ui.checkbox(&mut self.settings.backup_saves_before_install, "Back up saves before installing or updating").changed();
            changed |= ui.checkbox(&mut self.settings.backup_saves_before_play, "Back up saves every time I press Play").changed();
            ui.horizontal(|ui| {
                ui.label("Backups to keep per game");
                changed |= ui.add(egui::DragValue::new(&mut self.settings.keep_backups).range(1..=100)).changed();
                ui.label("Versions to keep for rollback");
                changed |= ui.add(egui::DragValue::new(&mut self.settings.keep_rollbacks).range(1..=10)).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Extra backup folder");
                ui.label(
                    RichText::new(if self.settings.backup_mirror.is_empty() {
                        "none".into()
                    } else {
                        util::redact(&self.settings.backup_mirror)
                    })
                    .color(DIM),
                );
                if ui.button("Choose…").clicked()
                    && let Some(p) =
                        rfd::FileDialog::new().set_title("Second place for save backups (SD card, USB stick, cloud folder)").pick_folder()
                {
                    self.settings.backup_mirror = p.to_string_lossy().to_string();
                    changed = true;
                }
                if !self.settings.backup_mirror.is_empty() && ui.button("Remove").clicked() {
                    self.settings.backup_mirror.clear();
                    changed = true;
                }
            });
            dim(ui, "A second copy of every backup, e.g. on the SD card, a USB stick or a folder your cloud drive syncs.");
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Nexus Mods");
            dim(
                ui,
                "Dark Souls III and Dark Souls Remastered are published on Nexus only, and new Elden Ring builds often land there first. Connect your Nexus account so BonfireLink can see and fetch them.",
            );
            self.nexus_connect_ui(ui);
            dim(ui, "The key is stored in plain text in BonfireLink-data/settings.json. Don't share that file.");
            ui.horizontal(|ui| {
                ui.label("Downloads folder");
                ui.label(
                    RichText::new(
                        ops::downloads_dir(&self.settings).map(|d| util::redact(&d.display().to_string())).unwrap_or("not found".into()),
                    )
                    .color(DIM),
                );
                if ui.button("Change…").clicked()
                    && let Some(p) = rfd::FileDialog::new().pick_folder()
                {
                    self.settings.downloads_dir = p.to_string_lossy().to_string();
                    changed = true;
                }
            });
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Games shown");
            ui.horizontal_wrapped(|ui| {
                for def in GAMES {
                    let mut shown = !self.settings.hidden_games.contains(&def.id);
                    if ui.checkbox(&mut shown, def.name).changed() {
                        if shown {
                            self.settings.hidden_games.retain(|g| *g != def.id);
                        } else {
                            self.settings.hidden_games.push(def.id);
                        }
                        changed = true;
                    }
                }
            });
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Storage");
            let data = config::data_dir();
            let dl = ops::download_cache();
            let (total, cache) = *self.storage.get_or_insert_with(|| (util::dir_size(data), util::dir_size(&dl)));
            ui.horizontal(|ui| {
                ui.label(format!("Data folder: {}", util::human_size(total)));
                if ui.button("Open").clicked() {
                    util::open_path(data);
                }
                if ui.button(format!("Clear download cache ({})", util::human_size(cache))).clicked() {
                    let _ = std::fs::remove_dir_all(&dl);
                    self.storage = None;
                    self.msg(MsgKind::Ok, "Download cache cleared. Backups and rollback versions were kept.");
                }
            });
            dim(
                ui,
                "BonfireLink is portable: everything it keeps (settings, backups, rollback copies, logs) lives in BonfireLink-data next to the app.",
            );
            if steam::is_linux()
                && ui.add_enabled(!self.busy() && !steam::in_gaming_mode(), egui::Button::new("Add BonfireLink to Steam")).clicked()
            {
                self.confirm = Some(Confirm::AddToSteam);
            }
        });
        if changed {
            self.save_settings();
        }
    }

    fn help_page(&mut self, ui: &mut Ui) {
        card(ui, |ui| {
            heading(ui, "How BonfireLink works");
            for line in [
                "1. Pick a game tab and press Install. BonfireLink downloads the official Seamless Co-op release, verifies it, backs up your saves and copies it into the game folder.",
                "2. Set a session password (Configure, or Play together > Generate) and pick a preset or tune scaling yourself.",
                "3. Play together > Copy message for friends. They paste it into their BonfireLink: same version, password and settings in one click.",
                "4. Press Play. On Windows BonfireLink starts the Seamless launcher; on Steam Deck, Steam's normal Play button does it (Gaming Mode too) and updates the mod first.",
                "5. Updates keep your settings, the replaced version is kept for rollback, and saves are backed up before every change.",
            ] {
                ui.label(line);
            }
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Steam Deck");
            for line in [
                "• Run BonfireLink in Desktop Mode the first time: it installs the mods and sets each game's Steam launch option (Steam restarts once).",
                "• Press \"Add BonfireLink to Steam\" (Home) to open it from Gaming Mode later. Steam Input's default \"Gamepad with mouse trackpad\" layout works; the touchscreen works too.",
                "• From then on everything is automatic: pressing Play in Gaming Mode runs BonfireLink's launch wrapper, which updates the mod (when a one-click update exists) and starts the Seamless launcher under Proton.",
                "• \"Mods off\" makes Steam start the normal game without touching Steam's settings.",
            ] {
                ui.label(line);
            }
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            heading(ui, "Credits & disclaimer");
            ui.label("Seamless Co-op is made by LukeYui (Yui). BonfireLink is an unofficial fan tool: it downloads the mod only from the author's official GitHub releases or from Nexus Mods, and never modifies the mod files.");
            ui.label("Support the mod author on Nexus Mods and Patreon. Elden Ring, Dark Souls and Sekiro are trademarks of their owners; this project isn't affiliated with FromSoftware, Bandai Namco or Activision.");
            ui.horizontal(|ui| {
                ui.hyperlink_to("BonfireLink on GitHub", format!("https://github.com/{}", config::APP_REPO));
                ui.hyperlink_to("Report a problem", format!("https://github.com/{}/issues", config::APP_REPO));
            });
        });
    }

    // ---------- confirmations ----------

    fn confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let (title, body, ok): (String, String, &str) = match c {
            Confirm::Uninstall(id) => (
                format!("Remove Seamless Co-op from {}?", Self::def(*id).name),
                "The mod files and the Steam launch option are removed. Your saves, backups and a copy of your settings file stay.".into(),
                "Uninstall",
            ),
            Confirm::RestoreBackup(_, p) => (
                "Restore this backup?".into(),
                format!("Your current saves are replaced by {}. They're backed up first, so this can be undone.", p.file_name().unwrap_or_default().to_string_lossy()),
                "Restore",
            ),
            Confirm::DeleteBackup(p) => ("Delete this backup?".into(), format!("{} is deleted for good.", p.file_name().unwrap_or_default().to_string_lossy()), "Delete"),
            Confirm::TransferSave(id, _, to_coop) => {
                let d = Self::def(*id);
                if *to_coop {
                    (
                        "Copy your vanilla save into co-op?".into(),
                        format!("{0}.sl2 is copied over your co-op save. The current co-op save is backed up first.", d.save_stem),
                        "Copy to co-op",
                    )
                } else {
                    (
                        "Copy your co-op save over the vanilla one?".into(),
                        format!("Your vanilla {0}.sl2 is replaced by the co-op character (backed up first). Only take a co-op character online in vanilla if you're sure it holds nothing the official game wouldn't allow.", d.save_stem),
                        "Copy to vanilla",
                    )
                }
            }
            Confirm::SteamRestart(id) => (
                "Restart Steam to finish setup?".into(),
                format!("Steam has to close for a moment so BonfireLink can add its launch wrapper to {}'s launch options. Your own launch options are kept. Steam starts again by itself.", Self::def(*id).name),
                "Restart Steam",
            ),
            Confirm::SteamRestartAll => (
                "Restart Steam to finish setup?".into(),
                "Steam closes for a moment while BonfireLink sets the launch option of every game with Seamless Co-op installed, then starts again.".into(),
                "Restart Steam",
            ),
            Confirm::AddToSteam => (
                "Add BonfireLink to your Steam library?".into(),
                "Steam closes for a moment (if it's running) and starts again with BonfireLink under Non-Steam games, so you can open it from Gaming Mode.".into(),
                "Add to Steam",
            ),
            Confirm::InstallRelease(_, r) => (
                format!("Install Seamless Co-op {}?", r.version),
                "Everyone you play with needs the same version. Your current version is kept for rollback and your settings carry over.".into(),
                "Install",
            ),
            Confirm::Rollback(_, _, v) => (format!("Go back to {v}?"), "The current version is kept for rollback too. Your settings carry over.".into(), "Roll back"),
            Confirm::ApplySession(_, s, _) => (
                format!("Use {}'s settings?", if s.n.is_empty() { "your friend" } else { &s.n }),
                "Your password and gameplay/scaling settings are replaced by theirs. Personal options (language, volume, display) stay. The old file is kept in BonfireLink-data/ini-backup.".into(),
                "Use their settings",
            ),
            Confirm::ResetDefaults(_) => ("Load the mod's default settings?".into(), "Every option goes back to what the release shipped. Your password stays. Nothing is saved until you press Save.".into(), "Load defaults"),
        };
        let mut result: Option<bool> = None;
        egui::Modal::new(egui::Id::unique("bonfirelink-confirm")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(RichText::new(title).size(18.0).strong());
            ui.add_space(4.0);
            ui.label(body);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new(ok).strong().color(Color32::from_rgb(20, 16, 12))).fill(EMBER)).clicked() {
                    result = Some(true);
                }
                if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    result = Some(false);
                }
            });
        });
        match result {
            Some(true) => {
                let c = self.confirm.take().unwrap();
                self.do_confirm(c);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

fn or_unknown(s: &str) -> &str {
    if s.is_empty() { "unknown" } else { s }
}

fn section_title(s: &str) -> String {
    match s.to_ascii_uppercase().as_str() {
        "GAMEPLAY" => "Gameplay".into(),
        "SCALING" => "Enemy & boss scaling (per extra player)".into(),
        "PASSWORD" => "Session".into(),
        "SAVE" => "Save file".into(),
        "LANGUAGE" => "Language".into(),
        "" => "General".into(),
        other => {
            let mut c = other.to_lowercase();
            if let Some(f) = c.get(..1) {
                c = f.to_uppercase() + &c[1..];
            }
            c
        }
    }
}

/// Drops the "0=FALSE 1=TRUE" style hints that the widgets already make obvious.
fn clean_help(help: &str) -> String {
    let mut out = help.to_string();
    for pat in ["0=FALSE  1=TRUE", "0=FALSE 1=TRUE", "0 = FALSE  1 = TRUE", "0 = FALSE 1 = TRUE", "1 = ON | 0 = OFF"] {
        out = out.replace(pat, "");
    }
    out.trim().to_string()
}

fn first_sentence(help: &str) -> String {
    let one = help.lines().next().unwrap_or_default();
    if one.chars().count() > 150 { format!("{}…", one.chars().take(150).collect::<String>()) } else { one.to_string() }
}

fn faq(def: &GameDef) -> Vec<(&'static str, String)> {
    let mut v = vec![
        (
            "My friend can't see or join my session",
            "Both of you need the same Seamless Co-op version, the same game patch and exactly the same password. Use Play together: paste their code and fix every red row. The host must use the mod's host item after resting at a bonfire/site of grace.".to_string(),
        ),
        (
            "The game starts without co-op",
            if steam::is_linux() {
                "Check Overview > \"Steam launch option\". If it's missing, set it (Desktop Mode, Steam restarts once). Also make sure Mods off isn't ticked.".to_string()
            } else {
                "Start through BonfireLink's Play button or the desktop shortcut. Starting from Steam's Play button runs the normal game.".to_string()
            },
        ),
        (
            "It broke after a game update",
            "Game patches can break the mod until the author updates it. Keep \"Check for updates\" on; BonfireLink shows the new version the moment it's out. Meanwhile play vanilla with Mods off.".to_string(),
        ),
        (
            "\"Password\" error or the mod refuses to connect",
            "Seamless Co-op needs a non-empty password. Set one in Configure, or Play together > Generate one.".to_string(),
        ),
        (
            "Antivirus deleted or blocked the launcher",
            format!(
                "Some antivirus programs flag {} because it injects a DLL into the game. Restore it from quarantine (or allow the game folder), then press Update/Reinstall in Versions.",
                def.launcher
            ),
        ),
    ];
    if steam::is_linux() {
        v.push((
            "Crash at start on Steam Deck",
            "In the game's Steam properties > Compatibility, try Proton Experimental or the newest Proton. Delete other DLL mods (dinput8.dll) from the game folder. Then save a bug report here.".to_string(),
        ));
    }
    if def.id == GameId::EldenRing {
        v.push((
            "Do I need to turn off Easy Anti-Cheat?",
            "No. The Seamless launcher starts the game without EAC by itself. Never put ersc.dll into the vanilla online game.".to_string(),
        ));
    }
    v
}
