//! Long-running operations. Each one loads `State` fresh, changes it and saves it, so the GUI and
//! the headless `--auto-update` run (started by Steam on the Deck) never overwrite each other.

use crate::config::{self, GameState, Settings, State, now_stamp};
use crate::games::{GameDef, GameId, SaveRoot};
use crate::ini::Ini;
use crate::remote::{self, Feed};
use crate::steam::{self, GameLocation, WRAPPER_NAME};
use crate::util;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ---------- job reporting ----------

#[derive(Default)]
pub struct JobInner {
    pub status: String,
    pub progress: Option<f32>,
    pub log: Vec<String>,
}

/// Progress sink shared between a worker thread and the GUI (or stdout in headless mode).
#[derive(Clone, Default)]
pub struct Job {
    pub inner: Arc<Mutex<JobInner>>,
    pub echo: bool,
}

impl Job {
    pub fn headless() -> Self {
        Job { echo: true, ..Default::default() }
    }
    pub fn status(&self, s: impl Into<String>) {
        let s = s.into();
        log_line(&s);
        if self.echo {
            println!("{s}");
        }
        let mut g = self.inner.lock().unwrap();
        g.log.push(s.clone());
        g.status = s;
        g.progress = None;
    }
    pub fn progress(&self, p: f32) {
        self.inner.lock().unwrap().progress = Some(p.clamp(0.0, 1.0));
    }
    pub fn note(&self, s: impl Into<String>) {
        let s = s.into();
        log_line(&s);
        if self.echo {
            println!("{s}");
        }
        self.inner.lock().unwrap().log.push(s);
    }
}

pub fn log_line(line: &str) {
    use std::io::Write;
    let path = config::sub("logs").join("bonfirelink.log");
    // Keep the log small: start over past 1 MB.
    if std::fs::metadata(&path).map(|m| m.len() > 1 << 20).unwrap_or(false) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), util::redact(line));
    }
}

/// One mutating operation at a time across processes (GUI + Steam-launched auto-update).
pub struct BusyLock(PathBuf);

impl BusyLock {
    pub fn acquire() -> Result<Self> {
        let path = config::data_dir().join(".busy");
        if let Ok(meta) = std::fs::metadata(&path) {
            let age = meta.modified().ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
            let owner = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<u32>().ok());
            // A lock left behind by a crashed run is ignored right away instead of after the timeout.
            let alive = owner.is_some_and(|pid| pid != std::process::id() && steam::pid_running(pid));
            if alive && age < 1800 {
                bail!("BonfireLink is already busy in another window (or the Steam auto-update is running). Try again in a moment.");
            }
        }
        std::fs::write(&path, std::process::id().to_string())?;
        Ok(BusyLock(path))
    }
}

impl Drop for BusyLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------- status ----------

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub loc: Option<GameLocation>,
    pub game_version: String,
    pub launcher: Option<PathBuf>,
    pub dll: Option<PathBuf>,
    pub ini: Option<PathBuf>,
    pub mod_version: String,
    /// Installed by BonfireLink (we know every file).
    pub managed: bool,
    pub mods_off: bool,
    pub password: String,
    pub save_root: Option<PathBuf>,
    pub save_slots: Vec<SaveSlot>,
    pub prefix: Option<PathBuf>,
    pub proton: Option<String>,
    pub launch_options: Vec<String>,
    pub wrapper_present: bool,
    pub game_updated_since_install: bool,
    pub conflicts: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SaveSlot {
    pub account_dir: PathBuf,
    pub vanilla: Option<(PathBuf, String)>,
    pub coop: Option<(PathBuf, String)>,
}

impl Status {
    pub fn installed(&self) -> bool {
        self.launcher.is_some() && self.dll.is_some()
    }
    pub fn launch_option_ok(&self) -> bool {
        !steam::is_linux() || (!self.launch_options.is_empty() && self.launch_options.iter().all(|o| steam::has_wrapper(o)))
    }
}

pub fn control_dir(exe_dir: &Path) -> PathBuf {
    exe_dir.join(".bonfirelink")
}

fn find_named(dir: &Path, wanted: &str, suffix: &str) -> Option<PathBuf> {
    let exact = dir.join(wanted);
    if exact.is_file() {
        return Some(exact);
    }
    // A future release might rename files; accept anything matching the pattern.
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(suffix)))
}

pub fn scan(def: &GameDef, settings: &Settings) -> Status {
    let state = State::load().game(def.id);
    let mut st = Status::default();
    let loc = steam::find_game(def, settings.game_dirs.get(&def.id).map(String::as_str));
    if let Some(loc) = &loc {
        let exe = loc.exe_dir.join(def.game_exe);
        st.game_version = util::pe_version(&exe).unwrap_or_default();
        st.launcher = find_named(&loc.exe_dir, def.launcher, "sc_launcher.exe");
        let sc = loc.exe_dir.join("SeamlessCoop");
        st.dll = find_named(&sc, def.dll, "sc.dll");
        st.ini = find_named(&sc, def.ini, "sc_settings.ini");
        st.managed = !state.files.is_empty() && st.installed();
        st.mod_version = if st.installed() {
            if !state.mod_version.is_empty() {
                state.mod_version.clone()
            } else {
                st.dll.as_deref().and_then(util::pe_version).map(|v| remote::normalize(v.trim_end_matches(".0"))).unwrap_or_default()
            }
        } else {
            String::new()
        };
        st.mods_off = control_dir(&loc.exe_dir).join("mods-off").exists();
        if let Some(ini) = &st.ini
            && let Ok(text) = std::fs::read_to_string(ini)
        {
            st.password = Ini::parse(&text).get_any("cooppassword").unwrap_or_default();
        }
        st.prefix = steam::proton_prefix(loc, def.steam_app_id);
        if steam::is_linux() {
            st.proton = steam::compat_tool(def.steam_app_id);
            st.launch_options = steam::launch_options(def.steam_app_id).into_iter().map(|(_, v)| v).collect();
            st.wrapper_present = loc.exe_dir.join(WRAPPER_NAME).is_file();
        }
        st.game_updated_since_install = st.managed
            && !state.game_build_at_install.is_empty()
            && !loc.build_id.is_empty()
            && state.game_build_at_install != loc.build_id;
        for (file, what) in [
            (
                "dinput8.dll",
                "dinput8.dll (Mod Engine / another DLL mod) is in the game folder. It can crash Seamless Co-op; remove it if the game won't start.",
            ),
            (
                "modengine2_launcher.exe",
                "Mod Engine 2 is installed. Launch co-op through the Seamless launcher, not Mod Engine, unless you set it up to load ersc.dll.",
            ),
            (
                "me3.exe",
                "me3 (mod loader) is installed. Launch co-op through the Seamless launcher unless your me3 profile loads the Seamless DLL.",
            ),
        ] {
            if loc.exe_dir.join(file).exists() {
                st.conflicts.push(what.to_string());
            }
        }
        if def.id == GameId::EldenRing && loc.exe_dir.join("start_protected_game.exe.bak").exists() {
            st.conflicts.push(
                "start_protected_game.exe was swapped by an older Steam Deck guide. BonfireLink's launch wrapper replaces that trick; \"Repair game files\" in Steam restores the original.".into(),
            );
        }
    }
    st.save_root = save_root(def, loc.as_ref());
    if let Some(root) = &st.save_root {
        let ext = coop_extension(&st);
        st.save_slots = save_slots(def, root, &ext);
    }
    st.loc = loc;
    st
}

