//! The interactive REPL: line input, slash commands, session wiring.
//!
//! v1 reads lines through the console's own line editor (canonical mode):
//! the terminal emulator renders the echo with proper BiDi, so Persian /
//! Arabic input displays correctly without any in-process text shaping.

use nah_core::config::NahConfig;
use nah_core::context::ContextManager;
use nah_core::error::CoreError;
use nah_core::history::{self, HistoryStore, SessionRecord};
use nah_core::memory;
use nah_core::provider::LlmProvider;
use nah_core::session::{Event, Session, SessionConfig};
use nah_core::tools::{ConfirmRequest, ToolKit};

use crate::setup;
use crate::ui::Ui;

pub struct Repl<'a> {
    pub ui: Ui,
    pub session: Session<'a>,
    pub config: NahConfig,
    /// Saved-conversation store (`~/.config/nah/history/`).
    hist: HistoryStore,
    /// Record id of the conversation currently in memory (None until the
    /// first message is saved).
    current_id: Option<String>,
    /// Creation time of the current record.
    current_unix: u64,
    /// Transcript length at the last save (the log only grows).
    saved_log: usize,
}

impl<'a> Repl<'a> {
    pub fn new(
        ui: Ui,
        provider: Box<dyn LlmProvider + 'a>,
        config: NahConfig,
    ) -> Result<Repl<'a>, CoreError> {
        let mut tools = ToolKit::new(config.root.clone(), config.max_tool_bytes);
        for w in tools.load_custom_tools(&nah_core::config::tools_dir()) {
            ui.info(&format!("[warn] {w}"));
        }
        let mut session = Session::new(
            provider,
            SessionConfig {
                model: config.model.clone(),
                options: config.chat_options(),
                max_steps: config.max_steps,
            },
            tools,
            ContextManager::new(config.context_config()),
        );
        // Ctrl+C while a turn runs cancels the turn (see ctrlc.rs)
        session.bind_stop_handle(crate::ctrlc::stop_handle());
        Ok(Repl {
            ui,
            session,
            config,
            hist: HistoryStore::new(nah_core::config::history_dir()),
            current_id: None,
            current_unix: 0,
            saved_log: 0,
        })
    }

    pub fn run(&mut self) -> Result<(), CoreError> {
        self.ui.banner();
        self.ui.rtl_font_notice();
        self.ui.info(&format!(
            "model: {} | server: {} | ctx: {} tokens | ctrl+c stops a running turn",
            self.config.model, self.config.url, self.config.ctx_tokens
        ));
        println!();

        let stdin = std::io::stdin();
        loop {
            self.ui.prompt();
            // Read one line per prompt with a *fresh* lock on stdin. Holding
            // `stdin.lock().lines()` across the loop would keep the global
            // stdin mutex locked and deadlock every later read_line (confirm
            // dialogs, /setup wizard).
            let mut line = String::new();
            let n = stdin
                .read_line(&mut line)
                .map_err(|e| CoreError::Io(e.to_string()))?;
            if n == 0 {
                println!();
                break; // EOF
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Ctrl+P then Enter: quick model cycling. Canonical line mode
            // cannot read single keys without breaking RTL input, so the
            // control char arrives with the line.
            if trimmed == "\u{10}" {
                self.cycle_model();
                continue;
            }

            if let Some(cmd) = trimmed.strip_prefix('/') {
                if self.handle_command(cmd.trim()) {
                    break;
                }
                continue;
            }
            // terminal habit: bare `clear`/`cls` clears the screen instead
            // of going to the model
            if trimmed.eq_ignore_ascii_case("clear") || trimmed.eq_ignore_ascii_case("cls") {
                self.ui.clear_screen();
                continue;
            }

            self.run_turn(trimmed);
        }
        self.save_history();
        self.ui.info("bye.");
        Ok(())
    }

    fn run_turn(&mut self, input: &str) {
        println!();
        self.ui.divider();
        self.ui.spinner_on();

        let mut confirm_fn = |req: &ConfirmRequest| -> bool {
            self.ui
                .confirm(&req.verb, &req.path, &req.preview, req.is_new_file)
        };
        let ui = &self.ui;
        let mut on_event = |ev: &Event<'_>| match ev {
            Event::Delta(text) => ui.delta(text),
            Event::Action(a) => {
                println!();
                ui.action(&nah_core::protocol::action_label(a));
            }
            Event::ToolResult(o) => {
                if o.is_error {
                    ui.tool_err(&o.body);
                } else {
                    ui.tool_ok(&o.body);
                }
            }
            Event::Info(msg) => {
                println!();
                ui.info(msg);
            }
            Event::TurnDone => {
                ui.stream_flush();
                ui.spinner_off();
                println!();
                println!();
            }
        };

        crate::ctrlc::set_turn_running(true);
        let result = self.session.run_turn(input, &mut confirm_fn, &mut on_event);
        crate::ctrlc::set_turn_running(false);
        match result {
            Ok(stats) => {
                if stats.est_tokens > 0 {
                    self.ui.stat(&format!(
                        "~{} tok/s - {} tokens - {:.1}s",
                        stats.tokens_per_sec().round() as u64,
                        stats.est_tokens,
                        stats.gen_secs
                    ));
                }
                let (tokens, msgs) = self.session.context_usage();
                self.ui.stat(&format!(
                    "context: ~{}/{} tokens - {} messages  (/compact summarizes)",
                    tokens, self.config.ctx_tokens, msgs
                ));
            }
            Err(e) => {
                self.ui.spinner_off();
                self.ui.stream_flush();
                self.ui.error(&e.to_string());
            }
        }
        // persist the transcript after every turn (idempotent)
        self.save_history();
    }

    /// Returns `true` when the REPL should exit. Never fails: command
    /// errors are printed and the session keeps running.
    /// `/tools [on|off <name>] | reload` - list and toggle the tools the
    /// model is allowed to see (disabled ones leave the prompt entirely).
    fn handle_tools(&mut self, rest: &str) {
        let mut parts = rest.split_whitespace();
        match parts.next() {
            None => self.list_tools(),
            Some("on") => self.toggle_tool(true, parts.next()),
            Some("off") => self.toggle_tool(false, parts.next()),
            Some("reload") => {
                let warnings = self
                    .session
                    .tools
                    .reload_custom_tools(&nah_core::config::tools_dir());
                for w in &warnings {
                    self.ui.info(&format!("[warn] {w}"));
                }
                let customs = self
                    .session
                    .tools
                    .registry()
                    .all()
                    .iter()
                    .filter(|t| t.kind == nah_core::tools::ToolKind::Custom)
                    .count();
                self.ui
                    .ok(&format!("tools reloaded - {customs} custom tool(s) active"));
            }
            Some(other) => {
                self.ui.info(&format!(
                    "unknown subcommand: {other} - usage: /tools [on|off <name>]"
                ));
            }
        }
    }

    fn list_tools(&self) {
        self.ui
            .info("tools (only enabled ones are shown to the model):");
        for def in self.session.tools.registry().all() {
            let status = if self.session.tools.registry().enabled(&def.name) {
                "on "
            } else {
                "off"
            };
            println!("  [{status}] {}", def.name);
        }
        self.ui
            .info("usage: /tools on|off <name>  |  /tools reload");
    }

    fn toggle_tool(&mut self, on: bool, name: Option<&str>) {
        let Some(name) = name else {
            self.ui.info("usage: /tools on|off <name>");
            return;
        };
        if !self.session.tools.registry_mut().set_enabled(name, on) {
            self.ui
                .info(&format!("unknown tool: {name} (see /tools for the list)"));
            return;
        }
        self.config.disabled_tools = self.session.tools.registry().disabled_names().to_vec();
        let _ = nah_core::config::save(&nah_core::config::config_path(), &self.config);
        self.ui.ok(&format!(
            "tool {} {} - takes effect on the next message",
            name.to_ascii_uppercase(),
            if on { "enabled" } else { "disabled" }
        ));
    }

    /// Quick model cycling (pi-style): `/model` with no name, or the
    /// Ctrl+P + Enter line.
    fn cycle_model(&mut self) {
        match self.session.provider.list_models() {
            Ok(models) if models.len() >= 2 => {
                let names: Vec<String> = models.into_iter().map(|m| m.name).collect();
                let idx = names
                    .iter()
                    .position(|n| *n == self.config.model)
                    .unwrap_or(0);
                let next = names[(idx + 1) % names.len()].clone();
                self.switch_model(&next);
            }
            Ok(_) => self.ui.info("only one model is available on this server"),
            Err(e) => self.ui.error(&e.to_string()),
        }
    }

    /// Switch the model; a different model starts a fresh conversation
    /// (it must not see the old one) and needs fresh token calibration.
    /// The old conversation is finalized into history first.
    fn switch_model(&mut self, name: &str) {
        if name == self.config.model {
            self.ui.info(&format!("model is already {name}"));
            return;
        }
        self.finish_session();
        self.config.model = name.to_string();
        self.session.reset();
        self.session.config.model = name.to_string();
        self.session.context.reset_token_scale();
        self.ui
            .ok(&format!("model set to {name} - conversation reset"));
    }

    /// Persist the transcript when it grew since the last save. The
    /// record id is allocated at first save and kept across resumes, so a
    /// resumed conversation keeps growing its original record.
    fn save_history(&mut self) {
        if self.session.log.is_empty() || self.session.log.len() == self.saved_log {
            return;
        }
        if self.current_id.is_none() {
            self.current_id = Some(history::new_session_id());
            self.current_unix = history::now_secs();
        }
        let rec = SessionRecord {
            id: self.current_id.clone().unwrap_or_default(),
            unix: self.current_unix,
            cwd: root_label(&self.config.root),
            model: self.config.model.clone(),
            messages: self.session.log.clone(),
        };
        match self.hist.save(&rec) {
            Ok(_) => self.saved_log = self.session.log.len(),
            Err(e) => self.ui.info(&format!("[warn] cannot save history: {e}")),
        }
    }

    /// Finalize the current record before the in-memory conversation is
    /// cleared (model switch, /reset, or a connection change).
    fn finish_session(&mut self) {
        self.save_history();
        self.current_id = None;
        self.current_unix = 0;
        self.saved_log = 0;
    }

    /// `/history` - browse saved conversations; `/history <text>` searches
    /// every saved message for the text (case-insensitive).
    fn handle_history(&self, rest: &str) {
        if !rest.is_empty() {
            self.search_history(rest);
            return;
        }
        let items = self.hist.list();
        if items.is_empty() {
            self.ui
                .info("no saved conversations yet - they are saved automatically as you chat.");
            return;
        }
        self.ui
            .info("saved conversations (/resume <n> continues one):");
        for (i, s) in items.iter().enumerate().take(12) {
            let mark = if Some(&s.id) == self.current_id.as_ref() {
                "  <- current"
            } else {
                ""
            };
            println!(
                "  #{:<2} {:>7}  {:>2} msgs  {}  {}{}",
                i + 1,
                history::ago_label(s.unix),
                s.messages,
                s.model,
                s.preview,
                mark
            );
        }
        if items.len() > 12 {
            self.ui.info(&format!(
                "... and {} more (/history <text> searches all of them)",
                items.len() - 12
            ));
        }
    }

    fn search_history(&self, query: &str) {
        let hits = self.hist.search(query);
        if hits.is_empty() {
            self.ui
                .info(&format!("no saved message matches \"{query}\"."));
            return;
        }
        self.ui
            .info(&format!("matches for \"{query}\" (newest first):"));
        for h in hits.iter().take(12) {
            println!(
                "  {}  [{}] {}: {}",
                history::ago_label(h.unix),
                h.model,
                h.role,
                h.snippet
            );
        }
        if hits.len() > 12 {
            self.ui
                .info(&format!("... {} more match(es)", hits.len() - 12));
        }
        self.ui
            .info("use /resume <n> (see /history) to continue one.");
    }

    /// `/resume [n|id]` - continue a saved conversation. No argument
    /// resumes the most recent one that is not already loaded.
    fn handle_resume(&mut self, rest: &str) {
        let items = self.hist.list();
        if items.is_empty() {
            self.ui.info("no saved conversations to resume.");
            return;
        }
        let target = if rest.is_empty() {
            items
                .iter()
                .find(|s| Some(&s.id) != self.current_id.as_ref())
                .cloned()
        } else if let Ok(n) = rest.parse::<usize>() {
            n.checked_sub(1).and_then(|i| items.get(i)).cloned()
        } else {
            items.iter().find(|s| s.id == rest).cloned()
        };
        let Some(info) = target else {
            self.ui
                .error("no such conversation - /history lists the saved ones");
            return;
        };
        let rec = match self.hist.load(&info.id) {
            Ok(r) => r,
            Err(e) => {
                self.ui.error(&e.to_string());
                return;
            }
        };
        // a conversation from another project keeps its transcript but not
        // its working directory
        if rec.cwd != root_label(&self.config.root) {
            self.ui.info(&format!(
                "note: that conversation was in another project ({})",
                nah_core::tools::display_path(std::path::Path::new(&rec.cwd))
            ));
        }
        // restore the model the conversation ran with, when available
        if rec.model != self.config.model {
            match self.session.provider.list_models() {
                Ok(models) if models.iter().any(|m| m.name == rec.model) => {
                    self.config.model = rec.model.clone();
                    self.session.config.model = rec.model.clone();
                    self.session.context.reset_token_scale();
                    self.ui.info(&format!("model set to {}", rec.model));
                }
                Ok(_) => self.ui.info(&format!(
                    "saved model '{}' is not on this server - continuing with '{}'",
                    rec.model, self.config.model
                )),
                Err(_) => {}
            }
        }
        // finalize whatever is in memory now, then load the record
        self.save_history();
        let age = history::ago_label(rec.unix);
        let n = rec.messages.len();
        self.session.restore(rec.messages);
        self.current_id = Some(rec.id);
        self.current_unix = rec.unix;
        self.saved_log = self.session.log.len();
        self.ui.ok(&format!(
            "resumed conversation from {age} ({n} messages) - just continue chatting"
        ));
    }

    /// `/memory [clear]` - show or clear the project memory file the
    /// model maintains (`.nah/memory.md`).
    fn handle_memory(&mut self, rest: &str) {
        let path = memory::memory_path(self.session.tools.root());
        match rest {
            "clear" => {
                if !path.exists() {
                    self.ui.info("no project memory to clear.");
                    return;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => self.ui.ok("project memory cleared."),
                    Err(e) => self.ui.error(&format!("cannot clear memory: {e}")),
                }
            }
            "" => {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    self.ui.info("no project memory yet.");
                    self.ui.info(
                        "the model writes durable project notes to .nah/memory.md - it is \
                         pinned into its prompt automatically.",
                    );
                    return;
                };
                let lines = text.lines().count();
                println!(
                    "  {} ({} lines)",
                    nah_core::tools::display_path(&path),
                    lines
                );
                for line in text.lines().take(12) {
                    println!("  | {line}");
                }
                if lines > 12 {
                    self.ui.info(&format!("... {} more line(s)", lines - 12));
                }
            }
            _ => self.ui.info("usage: /memory [clear]"),
        }
    }

    fn handle_command(&mut self, cmd: &str) -> bool {
        let (name, rest) = match cmd.split_once(' ') {
            Some((n, r)) => (n, r.trim()),
            None => (cmd, ""),
        };
        match name {
            "exit" | "quit" | "q" => return true,
            "help" | "h" | "?" => {
                println!(
                    r"commands:
  /setup            run the setup wizard (provider, URL, default model)
  /provider         scan for local model servers and switch to one
  /models           list models available on the server
  /model [name]     switch the model (no name = cycle to the next one)
  /history          browse saved conversations (/history <text> searches them)
  /resume [n|id]    continue a saved conversation (no name = most recent)
  /ctx <n>          set the context budget (and num_ctx)
  /tail <n>         number of trailing messages always kept
  /compact          summarize older history now (frees context space)
  /files            show files in the working set
  /tools            list tools and toggle them on/off (only enabled ones reach the model)
  /mode [name]      permission mode: read-only | confirm | auto (default: confirm)
  /root             show the project root
  /yes | /no        toggle auto-approve for file writes and commands
  /reset            start a fresh conversation (saved first; project memory is kept)
  /undo             reverse the last file change made by the model
  /memory [clear]   show (or clear) the project memory file
  /clear            clear the screen (aliases: cls, bare `clear`)
  /status           show current settings and context usage
  /exit             quit (Ctrl+C also exits)
  (anything else is sent to the model as-is)"
                );
            }
            "setup" => {
                let before = (
                    self.config.provider,
                    self.config.url.clone(),
                    self.config.model.clone(),
                );
                if let Err(e) = setup::run(&self.ui, &mut self.config) {
                    self.ui.error(&e.to_string());
                } else {
                    // reconnect: URL or provider kind may have changed
                    match nah_core::provider::connect(self.config.provider, &self.config.url) {
                        Ok(p) => {
                            self.session.provider = p;
                            let changed = before
                                != (
                                    self.config.provider,
                                    self.config.url.clone(),
                                    self.config.model.clone(),
                                );
                            if changed {
                                // a different model/server should not see
                                // the old conversation
                                self.finish_session();
                                self.session.reset();
                                self.ui
                                    .info("conversation reset (model or server changed).");
                            }
                            self.sync_session_config();
                        }
                        Err(e) => self
                            .ui
                            .error(&format!("saved config, but reconnect failed: {e}")),
                    }
                }
            }
            "provider" => {
                let before = (
                    self.config.provider,
                    self.config.url.clone(),
                    self.config.model.clone(),
                );
                match setup::configure_connection(&self.ui, &mut self.config) {
                    Ok(models) => {
                        setup::choose_model(&self.ui, &mut self.config, &models);
                        if let Err(e) =
                            nah_core::config::save(&nah_core::config::config_path(), &self.config)
                        {
                            self.ui.error(&e.to_string());
                        }
                        // reconnect: URL or provider kind may have changed
                        match nah_core::provider::connect(self.config.provider, &self.config.url) {
                            Ok(p) => {
                                self.session.provider = p;
                                let changed = before
                                    != (
                                        self.config.provider,
                                        self.config.url.clone(),
                                        self.config.model.clone(),
                                    );
                                if changed {
                                    // a different model/server should not
                                    // see the old conversation
                                    self.finish_session();
                                    self.session.reset();
                                }
                                self.sync_session_config();
                                self.ui.ok(&format!(
                                    "connected to {} at {} with model '{}'{}",
                                    self.config.provider.as_str(),
                                    self.config.url,
                                    self.config.model,
                                    if changed { " - conversation reset" } else { "" }
                                ));
                            }
                            Err(e) => self
                                .ui
                                .error(&format!("saved config, but reconnect failed: {e}")),
                        }
                    }
                    Err(e) => self.ui.error(&e.to_string()),
                }
            }
            "models" => match self.session.provider.list_models() {
                Ok(models) => {
                    let pairs: Vec<(String, String)> =
                        models.into_iter().map(|m| (m.name, m.detail)).collect();
                    self.ui.models(&pairs);
                }
                Err(e) => self.ui.error(&e.to_string()),
            },
            "clear" | "cls" => {
                self.ui.clear_screen();
            }
            "model" => {
                if rest.is_empty() {
                    self.cycle_model();
                } else {
                    self.switch_model(rest);
                }
            }
            "undo" => match self.session.tools.undo_last() {
                Ok(msg) => self.ui.ok(&msg),
                Err(e) => self.ui.error(&e.to_string()),
            },
            "ctx" => {
                if let Ok(n) = rest.parse::<usize>() {
                    self.config.ctx_tokens = n;
                    self.session.config.options.num_ctx = n as u32;
                    self.session.context.config.ctx_tokens = n;
                    self.ui.ok(&format!("context budget: {n} tokens"));
                } else {
                    self.ui.info(&format!(
                        "current context budget: {} tokens",
                        self.config.ctx_tokens
                    ));
                }
            }
            "history" => self.handle_history(rest),
            "resume" => self.handle_resume(rest),
            "memory" => self.handle_memory(rest),
            "tail" => {
                if let Ok(n) = rest.parse::<usize>() {
                    self.config.history_messages = n;
                    self.session.context.config.history_messages = n;
                    self.ui.ok(&format!("trailing window: {n} messages"));
                } else {
                    self.ui.info(&format!(
                        "current trailing window: {} messages",
                        self.config.history_messages
                    ));
                }
            }
            "files" => {
                let files = self.session.context.open_files();
                if files.is_empty() {
                    self.ui.info("no files in the working set yet.");
                } else {
                    for f in files {
                        println!("  - {} ({} lines)", f.path, f.content.lines().count());
                    }
                }
            }
            "mode" => {
                if rest.is_empty() {
                    self.ui.info(&format!(
                        "current mode: {} (read-only | confirm | auto)",
                        self.session.tools.registry().mode().as_str()
                    ));
                } else if let Some(mode) = nah_core::tools::Mode::parse(rest) {
                    self.session.tools.registry_mut().set_mode(mode);
                    self.ui.ok(&format!("mode set to {}", mode.as_str()));
                } else {
                    self.ui.info("usage: /mode read-only | confirm | auto");
                }
            }
            "tools" => self.handle_tools(rest),
            "root" => {
                self.ui.info(&format!(
                    "project root: {}",
                    nah_core::tools::display_path(self.session.tools.root())
                ));
            }
            "yes" => {
                self.config.auto_approve = true;
                self.ui
                    .ok("file writes and command runs will be auto-approved.");
            }
            "no" => {
                self.config.auto_approve = false;
                self.ui
                    .ok("file writes and command runs now require confirmation.");
            }
            "compact" => match self.session.force_compact() {
                Ok(0) => self.ui.info("nothing to compact yet."),
                Ok(n) => {
                    self.ui.ok(&format!("summarized {n} older message(s)."));
                    let (tokens, msgs) = self.session.context_usage();
                    self.ui.stat(&format!(
                        "context: ~{}/{} tokens - {} messages",
                        tokens, self.config.ctx_tokens, msgs
                    ));
                }
                Err(e) => self.ui.error(&e.to_string()),
            },
            "reset" => {
                self.finish_session();
                self.session.reset();
                self.ui.ok("session cleared.");
            }
            "status" => {
                let (tokens, msgs) = self.session.context_usage();
                println!(
                    "  model: {} | provider: {} | server: {}\n  ctx: {} tokens (~{} used, {} messages) | tail: {} | auto-approve: {}\n  root: {}",
                    self.config.model,
                    self.config.provider.as_str(),
                    self.config.url,
                    self.config.ctx_tokens,
                    tokens,
                    msgs,
                    self.config.history_messages,
                    if self.config.auto_approve { "on" } else { "off" },
                    nah_core::tools::display_path(self.session.tools.root())
                );
            }
            other => {
                self.ui
                    .error(&format!("unknown command: /{other} - type /help"));
            }
        }
        false
    }

    /// Push config changes (from the wizard) into the live session.
    fn sync_session_config(&mut self) {
        self.session.config.model = self.config.model.clone();
        self.session.config.options = self.config.chat_options();
        self.session.context.config = self.config.context_config();
    }
}

