//! Local AI assistant: a small window that chats with a local LLM (LM Studio, Ollama,
//! llama.cpp server, …) through the OpenAI-compatible HTTP API.
//!
//! The assistant gets a full picture of BonfireLink (system prompt) and can act on it
//! through tools: read status, diagnostics, logs, settings and the mod's ini; change
//! settings; and install / update / uninstall / back up saves — destructive actions
//! only after the user confirms them in the window.

use crate::config::{self, Settings, State};
use crate::games::{self, GameDef};
use crate::ini::Ini;
use crate::ops;
use crate::remote;
use crate::steam;
use crate::util;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// The id of the assistant's own window. Deterministic, so the same value is
/// produced every time it is asked for.
pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("bonfirelink-ai")
}

// ---------- wire types (OpenAI-compatible) ----------

#[derive(Deserialize)]
struct ChatReply {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: WireMessage,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: String,
    function: WireFn,
}

#[derive(Deserialize)]
struct WireFn {
    name: String,
    /// JSON object as a string, per the OpenAI spec.
    arguments: String,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// A message in the assistant's conversation (what we show and what we send).
#[derive(Clone)]
pub struct Msg {
    pub role: &'static str, // "user" | "assistant" | "tool"
    pub text: String,
    /// For role == "tool": the tool call id this answers.
    pub tool_call_id: Option<String>,
    /// For role == "tool": which tool produced it (shown in the UI).
    pub tool_name: Option<String>,
}

/// Events the chat thread sends to the window.
pub enum LlmEvent {
    /// The assistant produced a visible message.
    Say(Msg),
    /// A destructive tool wants to run; the window must ask the user.
    /// The sender is the reply channel the thread blocks on.
    Confirm { title: String, body: String, reply: Sender<bool> },
    /// The turn finished (busy = false).
    Done(Option<String>),
}

#[derive(Default)]
pub struct LlmUi {
    pub messages: Vec<Msg>,
    pub input: String,
    pub busy: bool,
    pub rx: Option<Receiver<LlmEvent>>,
    pub models: Vec<String>,
    pub model: String,
    pub models_error: String,
    pub pending_confirm: Option<(String, String, Sender<bool>)>,
    pub show_settings: bool,
}

impl LlmUi {
    pub fn new(settings: &Settings) -> Self {
        let mut ui = Self { model: settings.llm_model.clone(), ..Default::default() };
        ui.push_system(settings);
        ui
    }

    fn push_system(&mut self, settings: &Settings) {
        if self.messages.iter().all(|m| m.role != "system") {
            self.messages.insert(0, Msg { role: "system", text: system_prompt(settings), tool_call_id: None, tool_name: None });
        }
    }

    pub fn refresh_system(&mut self, settings: &Settings) {
        if let Some(m) = self.messages.iter_mut().find(|m| m.role == "system") {
            m.text = system_prompt(settings);
        } else {
            self.push_system(settings);
        }
    }

    pub fn push(&mut self, role: &'static str, text: impl Into<String>) {
        self.messages.push(Msg { role, text: text.into(), tool_call_id: None, tool_name: None });
    }

    pub fn clear_chat(&mut self) {
        self.messages.retain(|m| m.role == "system");
        self.input.clear();
        self.pending_confirm = None;
    }
}

// ---------- HTTP (blocking; always called from a background thread) ----------

fn base_url(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.is_empty() {
        "http://localhost:1234/v1".to_string()
    } else if u.ends_with("/v1") {
        u.to_string()
    } else {
        format!("{u}/v1")
    }
}

fn agent() -> ureq::Agent {
    // LLM turns can be slow; the global timeout is generous, the connect timeout short.
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(5)))
        .timeout_global(Some(Duration::from_secs(60 * 10)))
        .user_agent(format!("{}/{} (assistant)", config::APP_NAME, config::VERSION))
        .http_status_as_error(false)
        .build()
        .into()
}

fn headers(key: &str) -> Vec<(String, String)> {
    let mut h = Vec::new();
    if !key.trim().is_empty() {
        h.push(("Authorization".to_string(), format!("Bearer {}", key.trim())));
    }
    h
}