fn coop_extension(st: &Status) -> String {
    st.ini
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| Ini::parse(&t).get_any("save_file_extension"))
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "co2".into())
        .trim()
        .trim_start_matches('.')
        .to_string()
}

pub fn save_root(def: &GameDef, loc: Option<&GameLocation>) -> Option<PathBuf> {
    let p = if steam::is_linux() {
        let pfx = steam::proton_prefix(loc?, def.steam_app_id)?;
        let user = pfx.join("drive_c/users/steamuser");
        match def.save_root {
            SaveRoot::AppData(d) => user.join("AppData/Roaming").join(d),
            SaveRoot::Documents(d) => user.join("Documents").join(d),
        }
    } else {
        match def.save_root {
            SaveRoot::AppData(d) => dirs::data_dir()?.join(d),
            SaveRoot::Documents(d) => dirs::document_dir()?.join(d),
        }
    };
    Some(p)
}

fn save_slots(def: &GameDef, root: &Path, coop_ext: &str) -> Vec<SaveSlot> {
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let describe = |p: PathBuf| -> Option<(PathBuf, String)> {
            let m = std::fs::metadata(&p).ok()?;
            let when = m
                .modified()
                .ok()
                .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            Some((p, format!("{} · {}", util::human_size(m.len()), when)))
        };
        let vanilla = describe(dir.join(format!("{}.sl2", def.save_stem)));
        let coop = describe(dir.join(format!("{}.{coop_ext}", def.save_stem)));
        if vanilla.is_some() || coop.is_some() {
            out.push(SaveSlot { account_dir: dir, vanilla, coop });
        }
    }
    out
}

// ---------- installing ----------

/// Finds the folder inside an unpacked archive that maps onto the game's exe folder.
pub fn find_mod_root(staging: &Path, def: &GameDef) -> Result<PathBuf> {
    let mut queue = vec![(staging.to_path_buf(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        let has_launcher = find_named(&dir, def.launcher, "sc_launcher.exe");
        let has_sc = dir.join("SeamlessCoop").is_dir();
        if has_launcher.is_some() || has_sc {
            if let Some(l) = &has_launcher {
                let name = l.file_name().unwrap().to_string_lossy().to_lowercase();
                if let Some(other) = crate::games::GAMES.iter().find(|g| g.id != def.id && g.launcher.eq_ignore_ascii_case(&name)) {
                    bail!("This archive is Seamless Co-op for {}, not {}.", other.name, def.name);
                }
            }
            return Ok(dir);
        }
        if depth < 3
            && let Ok(rd) = std::fs::read_dir(&dir)
        {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    queue.push((e.path(), depth + 1));
                }
            }
        }
    }
    bail!("This file doesn't look like Seamless Co-op for {} (no *_launcher.exe or SeamlessCoop folder inside).", def.name)
}

fn require_location(def: &GameDef, settings: &Settings) -> Result<GameLocation> {
    steam::find_game(def, settings.game_dirs.get(&def.id).map(String::as_str))
        .with_context(|| format!("{} wasn't found. Install it on Steam, or pick its folder on the Troubleshoot page.", def.name))
}

fn require_not_running(def: &GameDef) -> Result<()> {
    if steam::game_running(def) {
        bail!("{} is running. Quit the game first.", def.name);
    }
    Ok(())
}

/// Installs from an archive (.zip/.7z) or an already-unpacked folder (rollback).
pub fn install_from(def: &GameDef, settings: &Settings, source: &Path, version: &str, origin: &str, job: &Job) -> Result<()> {
    let _lock = BusyLock::acquire()?;
    let loc = require_location(def, settings)?;
    require_not_running(def)?;
    if settings.backup_saves_before_install
        && let Err(e) = backup_saves(def, settings, "before-install", false, job)
    {
        job.note(format!("Save backup skipped: {e:#}"));
    }

    let staging = config::sub("staging").join(def.key);
    let root = if source.is_dir() {
        source.to_path_buf()
    } else {
        job.status("Unpacking…");
        util::extract_archive(source, &staging)?;
        find_mod_root(&staging, def)?
    };

    let mut state = State::load();
    let prev = state.game(def.id);
    let before = scan(def, settings);
    let old_ini = before.ini.as_ref().and_then(|p| std::fs::read_to_string(p).ok());

    if before.installed() {
        job.status("Keeping the current version for rollback…");
        let protect = source.is_dir().then_some(source);
        if let Err(e) = snapshot_for_rollback(def, settings, &loc, &before, protect) {
            job.note(format!("Rollback copy failed (continuing): {e:#}"));
        }
    }

    job.status(format!("Installing Seamless Co-op {version} into the game folder…"));
    let copied = util::copy_dir(&root, &loc.exe_dir)
        .context("could not copy the mod into the game folder (is the game folder read-only, or the game running?)")?;
    let new_files: Vec<String> = copied.iter().map(|p| util::rel_string(p)).collect();
    // Files from the previous managed install that this release no longer ships.
    for stale in prev.files.iter().filter(|f| !new_files.contains(f)) {
        if is_user_file(stale) {
            continue;
        }
        let _ = std::fs::remove_file(loc.exe_dir.join(stale));
    }

    let after = scan(def, settings);
    let ini_path = after.ini.clone().context("the release has no settings file; it may be damaged")?;
    let pristine = std::fs::read_to_string(&ini_path).unwrap_or_default();
    if let Some(old) = old_ini {
        let mut ini = Ini::parse(&pristine);
        let (gone, added) = ini.merge_user_values(&Ini::parse(&old));
        std::fs::write(&ini_path, ini.to_text()).context("could not write the settings file")?;
        job.note("Your settings and password were kept.");
        if !added.is_empty() {
            job.note(format!("New options in this release: {}", added.join(", ")));
        }
        if !gone.is_empty() {
            job.note(format!("Options no longer used: {}", gone.join(", ")));
        }
    }

    let g = state.game_mut(def.id);
    g.mod_version = remote::normalize(version);
    g.installed_at = now_stamp();
    g.files = new_files;
    g.launcher = after.launcher.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    g.ini_rel = ini_path.strip_prefix(&loc.exe_dir).map(util::rel_string).unwrap_or_default();
    g.dll_sha256 = after.dll.as_deref().and_then(|d| util::sha256_file(d).ok()).unwrap_or_default();
    g.source = origin.to_string();
    g.pristine_ini = pristine;
    g.game_build_at_install = loc.build_id.clone();
    state.save()?;

    if steam::is_linux() {
        write_wrapper(def, &loc)?;
    }
    if !source.is_dir() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    job.status(format!("Seamless Co-op {} installed for {}.", remote::normalize(version), def.name));
    Ok(())
}

/// Files the user creates or the mod writes at runtime; never deleted as "stale".
fn is_user_file(rel: &str) -> bool {
    let r = rel.to_lowercase();
    r.ends_with("_settings.ini")
        || r.contains("crashdumps")
        || r.ends_with(".log")
        || r.contains("/locale/") && !r.ends_with("english.json")
}