/// Non-interactive one-shot mode: `nah -p "prompt"` (used in scripts & CI).
pub fn run_oneshot(
    ui: &Ui,
    config: &NahConfig,
    provider: Box<dyn LlmProvider + '_>,
    prompt: &str,
) -> Result<(), CoreError> {
    let mut tools = ToolKit::new(config.root.clone(), config.max_tool_bytes);
    for w in tools.load_custom_tools(&nah_core::config::tools_dir()) {
        ui.info(&format!("[warn] {w}"));
    }
    let mut session = Session::new(
        provider,
        SessionConfig {
            model: config.model.clone(),
            options: config.chat_options(),
            max_steps: config.max_steps,
        },
        tools,
        ContextManager::new(config.context_config()),
    );
    session.bind_stop_handle(crate::ctrlc::stop_handle());

    let auto = config.auto_approve;
    ui.spinner_on();
    let mut confirm_fn = |req: &ConfirmRequest| -> bool {
        if auto {
            return true;
        }
        ui.confirm(&req.verb, &req.path, &req.preview, req.is_new_file)
    };
    let mut on_event = |ev: &Event<'_>| match ev {
        Event::Delta(text) => ui.delta(text),
        Event::Action(a) => {
            println!();
            ui.action(&nah_core::protocol::action_label(a));
        }
        Event::ToolResult(o) => {
            if o.is_error {
                ui.tool_err(&o.body);
            } else {
                ui.tool_ok(&o.body);
            }
        }
        Event::Info(msg) => {
            println!();
            ui.info(msg);
        }
        Event::TurnDone => {
            ui.stream_flush();
            ui.spinner_off();
            println!();
        }
    };
    crate::ctrlc::set_turn_running(true);
    let result = session.run_turn(prompt, &mut confirm_fn, &mut on_event);
    crate::ctrlc::set_turn_running(false);
    ui.spinner_off();
    ui.stream_flush();
    let stats = result?;
    if stats.est_tokens > 0 {
        ui.stat(&format!(
            "~{} tok/s - {} tokens - {:.1}s",
            stats.tokens_per_sec().round() as u64,
            stats.est_tokens,
            stats.gen_secs
        ));
    }
    Ok(())
}

