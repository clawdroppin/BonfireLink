//! Steam: install roots, libraries, app manifests, launch options, processes, Deck detection.

use crate::games::GameDef;
use crate::vdf::{self, Obj};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const WRAPPER_NAME: &str = "bonfirelink-launch.sh";

pub fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

pub fn is_steam_deck() -> bool {
    if !is_linux() {
        return false;
    }
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    if os.lines().any(|l| l.trim() == "ID=steamos") {
        return true;
    }
    let vendor = std::fs::read_to_string("/sys/devices/virtual/dmi/id/board_vendor").unwrap_or_default();
    vendor.trim() == "Valve"
}

/// Gaming Mode = Steam's fullscreen gamescope session. Steam must never be restarted from there.
pub fn in_gaming_mode() -> bool {
    if !is_linux() {
        return false;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase();
    if desktop.contains("gamescope") {
        return true;
    }
    std::env::var("SteamGamepadUI").is_ok() || std::env::var("GAMESCOPE_WAYLAND_DISPLAY").is_ok() && desktop.is_empty()
}

pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        if let Ok(k) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\Valve\\Steam")
            && let Ok(p) = k.get_value::<String, _>("SteamPath")
        {
            roots.push(PathBuf::from(p.replace('/', "\\")));
        }
        for key in ["SOFTWARE\\WOW6432Node\\Valve\\Steam", "SOFTWARE\\Valve\\Steam"] {
            if let Ok(k) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(key)
                && let Ok(p) = k.get_value::<String, _>("InstallPath")
            {
                roots.push(PathBuf::from(p));
            }
        }
        roots.push(PathBuf::from("C:\\Program Files (x86)\\Steam"));
    }
    #[cfg(not(windows))]
    if let Some(home) = dirs::home_dir() {
        for rel in [
            ".local/share/Steam",
            ".steam/steam",
            ".steam/root",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
        ] {
            roots.push(home.join(rel));
        }
    }
    let mut seen = Vec::new();
    roots
        .into_iter()
        .filter(|r| r.join("steamapps").is_dir())
        .filter_map(|r| {
            let canon = std::fs::canonicalize(&r).unwrap_or(r.clone());
            if seen.contains(&canon) {
                None
            } else {
                seen.push(canon);
                Some(r)
            }
        })
        .collect()
}

/// All `steamapps` folders (one per library), across every Steam root. Cached for 30 seconds:
/// every status scan of every game needs it.
pub fn libraries() -> Vec<PathBuf> {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(Instant, Vec<PathBuf>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, libs)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(30)
    {
        return libs.clone();
    }
    let libs = scan_libraries();
    *cache = Some((Instant::now(), libs.clone()));
    libs
}

fn scan_libraries() -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = Vec::new();
    for root in steam_roots() {
        let main = root.join("steamapps");
        push_unique(&mut libs, main.clone());
        if let Ok(text) = std::fs::read_to_string(main.join("libraryfolders.vdf"))
            && let Ok(o) = vdf::parse_text(&text)
            && let Some(folders) = o.obj("libraryfolders")
        {
            for (_, v) in &folders.0 {
                if let vdf::Value::Obj(f) = v
                    && let Some(p) = f.str("path")
                {
                    push_unique(&mut libs, PathBuf::from(p).join("steamapps"));
                }
            }
        }
    }
    libs.retain(|l| l.is_dir());
    libs
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    let canon = std::fs::canonicalize(&p).unwrap_or(p.clone());
    if !v.iter().any(|x| std::fs::canonicalize(x).unwrap_or(x.clone()) == canon) {
        v.push(p);
    }
}

#[derive(Clone, Debug, Default)]
pub struct GameLocation {
    pub install_dir: PathBuf,
    /// Folder containing the game exe; the mod goes here.
    pub exe_dir: PathBuf,
    /// The `steamapps` folder of the library (None for manually picked folders).
    pub library: Option<PathBuf>,
    pub build_id: String,
}