fn snapshot_for_rollback(def: &GameDef, settings: &Settings, loc: &GameLocation, st: &Status, protect: Option<&Path>) -> Result<()> {
    let version = if st.mod_version.is_empty() { "unknown".to_string() } else { st.mod_version.clone() };
    let dir = config::sub("rollback").join(def.key);
    let dest = dir.join(format!("{}__{}", now_stamp(), version));
    std::fs::create_dir_all(&dest)?;
    if let Some(l) = &st.launcher {
        std::fs::copy(l, dest.join(l.file_name().unwrap()))?;
    }
    let sc = loc.exe_dir.join("SeamlessCoop");
    if sc.is_dir() {
        let target = dest.join("SeamlessCoop");
        for e in std::fs::read_dir(&sc)?.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.contains("crashdump") || name.ends_with(".log") || name.ends_with(".dmp") {
                continue;
            }
            if e.path().is_dir() {
                util::copy_dir(&e.path(), &target.join(e.file_name()))?;
            } else {
                std::fs::create_dir_all(&target)?;
                std::fs::copy(e.path(), target.join(e.file_name()))?;
            }
        }
    }
    util::prune_dir(&dir, settings.keep_rollbacks.max(1), protect);
    Ok(())
}

pub struct RollbackEntry {
    pub path: PathBuf,
    pub version: String,
    pub when: String,
}

pub fn rollbacks(def: &GameDef) -> Vec<RollbackEntry> {
    let dir = config::data_dir().join("rollback").join(def.key);
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<_> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let (when, version) = name.split_once("__").map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((name.clone(), "?".into()));
            RollbackEntry { path: e.path(), version, when: when.replacen('_', " ", 1).replace('-', ":").replacen(':', "-", 2) }
        })
        .collect();
    v.sort_by(|a, b| b.path.cmp(&a.path));
    v
}

pub fn uninstall(def: &GameDef, settings: &Settings, job: &Job) -> Result<()> {
    let _lock = BusyLock::acquire()?;
    let loc = require_location(def, settings)?;
    require_not_running(def)?;
    let st = scan(def, settings);
    let mut state = State::load();
    let g = state.game(def.id);
    if let Some(ini) = &st.ini {
        // Keep the user's settings (password!) so a reinstall restores them.
        let keep = config::sub("ini-backup").join(format!("{}-{}.ini", def.key, now_stamp()));
        let _ = std::fs::copy(ini, keep);
    }
    job.status("Removing Seamless Co-op files…");
    if let Some(l) = &st.launcher {
        std::fs::remove_file(l).with_context(|| format!("could not remove {}", l.display()))?;
    }
    for f in &g.files {
        let _ = std::fs::remove_file(loc.exe_dir.join(f));
    }
    let sc = loc.exe_dir.join("SeamlessCoop");
    if sc.is_dir() {
        // Only the mod's own folder; saves live elsewhere.
        std::fs::remove_dir_all(&sc).with_context(|| format!("could not remove {}", sc.display()))?;
    }
    let _ = std::fs::remove_file(loc.exe_dir.join(WRAPPER_NAME));
    let _ = std::fs::remove_dir_all(control_dir(&loc.exe_dir));
    if steam::is_linux() {
        match steam::edit_launch_options(def.steam_app_id, &|o| steam::without_wrapper(o)) {
            Ok(_) => {}
            Err(e) => {
                job.note(format!("Steam launch option left in place ({e:#}). It's harmless: without the mod the game starts normally."))
            }
        }
    }
    let gs = state.game_mut(def.id);
    let presets = std::mem::take(&mut gs.presets);
    *gs = GameState { presets, ..Default::default() };
    state.save()?;
    job.status(format!("Seamless Co-op removed from {}. Saves and backups were not touched.", def.name));
    Ok(())
}

/// Adopts a mod installed by hand (or by another tool) so updates can manage it.
pub fn adopt_existing(def: &GameDef, settings: &Settings) -> Result<()> {
    let st = scan(def, settings);
    let loc = st.loc.as_ref().context("game not found")?;
    if !st.installed() {
        return Ok(());
    }
    let mut state = State::load();
    let g = state.game_mut(def.id);
    if !g.files.is_empty() {
        return Ok(());
    }
    let mut files = Vec::new();
    if let Some(l) = &st.launcher {
        files.push(l.file_name().unwrap().to_string_lossy().to_string());
    }
    let sc = loc.exe_dir.join("SeamlessCoop");
    let mut stack = vec![sc.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else if let Ok(rel) = e.path().strip_prefix(&loc.exe_dir) {
                let rel = util::rel_string(rel);
                if !rel.to_lowercase().contains("crashdumps/") {
                    files.push(rel);
                }
            }
        }
    }
    g.files = files;
    g.mod_version = st.mod_version.clone();
    g.source = "found in game folder".into();
    g.game_build_at_install = loc.build_id.clone();
    g.dll_sha256 = st.dll.as_deref().and_then(|d| util::sha256_file(d).ok()).unwrap_or_default();
    g.launcher = st.launcher.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    g.ini_rel = st.ini.as_ref().and_then(|p| p.strip_prefix(&loc.exe_dir).ok()).map(util::rel_string).unwrap_or_default();
    // pristine_ini stays empty: the file on disk holds the user's edits, not the defaults.
    // identify_version() fills it from the official archive.
    state.save()
}

/// Works out which release a hand-installed mod is by hashing its DLL against official archives
/// (downloaded into the cache, which an update would need anyway). Records the version when found.
pub fn identify_version(def: &GameDef, settings: &Settings, feed: &Feed) -> Result<Option<String>> {
    let st = scan(def, settings);
    let Some(dll) = st.dll.as_ref().filter(|_| st.mod_version.is_empty()) else { return Ok(None) };
    let want = util::sha256_file(dll)?;
    let dll_name = dll.file_name().unwrap().to_string_lossy().to_lowercase();
    for r in feed.releases.iter().take(3) {
        if !r.asset_name.to_lowercase().ends_with(".zip") {
            continue;
        }
        let dest = download_cache().join(format!("{}-{}-{}", def.key, r.version, sanitize_name(&r.asset_name)));
        if !dest.is_file() {
            remote::download(&r.asset_url, &dest, &|_, _| {})?;
        }
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&dest)?)?;
        let mut pristine = String::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.name()?.to_lowercase().ends_with("_settings.ini") {
                std::io::Read::read_to_string(&mut entry, &mut pristine)?;
            }
        }
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if !entry.name()?.to_lowercase().ends_with(&format!("/{dll_name}")) {
                continue;
            }
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data)?;
            if util::sha256_bytes(&data) == want {
                let mut state = State::load();
                let g = state.game_mut(def.id);
                g.mod_version = r.version.clone();
                g.dll_sha256 = want;
                if g.pristine_ini.is_empty() {
                    g.pristine_ini = pristine;
                }
                if g.source.is_empty() || g.source == "found in game folder" {
                    g.source = format!("found in game folder (matches GitHub {})", r.tag);
                }
                state.save()?;
                return Ok(Some(r.version.clone()));
            }
        }
    }
    Ok(None)
}

