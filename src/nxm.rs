//! Nexus "Mod Manager Download" links (`nxm://game/mods/ID/files/FID?key=â€¦&expires=â€¦`).
//! Nexus only hands direct API downloads to Premium members; for everyone else a click on the site's
//! "Mod Manager Download" button produces one of these links, whose one-time key makes the API download work.
//! BonfireLink can (opt-in) register as the handler. A click starts `BonfireLink nxm://â€¦`, which drops the link into
//! `BonfireLink-data/inbox/`; the open window (or the window that this start opens) installs it.

use crate::config::{self, Settings};
use crate::games::{GAMES, GameDef};
use crate::ops::{self, Job};
use crate::remote;
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub domain: String,
    pub mod_id: u32,
    pub file_id: u64,
    pub key: String,
    pub expires: String,
}

pub fn parse(url: &str) -> Result<Link> {
    let rest = url.trim().strip_prefix("nxm://").context("not an nxm:// link")?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let [domain, "mods", mod_id, "files", file_id] = parts.as_slice() else {
        bail!("unexpected Nexus link format: {url}");
    };
    let q = |name: &str| {
        query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v.to_string())).unwrap_or_default()
    };
    Ok(Link {
        domain: domain.to_ascii_lowercase(),
        mod_id: mod_id.parse().context("bad mod id in Nexus link")?,
        file_id: file_id.parse().context("bad file id in Nexus link")?,
        key: q("key"),
        expires: q("expires"),
    })
}

pub fn game_for(link: &Link) -> Option<&'static GameDef> {
    GAMES.iter().find(|g| g.nexus.is_some_and(|(d, id)| d.eq_ignore_ascii_case(&link.domain) && id == link.mod_id))
}

// ---------- inbox (hand-off from the browser-started process to the open window) ----------

fn inbox() -> PathBuf {
    config::sub("inbox")
}

pub fn queue(url: &str) -> Result<()> {
    parse(url)?;
    let name = format!("{}-{}.nxm", config::now_stamp(), std::process::id());
    std::fs::write(inbox().join(name), url.trim())?;
    Ok(())
}

/// Takes the queued links (oldest first) out of the inbox.
pub fn take_queued() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(inbox()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "nxm")).collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok();
            let _ = std::fs::remove_file(&p);
            text
        })
        .collect()
}

// ---------- the open window ----------

fn gui_pid_file() -> PathBuf {
    config::data_dir().join("gui.pid")
}

pub fn mark_gui_running() {
    let _ = std::fs::write(gui_pid_file(), std::process::id().to_string());
}

pub fn clear_gui_running() {
    if std::fs::read_to_string(gui_pid_file()).ok().and_then(|s| s.trim().parse::<u32>().ok()) == Some(std::process::id()) {
        let _ = std::fs::remove_file(gui_pid_file());
    }
}

/// Whether another BonfireLink window is open (it picks up the inbox by itself).
pub fn other_gui_running() -> bool {
    std::fs::read_to_string(gui_pid_file())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(|pid| pid != std::process::id() && crate::steam::pid_running(pid))
}

// ---------- installing ----------

pub fn install(url: &str, settings: &Settings, job: &Job) -> Result<String> {
    let link = parse(url)?;
    let def = game_for(&link).with_context(|| {
        format!("That Nexus file isn't a Seamless Co-op mod BonfireLink manages ({} mod {}).", link.domain, link.mod_id)
    })?;
    if settings.nexus_api_key.trim().is_empty() {
        bail!("Connect your Nexus account first (Settings > Nexus Mods): BonfireLink needs your API key to fetch the file.");
    }
    job.status(format!("Fetching {} from Nexusâ€¦", def.name));
    let info = remote::nexus_file(&link.domain, link.mod_id, link.file_id, &settings.nexus_api_key)?;
    let auth = (!link.key.is_empty()).then_some((link.key.as_str(), link.expires.as_str()));
    let dl = remote::nexus_download_url_with(&link.domain, link.mod_id, link.file_id, &settings.nexus_api_key, auth)?;
    let dest = ops::download_cache().join(format!("{}-{}-nexus-{}.{}", def.key, info.version, link.file_id, ext(&info.file_name)));
    if !dest.is_file() {
        job.status(format!("Downloading Seamless Co-op {} from Nexusâ€¦", info.version));
        let j = job.clone();
        remote::download(&dl, &dest, &move |done, total| {
            if let Some(t) = total.filter(|t| *t > 0) {
                j.progress(done as f32 / t as f32);
            }
        })?;
    }
    let version = if info.version.is_empty() { ops::version_from_filename(&info.file_name, def) } else { info.version.clone() };
    ops::install_from(def, settings, &dest, &version, &format!("Nexus file {}", link.file_id), job)?;
    Ok(format!("Seamless Co-op {version} for {} installed from Nexus.", def.name))
}

fn ext(name: &str) -> &'static str {
    if name.to_ascii_lowercase().ends_with(".7z") { "7z" } else { "zip" }
}

// ---------- handler registration (opt-in) ----------

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Handler {
    #[default]
    None,
    Us,
    /// Another program (Vortex, Mod Organizer 2, â€¦) with its command line.
    Other(String),
}

pub fn exe_command() -> Result<String> {
    Ok(format!("\"{}\" \"%1\"", std::env::current_exe()?.display()))
}

#[cfg(windows)]
pub fn current_handler() -> Handler {
    use winreg::RegKey;
    use winreg::enums::HKEY_CLASSES_ROOT;
    let Ok(k) = RegKey::predef(HKEY_CLASSES_ROOT).open_subkey("nxm\\shell\\open\\command") else { return Handler::None };
    let cmd: String = k.get_value("").unwrap_or_default();
    classify(&cmd)
}

