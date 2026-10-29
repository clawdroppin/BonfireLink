//! Release feeds: GitHub releases (with an HTML fallback when the API is rate-limited), the author's
//! `VERSION` files (which versions may connect), and the Nexus Mods API.

use crate::games::GameDef;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_global(Some(Duration::from_secs(60 * 20)))
            .user_agent(format!("{}/{}", crate::config::APP_NAME, crate::config::VERSION))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn get_text(url: &str, headers: &[(&str, &str)]) -> Result<(u16, String)> {
    let mut req = agent().get(url).config().timeout_global(Some(Duration::from_secs(25))).build();
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let mut resp = req.call().with_context(|| format!("no connection to {}", host(url)))?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().with_config().limit(8 << 20).read_to_string().unwrap_or_default();
    Ok((status, body))
}

fn host(url: &str) -> &str {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url)
}

/// "v2.0.1" / "2.0.1" / "2.01" (VERSION-file style) > [2, 0, 1].
pub fn version_parts(v: &str) -> Vec<u32> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let mut parts: Vec<&str> = v.split(['.', '-', '+', ' ']).filter(|p| !p.is_empty()).collect();
    // VERSION files write 2.0.1 as "2.01" and 0.0.6 as "0.06".
    if parts.len() == 2 && parts[1].len() == 2 && parts[1].chars().all(|c| c.is_ascii_digit()) {
        let (a, b) = parts[1].split_at(1);
        parts = vec![parts[0], a, b];
    }
    parts.iter().map_while(|p| p.parse::<u32>().ok()).collect()
}

pub fn normalize(v: &str) -> String {
    let p = version_parts(v);
    if p.is_empty() {
        return v.trim().to_string();
    }
    p.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

pub fn newer(a: &str, b: &str) -> bool {
    let (mut x, mut y) = (version_parts(a), version_parts(b));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x > y
}

pub fn same_version(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && !newer(a, b) && !newer(b, a)
}

#[derive(Clone, Debug, Default)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub published: String,
    pub notes: String,
    pub asset_name: String,
    pub asset_url: String,
    pub size: u64,
    /// From GitHub's asset digest when available.
    pub sha256: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Newest first. Only releases with a .zip/.7z asset are returned.
pub fn github_releases(repo: &str, include_pre: bool) -> Result<Vec<Release>> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=15");
    let (status, body) = get_text(&url, &[("Accept", "application/vnd.github+json")])?;
    if status == 403 || status == 429 {
        return github_releases_html(repo);
    }
    if status == 404 {
        return Ok(Vec::new());
    }
    if status != 200 {
        bail!("GitHub answered {status} for {repo}");
    }
    let list: Vec<GhRelease> = serde_json::from_str(&body).context("GitHub sent an unexpected release list")?;
    Ok(list
        .into_iter()
        .filter(|r| !r.draft && (include_pre || !r.prerelease))
        .filter_map(|r| {
            let asset = r.assets.iter().find(|a| is_archive(&a.name))?;
            let version = normalize(&r.tag_name);
            Some(Release {
                version,
                tag: r.tag_name.clone(),
                published: r.published_at.unwrap_or_default().chars().take(10).collect(),
                notes: r.body.filter(|b| !b.trim().is_empty()).or(r.name).unwrap_or_default(),
                asset_name: asset.name.clone(),
                asset_url: asset.browser_download_url.clone(),
                size: asset.size,
                sha256: asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).unwrap_or_default().to_string(),
            })
        })
        .collect())
}

fn is_archive(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".zip") || n.ends_with(".7z")
}

/// Fallback when the API is rate-limited (60 requests/hour per IP): the Atom feed for tags and
/// the "expanded_assets" fragment for download links.
fn github_releases_html(repo: &str) -> Result<Vec<Release>> {
    let (status, atom) = get_text(&format!("https://github.com/{repo}/releases.atom"), &[])?;
    if status != 200 {
        bail!("GitHub is rate-limiting update checks; try again in a few minutes");
    }
    let mut out = Vec::new();
    for entry in atom.split("<entry>").skip(1).take(5) {
        let Some(tag) = between(entry, "/releases/tag/", "\"") else { continue };
        let tag = tag.to_string();
        let published = between(entry, "<updated>", "</updated>").unwrap_or_default().chars().take(10).collect();
        let (s, html) = get_text(&format!("https://github.com/{repo}/releases/expanded_assets/{tag}"), &[])?;
        if s != 200 {
            continue;
        }
        let needle = format!("/{repo}/releases/download/{tag}/");
        let Some(pos) = html.find(&needle) else { continue };
        let name: String = html[pos + needle.len()..].chars().take_while(|c| *c != '"').collect();
        if !is_archive(&name) {
            continue;
        }
        out.push(Release {
            version: normalize(&tag),
            asset_url: format!("https://github.com{needle}{name}"),
            asset_name: name,
            tag,
            published,
            ..Default::default()
        });
    }
    Ok(out)
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let start = s.find(a)? + a.len();
    let end = s[start..].find(b)? + start;
    Some(&s[start..end])
}