pub fn read_manifest(library: &Path, app_id: u32) -> Option<Obj> {
    let text = std::fs::read_to_string(library.join(format!("appmanifest_{app_id}.acf"))).ok()?;
    vdf::parse_text(&text).ok()?.obj("AppState").cloned()
}

pub fn find_game(def: &GameDef, manual: Option<&str>) -> Option<GameLocation> {
    if let Some(m) = manual.filter(|m| !m.trim().is_empty()) {
        return location_from_folder(def, Path::new(m.trim()));
    }
    for lib in libraries() {
        if let Some(man) = read_manifest(&lib, def.steam_app_id) {
            let installdir = man.str("installdir").unwrap_or(def.install_dirs[0]).to_string();
            let install_dir = lib.join("common").join(&installdir);
            let exe_dir = install_dir.join(def.exe_subdir);
            if exe_dir.join(def.game_exe).is_file() || exe_dir.is_dir() {
                return Some(GameLocation {
                    install_dir,
                    exe_dir,
                    library: Some(lib.clone()),
                    build_id: man.str("buildid").unwrap_or_default().to_string(),
                });
            }
        }
        for dir in def.install_dirs {
            let install_dir = lib.join("common").join(dir);
            if install_dir.join(def.exe_subdir).join(def.game_exe).is_file() {
                return Some(GameLocation {
                    exe_dir: install_dir.join(def.exe_subdir),
                    install_dir,
                    library: Some(lib.clone()),
                    build_id: String::new(),
                });
            }
        }
    }
    None
}

/// Accepts the install folder, its `Game` subfolder, or the exe itself.
pub fn location_from_folder(def: &GameDef, picked: &Path) -> Option<GameLocation> {
    let picked = if picked.is_file() { picked.parent()?.to_path_buf() } else { picked.to_path_buf() };
    let candidates = [picked.join(def.exe_subdir), picked.clone(), picked.parent().map(|p| p.join(def.exe_subdir)).unwrap_or_default()];
    for exe_dir in candidates {
        if exe_dir.join(def.game_exe).is_file() {
            let install_dir = if def.exe_subdir.is_empty() {
                exe_dir.clone()
            } else {
                exe_dir.parent().map(Path::to_path_buf).unwrap_or(exe_dir.clone())
            };
            // A manual pick inside a Steam library still gets Proton/launch-option support.
            let library = install_dir
                .parent()
                .and_then(|common| common.parent())
                .filter(|sa| sa.file_name().is_some_and(|n| n.eq_ignore_ascii_case("steamapps")))
                .map(Path::to_path_buf);
            let build_id = library
                .as_ref()
                .and_then(|l| read_manifest(l, def.steam_app_id))
                .and_then(|m| m.str("buildid").map(str::to_string))
                .unwrap_or_default();
            return Some(GameLocation { install_dir, exe_dir, library, build_id });
        }
    }
    None
}

/// The game's Proton prefix (`drive_c` lives inside). Linux only.
pub fn proton_prefix(loc: &GameLocation, app_id: u32) -> Option<PathBuf> {
    if !is_linux() {
        return None;
    }
    let mut candidates = Vec::new();
    if let Some(lib) = &loc.library {
        candidates.push(lib.join("compatdata").join(app_id.to_string()).join("pfx"));
    }
    for root in steam_roots() {
        candidates.push(root.join("steamapps/compatdata").join(app_id.to_string()).join("pfx"));
    }
    candidates.into_iter().find(|p| p.is_dir())
}

/// Proton version chosen for the game in Steam (empty = Steam's default).
pub fn compat_tool(app_id: u32) -> Option<String> {
    for root in steam_roots() {
        let Ok(text) = std::fs::read_to_string(root.join("config/config.vdf")) else { continue };
        let Ok(o) = vdf::parse_text(&text) else { continue };
        let map = o.obj_path(&["InstallConfigStore", "Software", "Valve", "Steam", "CompatToolMapping"]);
        if let Some(entry) = map.and_then(|m| m.obj(&app_id.to_string())) {
            return entry.str("name").map(str::to_string);
        }
    }
    None
}