pub fn list_models(settings: &Settings) -> Result<Vec<String>> {
    let url = format!("{}/models", base_url(&settings.llm_url));
    let mut req = agent().get(&url);
    for (k, v) in headers(&settings.llm_api_key) {
        req = req.header(&k, &v);
    }
    let resp = req.call().with_context(|| format!("could not reach the local LLM at {}", base_url(&settings.llm_url)))?;
    if resp.status() != 200 {
        bail!("the local LLM answered with HTTP {} (is it running?)", resp.status());
    }
    let mut body = resp.into_body();
    let list: ModelList = body.read_json().with_context(|| "unexpected reply from /models".to_string())?;
    let mut v: Vec<String> = list.data.into_iter().map(|m| m.id).collect();
    v.sort();
    Ok(v)
}

struct ChatOutcome {
    content: String,
    tool_calls: Vec<WireToolCall>,
}

fn chat_once(settings: &Settings, model: &str, messages: &[Value]) -> Result<ChatOutcome> {
    let url = format!("{}/chat/completions", base_url(&settings.llm_url));
    let mut body = json!({
        "messages": messages,
        "temperature": settings.llm_temperature,
        "max_tokens": settings.llm_max_tokens,
        "stream": false,
    });
    if !model.trim().is_empty() {
        body["model"] = json!(model);
    }
    if !tool_specs().is_empty() {
        body["tools"] = json!(tool_specs());
        body["tool_choice"] = json!("auto");
    }
    let mut req = agent().post(&url);
    for (k, v) in headers(&settings.llm_api_key) {
        req = req.header(&k, &v);
    }
    let resp = req.send_json(body).with_context(|| format!("could not reach the local LLM at {}", base_url(&settings.llm_url)))?;
    let status = resp.status();
    if status != 200 {
        let mut b = resp.into_body();
        let text = b.read_to_string().unwrap_or_default();
        bail!("the local LLM answered with HTTP {}: {}", status, text.chars().take(300).collect::<String>());
    }
    let mut b = resp.into_body();
    let reply: ChatReply = b.read_json().with_context(|| "unexpected reply from /chat/completions".to_string())?;
    let Some(choice) = reply.choices.into_iter().next() else {
        bail!("the local LLM returned no answer");
    };
    Ok(ChatOutcome { content: choice.message.content.unwrap_or_default(), tool_calls: choice.message.tool_calls.unwrap_or_default() })
}

// ---------- tools ----------