pub fn set_mods_off(def: &GameDef, settings: &Settings, off: bool) -> Result<()> {
    let loc = require_location(def, settings)?;
    let dir = control_dir(&loc.exe_dir);
    std::fs::create_dir_all(&dir)?;
    let flag = dir.join("mods-off");
    if off {
        std::fs::write(&flag, b"1")?
    } else {
        let _ = std::fs::remove_file(&flag);
    }
    Ok(())
}

// ---------- downloads ----------

pub fn download_cache() -> PathBuf {
    config::sub("downloads")
}

/// Downloads the best build for someone on `installed` ("" = fresh install): the newest version from
/// GitHub, or from Nexus with a Premium key, or (when the newest is Nexus-only) the newest GitHub build
/// if that's still an upgrade. Returns (archive, version, origin).
pub fn fetch_latest(def: &GameDef, settings: &Settings, feed: &Feed, installed: &str, job: &Job) -> Result<(PathBuf, String, String)> {
    let latest = feed.latest_version().context("No release found. Check your internet connection.")?;
    if let Some(r) = feed.latest_on_github() {
        return fetch_github(def, r, job);
    }
    if let (Some((domain, id)), Some(nf)) = (def.nexus, feed.nexus.as_ref())
        && !settings.nexus_api_key.trim().is_empty()
        && remote::same_version(&nf.version, &latest)
    {
        job.status(format!("Asking Nexus for a download link for {}…", nf.version));
        let url = remote::nexus_download_url(domain, id, nf.file_id, &settings.nexus_api_key)?;
        let dest = download_cache().join(format!("{}-{}-{}", def.key, nf.version, sanitize_name(&nf.file_name)));
        if !dest.is_file() {
            job.status(format!("Downloading Seamless Co-op {} from Nexus…", nf.version));
            let j = job.clone();
            remote::download(&url, &dest, &move |done, total| {
                if let Some(t) = total.filter(|t| *t > 0) {
                    j.progress(done as f32 / t as f32);
                }
            })?;
        }
        prune_downloads(def, 3);
        return Ok((dest, nf.version.clone(), format!("Nexus file {}", nf.file_id)));
    }
    if let Some(r) = feed.one_click(installed) {
        job.note(format!("Version {latest} is on Nexus Mods only so far; installing {} (the newest on GitHub) meanwhile.", r.version));
        return fetch_github(def, r, job);
    }
    bail!(
        "Version {latest} is only on Nexus Mods. Press \"Get it from Nexus\": BonfireLink installs it as soon as the download finishes (or add a Nexus Premium API key in Settings for one-click updates)."
    )
}

fn fetch_github(def: &GameDef, r: &remote::Release, job: &Job) -> Result<(PathBuf, String, String)> {
    let dest = download_cache().join(format!("{}-{}-{}", def.key, r.version, sanitize_name(&r.asset_name)));
    if !dest.is_file() || (!r.sha256.is_empty() && util::sha256_file(&dest).ok().as_deref() != Some(r.sha256.as_str())) {
        job.status(format!("Downloading Seamless Co-op {} from GitHub…", r.version));
        let j = job.clone();
        remote::download(&r.asset_url, &dest, &move |done, total| {
            if let Some(t) = total.filter(|t| *t > 0) {
                j.progress(done as f32 / t as f32);
            }
        })?;
    }
    if !r.sha256.is_empty() {
        let got = util::sha256_file(&dest)?;
        if got != r.sha256 {
            let _ = std::fs::remove_file(&dest);
            bail!("The download is corrupted (checksum mismatch). Try again.");
        }
        job.note("Checksum verified.");
    }
    prune_downloads(def, 3);
    Ok((dest, r.version.clone(), format!("GitHub {}", r.tag)))
}

fn sanitize_name(n: &str) -> String {
    n.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect()
}

fn prune_downloads(def: &GameDef, keep: usize) {
    let dir = download_cache();
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut mine: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{}-", def.key)))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    mine.sort();
    while mine.len() > keep {
        let _ = std::fs::remove_file(mine.remove(0).1);
    }
}

pub fn update(def: &GameDef, settings: &Settings, job: &Job) -> Result<bool> {
    job.status("Checking for updates…");
    let feed = remote::fetch_feed(def, &settings.nexus_api_key);
    let st = scan(def, settings);
    let latest = feed.latest_version().context(feed.errors.first().cloned().unwrap_or_else(|| "No release found.".into()))?;
    if st.installed() && remote::same_version(&st.mod_version, &latest) {
        job.status(format!("Seamless Co-op {latest} is already the newest version."));
        return Ok(false);
    }
    let installed = if st.installed() { st.mod_version.as_str() } else { "" };
    let (archive, version, origin) = fetch_latest(def, settings, &feed, installed, job)?;
    install_from(def, settings, &archive, &version, &origin, job)?;
    Ok(true)
}

/// Archives that showed up in Downloads after `since` and look like this game's mod.
pub fn find_new_download(def: &GameDef, settings: &Settings, since: std::time::SystemTime) -> Option<PathBuf> {
    let dir = downloads_dir(settings)?;
    let id = def.nexus.map(|(_, id)| id).unwrap_or(0);
    let rd = std::fs::read_dir(dir).ok()?;
    let mut hits: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if !(name.ends_with(".zip") || name.ends_with(".7z")) {
                return None;
            }
            let looks_right = (id != 0 && name.contains(&format!("-{id}-"))) || name.contains("seamless");
            if !looks_right {
                return None;
            }
            let m = e.metadata().ok()?;
            let t = m.modified().ok()?;
            // Browsers write to a temp name and rename at the end, so a final name means the file is complete.
            (t > since && m.len() > 0).then(|| (t, e.path()))
        })
        .collect();
    hits.sort();
    hits.pop().map(|(_, p)| p)
}

pub fn downloads_dir(settings: &Settings) -> Option<PathBuf> {
    if !settings.downloads_dir.trim().is_empty() {
        return Some(PathBuf::from(settings.downloads_dir.trim()));
    }
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
}

