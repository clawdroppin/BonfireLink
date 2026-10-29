//! The Seamless Co-op settings file (`*_settings.ini`): a format-preserving editor plus a schema
//! inferred from the mod's own comments, so new options in future releases show up automatically.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
enum Line {
    Section(String),
    /// `raw` is the original line; it's written back untouched until the value changes.
    Pair {
        key: String,
        value: String,
        raw: Option<String>,
    },
    Other(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ini {
    lines: Vec<Line>,
    /// "\r\n" when the original file used it.
    newline: &'static str,
    /// Whether the original text ended with a newline.
    trailing_newline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Bool,
    /// (value, label)
    Choice(Vec<(String, String)>),
    Range {
        min: i64,
        max: i64,
        suffix: &'static str,
    },
    Int,
    Password,
    Text,
    Language,
}

#[derive(Clone, Debug)]
pub struct Field {
    pub section: String,
    pub key: String,
    pub label: String,
    pub help: String,
    pub kind: Kind,
}

impl Field {
    pub fn id(&self) -> String {
        format!("{}.{}", self.section, self.key)
    }
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let trailing_newline = text.is_empty() || text.ends_with('\n');
        let lines = text
            .lines()
            .map(|l| {
                let t = l.trim();
                if t.starts_with('[') && t.ends_with(']') {
                    Line::Section(t[1..t.len() - 1].trim().to_string())
                } else if !t.starts_with(';') && !t.starts_with('#') && t.contains('=') {
                    let eq = l.find('=').unwrap();
                    let key = l[..eq].trim().to_string();
                    let value = l[eq + 1..].trim().to_string();
                    Line::Pair { key, value, raw: Some(l.to_string()) }
                } else {
                    Line::Other(l.to_string())
                }
            })
            .collect();
        Ini { lines, newline, trailing_newline }
    }

    pub fn to_text(&self) -> String {
        let nl = if self.newline.is_empty() { "\n" } else { self.newline };
        let mut out = String::new();
        for (i, l) in self.lines.iter().enumerate() {
            match l {
                Line::Section(s) => out.push_str(&format!("[{s}]")),
                Line::Pair { raw: Some(raw), .. } => out.push_str(raw),
                Line::Pair { key, value, raw: None } => out.push_str(&format!("{key} = {value}")),
                Line::Other(s) => out.push_str(s),
            }
            if i + 1 < self.lines.len() || self.trailing_newline {
                out.push_str(nl);
            }
        }
        out
    }

    /// (section, key, value) in file order.
    pub fn entries(&self) -> Vec<(String, String, String)> {
        let mut section = String::new();
        let mut out = Vec::new();
        for l in &self.lines {
            match l {
                Line::Section(s) => section = s.clone(),
                Line::Pair { key, value, .. } => out.push((section.clone(), key.clone(), value.clone())),
                Line::Other(_) => {}
            }
        }
        out
    }

    pub fn values(&self) -> BTreeMap<String, String> {
        self.entries().into_iter().map(|(s, k, v)| (format!("{s}.{k}"), v)).collect()
    }

    pub fn get(&self, section: &str, key: &str) -> Option<String> {
        self.entries().into_iter().find(|(s, k, _)| s.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key)).map(|(_, _, v)| v)
    }

    /// Looks a key up in any section (keys are unique across sections in every Seamless ini).
    pub fn get_any(&self, key: &str) -> Option<String> {
        self.entries().into_iter().find(|(_, k, _)| k.eq_ignore_ascii_case(key)).map(|(_, _, v)| v)
    }