fn tool_specs() -> Vec<Value> {
    let game_prop = || json!({ "type": "string", "enum": games::GAMES.iter().map(|g| g.key).collect::<Vec<_>>() });
    vec![
        json!({ "type": "function", "function": {
            "name": "status",
            "description": "Current state of every game: found or not, mod installed and version, password set, mods off, save slots, launch option, warnings.",
            "parameters": { "type": "object", "properties": {}, "required": [] }
        }}),
        json!({ "type": "function", "function": {
            "name": "diagnostics",
            "description": "Full diagnostics text for one game (versions, folders, Proton, launch options, warnings).",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "log",
            "description": "The tail of BonfireLink's own log (what the app did and what failed).",
            "parameters": { "type": "object", "properties": {
                "lines": { "type": "integer", "description": "How many lines (default 80, max 400)." }
            }, "required": [] }
        }}),
        json!({ "type": "function", "function": {
            "name": "settings",
            "description": "BonfireLink's settings.json (secrets hidden).",
            "parameters": { "type": "object", "properties": {}, "required": [] }
        }}),
        json!({ "type": "function", "function": {
            "name": "state",
            "description": "BonfireLink's state.json: what it installed per game (versions, files, rollback info).",
            "parameters": { "type": "object", "properties": {}, "required": [] }
        }}),
        json!({ "type": "function", "function": {
            "name": "read_file",
            "description": "Read a text file. Allowed: anything inside the BonfireLink data folder, and files inside a game's mod folder (e.g. the *_settings.ini).",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string", "description": "Absolute path." }
            }, "required": ["path"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "ini",
            "description": "Read or change one value in a game's Seamless Co-op settings file (format-preserving; a backup is kept). Use action \"get\" to read, \"set\" to change.",
            "parameters": { "type": "object", "properties": {
                "game": game_prop(),
                "action": { "type": "string", "enum": ["get", "set"] },
                "key": { "type": "string", "description": "Key name, e.g. cooppassword, maxplayers, language." },
                "value": { "type": "string", "description": "New value (for set)." }
            }, "required": ["game", "action"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "set_setting",
            "description": "Change one BonfireLink setting (e.g. backup_saves_before_play, auto_update_on_play, keep_backups, ui_scale).",
            "parameters": { "type": "object", "properties": {
                "key": { "type": "string" },
                "value": { "type": "string" }
            }, "required": ["key", "value"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "backups",
            "description": "List save backups and rollback versions for a game.",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "releases",
            "description": "Available Seamless Co-op releases for a game (version, source, size).",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "install_update",
            "description": "Download and install the newest Seamless Co-op for a game (backs up saves first, keeps settings, keeps a rollback copy). DESTRUCTIVE: needs user confirmation.",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "uninstall",
            "description": "Remove Seamless Co-op from a game (saves and a settings copy are kept). DESTRUCTIVE: needs user confirmation.",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "backup_saves",
            "description": "Back up a game's saves now (safe, additive).",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "restore_backup",
            "description": "Restore a save backup (the current saves are backed up first). DESTRUCTIVE: needs user confirmation.",
            "parameters": { "type": "object", "properties": {
                "game": game_prop(),
                "backup": { "type": "string", "description": "Backup name from the backups tool." }
            }, "required": ["game", "backup"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "mods_off",
            "description": "Turn the mod on or off for a game without removing it (Steam starts the vanilla game when off).",
            "parameters": { "type": "object", "properties": {
                "game": game_prop(),
                "off": { "type": "boolean" }
            }, "required": ["game", "off"] }
        }}),
        json!({ "type": "function", "function": {
            "name": "launch",
            "description": "Start the game with Seamless Co-op, the same as the Play button.",
            "parameters": { "type": "object", "properties": { "game": game_prop() }, "required": ["game"] }
        }}),
    ]
}

fn def_for(key: &str) -> Result<&'static GameDef> {
    games::by_key(key).with_context(|| {
        format!("unknown game '{key}' (use one of: {})", games::GAMES.iter().map(|g| g.key).collect::<Vec<_>>().join(", "))
    })
}

/// Paths the assistant may read: the data folder plus each game's mod folder.
fn allowed_roots(settings: &Settings) -> Vec<PathBuf> {
    let mut v = vec![config::data_dir().to_path_buf()];
    for def in games::GAMES {
        if let Some(loc) = steam::find_game(def, settings.game_dirs.get(&def.id).map(String::as_str)) {
            v.push(loc.exe_dir);
        }
    }
    v
}

fn inside_any(path: &Path, roots: &[PathBuf]) -> bool {
    let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    roots.iter().any(|r| {
        let r = std::fs::canonicalize(r).unwrap_or_else(|_| r.clone());
        canon.starts_with(&r)
    })
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max).collect();
        format!("{head}\n…[truncated, {len} chars total]", len = text.chars().count())
    }
}

/// Execute one tool call. `confirm` is only used for destructive tools: it asks the
/// window (which asks the user) and blocks until the answer.
fn execute_tool(name: &str, args: &Value, settings: &Settings, confirm: &dyn Fn(&str, &str) -> bool) -> String {
    let game = |required: bool| -> Result<&'static GameDef> {
        let k = args.get("game").and_then(Value::as_str).unwrap_or("");
        if k.is_empty() && !required {
            bail!("missing \"game\"");
        }
        def_for(k)
    };
    let out = (|| -> Result<String> {
        match name {
            "status" => {
                let mut s = String::new();
                for def in games::GAMES {
                    let st = ops::scan(def, settings);
                    s.push_str(&format!(
                        "{}: game {} | mod {} | version {} | managed {} | mods_off {} | password {} | saves {} | launch_option_ok {}\n",
                        def.name,
                        if st.loc.is_some() { "found" } else { "NOT FOUND" },
                        if st.installed() { "installed" } else { "not installed" },
                        if st.mod_version.is_empty() { "?" } else { &st.mod_version },
                        st.managed,
                        st.mods_off,
                        if st.password.trim().is_empty() { "none" } else { "set" },
                        st.save_slots.len(),
                        st.launch_option_ok(),
                    ));
                    for c in &st.conflicts {
                        s.push_str(&format!("  warning: {c}\n"));
                    }
                }
                Ok(s)
            }
            "diagnostics" => Ok(ops::diagnostics(game(true)?, settings)),
            "log" => {
                let n = args.get("lines").and_then(Value::as_u64).unwrap_or(80).clamp(10, 400) as usize;
                let path = config::data_dir().join("logs/bonfirelink.log");
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let tail: Vec<&str> = text.lines().rev().take(n).collect();
                Ok(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
            }
            "settings" => {
                let s = Settings::load();
                let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.insert("nexus_api_key".into(), json!(if s.nexus_api_key.is_empty() { "" } else { "(hidden)" }));
                }
                Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
            }
            "state" => {
                let st = State::load();
                Ok(serde_json::to_string_pretty(&st).unwrap_or_default())
            }
            "read_file" => {
                let p = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or(""));
                let roots = allowed_roots(settings);
                if !inside_any(&p, &roots) {
                    bail!("that path is outside the allowed folders (BonfireLink data or a game's mod folder)");
                }
                let text = std::fs::read_to_string(&p).with_context(|| format!("could not read {}", p.display()))?;
                Ok(truncate(&text, 20_000))
            }
            "ini" => {
                let def = game(true)?;
                let st = ops::scan(def, settings);
                let (path, text) = ops::read_ini(&st)?;
                let mut ini = Ini::parse(&text);
                let action = args.get("action").and_then(Value::as_str).unwrap_or("get");
                let key = args.get("key").and_then(Value::as_str).unwrap_or("");
                match action {
                    "get" => {
                        if key.is_empty() {
                            let mut s = format!("{} ({}):\n", path.display(), def.name);
                            for (sec, k, v) in ini.entries() {
                                s.push_str(&format!("[{sec}] {k} = {v}\n"));
                            }
                            Ok(s)
                        } else {
                            Ok(format!("{key} = {}", ini.get_any(key).unwrap_or_else(|| "(not set)".into())))
                        }
                    }
                    "set" => {
                        let value = args.get("value").and_then(Value::as_str).unwrap_or("");
                        if key.is_empty() {
                            bail!("missing \"key\"");
                        }
                        if !ini.set_id(key, value) {
                            bail!("key '{key}' not found in the settings file");
                        }
                        ops::write_ini(&path, &ini.to_text())?;
                        Ok(format!("set {key} = {value} in {}", path.display()))
                    }
                    other => bail!("unknown action '{other}' (use get or set)"),
                }
            }
            "set_setting" => {
                let key = args.get("key").and_then(Value::as_str).unwrap_or("");
                let value = args.get("value").and_then(Value::as_str).unwrap_or("");
                let s = Settings::load();
                let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
                let Some(obj) = v.as_object_mut() else { bail!("settings unreadable") };
                let Some(field) = obj.get_mut(key) else {
                    bail!("unknown setting '{key}' (allowed: {})", obj.keys().cloned().collect::<Vec<_>>().join(", "));
                };
                let parsed: std::result::Result<Value, serde_json::Error> = serde_json::from_str(value);
                match parsed {
                    Ok(p) => *field = p,
                    Err(_) => *field = Value::String(value.to_string()),
                }
                let s: Settings = serde_json::from_value(v).with_context(|| "setting has the wrong type")?;
                s.save()?;
                Ok(format!("set {key} = {value}"))
            }
            "backups" => {
                let def = game(true)?;
                let mut s = String::new();
                for b in ops::list_backups(def) {
                    s.push_str(&format!("backup: {} ({})\n", b.name, util::human_size(b.size)));
                }
                for r in ops::rollbacks(def) {
                    s.push_str(&format!("rollback: {} ({})\n", r.version, r.when));
                }
                Ok(if s.is_empty() { "no backups or rollbacks yet".into() } else { s })
            }
            "releases" => {
                let def = game(true)?;
                let feed = remote::fetch_feed(def, &settings.nexus_api_key);
                let mut s = String::new();
                if let Some(l) = feed.latest_version() {
                    s.push_str(&format!("newest: {l}\n"));
                }
                for r in &feed.releases {
                    s.push_str(&format!("{} ({} bytes, {})\n", r.version, r.size, r.published));
                }
                for e in &feed.errors {
                    s.push_str(&format!("note: {e}\n"));
                }
                Ok(if s.is_empty() { "no releases found (offline?)".into() } else { s })
            }
            "install_update" => {
                let def = game(true)?;
                let st = ops::scan(def, settings);
                let installed = if st.installed() { st.mod_version.clone() } else { String::new() };
                let feed = remote::fetch_feed(def, &settings.nexus_api_key);
                let Some(latest) = feed.latest_version() else {
                    bail!("no release found (offline, or Nexus-only without a key)");
                };
                if !installed.is_empty() && remote::same_version(&installed, &latest) {
                    return Ok(format!("{} is already the newest version ({latest}).", def.name));
                }
                if !confirm(
                    &format!("Install Seamless Co-op {latest} for {}?", def.name),
                    "Downloads the official release, backs up saves first, keeps your settings and a rollback copy.",
                ) {
                    return Ok("The user declined; nothing was installed.".into());
                }
                let job = ops::Job::headless();
                let (archive, version, origin) = ops::fetch_latest(def, settings, &feed, &installed, &job)?;
                ops::install_from(def, settings, &archive, &version, &origin, &job)?;
                Ok(format!("{} now runs Seamless Co-op {version} (from {origin}).", def.name))
            }
            "uninstall" => {
                let def = game(true)?;
                if !confirm(
                    &format!("Remove Seamless Co-op from {}?", def.name),
                    "The mod files and the Steam launch option are removed. Saves, backups and a settings copy stay.",
                ) {
                    return Ok("The user declined; nothing was removed.".into());
                }
                let job = ops::Job::headless();
                ops::uninstall(def, settings, &job)?;
                Ok(format!("Seamless Co-op was removed from {}.", def.name))
            }
            "backup_saves" => {
                let def = game(true)?;
                let job = ops::Job::headless();
                match ops::backup_saves(def, settings, "assistant", true, &job)? {
                    Some(p) => Ok(format!("Saves backed up to {}", p.display())),
                    None => Ok("Saves unchanged since the last backup; no new copy needed.".into()),
                }
            }
            "restore_backup" => {
                let def = game(true)?;
                let backup = args.get("backup").and_then(Value::as_str).unwrap_or("");
                let entry = ops::list_backups(def)
                    .into_iter()
                    .find(|b| b.name == backup || b.path.to_string_lossy() == backup)
                    .with_context(|| format!("no backup named '{backup}' (see the backups tool)"))?;
                if !confirm(
                    &format!("Restore the {} save backup?", entry.name),
                    "Your current saves are replaced by this backup. They are backed up first, so this can be undone.",
                ) {
                    return Ok("The user declined; saves were not touched.".into());
                }
                let job = ops::Job::headless();
                ops::restore_saves(def, settings, &entry.path, &job)?;
                Ok(format!("Saves restored from {}.", entry.name))
            }
            "mods_off" => {
                let def = game(true)?;
                let off = args.get("off").and_then(Value::as_bool).unwrap_or(true);
                ops::set_mods_off(def, settings, off)?;
                Ok(if off {
                    format!("Mods off for {}: Steam now starts the vanilla game.", def.name)
                } else {
                    format!("Mods back on for {}.", def.name)
                })
            }
            "launch" => {
                let def = game(true)?;
                let job = ops::Job::headless();
                ops::launch(def, settings, &job)?;
                Ok(format!("{} is starting with Seamless Co-op.", def.name))
            }
            other => bail!("unknown tool '{other}'"),
        }
    })();
    match out {
        Ok(s) => s,
        Err(e) => format!("ERROR: {e:#}"),
    }
}

// ---------- the chat loop (runs on its own thread) ----------

fn to_wire(messages: &[Msg]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| match m.role {
            "tool" => json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.text,
            }),
            _ => json!({ "role": m.role, "content": m.text }),
        })
        .collect()
}