/// Version from a Nexus/GitHub archive name, e.g. "Seamless Co-op-1895-1-1-0-1712.zip" > "1.1.0".
pub fn version_from_filename(name: &str, def: &GameDef) -> String {
    let n = name.to_lowercase();
    for marker in [".v", " v", "-v", "_v"] {
        let Some(pos) = n.match_indices(marker).map(|(i, _)| i).find(|i| n[i + 2..].starts_with(|c: char| c.is_ascii_digit())) else {
            continue;
        };
        let v: String = n[pos + 2..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        let parts: Vec<&str> = v.split('.').filter(|p| !p.is_empty()).take(3).collect();
        if !parts.is_empty() {
            return remote::normalize(&parts.join("."));
        }
    }
    if let Some((_, id)) = def.nexus {
        let marker = format!("-{id}-");
        if let Some(pos) = n.find(&marker) {
            let rest = &n[pos + marker.len()..];
            let parts: Vec<&str> = rest.split('-').take_while(|p| p.chars().all(|c| c.is_ascii_digit()) && p.len() < 6).collect();
            if parts.len() >= 2 {
                return parts.join(".");
            }
        }
    }
    String::new()
}

// ---------- Steam Deck / Linux launch wrapper ----------

pub fn wrapper_script(def: &GameDef) -> String {
    format!(
        r#"#!/bin/sh
# BonfireLink launch wrapper for {name}.
# Steam runs this when you press Play. It starts the Seamless Co-op launcher instead of the game,
# unless mods are switched off in BonfireLink. Remove it with BonfireLink > Uninstall.
here="$(cd "$(dirname "$0")" && pwd)"
ctl="$here/.bonfirelink"
launcher="{launcher}"
target="{target}"
if [ -f "$ctl/auto-update" ] && [ -f "$ctl/helper-path" ]; then
  helper="$(cat "$ctl/helper-path")"
  if [ -x "$helper" ]; then
    timeout 90 "$helper" --auto-update {key} >"$ctl/last-auto-update.log" 2>&1
  fi
fi
[ -f "$ctl/launcher-name" ] && launcher="$(cat "$ctl/launcher-name")"
if [ -f "$ctl/mods-off" ] || [ ! -f "$here/$launcher" ]; then
  exec "$@"
fi
for arg; do
  shift
  case "$arg" in
    *"/$target"|*"\\$target") arg="${{arg%"$target"}}$launcher" ;;
  esac
  set -- "$@" "$arg"
done
exec "$@"
"#,
        name = def.name,
        launcher = def.launcher,
        target = def.steam_exe,
        key = def.key,
    )
}

pub fn write_wrapper(def: &GameDef, loc: &GameLocation) -> Result<PathBuf> {
    let path = loc.exe_dir.join(WRAPPER_NAME);
    std::fs::write(&path, wrapper_script(def)).with_context(|| format!("could not write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    refresh_control_files(def, loc, None)?;
    Ok(path)
}

/// Keeps the files the wrapper reads in sync with the helper's location and settings.
pub fn refresh_control_files(def: &GameDef, loc: &GameLocation, auto_update: Option<bool>) -> Result<()> {
    let dir = control_dir(&loc.exe_dir);
    std::fs::create_dir_all(&dir)?;
    if let Ok(exe) = std::env::current_exe() {
        std::fs::write(dir.join("helper-path"), exe.to_string_lossy().as_bytes())?;
    }
    let auto = auto_update.unwrap_or_else(|| Settings::load().auto_update_on_play);
    let flag = dir.join("auto-update");
    if auto {
        std::fs::write(&flag, b"1")?
    } else {
        let _ = std::fs::remove_file(&flag);
    }
    if let Some(l) = find_named(&loc.exe_dir, def.launcher, "sc_launcher.exe") {
        std::fs::write(dir.join("launcher-name"), l.file_name().unwrap().to_string_lossy().as_bytes())?;
    }
    Ok(())
}

pub enum LaunchOptionOutcome {
    AlreadySet,
    Set,
    /// Steam is running and we may restart it (Desktop Mode).
    NeedsSteamRestart,
    /// Gaming Mode: the user has to switch to Desktop Mode once.
    NeedsDesktopMode,
}

pub fn ensure_launch_option(def: &GameDef, settings: &Settings, restart_steam: bool, job: &Job) -> Result<LaunchOptionOutcome> {
    let loc = require_location(def, settings)?;
    let wrapper = write_wrapper(def, &loc)?;
    let current = steam::launch_options(def.steam_app_id);
    if current.is_empty() {
        bail!("No Steam account found on this device (log in to Steam once).");
    }
    if current.iter().all(|(_, o)| steam::with_wrapper(o, &wrapper) == *o) {
        return Ok(LaunchOptionOutcome::AlreadySet);
    }
    let was_running = steam::is_steam_running();
    if was_running {
        if steam::in_gaming_mode() {
            return Ok(LaunchOptionOutcome::NeedsDesktopMode);
        }
        if !restart_steam {
            return Ok(LaunchOptionOutcome::NeedsSteamRestart);
        }
        job.status("Closing Steam to set the launch option…");
        steam::shutdown_steam()?;
    }
    let n = steam::edit_launch_options(def.steam_app_id, &|o| steam::with_wrapper(o, &wrapper))?;
    let mut state = State::load();
    state.game_mut(def.id).launch_option_set = true;
    state.save()?;
    job.note(format!("Launch option set for {} ({n} Steam account(s)).", def.name));
    if was_running {
        job.status("Starting Steam again…");
        steam::start_steam();
    }
    Ok(LaunchOptionOutcome::Set)
}

/// Sets the launch option for several games with a single Steam restart.
pub fn ensure_launch_options_all(defs: &[&GameDef], settings: &Settings, job: &Job) -> Result<usize> {
    let mut todo = Vec::new();
    for def in defs {
        let Ok(loc) = require_location(def, settings) else { continue };
        let wrapper = write_wrapper(def, &loc)?;
        let current = steam::launch_options(def.steam_app_id);
        if current.iter().any(|(_, o)| steam::with_wrapper(o, &wrapper) != *o) {
            todo.push((*def, wrapper));
        }
    }
    if todo.is_empty() {
        return Ok(0);
    }
    let was_running = steam::is_steam_running();
    if was_running {
        if steam::in_gaming_mode() {
            bail!("Steam can't be restarted from Gaming Mode. Open BonfireLink once in Desktop Mode to finish setup.");
        }
        job.status("Closing Steam to set launch options…");
        steam::shutdown_steam()?;
    }
    let mut state = State::load();
    for (def, wrapper) in &todo {
        steam::edit_launch_options(def.steam_app_id, &|o| steam::with_wrapper(o, wrapper))?;
        state.game_mut(def.id).launch_option_set = true;
        job.note(format!("Launch option set for {}.", def.name));
    }
    state.save()?;
    if was_running {
        job.status("Starting Steam again…");
        steam::start_steam();
    }
    Ok(todo.len())
}

// ---------- playing ----------

pub fn launch(def: &GameDef, settings: &Settings, job: &Job) -> Result<()> {
    let st = scan(def, settings);
    let loc = st.loc.clone().context("game not found")?;
    if st.mods_off {
        bail!("Mods are switched off for {}. Turn them on (or start the game from Steam to play vanilla).", def.name);
    }
    if !st.installed() {
        bail!("Seamless Co-op isn't installed for {} yet.", def.name);
    }
    if st.password.trim().is_empty() {
        bail!("Set a session password first (Settings tab, or Play together > Generate). Seamless Co-op refuses to connect without one.");
    }
    if steam::game_running(def) {
        bail!("{} is already running.", def.name);
    }
    if settings.backup_saves_before_play
        && let Err(e) = backup_saves(def, settings, "before-play", false, job)
    {
        job.note(format!("Save backup skipped: {e:#}"));
    }
    if steam::is_linux() {
        let res = ensure_launch_option(def, settings, false, job)?;
        match res {
            LaunchOptionOutcome::AlreadySet | LaunchOptionOutcome::Set => {}
            LaunchOptionOutcome::NeedsSteamRestart => bail!("Finish setup first: press \"Set Steam launch option\" (Steam restarts once)."),
            LaunchOptionOutcome::NeedsDesktopMode => {
                bail!("The Steam launch option isn't set yet. Open BonfireLink once in Desktop Mode to finish setup.")
            }
        }
        job.status(format!("Asking Steam to start {} with Seamless Co-op…", def.name));
        let _ = loc;
        steam::run_game_via_steam(def.steam_app_id);
        return Ok(());
    }
    if !steam::is_steam_running() {
        job.status("Starting Steam first…");
        steam::start_steam();
        let start = std::time::Instant::now();
        while !steam::is_steam_running() && start.elapsed().as_secs() < 30 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    let launcher = st.launcher.context("launcher missing")?;
    job.status(format!("Starting {} with Seamless Co-op…", def.name));
    std::process::Command::new(&launcher)
        .current_dir(&loc.exe_dir)
        .spawn()
        .with_context(|| format!("could not start {}", launcher.display()))?;
    Ok(())
}

// ---------- saves ----------

fn backups_dir(def: &GameDef) -> PathBuf {
    config::sub("backups").join(def.key)
}

pub struct BackupEntry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
}

pub fn list_backups(def: &GameDef) -> Vec<BackupEntry> {
    let Ok(rd) = std::fs::read_dir(backups_dir(def)) else { return Vec::new() };
    let mut v: Vec<_> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".zip"))
        .map(|e| BackupEntry {
            name: e.file_name().to_string_lossy().trim_end_matches(".zip").to_string(),
            size: e.metadata().map(|m| m.len()).unwrap_or(0),
            path: e.path(),
        })
        .collect();
    v.sort_by(|a, b| b.name.cmp(&a.name));
    v
}

