//! Session codes: one string that carries game, mod version, game version, password and the shared
//! settings, so a friend can paste it and end up with an identical setup.
//! Format: `BL1.<base64url(deflate(json))>.<4 hex checksum>`; it survives being pasted into chat apps.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

const PREFIX: &str = "BL1.";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Session {
    /// Game key (`er`, `ds3`, …).
    pub g: String,
    /// Mod version.
    #[serde(default)]
    pub mv: String,
    /// Game version (exe file version).
    #[serde(default)]
    pub gv: String,
    /// Session password.
    #[serde(default)]
    pub pw: String,
    /// Who made the code.
    #[serde(default)]
    pub n: String,
    /// Shared settings, `section.key` > value.
    #[serde(default)]
    pub s: BTreeMap<String, String>,
}

/// Keys that only affect the local player; they're never sent or compared.
pub fn is_personal(id: &str) -> bool {
    let key = id.rsplit('.').next().unwrap_or(id).to_ascii_lowercase();
    matches!(
        key.as_str(),
        "cooppassword"
            | "mod_language_override"
            | "default_boot_master_volume"
            | "game_boot_volume"
            | "save_file_extension"
            | "skip_splash_screens"
            | "overhead_player_display"
            | "append_steam_id_to_players"
            | "controller_deadzone_fix"
            | "borderless_fullscreen"
            | "relative_player_audio"
            | "no_hollowing_visual_effect"
    )
}