/// Run one user turn: chat with the LLM, execute tools (confirming destructive ones
/// through `ask`), and report everything through `tx`. `history` is the full
/// conversation so far (system prompt first); the new user message is already in it.
pub fn run_turn(settings: Settings, model: String, history: Vec<Msg>, tx: Sender<LlmEvent>) {
    let mut messages = history;

    let ask = |title: &str, body: &str| -> bool {
        let (rtx, rrx) = channel();
        if tx.send(LlmEvent::Confirm { title: title.to_string(), body: body.to_string(), reply: rtx }).is_err() {
            return false;
        }
        // The window sends the user's answer; a dead window means "no".
        rrx.recv().unwrap_or(false)
    };

    let mut last_error = String::new();
    for _round in 0..8 {
        // Keep the conversation from growing without bound for small local models:
        // drop the oldest turns, but only at a user-message boundary so a tool call
        // and its result are never split.
        if messages.len() > 40 {
            let mut cut = None;
            for (i, m) in messages.iter().enumerate().rev().take(40) {
                if m.role == "user" {
                    cut = Some(i);
                    break;
                }
            }
            if let Some(i) = cut.filter(|i| *i > 1) {
                messages.drain(1..i);
            }
        }
        let wire: Vec<Value> = to_wire(&messages);
        match chat_once(&settings, &model, &wire) {
            Ok(outcome) => {
                if !outcome.content.is_empty() {
                    let m = Msg { role: "assistant", text: outcome.content.clone(), tool_call_id: None, tool_name: None };
                    messages.push(m.clone());
                    if tx.send(LlmEvent::Say(m)).is_err() {
                        return;
                    }
                }
                if outcome.tool_calls.is_empty() {
                    break;
                }
                for call in outcome.tool_calls {
                    let args: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Object(Default::default()));
                    let name = call.function.name.clone();
                    let display = format!("{} {}", name, args);
                    if tx
                        .send(LlmEvent::Say(Msg {
                            role: "tool",
                            text: display,
                            tool_call_id: Some(call.id.clone()),
                            tool_name: Some(name.clone()),
                        }))
                        .is_err()
                    {
                        return;
                    }
                    let result = execute_tool(&name, &args, &settings, &ask);
                    messages.push(Msg {
                        role: "tool",
                        text: truncate(&result, 12_000),
                        tool_call_id: Some(call.id.clone()),
                        tool_name: Some(name.clone()),
                    });
                }
                last_error.clear();
            }
            Err(e) => {
                last_error = format!("{e:#}");
                break;
            }
        }
    }
    let err = if last_error.is_empty() { None } else { Some(last_error) };
    let _ = tx.send(LlmEvent::Done(err));
}

