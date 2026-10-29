//! Built-in setting presets. Keys are bare ini keys; a preset only touches keys the installed
//! release actually has, so the same preset works across games and mod versions.

pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub values: &'static [(&'static str, &'static str)],
}

pub static PRESETS: &[Preset] = &[
    Preset {
        name: "Relaxed story",
        description: "Explore and chat. No invaders, no death debuffs, enemies barely scale with party size.",
        values: &[
            ("allow_invaders", "0"),
            ("death_debuffs", "0"),
            ("allow_summons", "1"),
            ("enemy_health_scaling", "10"),
            ("enemy_damage_scaling", "0"),
            ("enemy_posture_scaling", "5"),
            ("boss_health_scaling", "35"),
            ("boss_damage_scaling", "0"),
            ("boss_posture_scaling", "5"),
        ],
    },
    Preset {
        name: "Balanced",
        description: "Close to the mod author's defaults, invaders off so the party can focus on co-op.",
        values: &[
            ("allow_invaders", "0"),
            ("death_debuffs", "1"),
            ("enemy_health_scaling", "35"),
            ("enemy_damage_scaling", "0"),
            ("enemy_posture_scaling", "15"),
            ("boss_health_scaling", "100"),
            ("boss_damage_scaling", "0"),
            ("boss_posture_scaling", "20"),
        ],
    },
    Preset {
        name: "Veteran",
        description: "For players who've beaten the game. Tougher enemies and bosses, invaders on.",
        values: &[
            ("allow_invaders", "1"),
            ("death_debuffs", "1"),
            ("enemy_health_scaling", "50"),
            ("enemy_damage_scaling", "10"),
            ("enemy_posture_scaling", "20"),
            ("boss_health_scaling", "130"),
            ("boss_damage_scaling", "15"),
            ("boss_posture_scaling", "25"),
        ],
    },
    Preset {
        name: "Nightmare",
        description: "Brutal. Bosses get double health per extra player and hit noticeably harder.",
        values: &[
            ("allow_invaders", "1"),
            ("death_debuffs", "1"),
            ("allow_summons", "0"),
            ("enemy_health_scaling", "80"),
            ("enemy_damage_scaling", "25"),
            ("enemy_posture_scaling", "30"),
            ("boss_health_scaling", "200"),
            ("boss_damage_scaling", "35"),
            ("boss_posture_scaling", "40"),
        ],
    },
    Preset {
        name: "No scaling",
        description: "Enemies keep their single-player stats no matter how many of you there are.",
        values: &[
            ("enemy_health_scaling", "0"),
            ("enemy_damage_scaling", "0"),
            ("enemy_posture_scaling", "0"),
            ("boss_health_scaling", "0"),
            ("boss_damage_scaling", "0"),
            ("boss_posture_scaling", "0"),
        ],
    },
    Preset {
        name: "PvP night",
        description: "Invaders welcome, ping and level shown above players, voice with invaders on.",
        values: &[
            ("allow_invaders", "1"),
            ("allow_hostile_voice_comms", "1"),
            ("overhead_player_display", "5"),
            ("append_steam_id_to_players", "1"),
        ],
    },
    Preset {
        name: "Streamer / quiet start",
        description: "Skips the intro logos and starts the game quietly until your save loads.",
        values: &[
            ("skip_splash_screens", "1"),
            ("default_boot_master_volume", "2"),
            ("game_boot_volume", "20"),
            ("append_steam_id_to_players", "0"),
        ],
    },
];

/// Applies a preset; returns how many settings changed.
pub fn apply(ini: &mut crate::ini::Ini, values: &[(&str, &str)]) -> usize {
    let before = ini.values();
    for (k, v) in values {
        ini.set_id(k, v);
    }
    let after = ini.values();
    after.iter().filter(|(k, v)| before.get(*k) != Some(v)).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_only_touch_existing_keys() {
        let mut ini = crate::ini::Ini::parse("[GAMEPLAY]\nallow_invaders = 1\n[SCALING]\nboss_health_scaling = 100\n");
        let p = PRESETS.iter().find(|p| p.name == "Relaxed story").unwrap();
        let changed = apply(&mut ini, p.values);
        assert_eq!(changed, 2);
        assert_eq!(ini.to_text(), "[GAMEPLAY]\nallow_invaders = 0\n[SCALING]\nboss_health_scaling = 35\n");
        for p in PRESETS {
            for (k, _) in p.values {
                assert!(!k.contains('.'), "preset keys are bare: {k}");
            }
        }
    }
}