pub fn shared_settings(values: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    values.iter().filter(|(k, _)| !is_personal(k)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

pub fn encode(s: &Session) -> Result<String> {
    let json = serde_json::to_vec(s)?;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&json)?;
    let body = URL_SAFE_NO_PAD.encode(enc.finish()?);
    let sum = &crate::util::sha256_bytes(body.as_bytes())[..4];
    Ok(format!("{PREFIX}{body}.{sum}"))
}

pub fn decode(code: &str) -> Result<Session> {
    let code = code.trim();
    let rest = code.strip_prefix(PREFIX).context("not a BonfireLink code (it should start with BL1.)")?;
    let (body, sum) = rest.rsplit_once('.').context("the code is cut off; copy all of it")?;
    if crate::util::sha256_bytes(body.as_bytes())[..4] != *sum {
        bail!("the code is damaged or incomplete; ask for it again");
    }
    let raw = URL_SAFE_NO_PAD.decode(body).context("the code contains invalid characters")?;
    let mut json = Vec::new();
    flate2::read::DeflateDecoder::new(&raw[..]).take(1 << 20).read_to_end(&mut json).context("the code is damaged")?;
    let s: Session = serde_json::from_slice(&json).context("the code is from a newer BonfireLink")?;
    if crate::games::by_key(&s.g).is_none() {
        bail!("the code is for a game this version doesn't know ({})", s.g);
    }
    Ok(s)
}

/// Finds a code anywhere in pasted text (a whole Discord message works).
pub fn find_code(text: &str) -> Option<String> {
    let start = text.find(PREFIX)?;
    let code: String = text[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect();
    Some(code.trim_end_matches('.').to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Info,
    Warn,
    Block,
}

pub struct Row {
    pub what: String,
    pub mine: String,
    pub theirs: String,
    pub severity: Severity,
    pub hint: String,
}

pub fn compare(mine: &Session, theirs: &Session) -> Vec<Row> {
    let mut rows = Vec::new();
    let row = |rows: &mut Vec<Row>, what: &str, a: &str, b: &str, bad: Severity, hint: &str| {
        let sev = if a == b { Severity::Ok } else { bad };
        rows.push(Row {
            what: what.into(),
            mine: a.into(),
            theirs: b.into(),
            severity: sev,
            hint: if sev == Severity::Ok { String::new() } else { hint.into() },
        });
    };
    row(&mut rows, "Game", &mine.g, &theirs.g, Severity::Block, "This code is for another game.");
    row(
        &mut rows,
        "Mod version",
        or_dash(&mine.mv),
        or_dash(&theirs.mv),
        Severity::Block,
        "Seamless Co-op only connects players on the same mod version. Update both of you to the newest.",
    );
    row(
        &mut rows,
        "Game version",
        or_dash(&mine.gv),
        or_dash(&theirs.gv),
        Severity::Warn,
        "Different game patches usually can't connect. Let Steam update the game on both PCs.",
    );
    let pw_ok = !mine.pw.is_empty() && mine.pw == theirs.pw;
    rows.push(Row {
        what: "Password".into(),
        mine: mask(&mine.pw),
        theirs: mask(&theirs.pw),
        severity: if pw_ok { Severity::Ok } else { Severity::Block },
        hint: if pw_ok {
            String::new()
        } else {
            "Passwords must match exactly (and can't be empty). \"Use their settings\" copies it.".into()
        },
    });
    let keys: std::collections::BTreeSet<_> = mine.s.keys().chain(theirs.s.keys()).cloned().collect();
    for k in keys {
        if is_personal(&k) {
            continue;
        }
        let a = mine.s.get(&k).map(String::as_str).unwrap_or("—");
        let b = theirs.s.get(&k).map(String::as_str).unwrap_or("—");
        let label = crate::ini::label_for(k.rsplit('.').next().unwrap_or(&k));
        row(
            &mut rows,
            &label,
            a,
            b,
            Severity::Info,
            "Different setting. Matching them avoids surprises; the host's world rules apply in their session.",
        );
    }
    rows
}

fn or_dash(s: &str) -> &str {
    if s.is_empty() { "unknown" } else { s }
}

fn mask(pw: &str) -> String {
    if pw.is_empty() {
        "(empty)".into()
    } else {
        let shown: String = pw.chars().take(2).collect();
        format!("{shown}{}", "•".repeat(pw.chars().count().saturating_sub(2).min(12)))
    }
}

/// A readable password like `ashen-moonveil-47`.
pub fn generate_password() -> String {
    const A: &[&str] = &[
        "ashen", "hollow", "golden", "crimson", "frozen", "silent", "ember", "lunar", "abyssal", "radiant", "pale", "cursed", "sunlit",
        "scarlet", "astral", "sacred", "feral", "gilded", "molten", "twilight",
    ];
    const N: &[&str] = &[
        "bonfire",
        "knight",
        "estus",
        "tarnished",
        "pontiff",
        "dragon",
        "shinobi",
        "moonveil",
        "lantern",
        "chalice",
        "covenant",
        "grace",
        "ember",
        "sentinel",
        "wolf",
        "tree",
        "phantom",
        "throne",
        "spire",
        "omen",
    ];
    let mut seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(7) as u64;
    seed ^= (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut next = || {
        // xorshift64*
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let a = A[(next() % A.len() as u64) as usize];
    let n = N[(next() % N.len() as u64) as usize];
    let d = next() % 90 + 10;
    format!("{a}-{n}-{d}")
}

/// The text people paste into Discord/Steam chat.
pub fn friend_message(game_name: &str, s: &Session, code: &str) -> String {
    let who = if s.n.is_empty() { "me".to_string() } else { s.n.clone() };
    format!(
        "Join {who} in {game_name} Seamless Co-op!\n\
         • Mod version: {}\n\
         • Game version: {}\n\
         • Password: {}\n\
         Easiest: get BonfireLink (https://github.com/{}) > {game_name} > Play together > paste this message.\n\
         {code}",
        or_dash(&s.mv),
        or_dash(&s.gv),
        if s.pw.is_empty() { "(not set)" } else { &s.pw },
        crate::config::APP_REPO
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        let mut s = BTreeMap::new();
        s.insert("SCALING.boss_health_scaling".into(), "100".into());
        s.insert("GAMEPLAY.allow_invaders".into(), "0".into());
        Session { g: "er".into(), mv: "2.0.1".into(), gv: "2.6.0.0".into(), pw: "ashen-grace-12".into(), n: "Jakub".into(), s }
    }

    #[test]
    fn code_roundtrip_and_find() {
        let code = encode(&sample()).unwrap();
        assert!(code.starts_with("BL1."));
        assert_eq!(decode(&code).unwrap(), sample());
        let msg = friend_message("Elden Ring", &sample(), &code);
        assert_eq!(find_code(&msg).unwrap(), code);
        assert_eq!(decode(&find_code(&format!("hey: {code}. see you")).unwrap()).unwrap(), sample());
    }

    #[test]
    fn damaged_code_is_rejected() {
        let code = encode(&sample()).unwrap();
        let mut broken = code.clone();
        broken.insert(10, 'x');
        assert!(decode(&broken).is_err());
        assert!(decode(&code[..code.len() - 3]).is_err());
    }

    #[test]
    fn compare_flags_blockers() {
        let a = sample();
        let mut b = sample();
        b.mv = "2.0.0".into();
        b.pw = "other".into();
        b.s.insert("SCALING.boss_health_scaling".into(), "200".into());
        let rows = compare(&a, &b);
        let sev = |w: &str| rows.iter().find(|r| r.what == w).unwrap().severity;
        assert_eq!(sev("Mod version"), Severity::Block);
        assert_eq!(sev("Password"), Severity::Block);
        assert_eq!(sev("Game"), Severity::Ok);
        assert_eq!(sev("Boss health per extra player"), Severity::Info);
    }

    #[test]
    fn passwords_look_right() {
        let p = generate_password();
        assert_eq!(p.split('-').count(), 3);
        assert!(p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }
}