// ---------- the system prompt: the AI's picture of the app ----------

pub fn system_prompt(settings: &Settings) -> String {
    let deck = steam::is_steam_deck();
    let mut s = String::new();
    s.push_str(&format!(
        "You are the built-in assistant of BonfireLink {v}, a portable one-click helper for LukeYui's Seamless Co-op mods.\n",
        v = config::VERSION
    ));
    s.push_str(&format!(
        "Device: {}{} · data folder: {}\n\n",
        if steam::is_linux() { "SteamOS/Linux" } else { "Windows" },
        if deck { " (Steam Deck)" } else { "" },
        util::redact(&config::data_dir().display().to_string()),
    ));
    s.push_str("What BonfireLink does:\n");
    s.push_str("- Installs, updates and removes the Seamless Co-op mod for: ");
    s.push_str(&games::GAMES.iter().map(|g| g.name).collect::<Vec<_>>().join(", "));
    s.push_str(".\n");
    s.push_str("- Downloads only official releases (the author's GitHub or Nexus Mods), verifies them, backs up saves, keeps your settings across updates and keeps rollback copies.\n");
    s.push_str("- Manages the mod's *_settings.ini (password, scaling, language, …) format-preservingly.\n");
    if steam::is_linux() {
        s.push_str("- On the Deck it sets Steam launch options so the normal Play button starts the mod under Proton (a launch wrapper updates the mod first).\n");
    }
    s.push_str("\nCurrent state (live, re-read when you call tools):\n");
    for def in games::GAMES {
        let st = ops::scan(def, settings);
        s.push_str(&format!(
            "- {}: game {}, mod {}, version {}, password {}, mods_off {}, saves {}, launch_option_ok {}\n",
            def.name,
            if st.loc.is_some() { "found" } else { "NOT FOUND" },
            if st.installed() { "installed" } else { "not installed" },
            if st.mod_version.is_empty() { "?" } else { &st.mod_version },
            if st.password.trim().is_empty() { "none" } else { "set" },
            st.mods_off,
            st.save_slots.len(),
            st.launch_option_ok(),
        ));
        for c in &st.conflicts {
            s.push_str(&format!("  warning: {c}\n"));
        }
    }
    s.push_str("\nTools you can use (call them instead of guessing):\n");
    s.push_str("- status / diagnostics / log / settings / state / backups / releases: read the app's live state.\n");
    s.push_str("- read_file: read a file in the data folder or a game's mod folder.\n");
    s.push_str("- ini: read or change one value in a game's Seamless Co-op settings file (backed up automatically).\n");
    s.push_str("- set_setting: change a BonfireLink setting (see the settings tool for the list).\n");
    s.push_str("- backup_saves / mods_off / launch: safe actions you may take directly.\n");
    s.push_str("- install_update / uninstall / restore_backup: destructive; the user is asked to confirm in the window before they run.\n");
    s.push_str("\nRules:\n");
    s.push_str("- Be a helpful, plain-English helper for a gamer, not a developer. Short answers.\n");
    s.push_str("- Diagnose before acting: use status/diagnostics/log first, then fix.\n");
    s.push_str("- Never invent versions, paths or settings; read them with the tools.\n");
    s.push_str("- Never ask for or repeat passwords or API keys; they are hidden from you on purpose.\n");
    s.push_str("- If a destructive tool is declined, accept it and suggest an alternative.\n");
    s.push_str(
        "- If you cannot fix something, say what to check and offer to save a bug report (the user can do that on the Troubleshoot tab).\n",
    );
    s
}