    /// Sets an existing key; returns false if the key doesn't exist (we never invent keys the mod doesn't know).
    pub fn set(&mut self, section: &str, key: &str, new_value: &str) -> bool {
        let mut cur = String::new();
        for l in &mut self.lines {
            match l {
                Line::Section(s) => cur = s.clone(),
                Line::Pair { key: k, value, raw } if cur.eq_ignore_ascii_case(section) && k.eq_ignore_ascii_case(key) => {
                    let v = sanitize(new_value);
                    if *value != v {
                        *value = v;
                        *raw = None;
                    }
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// `section.key` or a bare `key` (first match in any section).
    pub fn set_id(&mut self, id: &str, value: &str) -> bool {
        match id.split_once('.') {
            Some((s, k)) => self.set(s, k, value),
            None => {
                let section = self.entries().into_iter().find(|(_, k, _)| k.eq_ignore_ascii_case(id)).map(|(s, _, _)| s);
                section.is_some_and(|s| self.set(&s, id, value))
            }
        }
    }

    /// Takes the user's values from `old` for every key that still exists in `self` (a newer default file).
    /// Returns keys that disappeared and keys that are new in this release.
    pub fn merge_user_values(&mut self, old: &Ini) -> (Vec<String>, Vec<String>) {
        let new_keys: Vec<_> = self.entries().into_iter().map(|(s, k, _)| (s, k)).collect();
        let mut gone = Vec::new();
        for (s, k, v) in old.entries() {
            if !self.set(&s, &k, &v) {
                // Section renamed between releases: fall back to the key alone.
                if !self.set_id(&k, &v) {
                    gone.push(format!("{s}.{k}"));
                }
            }
        }
        let old_keys: Vec<_> = old.entries().into_iter().map(|(_, k, _)| k.to_lowercase()).collect();
        let added = new_keys.into_iter().filter(|(_, k)| !old_keys.contains(&k.to_lowercase())).map(|(s, k)| format!("{s}.{k}")).collect();
        (gone, added)
    }

    /// Builds editable fields from the comments above each key.
    pub fn schema(&self) -> Vec<Field> {
        let mut out = Vec::new();
        let mut section = String::new();
        let mut comments: Vec<String> = Vec::new();
        for l in &self.lines {
            match l {
                Line::Section(s) => {
                    section = s.clone();
                    comments.clear();
                }
                Line::Other(s) => {
                    let t = s.trim();
                    if let Some(c) = t.strip_prefix(';').or_else(|| t.strip_prefix('#')) {
                        comments.push(c.trim().to_string());
                    } else if t.is_empty() && comments.last().is_some_and(|c| c.is_empty()) {
                        // keep paragraphs together, collapse runs of blank lines
                    }
                }
                Line::Pair { key, value, .. } => {
                    let help = comments.join("\n").trim().to_string();
                    let kind = infer_kind(key, value, &help);
                    out.push(Field { section: section.clone(), key: key.clone(), label: label_for(key), help: tidy_help(&help), kind });
                    comments.clear();
                }
            }
        }
        out
    }
}

fn sanitize(v: &str) -> String {
    // A newline would split the line and corrupt the file.
    v.replace(['\r', '\n'], " ").trim().to_string()
}

fn tidy_help(help: &str) -> String {
    help.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// Friendly names for keys we know; anything else is derived from the key itself.
pub fn label_for(key: &str) -> String {
    let known = match key.to_ascii_lowercase().as_str() {
        "cooppassword" => "Session password",
        "allow_invaders" => "Allow invaders",
        "allow_hostile_voice_comms" => "Voice chat with invaders",
        "death_debuffs" => "Death debuffs",
        "allow_summons" => "Allow spirit / NPC summons",
        "overhead_player_display" => "Above-player display",
        "skip_splash_screens" => "Skip intro logos",
        "append_steam_id_to_players" => "Show Steam IDs above players",
        "always_spectate_on_death" => "Always spectate after death",
        "default_boot_master_volume" => "Volume before a save loads",
        "game_boot_volume" => "Volume before a save loads",
        "enemy_health_scaling" => "Enemy health per extra player",
        "enemy_damage_scaling" => "Enemy damage per extra player",
        "enemy_posture_scaling" => "Enemy poise per extra player",
        "boss_health_scaling" => "Boss health per extra player",
        "boss_damage_scaling" => "Boss damage per extra player",
        "boss_posture_scaling" => "Boss poise per extra player",
        "save_file_extension" => "Co-op save file extension",
        "mod_language_override" => "Mod language",
        "grave_enemy_phantom_param" => "Highlight powerful enemies",
        "controller_deadzone_fix" => "Radial controller deadzone",
        "borderless_fullscreen" => "Borderless instead of fullscreen",
        "relative_player_audio" => "Audio from the player's position",
        "no_hollowing_visual_effect" => "Hide the hollowing look",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let mut s = key.replace('_', " ");
    if let Some(first) = s.get(..1) {
        s = first.to_uppercase() + &s[1..];
    }
    s
}

fn infer_kind(key: &str, value: &str, help: &str) -> Kind {
    let k = key.to_ascii_lowercase();
    let h = help.to_ascii_lowercase();
    if k.contains("password") {
        return Kind::Password;
    }
    if k == "mod_language_override" {
        return Kind::Language;
    }
    if k == "save_file_extension" {
        return Kind::Text;
    }
    let is_num = value.trim().parse::<i64>().is_ok() || value.trim().is_empty();
    if !is_num {
        return Kind::Text;
    }
    let compact: String = h.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("0=false") && compact.contains("1=true")
        || compact.contains("1=on") && compact.contains("0=off")
        || compact.contains("0=no") && compact.contains("1=yes")
    {
        return Kind::Bool;
    }
    if k.ends_with("_scaling") {
        return Kind::Range { min: 0, max: 400, suffix: "%" };
    }
    // "Range = 0 - 100" or "0 = MUTE 10 = MAX"
    if let Some(r) = h.find("range") {
        let nums = numbers(&h[r..]);
        if nums.len() >= 2 && nums[1] > nums[0] {
            return Kind::Range { min: nums[0], max: nums[1], suffix: "" };
        }
    }
    if k.contains("volume") {
        let nums = numbers(&h);
        let max = nums.iter().copied().max().unwrap_or(10).max(1);
        return Kind::Range { min: 0, max, suffix: "" };
    }
    let choices = parse_choices(help);
    if choices.len() >= 2 {
        return Kind::Choice(choices);
    }
    Kind::Int
}

fn numbers(s: &str) -> Vec<i64> {
    s.split(|c: char| !c.is_ascii_digit()).filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect()
}

/// Finds "N = label" options, whether separated by `|` or written one per line.
fn parse_choices(help: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in help.split(['|', '\n']) {
        let part = part.trim().trim_start_matches(';').trim();
        // May contain a leading sentence: "... display. 0 = Normal"
        let Some(eq) = part.find('=') else { continue };
        let left = part[..eq].trim();
        let num: String = left.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
        if num.is_empty() {
            continue;
        }
        let label = part[eq + 1..].trim().trim_end_matches('.').to_string();
        if label.is_empty() || out.iter().any(|(v, _)| *v == num) {
            continue;
        }
        out.push((num, capitalize(&label)));
    }
    out
}

fn capitalize(s: &str) -> String {
    let lower = if s.chars().filter(|c| c.is_alphabetic()).all(|c| c.is_uppercase()) { s.to_lowercase() } else { s.to_string() };
    let mut c = lower.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ER: &str = "[GAMEPLAY]\r\n\r\n; Invaders are other players.  0=FALSE  1=TRUE\r\nallow_invaders = 1\r\n\r\n; 0 = Normal | 1 = None | 2 = Display player ping | 3 = Display player soul level\r\noverhead_player_display = 0\r\n\r\n; Game volume before initial save load. 0 = MUTE 10 = MAX\r\ndefault_boot_master_volume = 5\r\n\r\n[SCALING]\r\n\r\n; Amount of enemy health (%) per player\r\nenemy_health_scaling = 35\r\n\r\n[PASSWORD]\r\n\r\n; Session password\r\ncooppassword = \r\n\r\n[SAVE]\r\nsave_file_extension = co2\r\n";

    const DS2: &str = "[GAMEPLAY]\n; Whether to allow invaders (hostile players) into your world. 1 = ON | 0 = OFF\nallow_invaders = 1\n; The text to display above other player's characters:\n; 0 = Generic\n; 1 = No overhead display\n; 2 = Display player ping\noverhead_player_display = 2\n; The volume at which the game will start in. Range = 0 - 100\ngame_boot_volume = 50\n";

    #[test]
    fn roundtrip_is_byte_exact() {
        assert_eq!(Ini::parse(ER).to_text(), ER);
        assert_eq!(Ini::parse(DS2).to_text(), DS2);
        let odd = "[A]\r\nx = 0 \r\npw = secret \r\nlast =";
        assert_eq!(Ini::parse(odd).to_text(), odd);
        let mut ini = Ini::parse(odd);
        ini.set("A", "x", "0");
        assert_eq!(ini.to_text(), odd, "setting the same value keeps the line");
        ini.set("A", "x", "1");
        assert_eq!(ini.to_text(), "[A]\r\nx = 1\r\npw = secret \r\nlast =");
    }

    #[test]
    fn schema_inference() {
        let f = Ini::parse(ER).schema();
        let kind = |k: &str| f.iter().find(|x| x.key == k).unwrap().kind.clone();
        assert_eq!(kind("allow_invaders"), Kind::Bool);
        assert_eq!(kind("cooppassword"), Kind::Password);
        assert_eq!(kind("default_boot_master_volume"), Kind::Range { min: 0, max: 10, suffix: "" });
        assert_eq!(kind("enemy_health_scaling"), Kind::Range { min: 0, max: 400, suffix: "%" });
        match kind("overhead_player_display") {
            Kind::Choice(c) => {
                assert_eq!(c.len(), 4);
                assert_eq!(c[2], ("2".into(), "Display player ping".into()));
            }
            other => panic!("{other:?}"),
        }
        let g = Ini::parse(DS2).schema();
        let kind = |k: &str| g.iter().find(|x| x.key == k).unwrap().kind.clone();
        assert_eq!(kind("allow_invaders"), Kind::Bool);
        assert_eq!(kind("game_boot_volume"), Kind::Range { min: 0, max: 100, suffix: "" });
        assert!(matches!(kind("overhead_player_display"), Kind::Choice(c) if c.len() == 3));
    }

    #[test]
    fn set_and_merge() {
        let mut ini = Ini::parse(ER);
        assert!(ini.set("PASSWORD", "cooppassword", "moon\nlight"));
        assert_eq!(ini.get("password", "COOPPASSWORD").unwrap(), "moon light");
        assert!(!ini.set("PASSWORD", "nope", "x"));
        let mut newer = Ini::parse(&ER.replace("[SAVE]", "[GAMEPLAY2]\r\nnew_option = 1\r\n[SAVE]"));
        let (gone, added) = newer.merge_user_values(&ini);
        assert!(gone.is_empty());
        assert_eq!(added, vec!["GAMEPLAY2.new_option".to_string()]);
        assert_eq!(newer.get_any("cooppassword").unwrap(), "moon light");
    }
}