/// Zips the game's whole save folder. Skips (returns Ok(None)) if nothing changed since the last automatic backup.
pub fn backup_saves(def: &GameDef, settings: &Settings, reason: &str, force: bool, job: &Job) -> Result<Option<PathBuf>> {
    let loc = steam::find_game(def, settings.game_dirs.get(&def.id).map(String::as_str));
    let root = save_root(def, loc.as_ref()).context("save folder unknown")?;
    if !root.is_dir() {
        bail!("No saves yet (the game has never saved on this device).");
    }
    let fp = util::fingerprint(&root);
    let mut state = State::load();
    if !force && state.game(def.id).last_backup_fingerprint == fp {
        job.note("Saves unchanged since the last backup; no new copy needed.");
        return Ok(None);
    }
    job.status("Backing up saves…");
    let dir = backups_dir(def);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(format!("{}-{}.zip", now_stamp(), reason));
    let n = util::zip_dir(&root, &dest, &|_| false)?;
    if n == 0 {
        let _ = std::fs::remove_file(&dest);
        bail!("The save folder is empty.");
    }
    if !settings.backup_mirror.trim().is_empty() {
        let mirror = PathBuf::from(settings.backup_mirror.trim()).join(config::APP_NAME).join(def.key);
        match std::fs::create_dir_all(&mirror).and_then(|_| std::fs::copy(&dest, mirror.join(dest.file_name().unwrap()))) {
            Ok(_) => util::prune_dir(&mirror, settings.keep_backups.max(1), None),
            Err(e) => job.note(format!("Could not copy the backup to the extra folder: {e}")),
        }
    }
    util::prune_dir(&dir, settings.keep_backups.max(1), Some(&dest));
    let g = state.game_mut(def.id);
    g.last_backup_fingerprint = fp;
    g.last_save_backup = now_stamp();
    state.save()?;
    job.status(format!("Saves backed up ({n} files)."));
    Ok(Some(dest))
}

pub fn restore_saves(def: &GameDef, settings: &Settings, backup: &Path, job: &Job) -> Result<()> {
    let _lock = BusyLock::acquire()?;
    require_not_running(def)?;
    let loc = steam::find_game(def, settings.game_dirs.get(&def.id).map(String::as_str));
    let root = save_root(def, loc.as_ref()).context("save folder unknown")?;
    // The current state is backed up first so a restore can always be undone.
    if root.is_dir() {
        let _ = backup_saves(def, settings, "before-restore", true, job);
    }
    job.status("Restoring saves…");
    let tmp = root.with_file_name(format!("{}.bonfirelink-restore", root.file_name().unwrap().to_string_lossy()));
    util::extract_archive(backup, &tmp)?;
    let old = root.with_file_name(format!("{}.bonfirelink-old", root.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&old);
    if root.is_dir() {
        util::move_path(&root, &old)?;
    }
    if let Err(e) = util::move_path(&tmp, &root) {
        // Put the original back.
        let _ = util::move_path(&old, &root);
        return Err(e).context("could not put the restored saves in place");
    }
    let _ = std::fs::remove_dir_all(&old);
    job.status("Saves restored.");
    Ok(())
}

/// Copies a vanilla save to the co-op slot (`to_coop`) or back. The overwritten file is backed up first.
pub fn transfer_save(def: &GameDef, settings: &Settings, slot: &SaveSlot, to_coop: bool, job: &Job) -> Result<()> {
    require_not_running(def)?;
    let st = scan(def, settings);
    let ext = coop_extension(&st);
    let vanilla = slot.account_dir.join(format!("{}.sl2", def.save_stem));
    let coop = slot.account_dir.join(format!("{}.{ext}", def.save_stem));
    let (from, to) = if to_coop { (vanilla, coop) } else { (coop, vanilla) };
    if !from.is_file() {
        bail!("{} doesn't exist.", from.display());
    }
    backup_saves(def, settings, if to_coop { "before-copy-to-coop" } else { "before-copy-to-vanilla" }, true, job)?;
    std::fs::copy(&from, &to).with_context(|| format!("could not write {}", to.display()))?;
    job.status(format!("Copied {} > {}.", from.file_name().unwrap().to_string_lossy(), to.file_name().unwrap().to_string_lossy()));
    Ok(())
}

// ---------- settings file ----------

pub fn read_ini(st: &Status) -> Result<(PathBuf, String)> {
    let path = st.ini.clone().context("No settings file yet; install Seamless Co-op first.")?;
    let text = std::fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    Ok((path, text))
}

pub fn write_ini(path: &Path, text: &str) -> Result<()> {
    let backup = config::sub("ini-backup");
    if let Ok(old) = std::fs::read_to_string(path)
        && old != text
    {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let _ = std::fs::write(backup.join(format!("{name}-{}.ini", now_stamp())), old);
        util::prune_dir(&backup, 30, None);
    }
    std::fs::write(path, text).with_context(|| format!("could not save {}", path.display()))
}

pub fn languages(st: &Status) -> Vec<String> {
    let Some(ini) = &st.ini else { return Vec::new() };
    let dir = ini.parent().unwrap().join("locale");
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".json").map(str::to_string))
        .collect();
    v.sort();
    v
}

// ---------- shortcuts ----------

#[cfg(windows)]
pub fn create_desktop_shortcut(def: &GameDef, settings: &Settings) -> Result<PathBuf> {
    let st = scan(def, settings);
    let loc = st.loc.context("game not found")?;
    let launcher = st.launcher.context("Install Seamless Co-op first.")?;
    let desktop = dirs::desktop_dir().context("Desktop folder not found")?;
    let lnk = desktop.join(format!("{} Seamless Co-op.lnk", def.name.replace(':', "")));
    let icon = loc.exe_dir.join(def.game_exe);
    let q = |p: &Path| p.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}');$s.TargetPath='{}';$s.WorkingDirectory='{}';$s.IconLocation='{},0';$s.Description='{} with Seamless Co-op (BonfireLink)';$s.Save()",
        q(&lnk),
        q(&launcher),
        q(&loc.exe_dir),
        q(&icon),
        def.name.replace('\'', "")
    );
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000)
        .status()
        .context("could not run PowerShell")?;
    if !status.success() {
        bail!("Windows refused to create the shortcut");
    }
    Ok(lnk)
}

