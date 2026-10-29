//! Settings (what the user chose) and state (what the helper installed), both JSON in the portable data folder.

use crate::games::GameId;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const APP_NAME: &str = "BonfireLink";
pub const APP_REPO: &str = "clawdroppin/BonfireLink";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `BonfireLink-data/` next to the exe (portable). Falls back to the user's config dir when that
/// folder can't be written (e.g. the exe sits in a read-only location).
pub fn data_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Ok(dir) = std::env::var("BONFIRELINK_DATA") {
            let p = PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&p);
            return p;
        }
        let portable = exe_dir().join(format!("{APP_NAME}-data"));
        if std::fs::create_dir_all(&portable).is_ok() && writable(&portable) {
            return portable;
        }
        let fallback = dirs::config_dir().unwrap_or_else(std::env::temp_dir).join(APP_NAME);
        let _ = std::fs::create_dir_all(&fallback);
        fallback
    })
}

pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(".write-test");
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

pub fn sub(name: &str) -> PathBuf {
    let p = data_dir().join(name);
    let _ = std::fs::create_dir_all(&p);
    p
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ui_scale: f32,
    pub check_updates_on_start: bool,
    pub check_helper_updates: bool,
    /// Steam Deck: the launch wrapper asks the helper to update the mod before the game starts.
    pub auto_update_on_play: bool,
    pub backup_saves_before_install: bool,
    pub backup_saves_before_play: bool,
    pub keep_backups: usize,
    pub keep_rollbacks: usize,
    /// Second copy of every save backup (SD card, USB stick, cloud-synced folder).
    pub backup_mirror: String,
    /// Personal API key from nexusmods.com (Settings > API). Stored in plain text in the data folder.
    pub nexus_api_key: String,
    /// Result of the last key check: account name and whether it's Premium (direct API downloads).
    pub nexus_user: String,
    pub nexus_premium: bool,
    /// nxm:// handler that was registered before BonfireLink took over (restored when switched off).
    pub nxm_previous_handler: String,
    /// Watch Downloads for Nexus archives after "Get it from Nexus".
    pub downloads_dir: String,
    /// Optional per-game override of the game folder (when it isn't in a Steam library).
    pub game_dirs: BTreeMap<GameId, String>,
    pub player_name: String,
    pub last_tab: String,
    pub dark_theme: bool,
    /// Games hidden from the tab bar (user doesn't own them).
    pub hidden_games: Vec<GameId>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_scale: if crate::steam::is_steam_deck() { 1.35 } else { 1.1 },
            check_updates_on_start: true,
            check_helper_updates: true,
            auto_update_on_play: true,
            backup_saves_before_install: true,
            backup_saves_before_play: false,
            keep_backups: 10,
            keep_rollbacks: 3,
            backup_mirror: String::new(),
            nexus_api_key: String::new(),
            nexus_user: String::new(),
            nexus_premium: false,
            nxm_previous_handler: String::new(),
            downloads_dir: String::new(),
            game_dirs: BTreeMap::new(),
            player_name: String::new(),
            last_tab: String::new(),
            dark_theme: true,
            hidden_games: Vec::new(),
        }
    }
}

/// Everything the helper knows about one game's installed mod.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GameState {
    pub mod_version: String,
    pub installed_at: String,
    /// Files we put in the game folder, relative to the exe folder (forward slashes).
    pub files: Vec<String>,
    pub launcher: String,
    pub ini_rel: String,
    pub dll_sha256: String,
    pub source: String,
    /// The settings file exactly as the release shipped it (for "Reset to mod defaults").
    pub pristine_ini: String,
    pub game_build_at_install: String,
    pub last_save_backup: String,
    pub last_backup_fingerprint: String,
    /// Custom presets: name > (section.key > value).
    pub presets: BTreeMap<String, BTreeMap<String, String>>,
    /// Steam Deck: whether we set the launch option ourselves.
    pub launch_option_set: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub games: BTreeMap<GameId, GameState>,
    pub last_update_check: String,
}

fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            // Keep the broken file for inspection instead of silently overwriting it later.
            let _ = std::fs::copy(path, path.with_extension("json.broken"));
            eprintln!("could not read {}: {e}", path.display());
            T::default()
        }),
        Err(_) => T::default(),
    }
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("settings.json")
    }
    pub fn load() -> Self {
        load_json(&Self::path())
    }
    pub fn save(&self) -> Result<()> {
        save_json(&Self::path(), self)
    }
}

impl State {
    pub fn path() -> PathBuf {
        data_dir().join("state.json")
    }
    pub fn load() -> Self {
        load_json(&Self::path())
    }
    pub fn save(&self) -> Result<()> {
        save_json(&Self::path(), self)
    }
    pub fn game(&self, id: GameId) -> GameState {
        self.games.get(&id).cloned().unwrap_or_default()
    }
    pub fn game_mut(&mut self, id: GameId) -> &mut GameState {
        self.games.entry(id).or_default()
    }
}

pub fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}
