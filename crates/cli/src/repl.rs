//! The interactive REPL: line input, slash commands, session wiring.
//!
//! v1 reads lines through the console's own line editor (canonical mode):
//! the terminal emulator renders the echo with proper BiDi — the safest
//! possible RTL story. A fancier in-process editor (history, completion)
//! can be layered on later without touching the core.

use std::io::BufRead;

use nah_core::config::NahConfig;
use nah_core::context::ContextManager;
use nah_core::error::CoreError;
use nah_core::provider::{self, LlmProvider};
use nah_core::session::{Event, Session, SessionConfig};
use nah_core::tools::{ConfirmRequest, ToolKit};

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
        self.ui.info(&format!(
            "مدل: {} · سرور: {} · کانتکست: {} توکن",
            self.config.model, self.config.url, self.config.ctx_tokens
        ));
        println!();

        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();
        loop {
            self.ui.prompt();
            let Some(line) = lines.next() else {
                println!();
                break; // EOF
            };
            let line = line.map_err(|e| CoreError::Io(e.to_string()))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(cmd) = trimmed.strip_prefix('/') {
                if self.handle_command(cmd.trim())? {
                    break;
                }
                continue;
            }

            self.run_turn(trimmed);
        }
        self.ui.info("خداحافظ! 👋");
        Ok(())
    }

    fn run_turn(&mut self, input: &str) {
        self.ui.user_echo(input);
        println!();

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
                println!();
                println!();
            }
        };

        if let Err(e) = self.session.run_turn(input, &mut confirm_fn, &mut on_event) {
            self.ui.error(&e.to_string());
        }
    }

    /// Returns Ok(true) when the REPL should exit.
    fn handle_command(&mut self, cmd: &str) -> Result<bool, CoreError> {
        let (name, rest) = match cmd.split_once(' ') {
            Some((n, r)) => (n, r.trim()),
            None => (cmd, ""),
        };
        match name {
            "exit" | "quit" | "q" => return Ok(true),
            "help" | "h" | "?" => {
                println!(
                    r#"دستورها:
  /models           نمایش مدل‌های موجود روی سرور
  /model <name>     تعویض مدل
  /ctx <n>          تنظیم بودجه‌ی کانتکست (و num_ctx)
  /history <n>      تعداد پیام‌های پایانی همیشگی
  /files            فایل‌های موجود در working set
  /root             نمایش ریشه‌ی پروژه
  /yes  | /no       تأیید خودکار نوشتن فایل روشن/خاموش
  /reset            پاک کردن تاریخچه و حافظه‌ی جلسه
  /status           وضعیت فعلی
  /exit             خروج
  (هر متن دیگری مستقیماً برای مدل فرستاده می‌شود)"#
                );
            }
            "models" => {
                let models = self
                    .session
                    .provider
                    .list_models()
                    .map_err(|e| CoreError::Provider(e.to_string()))?;
                let pairs: Vec<(String, String)> =
                    models.into_iter().map(|m| (m.name, m.detail)).collect();
                self.ui.models(&pairs);
            }
            "model" => {
                if rest.is_empty() {
                    self.ui.info(&format!("مدل فعلی: {}", self.config.model));
                } else {
                    self.config.model = rest.to_string();
                    self.session.config.model = rest.to_string();
                    self.ui.ok(&format!("مدل روی {rest} تنظیم شد."));
                }
            }
            "ctx" => {
                if let Ok(n) = rest.parse::<usize>() {
                    self.config.ctx_tokens = n;
                    self.session.config.options.num_ctx = n as u32;
                    self.session.context.config.ctx_tokens = n;
                    self.ui.ok(&format!("بودجه‌ی کانتکست: {n} توکن"));
                } else {
                    self.ui
                        .info(&format!("کانتکست فعلی: {} توکن", self.config.ctx_tokens));
                }
            }
            "history" => {
                if let Ok(n) = rest.parse::<usize>() {
                    self.config.history_messages = n;
                    self.session.context.config.history_messages = n;
                    self.ui.ok(&format!("پنجره‌ی تاریخچه: {n} پیام"));
                }
            }
            "files" => {
                let files = self.session.context.open_files();
                if files.is_empty() {
                    self.ui.info("فعلاً فایلی در حافظه نیست.");
                } else {
                    for f in files {
                        println!("  • {} ({} خط)", f.path, f.content.lines().count());
                    }
                }
            }
            "root" => {
                self.ui.info(&format!(
                    "ریشه‌ی پروژه: {}",
                    self.session.tools.root().display()
                ));
            }
            "yes" => {
                self.config.auto_approve = true;
                self.ui.ok("تأیید خودکار فایل‌ها روشن شد.");
            }
            "no" => {
                self.config.auto_approve = false;
                self.ui.ok("تأیید خودکار فایل‌ها خاموش شد.");
            }
            "reset" => {
                self.session.reset();
                self.ui.ok("جلسه پاک شد.");
            }
            "status" => {
                println!(
                    "  مدل: {} · سرور: {} · provider: {:?}\n  کانتکست: {} · تاریخچه: {} پیام · تأیید خودکار: {}",
                    self.config.model,
                    self.config.url,
                    self.config.provider,
                    self.config.ctx_tokens,
                    self.config.history_messages,
                    if self.config.auto_approve { "روشن" } else { "خاموش" }
                );
            }
            other => {
                self.ui
                    .error(&format!("دستور ناشناخته: /{other} — برای راهنما /help"));
            }
        }
        Ok(false)
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
        Event::TurnDone => println!(),
    };
    session.run_turn(prompt, &mut confirm_fn, &mut on_event)?;
    Ok(())
}

/// Keep the import used in every build.
pub fn _provider_connect(
    kind: provider::ProviderKind,
    url: &str,
) -> Result<Box<dyn LlmProvider>, CoreError> {
    provider::connect(kind, url)
}