#[cfg(not(windows))]
pub fn create_desktop_shortcut(_def: &GameDef, _settings: &Settings) -> Result<PathBuf> {
    bail!("On Steam Deck / Linux, Steam's own Play button starts co-op once the launch option is set.")
}

/// Adds BonfireLink itself to Steam as a non-Steam game (so it opens from Gaming Mode).
pub fn add_helper_to_steam(job: &Job) -> Result<usize> {
    if steam::is_steam_running() {
        bail!("Steam is running; it has to be closed to add a shortcut");
    }
    let exe = std::env::current_exe()?;
    let exe_s = exe.to_string_lossy().to_string();
    let quoted = format!("\"{exe_s}\"");
    let start_dir = format!("\"{}\"", exe.parent().unwrap().to_string_lossy());
    let mut touched = 0;
    for root in steam::steam_roots() {
        let Ok(rd) = std::fs::read_dir(root.join("userdata")) else { continue };
        for user in rd.flatten() {
            let cfg = user.path().join("config");
            if !cfg.is_dir() {
                continue;
            }
            let path = cfg.join("shortcuts.vdf");
            let mut map = if path.is_file() {
                crate::vdf::parse_binary(&std::fs::read(&path)?)?
            } else {
                vec![("shortcuts".to_string(), crate::vdf::BinValue::Map(Vec::new()))]
            };
            use crate::vdf::BinValue;
            let Some((_, BinValue::Map(list))) = map.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("shortcuts")) else { continue };
            let exists = list.iter().any(|(_, e)| match e {
                BinValue::Map(f) => {
                    f.iter().any(|(k, v)| k.eq_ignore_ascii_case("AppName") && matches!(v, BinValue::Str(s) if s == config::APP_NAME))
                }
                _ => false,
            });
            if exists {
                continue;
            }
            let app_id = crate::vdf::crc32(format!("{quoted}{}", config::APP_NAME).as_bytes()) | 0x8000_0000;
            let idx = list.len().to_string();
            let s = |v: &str| BinValue::Str(v.to_string());
            list.push((
                idx,
                BinValue::Map(vec![
                    ("appid".into(), BinValue::Int(app_id)),
                    ("AppName".into(), s(config::APP_NAME)),
                    ("Exe".into(), s(&quoted)),
                    ("StartDir".into(), s(&start_dir)),
                    ("icon".into(), s("")),
                    ("ShortcutPath".into(), s("")),
                    ("LaunchOptions".into(), s("")),
                    ("IsHidden".into(), BinValue::Int(0)),
                    ("AllowDesktopConfig".into(), BinValue::Int(1)),
                    ("AllowOverlay".into(), BinValue::Int(1)),
                    ("OpenVR".into(), BinValue::Int(0)),
                    ("Devkit".into(), BinValue::Int(0)),
                    ("DevkitGameID".into(), s("")),
                    ("DevkitOverrideAppID".into(), BinValue::Int(0)),
                    ("LastPlayTime".into(), BinValue::Int(0)),
                    ("FlatpakAppID".into(), s("")),
                    ("tags".into(), BinValue::Map(Vec::new())),
                ]),
            ));
            if path.is_file() {
                let _ = std::fs::copy(&path, config::sub("backups/steam-shortcuts").join(format!("{}-shortcuts.vdf", now_stamp())));
            }
            std::fs::write(&path, crate::vdf::to_binary(&map))?;
            touched += 1;
            job.note(format!("Added to Steam account {}.", user.file_name().to_string_lossy()));
        }
    }
    Ok(touched)
}

// ---------- reports ----------

pub fn diagnostics(def: &GameDef, settings: &Settings) -> String {
    let st = scan(def, settings);
    let mut out = String::new();
    let mut line = |k: &str, v: String| out.push_str(&format!("{k:<22} {v}\n"));
    line("BonfireLink", config::VERSION.into());
    line("OS", format!("{} {}", std::env::consts::OS, if steam::is_steam_deck() { "(Steam Deck)" } else { "" }));
    line("Gaming Mode", steam::in_gaming_mode().to_string());
    line("Game", def.name.into());
    line("Install folder", st.loc.as_ref().map(|l| l.install_dir.display().to_string()).unwrap_or("NOT FOUND".into()));
    line("Mod folder", st.loc.as_ref().map(|l| l.exe_dir.display().to_string()).unwrap_or_default());
    line("Steam build", st.loc.as_ref().map(|l| l.build_id.clone()).unwrap_or_default());
    line("Game exe version", st.game_version.clone());
    line("Mod installed", st.installed().to_string());
    line("Mod version", st.mod_version.clone());
    line("Managed by helper", st.managed.to_string());
    line("Mods off", st.mods_off.to_string());
    line("Password set", (!st.password.trim().is_empty()).to_string());
    line("Save folder", st.save_root.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
    line("Save slots", st.save_slots.len().to_string());
    if steam::is_linux() {
        line("Proton prefix", st.prefix.as_ref().map(|p| p.display().to_string()).unwrap_or("none yet (start the game once)".into()));
        line("Proton version", st.proton.clone().unwrap_or("Steam default".into()));
        line("Launch wrapper", st.wrapper_present.to_string());
        for o in &st.launch_options {
            line("Launch options", o.clone());
        }
    }
    for c in &st.conflicts {
        line("Warning", c.clone());
    }
    util::redact(&out)
}

pub fn bug_report(def: &GameDef, settings: &Settings, job: &Job) -> Result<PathBuf> {
    job.status("Collecting a bug report…");
    let st = scan(def, settings);
    let dir = config::sub("reports");
    let dest = dir.join(format!("bonfirelink-{}-{}.zip", def.key, now_stamp()));
    let tmp = config::sub("staging").join("report");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("diagnostics.txt"), diagnostics(def, settings))?;
    if let Ok((_, text)) = read_ini(&st) {
        let mut ini = Ini::parse(&text);
        ini.set_id("cooppassword", "(hidden)");
        std::fs::write(tmp.join("settings.ini"), ini.to_text())?;
    }
    let log = config::data_dir().join("logs/bonfirelink.log");
    if let Ok(t) = std::fs::read_to_string(&log) {
        let tail: Vec<&str> = t.lines().rev().take(400).collect();
        std::fs::write(tmp.join("bonfirelink.log"), tail.into_iter().rev().collect::<Vec<_>>().join("\n"))?;
    }
    if let Some(loc) = &st.loc {
        let auto = control_dir(&loc.exe_dir).join("last-auto-update.log");
        if auto.is_file() {
            let _ = std::fs::copy(auto, tmp.join("last-auto-update.log"));
        }
        // Small text logs the mod writes (crash dumps are large and personal; skipped).
        for e in std::fs::read_dir(loc.exe_dir.join("SeamlessCoop")).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if (name.ends_with(".log") || name.ends_with(".txt")) && e.metadata().map(|m| m.len() < 2 << 20).unwrap_or(false) {
                let _ = std::fs::copy(e.path(), tmp.join(e.file_name()));
            }
        }
    }
    util::zip_dir(&tmp, &dest, &|_| false)?;
    let _ = std::fs::remove_dir_all(&tmp);
    util::prune_dir(&dir, 5, Some(&dest));
    job.status("Bug report saved.");
    Ok(dest)
}

