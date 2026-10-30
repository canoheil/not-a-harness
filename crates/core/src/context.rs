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

/// Fast chars/4 heuristic used ONLY by the tok/s speed line
/// (locked product decision: estimate with `~`, never exact counts).
pub fn speed_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Static token estimate: ASCII text scores about 4 characters per
/// token; non-ASCII scripts (Persian, CJK, ...) score heavier because
/// most tokenizers spend a token per character or two on them. Being
/// slightly conservative here is safe - calibration then pulls the
/// number towards the model's real tokenizer.
pub fn estimate_tokens(text: &str) -> usize {
    let mut milli = 0usize;
    for c in text.chars() {
        milli += if c.is_ascii() { 250 } else { 700 };
    }
    milli.div_ceil(1000)
}

/// Calibration multiplier learned from the server's real prompt counts.
#[derive(Debug, Clone, Copy)]
pub struct TokenScale {
    factor: f64,
}

impl Default for TokenScale {
    fn default() -> Self {
        TokenScale { factor: 1.0 }
    }
}

impl TokenScale {
    /// Multiplier applied to static estimates.
    pub fn factor(&self) -> f64 {
        self.factor
    }

    /// Fold one real observation into the scale. `estimated` must be the
    /// unscaled estimate of exactly the text the server counted.
    pub fn observe(&mut self, actual: usize, estimated: usize) {
        if actual == 0 || estimated == 0 {
            return;
        }
        let ratio = (actual as f64 / estimated as f64).clamp(0.25, 4.0);
        self.factor = (self.factor * 0.5 + ratio * 0.5).clamp(0.25, 4.0);
    }

    pub fn reset(&mut self) {
        self.factor = 1.0;
    }
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
    scale: TokenScale,
}

impl ContextManager {
    pub fn new(config: ContextConfig) -> Self {
        ContextManager {
            config,
            open_files: Vec::new(),
            summary: String::new(),
            scale: TokenScale::default(),
        }
    }

    /// Scaled token estimate used for every budget decision and for the
    /// context-usage display.
    fn est(&self, text: &str) -> usize {
        ((estimate_tokens(text) as f64) * self.scale.factor()).round() as usize
    }

    /// Feed the server's real prompt-token count back so future
    /// estimates track the model's actual tokenizer. `sent` is exactly
    /// the message list that was transmitted.
    pub fn observe_usage(&mut self, prompt_tokens: usize, sent: &[ChatMessage]) {
        let base: usize = sent.iter().map(|m| estimate_tokens(&m.content)).sum();
        self.scale.observe(prompt_tokens, base);
    }

    /// Current calibration factor (1.0 = no observations yet).
    pub fn token_scale_factor(&self) -> f64 {
        self.scale.factor()
    }