fn localconfig_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in steam_roots() {
        let Ok(rd) = std::fs::read_dir(root.join("userdata")) else { continue };
        for e in rd.flatten() {
            let p = e.path().join("config").join("localconfig.vdf");
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out
}

const APPS_PATH: [&str; 5] = ["UserLocalConfigStore", "Software", "Valve", "Steam", "apps"];

/// Launch options per Steam account (`localconfig.vdf` path, current value).
pub fn launch_options(app_id: u32) -> Vec<(PathBuf, String)> {
    localconfig_paths()
        .into_iter()
        .filter_map(|p| {
            let o = vdf::parse_text(&std::fs::read_to_string(&p).ok()?).ok()?;
            let v = o
                .obj_path(&APPS_PATH)
                .and_then(|a| a.obj(&app_id.to_string()))
                .and_then(|a| a.str("LaunchOptions"))
                .unwrap_or_default()
                .to_string();
            Some((p, v))
        })
        .collect()
}

/// Rewrites the launch options of `app_id` for every Steam account with `edit`.
/// Steam must be closed (it rewrites localconfig.vdf on exit). Each file is backed up first.
pub fn edit_launch_options(app_id: u32, edit: &dyn Fn(&str) -> String) -> Result<usize> {
    if is_steam_running() {
        bail!("Steam is running; it has to be closed to change launch options");
    }
    let mut changed = 0;
    for path in localconfig_paths() {
        let text = std::fs::read_to_string(&path)?;
        let mut o = vdf::parse_text(&text).with_context(|| format!("could not read {}", path.display()))?;
        let app = o.obj_path_mut(&APPS_PATH).obj_path_mut(&[&app_id.to_string()]);
        let old = app.str("LaunchOptions").unwrap_or_default().to_string();
        let new = edit(&old);
        if new == old {
            continue;
        }
        if new.is_empty() {
            app.remove("LaunchOptions");
        } else {
            app.set_str("LaunchOptions", &new);
        }
        let backup = crate::config::sub("backups/steam-localconfig").join(format!(
            "{}-{}.vdf",
            crate::config::now_stamp(),
            path.parent().and_then(|p| p.parent()).and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
        ));
        std::fs::copy(&path, &backup).context("could not back up localconfig.vdf")?;
        std::fs::write(&path, vdf::to_text(&o))?;
        changed += 1;
    }
    Ok(changed)
}

/// Inserts the wrapper in front of `%command%`, keeping whatever the user had.
/// Also replaces the community `echo "%command%" | sed ... | sh` trick, which the wrapper supersedes.
pub fn with_wrapper(existing: &str, wrapper: &Path) -> String {
    let token = format!("\"{}\"", wrapper.to_string_lossy());
    let mut base = without_wrapper(existing);
    if base.contains("sed 's/") && base.contains("sc_launcher.exe") {
        base = String::new();
    }
    if base.contains("%command%") {
        base.replacen("%command%", &format!("{token} %command%"), 1)
    } else if base.trim().is_empty() {
        format!("{token} %command%")
    } else {
        format!("{token} %command% {}", base.trim())
    }
}

pub fn without_wrapper(existing: &str) -> String {
    let Some(pos) = existing.find(WRAPPER_NAME) else { return existing.to_string() };
    // The token is a quoted absolute path ending in the wrapper name.
    let start = existing[..pos].rfind('"').unwrap_or(pos);
    let mut end = pos + WRAPPER_NAME.len();
    if existing[end..].starts_with('"') {
        end += 1;
    }
    let mut out = format!("{}{}", &existing[..start], existing[end..].trim_start());
    if out.trim() == "%command%" {
        out.clear();
    }
    out.trim().to_string()
}

pub fn has_wrapper(opts: &str) -> bool {
    opts.contains(WRAPPER_NAME)
}

// ---------- processes ----------

fn processes() -> sysinfo::System {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    sys
}

pub fn process_running(names: &[&str]) -> bool {
    let sys = processes();
    sys.processes().values().any(|p| {
        let n = p.name().to_string_lossy().to_lowercase();
        names.iter().any(|want| n == want.to_lowercase() || n.trim_end_matches(".exe") == want.to_lowercase().trim_end_matches(".exe"))
    })
}

pub fn pid_running(pid: u32) -> bool {
    let mut sys = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some()
}

pub fn is_steam_running() -> bool {
    if cfg!(windows) { process_running(&["steam.exe"]) } else { process_running(&["steam", "steamwebhelper"]) }
}

pub fn game_running(def: &GameDef) -> bool {
    process_running(&[def.game_exe, def.launcher])
}

pub fn shutdown_steam() -> Result<()> {
    if !is_steam_running() {
        return Ok(());
    }
    if in_gaming_mode() {
        bail!("Steam can't be closed from Gaming Mode. Switch to Desktop Mode and try again.");
    }
    run_steam(&["-shutdown"])?;
    let start = Instant::now();
    while is_steam_running() {
        if start.elapsed() > Duration::from_secs(40) {
            bail!("Steam didn't close within 40 seconds. Close it yourself (Steam > Exit) and try again.");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // Steam writes its config files right before exiting; give the filesystem a moment.
    std::thread::sleep(Duration::from_secs(1));
    Ok(())
}

pub fn start_steam() {
    let _ = run_steam(&[]);
}

fn run_steam(args: &[&str]) -> Result<()> {
    #[cfg(windows)]
    {
        let exe = steam_roots().into_iter().map(|r| r.join("steam.exe")).find(|p| p.is_file()).context("steam.exe not found")?;
        std::process::Command::new(exe).args(args).spawn().context("could not start Steam")?;
    }
    #[cfg(not(windows))]
    {
        let flatpak = steam_roots().iter().any(|r| r.to_string_lossy().contains("com.valvesoftware.Steam"));
        let mut cmd = if flatpak && which("steam").is_none() {
            let mut c = std::process::Command::new("flatpak");
            c.args(["run", "com.valvesoftware.Steam"]);
            c
        } else {
            std::process::Command::new("steam")
        };
        cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        cmd.spawn().context("could not start Steam")?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn which(cmd: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|p| p.join(cmd)).find(|p| p.is_file()))
}

/// Asks Steam to start the game (used on Linux where Steam runs the wrapper > Seamless launcher).
pub fn run_game_via_steam(app_id: u32) {
    crate::util::open_url(&format!("steam://rungameid/{app_id}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapper_merge_and_strip() {
        let w = Path::new("/home/deck/x/ELDEN RING/Game/bonfirelink-launch.sh");
        assert_eq!(with_wrapper("", w), "\"/home/deck/x/ELDEN RING/Game/bonfirelink-launch.sh\" %command%");
        let with_user = with_wrapper("gamemoderun %command% -nosplash", w);
        assert_eq!(with_user, "gamemoderun \"/home/deck/x/ELDEN RING/Game/bonfirelink-launch.sh\" %command% -nosplash");
        assert_eq!(without_wrapper(&with_user), "gamemoderun %command% -nosplash");
        assert_eq!(with_wrapper(&with_user, w), with_user, "idempotent");
        assert_eq!(without_wrapper(&with_wrapper("", w)), "");
        assert_eq!(with_wrapper("-skipintro", w), "\"/home/deck/x/ELDEN RING/Game/bonfirelink-launch.sh\" %command% -skipintro");
        let sed = "echo \"%command%\" | sed 's/DarkSoulsII.exe/ds2sc_launcher.exe/' | sh";
        assert!(!with_wrapper(sed, w).contains("sed"));
    }
}
