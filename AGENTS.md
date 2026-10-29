# AGENTS.md: guide for AI coding agents (and humans)

This repo is **BonfireLink**: a single-binary Rust GUI (egui/eframe 0.37) that installs, updates, configures and
troubleshoots LukeYui's **Seamless Co-op** mods for Elden Ring, Dark Souls III, Dark Souls II: SotFS,
Dark Souls Remastered and Sekiro (announced, not released yet).
**Targets: Windows (portable .exe) and Steam Deck (SteamOS, games under Proton).** The end user is a gamer, not a
developer: every change must keep the one-button flows working without a terminal.

## Ground rules

- **Never break the one-click paths**: game tab → *Install* → *Play co-op* on Windows, and on the Deck
  Home → *Set launch options* once, then Steam's Play button (also in Gaming Mode).
- **Portable**: no installer, no runtime dependencies. Data lives in `BonfireLink-data/` next to the exe
  (`config::data_dir()`), falling back to the user config dir when that isn't writable. `BONFIRELINK_DATA` overrides it
  (tests and demos use this).
- **Never modify the mod's own files** beyond copying a release into the game folder. Only the user's
  `*_settings.ini` is edited, and only through `ini::Ini`, which keeps untouched lines byte-for-byte.
- **Saves are touched only through backup / restore / transfer**, and each of those backs up first.
- **Steam config edits require Steam to be closed** (it rewrites `localconfig.vdf` / `shortcuts.vdf` on exit). Back up
  the file first (`steam::edit_launch_options` does). **Never restart Steam from Gaming Mode** (`steam::in_gaming_mode()`):
  it kills the Deck's session.
- **Linux binary: no new link-time system libraries.** Prefer pure-Rust crates; anything needing GTK/OpenSSL at link
  time is out. `scripts/check_elf.py` must pass (libc/libm/libdl/libpthread, glibc ≤ 2.28).
- **Downloads only from official sources**: the author's GitHub releases (`yuiamoroll/*SeamlessCoopRelease`) or
  Nexus Mods. Verify SHA-256 when GitHub provides an asset digest.
- UI copy is plain English for a gamer: say what to do, not what failed internally. Avoid glyphs the default egui
  fonts lack (for example "→" renders as a box; use ">").
- Match the existing style: small modules, `anyhow` errors with user-readable context, comments only where the *why*
  isn't obvious. `cargo fmt` (see `rustfmt.toml`) and `cargo clippy --all-targets` must stay clean.

## Layout