    /// Forget calibration (e.g. the model - and its tokenizer - changed).
    pub fn reset_token_scale(&mut self) {
        self.scale.reset();
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

    /// Token usage of what a request would carry: summary, working set
    /// and history. Calibrated against the server's real counts; feeds
    /// the UI's context-usage line.
    pub fn estimate_usage(&self, history: &[ChatMessage]) -> usize {
        let mut total = self.est(&self.summary);
        for f in &self.open_files {
            total += self.est(&f.content);
        }
        for m in history {
            total += self.est(&m.content);
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
        used += self.est(&system);

        // 2) rolling summary (if any)
        if !self.summary.is_empty() {
            let block = format!("\n\n[SUMMARY OF EARLIER CONVERSATION]\n{}\n", self.summary);
            used += self.est(&block);
            system.push_str(&block);
        }

        // 3) pinned task — the original request never leaves the context
        if let Some(task) = pinned_task {
            let block = format!("\n\n[ORIGINAL USER TASK]\n{task}\n");
            used += self.est(&block);
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
            let cost = self.est(&block);
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
            let cost = self.est(&m.content);
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
                 Keep file names, decisions, code state and open problems - with \
                 paths exactly as used (they are relative to the project root). \
                 Record only actions that tool results actually confirmed; never \
                 record claimed-but-unverified actions as done. \
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
        if let Some(u) = provider.take_usage() {
            self.observe_usage(u.prompt_tokens, &prompt);
        }
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
    /// Minimal provider that reports real token counts once.
    struct Counting {
        usage: Option<crate::provider::TokenUsage>,
    }

    impl LlmProvider for Counting {
        fn list_models(
            &mut self,
        ) -> Result<Vec<crate::provider::ModelInfo>, crate::error::CoreError> {
            Ok(vec![])
        }

        fn chat_stream(
            &mut self,
            _req: &ChatRequest<'_>,
            _on_delta: &mut dyn FnMut(&str),
        ) -> Result<String, crate::error::CoreError> {
            Ok("summary text".to_string())
        }

        fn take_usage(&mut self) -> Option<crate::provider::TokenUsage> {
            self.usage.take()
        }
    }

    #[test]
    fn estimator_weights_non_ascii_scripts() {
        // pure ASCII keeps the classic chars/4 behaviour
        assert_eq!(estimate_tokens("abcdefghijklmnop"), 4);
        assert_eq!(estimate_tokens(""), 0);
        // Persian text must cost more than chars/4 - most tokenizers
        // spend far more than a quarter token per character on it
        let persian = "سلام دنیا، این یک متن فارسی است";
        assert!(
            estimate_tokens(persian) > persian.chars().count() / 4,
            "{} vs {}",
            estimate_tokens(persian),
            persian.chars().count() / 4
        );
        // and the speed line keeps its simple chars/4 estimate (locked)
        assert_eq!(speed_tokens("abcdefghijklmnop"), 4);
    }

    #[test]
    fn token_scale_calibrates_clamps_and_resets() {
        let mut scale = TokenScale::default();
        assert_eq!(scale.factor(), 1.0);
        // the server says the real prompt is twice our estimate
        scale.observe(200, 100);
        assert!((scale.factor() - 1.5).abs() < 1e-9, "{}", scale.factor());
        // next observation pulls it back down (EMA)
        scale.observe(100, 200);
        assert!((scale.factor() - 1.0).abs() < 1e-9, "{}", scale.factor());
        // wild ratios are clamped
        for _ in 0..10 {
            scale.observe(10_000, 1);
        }
        assert!(scale.factor() <= 4.0, "{}", scale.factor());
        // zero counts never divide by zero
        scale.observe(0, 0);
        scale.reset();
        assert_eq!(scale.factor(), 1.0);
    }

    #[test]
    fn calibration_scales_budget_and_usage() {
        let mut ctx = ContextManager::new(ContextConfig::default());
        let history = vec![crate::provider::ChatMessage::user("x".repeat(400))];
        let before = ctx.estimate_usage(&history);
        // the model's tokenizer really counts 3x our static estimate
        for _ in 0..5 {
            ctx.observe_usage(300, &history);
        }
        let after = ctx.estimate_usage(&history);
        assert!(
            after > before * 2,
            "before={before} after={after} factor={}",
            ctx.token_scale_factor()
        );
        // switching models forgets the calibration
        ctx.reset_token_scale();
        assert_eq!(ctx.estimate_usage(&history), before);
    }

    #[test]
    fn compact_learns_the_real_tokenizer() {
        let mut ctx = ContextManager::new(ContextConfig::default());
        let history: Vec<crate::provider::ChatMessage> = (0..12)
            .map(|i| {
                crate::provider::ChatMessage::user(format!("message number {i} with some text"))
            })
            .collect();
        let mut provider = Counting {
            usage: Some(crate::provider::TokenUsage {
                prompt_tokens: 5_000,
                completion_tokens: 10,
            }),
        };
        let cut = ctx.compact(&mut provider, "m", &history).unwrap();
        assert!(cut > 0);
        // the compact call fed the real prompt count into the scale
        assert!(
            ctx.token_scale_factor() > 1.0,
            "{}",
            ctx.token_scale_factor()
        );
    }
}
