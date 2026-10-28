//! The agent loop: user text -> model -> tool blocks -> tool results -> model ...

use crate::context::ContextManager;
use crate::error::CoreError;
use crate::protocol::{self, Action};
use crate::provider::{ChatMessage, ChatOptions, ChatRequest, LlmProvider};
use crate::tools::{ConfirmRequest, ToolKit, ToolOutput};

/// Events surfaced to the UI while a turn runs.
pub enum Event<'a> {
    /// A raw text fragment streamed from the model.
    Delta(&'a str),
    /// The model asked for a tool.
    Action(&'a Action),
    /// A tool finished (ok or error).
    ToolResult(&'a ToolOutput),
    /// Progress / warning line.
    Info(String),
    /// The turn is over.
    TurnDone,
}

pub struct SessionConfig {
    pub model: String,
    pub options: ChatOptions,
    /// Hard cap on model<->tool round trips per user turn.
    pub max_steps: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            model: String::new(),
            options: ChatOptions::default(),
            max_steps: 8,
        }
    }
}

pub fn system_intro(root: &str) -> String {
    format!(
        "You are `nah`, a careful coding agent working inside a project.\n\
         Project root: {root}\n\
         You read and edit files with the text tool protocol described below.\n\
         Be concise and practical. Prefer EDIT_FILE over rewriting whole files."
    )
}

pub struct Session<'a> {
    pub provider: Box<dyn LlmProvider + 'a>,
    pub config: SessionConfig,
    pub tools: ToolKit,
    pub context: ContextManager,
    pub history: Vec<ChatMessage>,
    pinned_task: Option<String>,
}

impl<'a> Session<'a> {
    pub fn new(
        provider: Box<dyn LlmProvider + 'a>,
        config: SessionConfig,
        tools: ToolKit,
        context: ContextManager,
    ) -> Self {
        Session {
            provider,
            config,
            tools,
            context,
            history: Vec::new(),
            pinned_task: None,
        }
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.context.reset();
        self.pinned_task = None;
    }

