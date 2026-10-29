#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! BonfireLink: installer, updater, configurator and co-op companion for LukeYui's Seamless Co-op mods.

mod app;
mod config;
mod games;
mod ini;
mod nxm;
mod ops;
mod presets;
mod remote;
mod selfupdate;
mod share;
mod steam;
mod util;
mod vdf;

#[cfg(windows)]
unsafe extern "system" {
    fn AttachConsole(pid: u32) -> i32;
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // The release exe is a GUI app; borrow the terminal it was started from for CLI output.
    #[cfg(windows)]
    if !args.is_empty() {
        // SAFETY: plain Win32 call; failure (no parent console) is harmless.
        unsafe {
            AttachConsole(u32::MAX);
        }
    }
    if let Some(code) = cli(&args) {
        std::process::exit(code);
    }
    // Started by the browser for a Nexus "Mod Manager Download" click: queue the link for the window.
    if let Some(url) = args.iter().find(|a| a.to_ascii_lowercase().starts_with("nxm://")) {
        if let Err(e) = nxm::queue(url) {
            ops::log_line(&format!("ignored Nexus link: {e:#}"));
        }
        if nxm::other_gui_running() {
            return;
        }
    }
    selfupdate::cleanup();
    // Panics end up in the log (and so in bug reports) instead of vanishing with the window.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ops::log_line(&format!("CRASH: {info}"));
        default_hook(info);
    }));

    let settings = config::Settings::load();
    let deck = steam::is_steam_deck();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(format!("{} {}", config::APP_NAME, config::VERSION))
        .with_inner_size([1180.0, 780.0])
        .with_min_inner_size([760.0, 520.0])
        .with_app_id("bonfirelink");
    if deck || steam::in_gaming_mode() {
        viewport = viewport.with_maximized(true);
    }
    viewport = viewport.with_icon(std::sync::Arc::new(app::icon()));
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(config::APP_NAME, options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, settings)))));
    if let Err(e) = result {
        ops::log_line(&format!("window could not start: {e}"));
        eprintln!("BonfireLink could not open its window: {e}");
        std::process::exit(1);
    }
}

/// Headless commands. Returns an exit code when one ran.
fn cli(args: &[String]) -> Option<i32> {
    let first = args.first()?.as_str();
    let run = |r: anyhow::Result<()>| match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            ops::log_line(&format!("cli error: {e:#}"));
            1
        }
    };
    match first {
        "--auto-update" => Some(run(ops::auto_update_headless(args.get(1).map(String::as_str).unwrap_or_default()))),
        "--update" => {
            let settings = config::Settings::load();
            let job = ops::Job::headless();
            let keys: Vec<&str> =
                if args.len() > 1 { args[1..].iter().map(String::as_str).collect() } else { games::GAMES.iter().map(|g| g.key).collect() };
            let mut code = 0;
            for key in keys {
                let Some(def) = games::by_key(key) else {
                    eprintln!("unknown game {key}");
                    code = 2;
                    continue;
                };
                if !def.released() || steam::find_game(def, settings.game_dirs.get(&def.id).map(String::as_str)).is_none() {
                    continue;
                }
                if let Err(e) = ops::update(def, &settings, &job) {
                    eprintln!("{}: {e:#}", def.name);
                    code = 1;
                }
            }
            Some(code)
        }
        "--status" => {
            let settings = config::Settings::load();
            for def in games::GAMES {
                let st = ops::scan(def, &settings);
                println!(
                    "{:<24} game: {:<10} mod: {}",
                    def.name,
                    if st.loc.is_some() { "found" } else { "not found" },
                    if st.installed() { st.mod_version.clone() } else { "not installed".into() }
                );
            }
            Some(0)
        }
        "--print-wrapper" => match args.get(1).and_then(|k| games::by_key(k)) {
            Some(def) => {
                print!("{}", ops::wrapper_script(def));
                Some(0)
            }
            None => {
                eprintln!("usage: --print-wrapper <er|ds3|ds2|dsr|sekiro>");
                Some(2)
            }
        },
        "--install-nxm" => {
            let settings = config::Settings::load();
            let job = ops::Job::headless();
            Some(run(args.get(1).map_or_else(
                || Err(anyhow::anyhow!("usage: --install-nxm <nxm://…>")),
                |u| nxm::install(u, &settings, &job).map(|m| println!("{m}")),
            )))
        }
        "--version" | "-V" => {
            println!("{} {}", config::APP_NAME, config::VERSION);
            Some(0)
        }
        "--help" | "-h" => {
            println!(
                "{} {}\n\nWithout arguments the window opens. Headless commands:\n  --status               list games and installed mod versions\n  --update [game…]       install the newest mod for each game (er ds3 ds2 dsr sekiro)\n  --auto-update <game>   used by the Steam launch wrapper before a game starts\n  --print-wrapper <game> print the Steam Deck launch wrapper script\n  --install-nxm <link>   install a Nexus \"Mod Manager Download\" link\n  --version",
                config::APP_NAME,
                config::VERSION
            );
            Some(0)
        }
        _ => None,
    }
}