fn classify(cmd: &str) -> Handler {
    if cmd.trim().is_empty() {
        return Handler::None;
    }
    let ours = std::env::current_exe().map(|e| e.display().to_string().to_lowercase()).unwrap_or_default();
    if !ours.is_empty() && cmd.to_lowercase().contains(&ours) { Handler::Us } else { Handler::Other(cmd.to_string()) }
}

/// Makes BonfireLink the handler for nxm:// links (per user, no admin rights). Returns the previous handler.
#[cfg(windows)]
pub fn register() -> Result<Handler> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let before = current_handler();
    let (root, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Software\\Classes\\nxm").context("could not write the registry")?;
    root.set_value("", &"URL:NXM Protocol")?;
    root.set_value("URL Protocol", &"")?;
    let (cmd, _) = root.create_subkey("shell\\open\\command")?;
    cmd.set_value("", &exe_command()?)?;
    Ok(before)
}

/// Gives nxm:// links back to `previous` (the command saved when we registered), or removes our entry.
#[cfg(windows)]
pub fn unregister(previous: &str) -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if previous.trim().is_empty() {
        let _ = hkcu.delete_subkey_all("Software\\Classes\\nxm");
    } else {
        let (cmd, _) = hkcu.create_subkey("Software\\Classes\\nxm\\shell\\open\\command")?;
        cmd.set_value("", &previous.to_string())?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn desktop_file() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("applications").join("bonfirelink-nxm.desktop"))
}

#[cfg(not(windows))]
fn xdg_mime(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("xdg-mime").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(windows))]
pub fn current_handler() -> Handler {
    match xdg_mime(&["query", "default", "x-scheme-handler/nxm"]) {
        Some(d) if d == "bonfirelink-nxm.desktop" => Handler::Us,
        Some(d) if !d.is_empty() => Handler::Other(d),
        _ => Handler::None,
    }
}

#[cfg(not(windows))]
pub fn register() -> Result<Handler> {
    let before = current_handler();
    let file = desktop_file().context("no applications folder")?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    let exe = std::env::current_exe()?;
    std::fs::write(
        &file,
        format!(
            "[Desktop Entry]\nType=Application\nName=BonfireLink (Nexus links)\nExec=\"{}\" %u\nPath={}\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/nxm;\n",
            exe.display(),
            exe.parent().map(|p| p.display().to_string()).unwrap_or_default()
        ),
    )?;
    if xdg_mime(&["default", "bonfirelink-nxm.desktop", "x-scheme-handler/nxm"]).is_none() {
        set_mimeapps_default("bonfirelink-nxm.desktop")?;
    }
    let _ = std::process::Command::new("update-desktop-database").arg(file.parent().unwrap()).status();
    Ok(before)
}

/// Fallback when xdg-mime is missing: edit ~/.config/mimeapps.list directly.
#[cfg(not(windows))]
fn set_mimeapps_default(desktop: &str) -> Result<()> {
    let path = dirs::config_dir().context("no config dir")?.join("mimeapps.list");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = Vec::new();
    let mut in_default = false;
    let mut written = false;
    for line in text.lines() {
        if line.starts_with('[') {
            if in_default && !written {
                out.push(format!("x-scheme-handler/nxm={desktop}"));
                written = true;
            }
            in_default = line.trim() == "[Default Applications]";
        }
        if in_default && line.starts_with("x-scheme-handler/nxm=") {
            continue;
        }
        out.push(line.to_string());
    }
    if !written {
        if !in_default {
            out.push("[Default Applications]".into());
        }
        out.push(format!("x-scheme-handler/nxm={desktop}"));
    }
    std::fs::write(path, out.join("\n") + "\n")?;
    Ok(())
}

#[cfg(not(windows))]
pub fn unregister(previous: &str) -> Result<()> {
    if previous.trim().is_empty() {
        if let Some(f) = desktop_file() {
            let _ = std::fs::remove_file(f);
        }
    } else if xdg_mime(&["default", previous.trim(), "x-scheme-handler/nxm"]).is_none() {
        set_mimeapps_default(previous.trim())?;
    }
    Ok(())
}

/// The page that shows one file with its "Mod Manager Download" button.
pub fn file_page(def: &GameDef, file_id: Option<u64>) -> Option<String> {
    let (domain, id) = def.nexus.filter(|(_, id)| *id != 0)?;
    Some(match file_id {
        Some(f) => format!("https://www.nexusmods.com/{domain}/mods/{id}?tab=files&file_id={f}&nmm=1"),
        None => format!("https://www.nexusmods.com/{domain}/mods/{id}?tab=files"),
    })
}

pub const API_KEY_PAGE: &str = "https://next.nexusmods.com/settings/api-keys";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_links_and_maps_games() {
        let l = parse("nxm://darksouls3/mods/1895/files/12345?key=abcDEF&expires=1793000000&user_id=42").unwrap();
        assert_eq!(
            l,
            Link { domain: "darksouls3".into(), mod_id: 1895, file_id: 12345, key: "abcDEF".into(), expires: "1793000000".into() }
        );
        assert_eq!(game_for(&l).unwrap().key, "ds3");
        let premium = parse("nxm://EldenRing/mods/510/files/9").unwrap();
        assert!(premium.key.is_empty());
        assert_eq!(game_for(&premium).unwrap().key, "er");
        assert!(game_for(&parse("nxm://skyrimspecialedition/mods/1/files/2").unwrap()).is_none());
        assert!(parse("https://example.com").is_err());
        assert!(parse("nxm://darksouls3/collections/x").is_err());
    }

    #[test]
    fn handler_classification() {
        assert_eq!(classify(""), Handler::None);
        assert!(matches!(classify("\"C:\\Vortex\\Vortex.exe\" -d \"%1\""), Handler::Other(_)));
    }
}