| File | Responsibility |
|---|---|
| `src/main.rs` | Window setup (maximized on Deck), headless CLI (`--status`, `--update`, `--auto-update`, `--print-wrapper`). |
| `src/games.rs` | **The game table** (`GAMES`): Steam app ids, folders, exe/launcher/dll/ini names, release repos, Nexus ids, save locations, notes, accent colours. Adding a game is mostly one entry here. |
| `src/app.rs` | All GUI pages, the job runner (one background thread at a time), confirm modals, styling, caches. |
| `src/ops.rs` | Operations: status scan, install/update/rollback/uninstall, adopt hand installs, identify versions by DLL hash, Nexus Downloads watcher, Deck launch wrapper + launch options, launch, saves (backup/restore/transfer), shortcuts, diagnostics, bug report, headless auto-update. Has the full install-cycle integration test. |
| `src/steam.rs` | Steam roots & libraries (cached 30 s), app manifests, Proton prefix & version, launch-option merge/strip, process detection, Steam shutdown/start, Deck & Gaming Mode detection. |
| `src/remote.rs` | GitHub releases API (+ Atom/HTML fallback when rate-limited), author `VERSION` files, Nexus API, downloads, helper self-update lookup, version comparison. |
| `src/ini.rs` | Format-preserving INI editor + schema inference from the mod's comments (bool / choice / range / password / language). |
| `src/presets.rs` | Built-in presets (bare keys; only existing keys are touched). |
| `src/nxm.rs` | Nexus `nxm://` links: parsing, opt-in handler registration (HKCU\Software\Classes\nxm on Windows, `bonfirelink-nxm.desktop` + xdg-mime on Linux, previous handler restored on opt-out), the `inbox/` hand-off from a browser-started process to the open window (`gui.pid`), and installing a link. |
| `src/share.rs` | `BL1.` session codes (deflate + base64url + checksum), comparison rows, password generator, friend message. |
| `src/vdf.rs` | Valve KeyValues: text (`localconfig.vdf`, `libraryfolders.vdf`, `.acf`) and binary (`shortcuts.vdf`), round-tripping unknown data. |
| `src/config.rs` | `Settings` (user choices) and `State` (what's installed), JSON with atomic writes. |
| `src/selfupdate.rs` | Updating BonfireLink itself (SHA256SUMS.txt check, rename-swap of the running exe). |
| `src/util.rs` | Hashing, archive extract (zip/7z), zip folders, PE version, folder fingerprint, redaction, pruning. |
| `scripts/` | `package.py` (dist/), `check_elf.py` (Linux link check), `install-steamdeck.sh` (one-line Deck installer). |
| `docs/` | GitHub Pages site (`index.html`) and README screenshots. |

## How the pieces fit

- **Status** (`ops::scan`) is the single source of truth for a game: location, versions, files, password, saves,
  Proton, launch options, conflicts. The GUI caches it (`App::rescan`, every 20 s, after jobs, on tab switch) and
  never reads the disk per frame; backups, rollbacks, locales and `State` are cached the same way.
- **Release feed** (`remote::fetch_feed`): GitHub releases + the author's `VERSION` file (`>X.YZ 1` marks the version
  players must be on; `2.02` means `2.0.2`) + Nexus (only with an API key). `Feed::latest_version()` is the max of all
  three. `Feed::one_click(installed)` is what a button press installs: the newest version when GitHub has it,
  otherwise the newest GitHub build if it's still an upgrade. Otherwise the UI shows the Nexus flow.