// ---------- headless auto-update (run by the Steam launch wrapper) ----------

pub fn auto_update_headless(key: &str) -> Result<()> {
    let def = crate::games::by_key(key).with_context(|| format!("unknown game '{key}'"))?;
    let settings = Settings::load();
    let job = Job::headless();
    if !settings.auto_update_on_play {
        job.note("Auto-update is off.");
        return Ok(());
    }
    let st = scan(def, &settings);
    if st.mods_off || !st.installed() {
        job.note("Mod not active; nothing to update.");
        return Ok(());
    }
    let feed = remote::fetch_feed(def, &settings.nexus_api_key);
    let Some(latest) = feed.latest_version() else {
        job.note("Offline or no release info; starting the installed version.");
        return Ok(());
    };
    if remote::same_version(&st.mod_version, &latest) || !remote::newer(&latest, &st.mod_version) && !st.mod_version.is_empty() {
        job.note(format!("Up to date ({}).", st.mod_version));
        return Ok(());
    }
    // The wrapper runs before the game starts, so the game isn't running yet; skip the process check's false positives.
    match fetch_latest(def, &settings, &feed, &st.mod_version, &job) {
        Ok((archive, version, origin)) => {
            install_from(def, &settings, &archive, &version, &origin, &job)?;
            job.note(format!("Updated {} to {version} before launch.", def.name));
        }
        Err(e) => job.note(format!("Update {latest} available but not installed automatically: {e:#}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::{GAMES, get};

    #[test]
    fn filename_versions() {
        let ds3 = get(GameId::DarkSouls3);
        assert_eq!(version_from_filename("Seamless Co-op-1895-1-1-0-1712345678.zip", ds3), "1.1.0");
        let er = get(GameId::EldenRing);
        assert_eq!(version_from_filename("Seamless.Co-op.v2.0.1.zip", er), "2.0.1");
        let ds2 = get(GameId::DarkSouls2);
        assert_eq!(
            version_from_filename("Dark.Souls.II.SoTFS.-.Seamless.Co-op.v0.0.6.1468.0.0.6.2026-10-26T15-00Z.823VAhJ2.zip", ds2),
            "0.0.6"
        );
    }

    #[test]
    fn wrapper_mentions_the_right_files() {
        for def in GAMES {
            let s = wrapper_script(def);
            assert!(s.contains(def.launcher));
            assert!(s.contains(def.steam_exe));
            assert!(s.contains(&format!("--auto-update {}", def.key)));
            assert!(s.starts_with("#!/bin/sh\n"));
        }
    }

    /// Full install > update > uninstall cycle against a fake game folder.
    #[test]
    fn install_update_uninstall_cycle() {
        let base = std::env::temp_dir().join(format!("bl-ops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // SAFETY: tests in this module run in one process; the data dir is set before first use.
        unsafe { std::env::set_var("BONFIRELINK_DATA", base.join("data")) };
        let def = get(GameId::EldenRing);
        let game = base.join("ELDEN RING/Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::write(game.join("eldenring.exe"), b"MZ").unwrap();
        let mut settings = Settings { backup_saves_before_install: false, ..Default::default() };
        settings.game_dirs.insert(def.id, base.join("ELDEN RING").to_string_lossy().to_string());

        let make_release = |dir: &Path, ini: &str, extra: bool| {
            std::fs::create_dir_all(dir.join("SeamlessCoop/locale")).unwrap();
            std::fs::write(dir.join("ersc_launcher.exe"), b"launcher").unwrap();
            std::fs::write(dir.join("SeamlessCoop/ersc.dll"), b"dll").unwrap();
            std::fs::write(dir.join("SeamlessCoop/ersc_settings.ini"), ini).unwrap();
            std::fs::write(dir.join("SeamlessCoop/locale/english.json"), b"{}").unwrap();
            if extra {
                std::fs::write(dir.join("SeamlessCoop/old_only.dat"), b"x").unwrap();
            }
        };
        let r1 = base.join("rel1/Seamless Co-op");
        make_release(&r1, "[PASSWORD]\ncooppassword = \n[SCALING]\nboss_health_scaling = 100\n", true);
        let job = Job::default();
        install_from(def, &settings, &r1, "1.9.8", "test", &job).unwrap();
        let st = scan(def, &settings);
        assert!(st.installed() && st.managed);
        assert_eq!(st.mod_version, "1.9.8");

        // User edits settings, then updates.
        let ini_path = st.ini.clone().unwrap();
        let mut ini = Ini::parse(&std::fs::read_to_string(&ini_path).unwrap());
        ini.set_id("cooppassword", "ashen-grace-12");
        ini.set_id("boss_health_scaling", "150");
        std::fs::write(&ini_path, ini.to_text()).unwrap();

        let r2 = base.join("rel2");
        make_release(&r2, "[PASSWORD]\ncooppassword = \n[SCALING]\nboss_health_scaling = 100\nnew_thing = 3\n", false);
        install_from(def, &settings, &r2, "v2.0.1", "test", &job).unwrap();
        let st = scan(def, &settings);
        assert_eq!(st.mod_version, "2.0.1");
        assert_eq!(st.password, "ashen-grace-12");
        let text = std::fs::read_to_string(st.ini.as_ref().unwrap()).unwrap();
        assert!(text.contains("boss_health_scaling = 150") && text.contains("new_thing = 3"));
        assert!(!game.join("SeamlessCoop/old_only.dat").exists(), "stale file removed");
        assert_eq!(rollbacks(def).len(), 1);
        assert_eq!(rollbacks(def)[0].version, "1.9.8");

        // Wrong game's archive is refused.
        let wrong = base.join("wrong");
        std::fs::create_dir_all(&wrong).unwrap();
        std::fs::write(wrong.join("ds3sc_launcher.exe"), b"x").unwrap();
        assert!(find_mod_root(&wrong, def).unwrap_err().to_string().contains("Dark Souls III"));

        // Rollback = install from the snapshot folder.
        let rb = &rollbacks(def)[0];
        install_from(def, &settings, &rb.path, &rb.version, "rollback", &job).unwrap();
        assert_eq!(scan(def, &settings).mod_version, "1.9.8");
        assert_eq!(scan(def, &settings).password, "ashen-grace-12");

        set_mods_off(def, &settings, true).unwrap();
        assert!(scan(def, &settings).mods_off);
        set_mods_off(def, &settings, false).unwrap();

        uninstall(def, &settings, &job).unwrap();
        let st = scan(def, &settings);
        assert!(!st.installed());
        assert!(game.join("eldenring.exe").exists(), "game untouched");
        std::fs::remove_dir_all(&base).ok();
    }
}
