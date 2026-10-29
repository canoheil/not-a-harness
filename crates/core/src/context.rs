//! Context manager: keep the prompt inside a fixed token budget.
//!
//! Policy (per the product spec):
//! * system prompt + tool protocol + pinned task are always sent;
//! * the most recent `history_messages` messages (configurable, default 5);
//! * contents of recently used files (bounded, newest first);
//! * an optional rolling summary produced by the model itself
//!   (compaction) once the history grows beyond a threshold.
//!
//! Everything is measured with a cheap `chars/4` token estimate — good
//! enough to stay inside a 4K window without pulling in a tokenizer.

use crate::provider::{ChatMessage, ChatOptions, ChatRequest, LlmProvider};

#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Total token budget for the request (maps to Ollama `num_ctx`).
    pub ctx_tokens: usize,
    /// How many trailing conversation messages to keep verbatim.
    pub history_messages: usize,
    /// How many opened files to keep in context.
    pub max_open_files: usize,
    /// Ask the model to summarize older history when it gets too long.
    pub auto_compact: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        ContextConfig {
            ctx_tokens: 4096,
            history_messages: 5,
            max_open_files: 3,
            auto_compact: true,
        }
    }
}

pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[derive(Debug, Clone)]
pub struct OpenFile {
    pub path: String,
    pub content: String,
}

pub struct ContextManager {
    pub config: ContextConfig,
    open_files: Vec<OpenFile>,
    pub summary: String,
}

impl ContextManager {
    pub fn new(config: ContextConfig) -> Self {
        ContextManager {
            config,
            open_files: Vec::new(),
            summary: String::new(),
        }
    }

    /// The model read a file — remember its content (replacing any older copy).
    pub fn note_file(&mut self, path: &str, content: &str) {
        self.open_files.retain(|f| f.path != path);
        self.open_files.push(OpenFile {
            path: path.to_string(),
            content: content.to_string(),
        });
        while self.open_files.len() > self.config.max_open_files {
            self.open_files.remove(0);
        }
    }

    /// A file changed on disk — drop any stale cached copy.
    pub fn invalidate_file(&mut self, path: &str) {
        self.open_files.retain(|f| f.path != path);
    }

    pub fn open_files(&self) -> &[OpenFile] {
        &self.open_files
    }

    /// Rough token usage of what a request would carry: summary + working
    /// set + history. Deliberately approximate (chars/4) - it feeds the
    /// UI's context-usage line, not the budget logic.
    pub fn estimate_usage(&self, history: &[ChatMessage]) -> usize {
        let mut total = estimate_tokens(&self.summary);
        for f in &self.open_files {
            total += estimate_tokens(&f.content);
        }
        for m in history {
            total += estimate_tokens(&m.content);
        }
        total
    }

    pub fn reset(&mut self) {
        self.open_files.clear();
        self.summary.clear();
    }

    /// Build the final message list for one model call, within budget.
    pub fn build(
        &self,
        system_intro: &str,
        tool_doc: &str,
        pinned_task: Option<&str>,
        history: &[ChatMessage],
    ) -> Vec<ChatMessage> {
        let budget = self.config.ctx_tokens.max(512);
        let mut used = 0usize;

        // 1) mandatory head: system prompt
        let mut system = String::new();
        system.push_str(system_intro);
        system.push_str("\n\n");
        system.push_str(tool_doc);
        used += estimate_tokens(&system);

        // 2) rolling summary (if any)
        if !self.summary.is_empty() {
            let block = format!("\n\n[SUMMARY OF EARLIER CONVERSATION]\n{}\n", self.summary);
            used += estimate_tokens(&block);
            system.push_str(&block);
        }

        // 3) pinned task — the original request never leaves the context
        if let Some(task) = pinned_task {
            let block = format!("\n\n[ORIGINAL USER TASK]\n{task}\n");
            used += estimate_tokens(&block);
            system.push_str(&block);
        }

        let mut out = vec![ChatMessage::system(system)];

        // 4) open files — newest ones have priority, kept in that order
        let mut files_block = String::new();
        for f in self.open_files.iter().rev() {
            let block = format!(
                "\n--- FILE {} (current content) ---\n{}\n--- END FILE ---\n",
                f.path, f.content
            );
            let cost = estimate_tokens(&block);
            if used + cost > budget * 8 / 10 {
                files_block.push_str("\n(older file contents omitted to fit the context window)\n");
                break;
            }
            used += cost;
            files_block.push_str(&block);
        }
        if !files_block.is_empty() {
            out.push(ChatMessage::system(format!(
                "[WORKING SET — FILES THE HARNESS CURRENTLY HOLDS]{files_block}"
            )));
        }

        // 5) trailing history window: drop oldest first, keep order
        let mut tail: Vec<&ChatMessage> = history
            .iter()
            .rev()
            .take(self.config.history_messages)
            .collect();
        tail.reverse();
        let mut kept: Vec<ChatMessage> = Vec::new();
        for m in tail.into_iter().rev() {
            let cost = estimate_tokens(&m.content);
            if used + cost > budget {
                break;
            }
            used += cost;
            kept.push(m.clone());
        }
        kept.reverse();
        out.extend(kept);
        out
    }