    /// Run one user turn to completion (or until max_steps).
    pub fn run_turn(
        &mut self,
        user_input: &str,
        confirm: &mut dyn FnMut(&ConfirmRequest) -> bool,
        on_event: &mut dyn FnMut(&Event<'_>),
    ) -> Result<(), CoreError> {
        if self.pinned_task.is_none() {
            self.pinned_task = Some(user_input.to_string());
        }
        self.history.push(ChatMessage::user(user_input));

        for _step in 0..self.config.max_steps {
            // compaction keeps old turns from eating the window
            if self.context.wants_compaction(&self.history) {
                match self.context.compact(
                    self.provider.as_mut(),
                    &self.config.model,
                    &self.history,
                ) {
                    Ok(cut) if cut > 0 => {
                        self.history.drain(..cut);
                        on_event(&Event::Info(format!(
                            "[context] summarized {cut} older message(s)"
                        )));
                    }
                    Ok(_) => {}
                    Err(e) => on_event(&Event::Info(format!("[context] compaction skipped: {e}"))),
                }
            }

            let messages = self.context.build(
                &system_intro(&self.tools.root().display().to_string()),
                protocol::tool_protocol_doc(),
                self.pinned_task.as_deref(),
                &self.history,
            );
            let req = ChatRequest {
                model: &self.config.model,
                messages: &messages,
                options: &self.config.options,
            };
            let reply = self
                .provider
                .chat_stream(&req, &mut |delta| on_event(&Event::Delta(delta)))?;

            let parsed = protocol::parse(&reply);
            for block in &parsed.invalid_blocks {
                on_event(&Event::Info(format!(
                    "[warn] model emitted an invalid tool block (kept as text):\n{block}"
                )));
            }

            if parsed.actions.is_empty() {
                self.history.push(ChatMessage::assistant(reply));
                on_event(&Event::TurnDone);
                return Ok(());
            }

            self.history.push(ChatMessage::assistant(reply));

            let mut results = String::from("[TOOL RESULTS]");
            for action in &parsed.actions {
                on_event(&Event::Action(action));
                let output = self.tools.run(action, confirm);
                // keep the file cache coherent
                match action {
                    Action::ReadFile { .. } if !output.is_error => {}
                    Action::WriteFile { path, .. } | Action::EditFile { path, .. } => {
                        self.context.invalidate_file(path);
                    }
                    _ => {}
                }
                on_event(&Event::ToolResult(&output));
                results.push('\n');
                results.push_str(&output.to_model_message());
                results.push('\n');
            }
            self.history.push(ChatMessage::user(results));
        }

        on_event(&Event::Info(
            "[warn] step limit reached — asking for a final answer".into(),
        ));
        let messages = self.context.build(
            &system_intro(&self.tools.root().display().to_string()),
            protocol::tool_protocol_doc(),
            self.pinned_task.as_deref(),
            &self.history,
        );
        let req = ChatRequest {
            model: &self.config.model,
            messages: &messages,
            options: &self.config.options,
        };
        let reply = self
            .provider
            .chat_stream(&req, &mut |delta| on_event(&Event::Delta(delta)))?;
        self.history.push(ChatMessage::assistant(reply));
        on_event(&Event::TurnDone);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextConfig;
    use crate::provider::{ChatRequest, LlmProvider, ModelInfo};
    use std::cell::RefCell;

    /// Scripted provider: plays back canned replies in order.
    struct Scripted {
        replies: Vec<String>,
        idx: RefCell<usize>,
    }

    impl LlmProvider for Scripted {
        fn list_models(&mut self) -> Result<Vec<ModelInfo>, CoreError> {
            Ok(vec![ModelInfo {
                name: "test".into(),
                detail: String::new(),
            }])
        }

        fn chat_stream(
            &mut self,
            _req: &ChatRequest<'_>,
            on_delta: &mut dyn FnMut(&str),
        ) -> Result<String, CoreError> {
            let i = *self.idx.borrow();
            let reply = self
                .replies
                .get(i)
                .cloned()
                .unwrap_or_else(|| "done".to_string());
            *self.idx.borrow_mut() = i + 1;
            on_delta(&reply);
            Ok(reply)
        }
    }

    fn session_with(replies: Vec<String>, dir: &std::path::Path) -> Session<'static> {
        let provider = Scripted {
            replies,
            idx: RefCell::new(0),
        };
        let cfg = ContextConfig {
            ctx_tokens: 4096,
            history_messages: 5,
            max_open_files: 3,
            auto_compact: false,
        };
        Session::new(
            Box::new(provider),
            SessionConfig {
                model: "test".into(),
                ..Default::default()
            },
            ToolKit::new(dir.to_path_buf(), 64 * 1024),
            ContextManager::new(cfg),
        )
    }

    #[test]
    fn system_prompt_does_not_force_a_language() {
        // The system prompt must not name or force any language; models
        // mirror the user's language naturally. Forcing one makes the model
        // answer e.g. Persian to English input.
        let s = system_intro("/tmp/project");
        let lower = s.to_lowercase();
        assert!(!lower.contains("persian"), "language forced: {s}");
        assert!(!lower.contains("arabic"), "language forced: {s}");
        assert!(
            !lower.contains("language"),
            "language directive present: {s}"
        );
        assert!(!s.contains("زبان"), "language forced: {s}");
    }

    #[test]
    fn full_loop_writes_file_and_finishes() {
        let dir = std::env::temp_dir().join("nah_test_session");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let replies = vec![
            "می‌سازمش:\n```tool\nWRITE_FILE\npath: hello.txt\ncontent:\nسلام دنیا\nEND_OF_CONTENT\n```"
                .to_string(),
            "تمام شد.".to_string(),
        ];
        let mut s = session_with(replies, &dir);
        let mut events = 0;
        let mut turn_done = false;
        s.run_turn(
            "یه فایل بساز",
            &mut |_: &ConfirmRequest| true,
            &mut |ev| {
                events += 1;
                if matches!(ev, Event::TurnDone) {
                    turn_done = true;
                }
            },
        )
        .unwrap();
        assert!(turn_done);
        assert!(events > 0);
        let written = std::fs::read_to_string(dir.join("hello.txt")).unwrap();
        assert_eq!(written, "سلام دنیا\n");
    }

    #[test]
    fn step_limit_stops_runaway_models() {
        let dir = std::env::temp_dir().join("nah_test_steps");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // model never stops requesting tool calls
        let replies = vec!["```tool\nLIST_DIR\npath: .\n```".to_string(); 20];
        let mut s = session_with(replies, &dir);
        s.run_turn("go", &mut |_: &ConfirmRequest| true, &mut |_| {})
            .unwrap();
        // history: user + (assistant+user)*steps + final assistant
        assert!(s.history.len() <= 2 + 8 * 2 + 1);
    }
}
