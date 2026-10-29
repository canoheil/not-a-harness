//! The interactive REPL: line input, slash commands, session wiring.
//!
//! v1 reads lines through the console's own line editor (canonical mode):
//! the terminal emulator renders the echo with proper BiDi, so Persian /
//! Arabic input displays correctly without any in-process text shaping.

use nah_core::config::NahConfig;
use nah_core::context::ContextManager;
use nah_core::error::CoreError;
use nah_core::provider::LlmProvider;
use nah_core::session::{Event, Session, SessionConfig};
use nah_core::tools::{ConfirmRequest, ToolKit};

use crate::setup;
use crate::ui::Ui;

pub struct Repl<'a> {
    pub ui: Ui,
    pub session: Session<'a>,
    pub config: NahConfig,
}

impl<'a> Repl<'a> {
    pub fn new(
        ui: Ui,
        provider: Box<dyn LlmProvider + 'a>,
        config: NahConfig,
    ) -> Result<Repl<'a>, CoreError> {
        let tools = ToolKit::new(config.root.clone(), config.max_tool_bytes);
        let session = Session::new(
            provider,
            SessionConfig {
                model: config.model.clone(),
                options: config.chat_options(),
                max_steps: config.max_steps,
            },
            tools,
            ContextManager::new(config.context_config()),
        );
        Ok(Repl {
            ui,
            session,
            config,
        })
    }

    pub fn run(&mut self) -> Result<(), CoreError> {
        self.ui.banner();
        self.ui.rtl_font_notice();
        self.ui.info(&format!(
            "model: {} | server: {} | ctx: {} tokens",
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

            if let Some(cmd) = trimmed.strip_prefix('/') {
                if self.handle_command(cmd.trim()) {
                    break;
                }
                continue;
            }

            self.run_turn(trimmed);
        }
        self.ui.info("bye.");
        Ok(())
    }

    fn run_turn(&mut self, input: &str) {
        println!();
        self.ui.divider();
        self.ui.spinner_on();

        let mut confirm_fn = |req: &ConfirmRequest| -> bool {
            self.ui
                .confirm(req.verb, &req.path, &req.preview, req.is_new_file)
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

        match self.session.run_turn(input, &mut confirm_fn, &mut on_event) {
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
    }

    /// Returns `true` when the REPL should exit. Never fails: command
    /// errors are printed and the session keeps running.
    fn handle_command(&mut self, cmd: &str) -> bool {
        let (name, rest) = match cmd.split_once(' ') {
            Some((n, r)) => (n, r.trim()),
            None => (cmd, ""),
        };
        match name {
            "exit" | "quit" | "q" => return true,
            "help" | "h" | "?" => {
                println!(
                    r#"commands:
  /setup            run the setup wizard (provider, URL, default model)
  /provider         scan for local model servers and switch to one
  /models           list models available on the server
  /model <name>     switch the model
  /ctx <n>          set the context budget (and num_ctx)
  /history <n>      number of trailing messages always kept
  /compact          summarize older history now (frees context space)
  /files            show files in the working set
  /root             show the project root
  /yes | /no        toggle auto-approve for file writes and commands
  /reset            clear conversation history and session memory
  /status           show current settings and context usage
  /exit             quit (Ctrl+C also exits)
  (anything else is sent to the model as-is)"#
                );
            }
            "setup" => {
                if let Err(e) = setup::run(&self.ui, &mut self.config) {
                    self.ui.error(&e.to_string());
                } else {
                    // reconnect: URL or provider kind may have changed
                    match nah_core::provider::connect(self.config.provider, &self.config.url) {
                        Ok(p) => {
                            self.session.provider = p;
                            self.sync_session_config();
                        }
                        Err(e) => self
                            .ui
                            .error(&format!("saved config, but reconnect failed: {e}")),
                    }
                }
            }
            "provider" => {
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
                                self.sync_session_config();
                                self.ui.ok(&format!(
                                    "connected to {} at {} with model '{}'",
                                    self.config.provider.as_str(),
                                    self.config.url,
                                    self.config.model
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
            "model" => {
                if rest.is_empty() {
                    self.ui
                        .info(&format!("current model: {}", self.config.model));
                } else {
                    self.config.model = rest.to_string();
                    self.session.config.model = rest.to_string();
                    self.ui.ok(&format!("model set to {rest}"));
                }
            }
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
            "history" => {
                if let Ok(n) = rest.parse::<usize>() {
                    self.config.history_messages = n;
                    self.session.context.config.history_messages = n;
                    self.ui.ok(&format!("history window: {n} messages"));
                } else {
                    self.ui.info(&format!(
                        "current history window: {} messages",
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
            "root" => {
                self.ui.info(&format!(
                    "project root: {}",
                    self.session.tools.root().display()
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
                self.session.reset();
                self.ui.ok("session cleared.");
            }
            "status" => {
                let (tokens, msgs) = self.session.context_usage();
                println!(
                    "  model: {} | provider: {} | server: {}\n  ctx: {} tokens (~{} used, {} messages) | history: {} | auto-approve: {}\n  root: {}",
                    self.config.model,
                    self.config.provider.as_str(),
                    self.config.url,
                    self.config.ctx_tokens,
                    tokens,
                    msgs,
                    self.config.history_messages,
                    if self.config.auto_approve { "on" } else { "off" },
                    self.session.tools.root().display()
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
    let tools = ToolKit::new(config.root.clone(), config.max_tool_bytes);
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

    let auto = config.auto_approve;
    ui.spinner_on();
    let mut confirm_fn = |req: &ConfirmRequest| -> bool {
        if auto {
            return true;
        }
        ui.confirm(req.verb, &req.path, &req.preview, req.is_new_file)
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
    let result = session.run_turn(prompt, &mut confirm_fn, &mut on_event);
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