- **Installs** (`ops::install_from`) take a .zip/.7z or a folder (rollback snapshots). They: back up saves → unpack →
  find the mod root (`*_launcher.exe` / `SeamlessCoop/`, refusing another game's archive) → snapshot the current
  version into `rollback/` → copy → remove stale files from the previous managed install → merge the user's old ini
  values into the new release's ini → record `GameState` → (Linux) write the wrapper.
- **State** is loaded fresh and saved by every operation (`State::load()` … `save()`), because the Steam-launched
  `--auto-update` process may run while the GUI is open. `ops::BusyLock` (`BonfireLink-data/.busy`, owner PID checked)
  serialises mutating operations across processes.
- **Nexus**: the API gives direct download links to Premium keys only (`Settings.nexus_premium`, cached from
  `/v1/users/validate.json`). Free accounts need the one-time `key`/`expires` from an `nxm://` link, which is why
  `nxm.rs` exists. Never register the nxm handler without the user ticking the box.
- **Steam Deck**: `ops::wrapper_script` → `<exe dir>/bonfirelink-launch.sh`; control files in `<exe dir>/.bonfirelink/`
  (`helper-path`, `auto-update`, `mods-off`, `launcher-name`, `last-auto-update.log`). `steam::with_wrapper` puts the
  quoted script path before the first `%command%`, keeps the user's options, and replaces the community
  `echo "%command%" | sed … | sh` trick. Save paths on Linux resolve inside `compatdata/<appid>/pfx/drive_c/users/steamuser`.

## Mod facts (verified October 2026)

| Game | App id | Mod folder | Launcher / DLL / ini | Release source |
|---|---|---|---|---|
| Elden Ring | 1245620 | `ELDEN RING/Game` | `ersc_launcher.exe`, `SeamlessCoop/ersc.dll`, `SeamlessCoop/ersc_settings.ini` | GitHub `yuiamoroll/EldenRingSeamlessCoopRelease` (+ Nexus 510) |
| Dark Souls III | 374320 | `DARK SOULS III/Game` | `ds3sc_launcher.exe`, `ds3sc.dll`, `ds3sc_settings.ini` | Nexus 1895 only; GitHub repo has `VERSION` only |
| Dark Souls II SotFS | 335300 | `Dark Souls II Scholar of the First Sin/Game` | `ds2sc_launcher.exe`, `ds2sc.dll`, `ds2sc_settings.ini` | GitHub `yuiamoroll/DarkSouls2SeamlessCoopRelease` (+ Nexus 1468) |
| Dark Souls Remastered | 570940 | `DARK SOULS REMASTERED` (root) | `ds1sc_launcher.exe`, `ds1sc.dll`, `ds1sc_settings.ini` | Nexus 899 only; GitHub `DarkSouls1SeamlessCoopRelease` has `VERSION` only |
| Sekiro | 814380 | `Sekiro` (root) | names guessed (`sksc_*`); detection is pattern-based | `yuiamoroll/SekiroSeamlessCoopRelease` exists with `VERSION` `0.00 0`: not released |

The ER and DS2 archive layouts were checked against real releases (v2.0.1, v0.0.6). DS3/DSR/Sekiro names come from
the mod pages and community guides; the installer also matches `*sc_launcher.exe` / `*sc_settings.ini`, so a
renamed file still installs. Steam starts ER through `start_protected_game.exe` (EAC bootstrapper); the wrapper
swaps that for `ersc_launcher.exe`. The others swap the game exe itself.

## Testing

```powershell
cargo test                      # 21 unit/integration tests (ini, vdf, codes, versions, wrapper merge, full install cycle)
cargo clippy --all-targets      # must be clean
$env:BONFIRELINK_DATA="$env:TEMP\bl-test"; .\target\debug\BonfireLink.exe --status
```

- `ops::tests::install_update_uninstall_cycle` exercises install → user edits → update (settings kept, stale files
  removed, rollback kept) → rollback → mods off → uninstall against a fake game folder.
- The Deck wrapper can be tested anywhere with a POSIX shell:
  `BonfireLink --print-wrapper er > w.sh; sh w.sh printf '[%s]\n' /proton waitforexitandrun "/x/Game/start_protected_game.exe"`
  (create `/x/Game/ersc_launcher.exe` next to it first, or it falls back to vanilla by design).
- For GUI checks use a throwaway data dir (`BONFIRELINK_DATA`) and a game-folder override in its `settings.json`
  (`"game_dirs": {"EldenRing": "C:/path/ELDEN RING"}`); never point tests at a real game folder you don't own.

## Releasing

1. Bump `version` in `Cargo.toml`, add a CHANGELOG entry.
2. `.\build.ps1` → `dist\` holds `BonfireLink-windows-x64.exe`, `BonfireLink-SteamDeck.tar.gz`,
   `BonfireLink-linux-x64` (self-updater asset), `install-steamdeck.sh`, `SHA256SUMS.txt`.
3. Create a GitHub release tagged `vX.Y.Z` with **all five files**. The self-updater and the Deck installer look for
   these exact names under `/releases/latest/download/`.

## Verification status / open items

- [x] Windows: game detection across Steam libraries, adopting a hand-installed ER 2.0.1 (identified by DLL hash),
      real GitHub download + checksum + install into a test folder, settings editor round-trip, save backup.
- [x] Linux build links only libc/libm/libdl/libpthread, glibc 2.28.
- [x] Deck launch wrapper logic tested in a POSIX shell (exe swap with spaces in paths, mods off, missing launcher, auto-update hook).
- [ ] On a real Steam Deck: launch option written in Desktop Mode, Play in Gaming Mode starts the launcher under Proton,
      `--auto-update` from the wrapper, "Add BonfireLink to Steam" shortcut, xdg-portal file picker.
- [ ] DS3 / DSR / DS2 end-to-end installs on real game folders (archive layouts for DS3/DSR not seen yet).
- [x] nxm:// hand-off: a second process queues the link and exits; the open window installs it (tested with a fake link; reports "connect Nexus first" without a key).
- [ ] Nexus Premium download path and free-account nxm download with a real key; handler registration on a real Deck/desktop (not run on the dev PC to avoid taking over the owner's Vortex/MO2 links); Downloads watcher with Firefox/Chrome/Edge.
- Ideas: controller-friendly focus navigation for Gaming Mode, Heroic/Lutris detection, per-game Proton
  recommendation, Windows exe icon resource, Nexus SSO login (needs an application slug registered with Nexus).
