//! The agent loop: user text -> model -> tool blocks -> tool results -> model ...

use crate::context::ContextManager;
use crate::error::CoreError;
use crate::protocol::{self, Action};
use crate::provider::{ChatMessage, ChatOptions, ChatRequest, LlmProvider};
use crate::tools::{ConfirmRequest, ToolKit, ToolOutput};
use std::collections::HashMap;

/// Generation statistics for one completed turn: estimated tokens the
/// model produced and the time spent generating (first token to last
/// token per reply, summed - tool execution gaps are excluded).
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnStats {
    pub est_tokens: usize,
    pub gen_secs: f64,
}

impl TurnStats {
    pub fn tokens_per_sec(&self) -> f64 {
        if self.gen_secs > 0.0 {
            self.est_tokens as f64 / self.gen_secs
        } else {
            0.0
        }
    }
}

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
    let (platform, shell_hint) = if cfg!(windows) {
        (
            "Windows",
            "cmd /C - use Windows command syntax (e.g. `mkdir` without `-p`, `dir`, `type`)",
        )
    } else {
        ("Linux/macOS", "sh -c - use POSIX shell syntax")
    };
    format!(
        "You are `nah`, a coding agent working inside a project directory.\n\
         Project root: {root}\n\
         Platform: {platform}; commands run via {shell_hint}\n\n\
         You can only read and change files through the tools below. Work step\n\
         by step: look at what exists, change one thing at a time, check the\n\
         result, and continue until the task is complete.\n\n\
         Speak to the user directly in the first person and in plain prose -\n\
         never call yourself \"the assistant\", never narrate your internal\n\
         reasoning, never repeat the same sentence or message twice, and never\n\
         copy [TOOL RESULTS] markers into your own text.\n\
         Never claim a step succeeded unless a tool result confirms it, and\n\
         never fabricate tool results or file contents. For simple greetings\n\
         or questions that need no file work, reply in plain text without\n\
         calling any tools."
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

    /// Approximate context usage for the UI: (tokens, messages).
    pub fn context_usage(&self) -> (usize, usize) {
        (
            self.context.estimate_usage(&self.history),
            self.history.len(),
        )
    }

    /// Manually trigger history summarization (/compact). Returns the
    /// number of messages absorbed into the rolling summary.
    pub fn force_compact(&mut self) -> Result<usize, CoreError> {
        let cut =
            self.context
                .compact(self.provider.as_mut(), &self.config.model, &self.history)?;
        if cut > 0 {
            self.history.drain(..cut);
        }
        Ok(cut)
    }

    /// Run one user turn to completion (or until max_steps). Returns
    /// generation statistics (estimated tokens, generation time).
    pub fn run_turn(
        &mut self,
        user_input: &str,
        confirm: &mut dyn FnMut(&ConfirmRequest) -> bool,
        on_event: &mut dyn FnMut(&Event<'_>),
    ) -> Result<TurnStats, CoreError> {
        if self.pinned_task.is_none() {
            self.pinned_task = Some(user_input.to_string());
        }
        self.history.push(ChatMessage::user(user_input));

        // duplicate-call loop breaker state (per turn)
        let mut action_counts: HashMap<String, usize> = HashMap::new();
        // generation statistics (per reply: first token -> last token)
        let mut stats = TurnStats::default();

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
                &system_intro(&crate::tools::display_path(self.tools.root())),
                &protocol::tool_protocol_doc(self.tools.registry()),
                self.pinned_task.as_deref(),
                &self.history,
            );
            let req = ChatRequest {
                model: &self.config.model,
                messages: &messages,
                options: &self.config.options,
            };
            let mut first_token: Option<std::time::Instant> = None;
            let reply = self.provider.chat_stream(&req, &mut |delta| {
                if first_token.is_none() {
                    first_token = Some(std::time::Instant::now());
                }
                on_event(&Event::Delta(delta))
            })?;
            stats.est_tokens += crate::context::estimate_tokens(&reply);
            if let Some(t) = first_token {
                stats.gen_secs += t.elapsed().as_secs_f64();
            }

            let parsed = protocol::parse(&reply);
            for _block in &parsed.invalid_blocks {
                on_event(&Event::Info(
                    "[warn] model emitted an invalid tool block (ignored) - check the tool name and required parameters, then retry".to_string(),
                ));
            }

            // store the reply clean: XML calls normalized to fenced blocks
            // (so the model learns the right format from its own history),
            // echoed internal markers and runaway repetition removed.
            let stored = protocol::sanitize_text(&protocol::normalize_function_calls(&reply), 40);

            if parsed.actions.is_empty() {
                self.history.push(ChatMessage::assistant(stored));
                on_event(&Event::TurnDone);
                return Ok(stats);
            }

            self.history.push(ChatMessage::assistant(stored));

            let mut results = String::from("[TOOL RESULTS]");
            for action in &parsed.actions {
                on_event(&Event::Action(action));
                // Loop breaker: local models sometimes get stuck repeating
                // the same call. A repeat gets a hint; a third identical
                // call is blocked so the turn cannot spin forever.
                let sig = action_signature(action);
                let n = *action_counts.get(&sig).unwrap_or(&0);
                let output = if n >= 2 {
                    ToolOutput {
                        verb: action.verb(),
                        path: action.path().to_string(),
                        body: "duplicate call blocked: this exact action already ran twice \
                               in this turn and had no further effect. Do the NEXT step, \
                               or finish with a plain-text answer."
                            .to_string(),
                        is_error: true,
                        changed: false,
                    }
                } else {
                    let mut out = self.tools.run(action, confirm);
                    if n == 1 {
                        out.body = if out.is_error {
                            format!(
                                "{}\n(this call FAILED - repeating it will not help. Try a \
                                 different path or approach, and never claim success after \
                                 an error.)",
                                out.body.trim_end()
                            )
                        } else {
                            format!(
                                "{}\n(this is a repeat of an earlier call - if the goal is \
                                 already achieved, move on to the next step or finish)",
                                out.body.trim_end()
                            )
                        };
                    }
                    if out.changed {
                        // real state change: the loop breaker resets
                        action_counts.clear();
                    }
                    out
                };
                *action_counts.entry(sig).or_insert(0) += 1;
                // keep the file cache coherent
                match action {
                    Action::ReadFile { .. } if !output.is_error => {}
                    Action::WriteFile { path, .. } | Action::EditFile { path, .. } => {
                        self.context.invalidate_file(path);
                    }
                    Action::RunCommand { .. } => {
                        // a command may touch any file - drop all cached contents
                        self.context.reset();
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
        // Tell the model explicitly to stop calling tools - otherwise it
        // answers with yet another tool block that nobody can run.
        self.history.push(ChatMessage::user(
            "[SYSTEM] Tool-call budget is exhausted. Do not request any tools. \
             Reply now with a plain-text final answer for the user, summarizing \
             what was done."
                .to_string(),
        ));
        let messages = self.context.build(
            &system_intro(&crate::tools::display_path(self.tools.root())),
            &protocol::tool_protocol_doc(self.tools.registry()),
            self.pinned_task.as_deref(),
            &self.history,
        );
        let req = ChatRequest {
            model: &self.config.model,
            messages: &messages,
            options: &self.config.options,
        };
        let mut first_token: Option<std::time::Instant> = None;
        let reply = self.provider.chat_stream(&req, &mut |delta| {
            if first_token.is_none() {
                first_token = Some(std::time::Instant::now());
            }
            on_event(&Event::Delta(delta))
        })?;
        stats.est_tokens += crate::context::estimate_tokens(&reply);
        if let Some(t) = first_token {
            stats.gen_secs += t.elapsed().as_secs_f64();
        }
        // store the final answer clean as well - a reply that is only
        // echoed markers is no reply at all (the user must see that).
        let stored = protocol::sanitize_text(&protocol::normalize_function_calls(&reply), 40);
        if protocol::parse(&stored).prose.trim().is_empty() {
            on_event(&Event::Info(
                "[warn] the model produced no final answer text".into(),
            ));
        }
        self.history.push(ChatMessage::assistant(stored));
        on_event(&Event::TurnDone);
        Ok(stats)
    }
}

/// Identity of an action for the duplicate-call loop breaker. Content
/// hashes keep "write A, edit B, write A-back" distinct from loops.
fn action_signature(action: &Action) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    fn h(s: &str) -> u64 {
        let mut d = DefaultHasher::new();
        s.hash(&mut d);
        d.finish()
    }
    match action {
        Action::ReadFile {
            path,
            offset,
            limit,
        } => format!("R:{path}:{offset:?}:{limit:?}"),
        Action::WriteFile { path, content } => format!("W:{path}:{}", h(content)),
        Action::EditFile {
            path,
            search,
            replace,
        } => format!("E:{path}:{}:{}", h(search), h(replace)),
        Action::ListDir { path } => format!("L:{path}"),
        Action::DeleteFile { path } => format!("D:{path}"),
        Action::MoveFile { path, to } => format!("M:{path}->{to}"),
        Action::RunCommand {
            command,
            cwd,
            timeout,
        } => format!("C:{command}:{cwd:?}:{timeout:?}"),
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
        // answer e.g. Persian to English input. Covers the full prompt:
        // intro + tool doc (incl. the worked example).
        let s = format!(
            "{}\n\n{}",
            system_intro("/tmp/project"),
            crate::protocol::tool_protocol_doc(&crate::tools::ToolRegistry::builtin())
        );
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
        // history: user + (assistant+user)*steps + [SYSTEM] user + final assistant
        assert!(s.history.len() <= 2 + 8 * 2 + 2);
    }

    #[test]
    fn duplicate_actions_get_a_note_then_are_blocked() {
        let dir = std::env::temp_dir().join("nah_test_dup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // model repeats the same LIST_DIR forever
        let replies = vec!["```tool\nLIST_DIR\npath: .\n```".to_string(); 8];
        let mut s = session_with(replies, &dir);
        let mut notes = Vec::new();
        let mut blocks = 0;
        s.run_turn("go", &mut |_: &ConfirmRequest| true, &mut |ev| {
            if let Event::ToolResult(o) = ev {
                if o.body.contains("this is a repeat of an earlier call") {
                    notes.push(());
                }
                if o.body.contains("duplicate call blocked") {
                    blocks += 1;
                }
            }
        })
        .unwrap();
        // 2nd identical call gets a note, 3rd+ are blocked
        assert_eq!(notes.len(), 1, "exactly one repeat-note expected");
        assert!(blocks >= 1, "duplicates must be blocked");
    }

    #[test]
    fn real_changes_reset_the_loop_breaker() {
        let dir = std::env::temp_dir().join("nah_test_dup_reset");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // write a, write b, write a again - content differs so these are
        // three distinct signatures; a real change must also reset counts
        let replies = vec![
            "```tool\nWRITE_FILE\npath: a.txt\ncontent:\n1\nEND_OF_CONTENT\n```".to_string(),
            "```tool\nWRITE_FILE\npath: a.txt\ncontent:\n2\nEND_OF_CONTENT\n```".to_string(),
            "```tool\nWRITE_FILE\npath: a.txt\ncontent:\n1\nEND_OF_CONTENT\n```".to_string(),
            "done".to_string(),
        ];
        let mut s = session_with(replies, &dir);
        let mut blocked = 0;
        let mut notes = 0;
        s.run_turn("go", &mut |_: &ConfirmRequest| true, &mut |ev| {
            if let Event::ToolResult(o) = ev {
                if o.body.contains("duplicate call blocked") {
                    blocked += 1;
                }
                if o.body.contains("this is a repeat of an earlier call") {
                    notes += 1;
                }
            }
        })
        .unwrap();
        assert_eq!(blocked, 0, "distinct writes must never be blocked");
        assert_eq!(notes, 0, "distinct writes must never be flagged");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "1\n");
    }

    #[test]
    fn final_answer_is_requested_without_tools() {
        let dir = std::env::temp_dir().join("nah_test_final");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // model only ever asks for tools
        let replies = vec!["```tool\nLIST_DIR\npath: .\n```".to_string(); 10];
        let mut s = session_with(replies, &dir);
        s.run_turn("go", &mut |_: &ConfirmRequest| true, &mut |_| {})
            .unwrap();
        // the model must have been told to stop calling tools
        let told = s.history.iter().any(|m| {
            matches!(m.role, crate::provider::Role::User)
                && m.content.contains("Tool-call budget is exhausted")
        });
        assert!(told, "final-answer instruction missing from history");
    }

    #[test]
    fn manual_compaction_absorbs_history() {
        let dir = std::env::temp_dir().join("nah_test_manual_compact");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let replies = vec!["ok".to_string()];
        let mut s = session_with(replies, &dir);
        // build some history the summary can absorb
        for i in 0..8 {
            s.history
                .push(ChatMessage::user(format!("turn {i} happened")));
            s.history
                .push(ChatMessage::assistant(format!("turn {i} done")));
        }
        let before = s.history.len();
        let cut = s.force_compact().unwrap();
        assert!(cut > 0, "something must be summarized");
        assert_eq!(s.history.len(), before - cut);
        assert!(!s.context.summary.is_empty(), "summary must be filled");
        // usage reflects the shortened history
        let (tokens, msgs) = s.context_usage();
        assert_eq!(msgs, s.history.len());
        assert!(tokens > 0);
    }

    #[test]
    fn turn_stats_accumulate_generated_tokens() {
        let dir = std::env::temp_dir().join("nah_test_stats");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // one tool call, then a short final answer
        let replies = vec![
            "```tool\nLIST_DIR\npath: .\n```".to_string(),
            "done.".to_string(),
        ];
        let mut s = session_with(replies, &dir);
        let stats = s
            .run_turn("go", &mut |_: &ConfirmRequest| true, &mut |_| {})
            .unwrap();
        // both replies count: the tool block and the final answer
        let expected = crate::context::estimate_tokens("```tool\nLIST_DIR\npath: .\n```")
            + crate::context::estimate_tokens("done.");
        assert_eq!(stats.est_tokens, expected);
        assert!(stats.gen_secs >= 0.0);
        // division guard
        assert_eq!(TurnStats::default().tokens_per_sec(), 0.0);
        assert!(stats.tokens_per_sec() >= 0.0);
    }
}