/// What the author's VERSION file says: the version players must be on, and every version it lists.
#[derive(Clone, Debug, Default)]
pub struct VersionGate {
    pub current: Option<String>,
    pub known: Vec<String>,
}

pub fn parse_version_file(text: &str) -> VersionGate {
    let mut gate = VersionGate::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let current = line.starts_with('>');
        let mut it = line.trim_start_matches('>').split_whitespace();
        let Some(v) = it.next() else { continue };
        let allowed = it.next() == Some("1");
        let v = normalize(v);
        if current && allowed {
            gate.current = Some(v.clone());
        }
        gate.known.push(v);
    }
    gate
}

pub fn version_gate(repo: &str) -> Result<VersionGate> {
    for branch in ["main", "master"] {
        let (status, body) = get_text(&format!("https://raw.githubusercontent.com/{repo}/{branch}/VERSION"), &[])?;
        if status == 200 {
            return Ok(parse_version_file(&body));
        }
    }
    Ok(VersionGate::default())
}

// ---------- Nexus ----------

#[derive(Clone, Debug, Default)]
pub struct NexusFile {
    pub file_id: u64,
    pub name: String,
    pub version: String,
    pub file_name: String,
    pub size: u64,
    pub uploaded: String,
    pub changelog: String,
}

#[derive(Deserialize)]
struct NxFiles {
    files: Vec<NxFile>,
}

#[derive(Deserialize)]
struct NxFile {
    file_id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    category_name: Option<String>,
    #[serde(default)]
    file_name: String,
    #[serde(default)]
    size_kb: u64,
    #[serde(default)]
    uploaded_timestamp: i64,
    #[serde(default)]
    changelog_html: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct NexusUser {
    pub name: String,
    pub premium: bool,
}

fn nexus_get(url: &str, key: &str) -> Result<String> {
    let (status, body) = get_text(
        url,
        &[("apikey", key.trim()), ("Application-Name", crate::config::APP_NAME), ("Application-Version", crate::config::VERSION)],
    )?;
    match status {
        200 => Ok(body),
        401 => bail!("Nexus didn't accept the API key. Copy it again from nexusmods.com > Settings > API Keys"),
        403 => bail!("Nexus only allows direct downloads for Premium members"),
        404 => bail!("Nexus doesn't have this file (anymore)"),
        429 => bail!("Nexus rate limit reached; try again in an hour"),
        s => bail!("Nexus answered {s}"),
    }
}

pub fn nexus_validate(key: &str) -> Result<NexusUser> {
    let body = nexus_get("https://api.nexusmods.com/v1/users/validate.json", key)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(NexusUser { name: v["name"].as_str().unwrap_or_default().to_string(), premium: v["is_premium"].as_bool().unwrap_or(false) })
}

/// Newest "main" file of the mod.
pub fn nexus_latest(domain: &str, mod_id: u32, key: &str) -> Result<Option<NexusFile>> {
    let body = nexus_get(&format!("https://api.nexusmods.com/v1/games/{domain}/mods/{mod_id}/files.json?category=main"), key)?;
    let files: NxFiles = serde_json::from_str(&body).context("Nexus sent an unexpected file list")?;
    let best = files
        .files
        .into_iter()
        .filter(|f| f.category_name.as_deref().is_none_or(|c| c.eq_ignore_ascii_case("MAIN")))
        .filter(|f| is_archive(&f.file_name))
        .max_by_key(|f| f.uploaded_timestamp);
    Ok(best.map(|f| NexusFile {
        file_id: f.file_id,
        version: normalize(f.version.as_deref().unwrap_or_default()),
        name: f.name,
        file_name: f.file_name,
        size: f.size_kb * 1024,
        uploaded: chrono::DateTime::from_timestamp(f.uploaded_timestamp, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
        changelog: strip_html(f.changelog_html.as_deref().unwrap_or_default()),
    }))
}

pub fn nexus_download_url(domain: &str, mod_id: u32, file_id: u64, key: &str) -> Result<String> {
    nexus_download_url_with(domain, mod_id, file_id, key, None)
}

/// `nxm_auth` = (key, expires) from a "Mod Manager Download" link, which lets free accounts download too.
pub fn nexus_download_url_with(domain: &str, mod_id: u32, file_id: u64, key: &str, nxm_auth: Option<(&str, &str)>) -> Result<String> {
    let mut url = format!("https://api.nexusmods.com/v1/games/{domain}/mods/{mod_id}/files/{file_id}/download_link.json");
    if let Some((k, e)) = nxm_auth {
        let clean = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).collect::<String>();
        url.push_str(&format!("?key={}&expires={}", clean(k), clean(e)));
    }
    let body = nexus_get(&url, key)?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    v.as_array().and_then(|a| a.first()).and_then(|m| m["URI"].as_str()).map(str::to_string).context("Nexus didn't return a download link")
}

/// One file's details (version, name) by id.
pub fn nexus_file(domain: &str, mod_id: u32, file_id: u64, key: &str) -> Result<NexusFile> {
    let body = nexus_get(&format!("https://api.nexusmods.com/v1/games/{domain}/mods/{mod_id}/files/{file_id}.json"), key)?;
    let f: NxFile = serde_json::from_str(&body).context("Nexus sent unexpected file details")?;
    Ok(NexusFile {
        file_id: f.file_id,
        version: normalize(f.version.as_deref().unwrap_or_default()),
        name: f.name,
        file_name: f.file_name,
        size: f.size_kb * 1024,
        uploaded: String::new(),
        changelog: String::new(),
    })
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.replace("<br />", "\n").replace("<br>", "\n").chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").trim().to_string()
}

// ---------- combined ----------

/// Everything known about a game's mod releases.
#[derive(Clone, Debug, Default)]
pub struct Feed {
    pub releases: Vec<Release>,
    pub gate: VersionGate,
    pub nexus: Option<NexusFile>,
    pub errors: Vec<String>,
}

impl Feed {
    /// The newest version anyone can get, from any source.
    pub fn latest_version(&self) -> Option<String> {
        let mut best: Option<String> = None;
        let mut consider = |v: &str| {
            if !v.is_empty() && best.as_deref().is_none_or(|b| newer(v, b)) {
                best = Some(v.to_string());
            }
        };
        if let Some(r) = self.releases.first() {
            consider(&r.version);
        }
        if let Some(n) = &self.nexus {
            consider(&n.version);
        }
        if let Some(c) = &self.gate.current {
            consider(c);
        }
        best
    }