    /// Should we compact the history before the next call?
    pub fn wants_compaction(&self, history: &[ChatMessage]) -> bool {
        self.config.auto_compact
            && history.len() > self.config.history_messages * 3
            && history.len() > 6
    }

    /// Ask the model to summarize `history` and fold it into the rolling summary.
    /// Returns the number of messages absorbed into the summary.
    pub fn compact(
        &mut self,
        provider: &mut dyn LlmProvider,
        model: &str,
        history: &[ChatMessage],
    ) -> Result<usize, crate::error::CoreError> {
        // keep the most recent window untouched
        let cut = history.len().saturating_sub(self.config.history_messages);
        if cut == 0 {
            return Ok(0);
        }
        let old: Vec<String> = history[..cut]
            .iter()
            .map(|m| format!("{}: {}", m.role.as_str(), m.content))
            .collect();
        let prompt = vec![
            ChatMessage::system(
                "You compress conversation history into a short factual summary. \
                 Keep file names, decisions, code state and open problems. \
                 Reply with the summary only.",
            ),
            ChatMessage::user(format!(
                "Current summary (if any):\n{}\n\nConversation to absorb:\n{}\n\nNew summary:",
                if self.summary.is_empty() {
                    "(none)"
                } else {
                    &self.summary
                },
                old.join("\n\n")
            )),
        ];
        let req = ChatRequest {
            model,
            messages: &prompt,
            options: &ChatOptions {
                num_ctx: self.config.ctx_tokens as u32,
                temperature: 0.0,
            },
        };
        let summary = provider.chat_stream(&req, &mut |_s| {})?;
        self.summary = summary.trim().to_string();
        Ok(cut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn builds_within_budget_and_keeps_tail() {
        let cfg = ContextConfig {
            ctx_tokens: 600,
            history_messages: 2,
            max_open_files: 2,
            auto_compact: false,
        };
        let cm = ContextManager::new(cfg);
        let history: Vec<ChatMessage> = (0..10)
            .map(|i| ChatMessage::user(format!("message number {i} with some padding words")))
            .collect();
        let built = cm.build("SYS", "TOOLS", Some("do the thing"), &history);
        assert!(built[0].content.contains("SYS"));
        assert!(built[0].content.contains("do the thing"));
        let msgs: Vec<&ChatMessage> = built
            .iter()
            .filter(|m| m.content.starts_with("message number"))
            .collect();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("number 8"));
        assert!(msgs[1].content.contains("number 9"));
    }

    #[test]
    fn open_files_are_included_and_dropped_first() {
        let cfg = ContextConfig {
            ctx_tokens: 2000,
            history_messages: 2,
            max_open_files: 3,
            auto_compact: false,
        };
        let mut cm = ContextManager::new(cfg);
        cm.note_file("a.rs", "fn a() {}\n");
        cm.note_file("b.rs", "fn b() {}\n");
        let history = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
        let built = cm.build("S", "T", None, &history);
        let joined: String = built.iter().map(|m| m.content.as_str()).collect();
        assert!(joined.contains("a.rs"));
        assert!(joined.contains("b.rs"));
        // newest first order in the working set block
        let ia = joined.find("a.rs").unwrap();
        let ib = joined.find("b.rs").unwrap();
        assert!(ib < ia);
    }

    #[test]
    fn file_replacement_updates_cache() {
        let cfg = ContextConfig::default();
        let mut cm = ContextManager::new(cfg);
        cm.note_file("a.rs", "old");
        cm.note_file("a.rs", "new");
        assert_eq!(cm.open_files().len(), 1);
        assert_eq!(cm.open_files()[0].content, "new");
    }

    #[test]
    fn estimate_usage_counts_files_and_history() {
        let cm = ContextManager::new(ContextConfig {
            ctx_tokens: 4096,
            history_messages: 4,
            max_open_files: 4,
            auto_compact: true,
        });
        let empty = cm.estimate_usage(&[]);
        assert_eq!(empty, 0);

        let mut cm = cm;
        cm.note_file("a.rs", "12345678"); // 8 chars -> 2 tokens
        let history = vec![
            ChatMessage::user("abcdefgh"),              // 2 tokens
            ChatMessage::assistant("abcdefghijklmnop"), // 4 tokens
        ];
        assert_eq!(cm.estimate_usage(&history), 2 + 2 + 4);
    }
}