/// Canonical project path for history records (stable across runs and
/// free of Windows' `\\?\` namespace prefix).
fn root_label(root: &std::path::Path) -> String {
    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    nah_core::tools::display_path(&abs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nah_core::provider::{ChatMessage, ChatRequest, ModelInfo};
    use std::path::{Path, PathBuf};

    /// Provider with a fixed model list and canned replies.
    struct Scripted {
        models: Vec<String>,
    }

    impl LlmProvider for Scripted {
        fn list_models(&mut self) -> Result<Vec<ModelInfo>, CoreError> {
            Ok(self
                .models
                .iter()
                .map(|n| ModelInfo {
                    name: n.clone(),
                    detail: String::new(),
                })
                .collect())
        }

        fn chat_stream(
            &mut self,
            _req: &ChatRequest<'_>,
            _on_delta: &mut dyn FnMut(&str) -> bool,
        ) -> Result<String, CoreError> {
            Ok("ok".to_string())
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn repl_with(models: &[&str], hist_dir: &Path) -> Repl<'static> {
        let config = NahConfig {
            root: PathBuf::from("/tmp/nah_test_repl_root"),
            ..Default::default()
        };
        let mut r = Repl::new(
            Ui::new(false, Some(80)),
            Box::new(Scripted {
                models: models.iter().map(|s| s.to_string()).collect(),
            }),
            config,
        )
        .unwrap();
        r.hist = HistoryStore::new(hist_dir.to_path_buf());
        r
    }

    fn record(id: &str, unix: u64, model: &str) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            unix,
            cwd: "/tmp/nah_test_repl_root".to_string(),
            model: model.to_string(),
            messages: vec![
                ChatMessage::user("first request"),
                ChatMessage::assistant("done"),
            ],
        }
    }

    #[test]
    fn tail_command_sets_the_trailing_window() {
        let dir = temp("nah_test_repl_tail");
        let mut r = repl_with(&[], &dir);
        assert!(!r.handle_command("tail 7"));
        assert_eq!(r.config.history_messages, 7);
        assert_eq!(r.session.context.config.history_messages, 7);
        // /history no longer changes the setting - it browses sessions
        r.handle_command("history 7");
        assert_eq!(r.config.history_messages, 7);
    }

    #[test]
    fn switch_model_finalizes_the_record() {
        let dir = temp("nah_test_repl_switch");
        let mut r = repl_with(&["m1", "m2"], &dir);
        r.config.model = "m1".to_string();
        r.session.config.model = "m1".to_string();
        r.session.log.push(ChatMessage::user("hello"));
        r.session.history.push(ChatMessage::user("hello"));
        r.switch_model("m2");
        let items = r.hist.list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].model, "m1", "record keeps the model it ran with");
        assert!(r.current_id.is_none());
        assert!(r.session.log.is_empty());
    }

    #[test]
    fn resume_restores_saved_conversation() {
        let dir = temp("nah_test_repl_resume");
        let mut r = repl_with(&["m1", "m2"], &dir);
        r.hist.save(&record("2000-bbbb", 2000, "m2")).unwrap();
        r.config.model = "m1".to_string();
        r.session.config.model = "m1".to_string();
        r.handle_resume("2000-bbbb");
        assert_eq!(r.session.history.len(), 2);
        assert_eq!(r.session.log.len(), 2);
        assert_eq!(r.current_id.as_deref(), Some("2000-bbbb"));
        assert_eq!(r.config.model, "m2", "model restored from the record");
        assert_eq!(r.saved_log, 2);
        // continuing the conversation grows the same record
        r.session.log.push(ChatMessage::user("more"));
        r.session.history.push(ChatMessage::user("more"));
        r.save_history();
        let items = r.hist.list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].messages, 3);
    }

    #[test]
    fn resume_without_args_skips_the_current_conversation() {
        let dir = temp("nah_test_repl_resume2");
        let mut r = repl_with(&["m1"], &dir);
        r.hist.save(&record("1000-aaaa", 1000, "m1")).unwrap();
        r.hist.save(&record("2000-bbbb", 2000, "m1")).unwrap();
        r.config.model = "m1".to_string();
        r.session.config.model = "m1".to_string();
        r.current_id = Some("2000-bbbb".to_string());
        r.handle_resume("");
        assert_eq!(r.current_id.as_deref(), Some("1000-aaaa"));
    }
}