    /// Whether `version` is too old to connect, per the author's VERSION file.
    pub fn is_blocked(&self, version: &str) -> bool {
        match &self.gate.current {
            Some(cur) if !version.is_empty() => newer(cur, version),
            _ => false,
        }
    }

    /// The GitHub release of the newest version, if GitHub has it.
    pub fn latest_on_github(&self) -> Option<&Release> {
        let latest = self.latest_version()?;
        self.releases.iter().find(|r| same_version(&r.version, &latest))
    }

    /// What a one-click install/update would get for someone on `installed` ("" = nothing yet):
    /// the newest version if GitHub has it, otherwise the newest GitHub build when that's still an upgrade
    /// (the author sometimes lists a version on Nexus before mirroring it to GitHub).
    pub fn one_click(&self, installed: &str) -> Option<&Release> {
        self.latest_on_github().or_else(|| self.releases.first().filter(|r| installed.is_empty() || newer(&r.version, installed)))
    }
}

pub fn fetch_feed(def: &GameDef, nexus_key: &str) -> Feed {
    let mut feed = Feed::default();
    std::thread::scope(|s| {
        let gh = def.github_releases.map(|repo| s.spawn(move || github_releases(repo, false)));
        let gate = def.version_repo.map(|repo| s.spawn(move || version_gate(repo)));
        let nexus = def
            .nexus
            .filter(|(_, id)| *id != 0 && !nexus_key.trim().is_empty())
            .map(|(domain, id)| s.spawn(move || nexus_latest(domain, id, nexus_key)));
        if let Some(h) = gh {
            match h.join().unwrap_or_else(|_| bail_msg("GitHub check crashed")) {
                Ok(r) => feed.releases = r,
                Err(e) => feed.errors.push(format!("GitHub: {e:#}")),
            }
        }
        if let Some(h) = gate {
            match h.join().unwrap_or_else(|_| bail_msg("version check crashed")) {
                Ok(g) => feed.gate = g,
                Err(e) => feed.errors.push(format!("Version list: {e:#}")),
            }
        }
        if let Some(h) = nexus {
            match h.join().unwrap_or_else(|_| bail_msg("Nexus check crashed")) {
                Ok(n) => feed.nexus = n,
                Err(e) => feed.errors.push(format!("Nexus: {e:#}")),
            }
        }
    });
    feed
}

fn bail_msg<T>(msg: &str) -> Result<T> {
    Err(anyhow::anyhow!(msg.to_string()))
}

/// Streams `url` to `dest` (via a .part file), reporting (done, total).
pub fn download(url: &str, dest: &Path, progress: &dyn Fn(u64, Option<u64>)) -> Result<()> {
    let mut resp = agent().get(url).call().with_context(|| format!("no connection to {}", host(url)))?;
    let status = resp.status().as_u16();
    if status != 200 {
        bail!("download failed: {} answered {status}", host(url));
    }
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let part = dest.with_extension("part");
    let mut out = std::fs::File::create(&part).with_context(|| format!("could not write {}", part.display()))?;
    let mut reader = resp.body_mut().with_config().limit(2 << 30).reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).context("the download was interrupted")?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    out.flush()?;
    drop(out);
    if let Some(t) = total
        && done != t
    {
        bail!("the download was cut short ({done} of {t} bytes)");
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// The helper's own newest release (tag, exe/tarball URL, SHA256SUMS url).
pub struct HelperRelease {
    pub version: String,
    pub asset_url: String,
    pub sums_url: String,
    pub notes: String,
}

pub fn helper_latest(asset_name: &str) -> Result<Option<HelperRelease>> {
    let repo = crate::config::APP_REPO;
    let (status, body) = get_text(&format!("https://api.github.com/repos/{repo}/releases/latest"), &[])?;
    if status == 404 {
        return Ok(None);
    }
    if status == 200 {
        let r: GhRelease = serde_json::from_str(&body)?;
        let find = |n: &str| r.assets.iter().find(|a| a.name == n).map(|a| a.browser_download_url.clone());
        return Ok(find(asset_name).map(|asset_url| HelperRelease {
            version: normalize(&r.tag_name),
            sums_url: find("SHA256SUMS.txt").unwrap_or_default(),
            asset_url,
            notes: r.body.clone().unwrap_or_default(),
        }));
    }
    // Rate-limited: github.com redirects /releases/latest to the tag.
    let resp = agent().get(&format!("https://github.com/{repo}/releases/latest")).config().max_redirects(0).build().call()?;
    let loc = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let Some(tag) = loc.rsplit("/tag/").next().filter(|t| !t.is_empty() && *t != loc) else {
        return Ok(None);
    };
    Ok(Some(HelperRelease {
        version: normalize(tag),
        asset_url: format!("https://github.com/{repo}/releases/download/{tag}/{asset_name}"),
        sums_url: format!("https://github.com/{repo}/releases/download/{tag}/SHA256SUMS.txt"),
        notes: String::new(),
    }))
}

pub fn fetch_text(url: &str) -> Result<String> {
    let (status, body) = get_text(url, &[])?;
    if status != 200 {
        bail!("{} answered {status}", host(url));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version_parts("v2.0.1"), vec![2, 0, 1]);
        assert_eq!(version_parts("2.01"), vec![2, 0, 1]);
        assert_eq!(normalize("0.06"), "0.0.6");
        assert!(newer("2.0.2", "v2.0.1"));
        assert!(newer("2.02", "2.0.1"));
        assert!(!newer("1.9.8", "2.0.1"));
        assert!(same_version("v1.9.8", "1.98"));
        assert!(newer("1.10.0", "1.9.9"));
    }

    #[test]
    fn version_file() {
        let g = parse_version_file("1.17 0\n1.98 0\n2.01 0\n>2.02 1\n");
        assert_eq!(g.current.as_deref(), Some("2.0.2"));
        assert_eq!(g.known.len(), 4);
        let f = Feed { gate: g, ..Default::default() };
        assert!(f.is_blocked("2.0.1"));
        assert!(!f.is_blocked("2.0.2"));
        assert_eq!(f.latest_version().as_deref(), Some("2.0.2"));
        assert!(f.latest_on_github().is_none(), "no GitHub build for 2.0.2 yet");
        let f = Feed { releases: vec![Release { version: "2.0.1".into(), ..Default::default() }], ..f };
        assert_eq!(f.one_click("").map(|r| r.version.as_str()), Some("2.0.1"), "fresh installs get the newest GitHub build");
        assert_eq!(f.one_click("1.9.8").map(|r| r.version.as_str()), Some("2.0.1"));
        assert!(f.one_click("2.0.1").is_none(), "2.0.2 is Nexus-only, so nothing one-click for 2.0.1 users");
        let sek = parse_version_file("0.00 0\n");
        assert!(sek.current.is_none());
    }

    #[test]
    fn html_strip() {
        assert_eq!(strip_html("<p>Fix &amp; tune<br />more</p>"), "Fix & tune\nmore");
    }
}
