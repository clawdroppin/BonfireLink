//! Updating BonfireLink itself from its GitHub releases (SHA-256 checked against SHA256SUMS.txt).

use crate::remote;
use anyhow::{Context, Result, bail};

pub fn asset_name() -> &'static str {
    if cfg!(windows) { "BonfireLink-windows-x64.exe" } else { "BonfireLink-linux-x64" }
}

pub struct Available {
    pub version: String,
    pub notes: String,
    url: String,
    sums_url: String,
}

pub fn check() -> Result<Option<Available>> {
    let Some(r) = remote::helper_latest(asset_name())? else { return Ok(None) };
    if !remote::newer(&r.version, crate::config::VERSION) {
        return Ok(None);
    }
    Ok(Some(Available { version: r.version, notes: r.notes, url: r.asset_url, sums_url: r.sums_url }))
}

/// Downloads, verifies and swaps the running executable. The caller restarts the app.
pub fn apply(a: &Available, job: &crate::ops::Job) -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().context("no exe folder")?;
    let new = dir.join(format!(".{}.new", asset_name()));
    job.status(format!("Downloading BonfireLink {}…", a.version));
    let j = job.clone();
    remote::download(&a.url, &new, &move |d, t| {
        if let Some(t) = t.filter(|t| *t > 0) {
            j.progress(d as f32 / t as f32);
        }
    })?;
    let sums = remote::fetch_text(&a.sums_url).context("could not fetch SHA256SUMS.txt")?;
    let want = sums
        .lines()
        .find_map(|l| {
            let mut it = l.split_whitespace();
            let h = it.next()?;
            let name = it.next()?.trim_start_matches('*');
            (name == asset_name()).then(|| h.to_lowercase())
        })
        .context("the release has no checksum for this file")?;
    let got = crate::util::sha256_file(&new)?;
    if got != want {
        let _ = std::fs::remove_file(&new);
        bail!("The downloaded update is corrupted (checksum mismatch); nothing was changed.");
    }
    // A running exe can be renamed (Windows) or replaced (Linux), not overwritten in place.
    let old = dir.join(format!(".{}.old", asset_name()));
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).context("could not move the running exe aside")?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e).context("could not put the new exe in place");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
    }
    job.status(format!("BonfireLink {} is ready. Restart to use it.", a.version));
    Ok(())
}

pub fn restart() -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0)
}

/// Removes the previous exe left behind by an update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let _ = std::fs::remove_file(dir.join(format!(".{}.old", asset_name())));
    }
}
