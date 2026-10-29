# Changelog

## 1.0.0 (2026-10-29)

First release.

- Tabs for Elden Ring, Dark Souls III, Dark Souls II: SotFS, Dark Souls Remastered and Sekiro (watching for release),
  each with Overview, Configure, Play together, Saves, Versions and Troubleshoot.
- Automatic update checks: GitHub releases (checksum verified, HTML fallback when the API is rate-limited), the
  author's VERSION lists, and Nexus Mods (API key). One-click install/update from GitHub; Nexus Premium downloads;
  "Get it from Nexus" installs the file the moment it reaches the Downloads folder. When the newest version is
  Nexus-only, fresh installs still get the newest GitHub build.
- Nexus account link: opens the API-key page, paste + check in the app (Gaming Mode friendly). Premium keys make
  Nexus installs/updates fully automatic (also from the Deck launch wrapper). Free accounts can let BonfireLink handle
  Nexus "Mod Manager Download" buttons (opt-in, previous handler restored), so one click on Nexus installs the file
  even when BonfireLink is closed.
- Settings editor built from the mod's own ini comments (toggles, dropdowns, sliders with 2P/3P/4P multipliers,
  password generator, language list), raw text mode, per-option reset, mod defaults, 7 built-in presets plus custom
  presets. Untouched lines are kept byte-for-byte; saving after a mod update merges instead of overwriting.
- Play together: `BL1.` session codes with version/password/settings comparison and one-click apply or update.
- Saves: backups before installs (and optionally before every launch), manual backups, restore with undo, extra
  backup folder, vanilla ⇄ co-op save copy.
- Versions: release notes, install any GitHub release, rollback snapshots, install from .zip/.7z.
- Adopts hand-installed mods and identifies their version by hashing the DLL against official archives.
- Steam Deck: launch wrapper + Steam launch options (one Steam restart), auto-update before the game starts, Mods off
  without touching Steam, "Add BonfireLink to Steam", one-line installer script.
- Troubleshoot: diagnostics, conflict detection, game-patch alert, FAQ, bug report zip (password removed),
  Windows desktop shortcut, game-folder override.
- Crash safety: panics are written to the log; fixed a crash when opening the Overview of a game that isn't installed.
- Portable data folder, self-update with SHA-256 check, headless CLI (`--status`, `--update`, `--auto-update`,
  `--print-wrapper`).
