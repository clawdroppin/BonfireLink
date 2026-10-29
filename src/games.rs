//! Static facts about every supported game and its Seamless Co-op mod.
//! Adding a game = adding one entry to `GAMES` (plus a tab icon colour); everything else is data-driven.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub enum GameId {
    EldenRing,
    DarkSouls3,
    DarkSouls2,
    DarkSoulsRemastered,
    Sekiro,
}

/// Where a game keeps its saves on Windows (on Linux the same path lives inside the Proton prefix).
#[derive(Clone, Copy, Debug)]
pub enum SaveRoot {
    /// `%APPDATA%\<dir>`
    AppData(&'static str),
    /// `Documents\<dir>`
    Documents(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The mod is out; downloads are possible.
    Released,
    /// Announced by the author but not out yet. The helper watches for the first release.
    Upcoming,
}

pub struct GameDef {
    pub id: GameId,
    /// Stable lowercase key used in file names, CLI args and session codes.
    pub key: &'static str,
    pub name: &'static str,
    pub short: &'static str,
    pub steam_app_id: u32,
    /// Folder names under `steamapps/common` (first is the usual one).
    pub install_dirs: &'static [&'static str],
    /// Folder (relative to the install dir) that holds the game exe and where the mod goes.
    pub exe_subdir: &'static str,
    pub game_exe: &'static str,
    /// The exe Steam starts when you press Play (Elden Ring goes through the EAC bootstrapper).
    pub steam_exe: &'static str,
    /// Expected mod file names. Archives are also probed generically (`*_launcher.exe`,
    /// `SeamlessCoop/*_settings.ini`) so a renamed file in a future release still installs.
    pub launcher: &'static str,
    pub dll: &'static str,
    pub ini: &'static str,
    /// GitHub repo with downloadable releases (`owner/name`), if the author publishes there.
    pub github_releases: Option<&'static str>,
    /// GitHub repo whose `VERSION` file lists which mod versions may connect (">X.YZ 1" = current).
    pub version_repo: Option<&'static str>,
    /// Nexus Mods game domain and mod id.
    pub nexus: Option<(&'static str, u32)>,
    pub save_root: SaveRoot,
    /// Save file name without extension, e.g. `ER0000`.
    pub save_stem: &'static str,
    pub availability: Availability,
    /// RGB accent for the tab.
    pub accent: [u8; 3],
    pub blurb: &'static str,
    /// Extra hints shown on the game's overview page.
    pub notes: &'static [&'static str],
}

impl GameDef {
    pub fn nexus_url(&self) -> Option<String> {
        self.nexus.map(|(domain, id)| format!("https://www.nexusmods.com/{domain}/mods/{id}"))
    }
    pub fn nexus_files_url(&self) -> Option<String> {
        self.nexus_url().map(|u| format!("{u}?tab=files"))
    }
    pub fn github_url(&self) -> Option<String> {
        self.github_releases.or(self.version_repo).map(|r| format!("https://github.com/{r}"))
    }
    pub fn released(&self) -> bool {
        self.availability == Availability::Released
    }
}

pub static GAMES: &[GameDef] = &[
    GameDef {
        id: GameId::EldenRing,
        key: "er",
        name: "Elden Ring",
        short: "ER",
        steam_app_id: 1245620,
        install_dirs: &["ELDEN RING"],
        exe_subdir: "Game",
        game_exe: "eldenring.exe",
        steam_exe: "start_protected_game.exe",
        launcher: "ersc_launcher.exe",
        dll: "ersc.dll",
        ini: "ersc_settings.ini",
        github_releases: Some("yuiamoroll/EldenRingSeamlessCoopRelease"),
        version_repo: Some("yuiamoroll/EldenRingSeamlessCoopRelease"),
        nexus: Some(("eldenring", 510)),
        save_root: SaveRoot::AppData("EldenRing"),
        save_stem: "ER0000",
        availability: Availability::Released,
        accent: [212, 175, 55],
        blurb: "Explore the Lands Between together: no fog walls, no disconnects after bosses, shared progression.",
        notes: &[
            "Always start co-op through the Seamless launcher (BonfireLink's Play button does this). Easy Anti-Cheat stays off, so you can't be banned for playing online with the mod.",
            "Co-op saves use their own file (ER0000.co2), so your vanilla save (ER0000.sl2) is never touched by the mod.",
        ],
    },
    GameDef {
        id: GameId::DarkSouls3,
        key: "ds3",
        name: "Dark Souls III",
        short: "DS3",
        steam_app_id: 374320,
        install_dirs: &["DARK SOULS III"],
        exe_subdir: "Game",
        game_exe: "DarkSoulsIII.exe",
        steam_exe: "DarkSoulsIII.exe",
        launcher: "ds3sc_launcher.exe",
        dll: "ds3sc.dll",
        ini: "ds3sc_settings.ini",
        github_releases: None,
        version_repo: Some("yuiamoroll/DarkSouls3SeamlessCoopRelease"),
        nexus: Some(("darksouls3", 1895)),
        save_root: SaveRoot::AppData("DarkSoulsIII"),
        save_stem: "DS30000",
        availability: Availability::Released,
        accent: [200, 90, 50],
        blurb: "Link the fire together: drop-in co-op across Lothric with no summon signs or ember requirements.",
        notes: &[
            "The author publishes Dark Souls III builds on Nexus Mods only. With a Nexus Premium API key BonfireLink downloads them for you; otherwise press \"Get it from Nexus\" and BonfireLink installs the file as soon as it lands in your Downloads folder.",
        ],
    },
    GameDef {
        id: GameId::DarkSouls2,
        key: "ds2",
        name: "Dark Souls II: SotFS",
        short: "DS2",
        steam_app_id: 335300,
        install_dirs: &["Dark Souls II Scholar of the First Sin", "DARK SOULS II Scholar of the First Sin"],
        exe_subdir: "Game",
        game_exe: "DarkSoulsII.exe",
        steam_exe: "DarkSoulsII.exe",
        launcher: "ds2sc_launcher.exe",
        dll: "ds2sc.dll",
        ini: "ds2sc_settings.ini",
        github_releases: Some("yuiamoroll/DarkSouls2SeamlessCoopRelease"),
        version_repo: Some("yuiamoroll/DarkSouls2SeamlessCoopRelease"),
        nexus: Some(("darksouls2", 1468)),
        save_root: SaveRoot::AppData("DarkSoulsII"),
        save_stem: "DS2SOFS0000",
        availability: Availability::Released,
        accent: [70, 140, 200],
        blurb: "Seek power in Drangleic as a group. Scholar of the First Sin (the Steam edition) is required.",
        notes: &[
            "Only Scholar of the First Sin is supported, not the original 2014 release.",
            "The mod is young (0.0.x) and updates often; keep \"Check for updates\" on.",
        ],
    },
    GameDef {
        id: GameId::DarkSoulsRemastered,
        key: "dsr",
        name: "Dark Souls Remastered",
        short: "DSR",
        steam_app_id: 570940,
        install_dirs: &["DARK SOULS REMASTERED"],
        exe_subdir: "",
        game_exe: "DarkSoulsRemastered.exe",
        steam_exe: "DarkSoulsRemastered.exe",
        launcher: "ds1sc_launcher.exe",
        dll: "ds1sc.dll",
        ini: "ds1sc_settings.ini",
        github_releases: None,
        version_repo: Some("yuiamoroll/DarkSouls1SeamlessCoopRelease"),
        nexus: Some(("darksoulsremastered", 899)),
        save_root: SaveRoot::Documents("NBGI/DARK SOULS REMASTERED"),
        save_stem: "DRAKS0005",
        availability: Availability::Released,
        accent: [230, 130, 40],
        blurb: "Praise the sun with friends in Lordran. Works with Dark Souls Remastered on Steam.",
        notes: &[
            "Published on Nexus Mods only. A Nexus Premium API key lets BonfireLink download it; otherwise use \"Get it from Nexus\" and BonfireLink picks the file up from Downloads.",
            "After loading in, rest at the nearest bonfire to receive the mod's items.",
        ],
    },
    GameDef {
        id: GameId::Sekiro,
        key: "sekiro",
        name: "Sekiro",
        short: "SEK",
        steam_app_id: 814380,
        install_dirs: &["Sekiro"],
        exe_subdir: "",
        game_exe: "sekiro.exe",
        steam_exe: "sekiro.exe",
        launcher: "sksc_launcher.exe",
        dll: "sksc.dll",
        ini: "sksc_settings.ini",
        github_releases: Some("yuiamoroll/SekiroSeamlessCoopRelease"),
        version_repo: Some("yuiamoroll/SekiroSeamlessCoopRelease"),
        nexus: Some(("sekiro", 0)),
        save_root: SaveRoot::AppData("Sekiro"),
        save_stem: "S0000",
        availability: Availability::Upcoming,
        accent: [190, 40, 50],
        blurb: "Seamless Co-op for Sekiro is announced by the author but not released yet. BonfireLink watches the official repo and installs it the day it ships.",
        notes: &[
            "Until then, \"Install from file\" accepts any Seamless-style archive, and the configurator reads whatever settings file it ships with.",
            "Sekiro Online (an older PvP/co-op mod by the same author) is a different, dinput8-based mod; BonfireLink doesn't manage it.",
        ],
    },
];

pub fn get(id: GameId) -> &'static GameDef {
    GAMES.iter().find(|g| g.id == id).expect("every GameId has a GameDef")
}

pub fn by_key(key: &str) -> Option<&'static GameDef> {
    let key = key.trim().to_ascii_lowercase();
    GAMES.iter().find(|g| g.key == key || g.short.eq_ignore_ascii_case(&key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_lookup_works() {
        for g in GAMES {
            assert_eq!(by_key(g.key).unwrap().id, g.id);
            assert_eq!(get(g.id).key, g.key);
            assert!(g.launcher.ends_with("_launcher.exe"));
            assert!(g.ini.ends_with("_settings.ini"));
        }
        let mut keys: Vec<_> = GAMES.iter().map(|g| g.key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), GAMES.len());
    }
}
