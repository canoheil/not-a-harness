//! Reasoning ("thinking") block handling.
//!
//! Reasoning models wrap their scratchpad in `think` tags (a `think`
//! open/close pair, sometimes spelled `thinking`). Those blocks are
//! never part of the answer: they must not reach the user's screen as
//! prose, must not be stored in history, and tool calls *inside* them
//! must never execute. Everything here works on complete pairs - an
//! unclosed open tag runs to the end of the text.
//!
//! Angle brackets in tag literals are written as `\x3c` / `\x3e` hex
//! escapes so the literals survive text tooling untouched.

/// Open/close markers used to wrap provider "reasoning" fields so the
/// rest of the pipeline only ever sees tagged text.
pub const THINK_OPEN: &str = "\x3cthink\x3e";
pub const THINK_CLOSE: &str = "\x3c/think\x3e";

const OPENS: [&str; 2] = ["\x3cthink\x3e", "\x3cthinking\x3e"];
const CLOSES: [&str; 2] = ["\x3c/think\x3e", "\x3c/thinking\x3e"];
fn ascii_lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Earliest complete tag in `s`: (byte index, byte length). Only tags
/// valid for the current state are matched (opens while outside a
/// block, closes while inside one), so a stray close tag in ordinary
/// prose is just text.
fn find_tag(s: &str, closing: bool) -> Option<(usize, usize)> {
    let lower = ascii_lower(s);
    let mut best: Option<(usize, usize)> = None;
    for tag in if closing { CLOSES } else { OPENS } {
        if let Some(i) = lower.find(tag) {
            if best.is_none_or(|(b, _)| i < b) {
                best = Some((i, tag.len()));
            }
        }
    }
    best
}

/// Remove all reasoning blocks (tags included) from `text`.
///
/// * complete open..close pairs are removed entirely;
/// * an unclosed open tag removes everything from the tag to the end;
/// * text outside blocks is kept verbatim.
pub fn strip_thinking(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some((i, len)) = find_tag(rest, false) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        rest = &rest[i + len..];
        // inside a block: skip to the close tag (or to the end)
        match find_tag(rest, true) {
            Some((j, clen)) => rest = &rest[j + clen..],
            None => break,
        }
    }
    out
}

/// One visible run of streamed text: either ordinary prose or reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThinkChunk {
    pub thinking: bool,
    pub text: String,
}

/// Streaming counterpart of [`strip_thinking`]: folds reasoning blocks
/// out of a delta stream while it arrives. Tags may be split across
/// deltas, so a short tail is buffered until it can be matched.
///
/// With `show` enabled the reasoning text is returned as `thinking`
/// chunks (the UI dims it); otherwise it is dropped. State resets per
/// reply - one filter per streamed answer.
pub struct ThinkFilter {
    show: bool,
    in_think: bool,
    tail: String,
}

impl ThinkFilter {
    pub fn new(show: bool) -> ThinkFilter {
        ThinkFilter {
            show,
            in_think: false,
            tail: String::new(),
        }
    }

    pub fn set_show(&mut self, show: bool) {
        self.show = show;
    }

    /// Feed one streamed delta; returns the chunks to display.
    pub fn push(&mut self, delta: &str) -> Vec<ThinkChunk> {
        self.tail.push_str(delta);
        let mut out: Vec<ThinkChunk> = Vec::new();
        loop {
            let Some((i, len)) = find_tag(&self.tail, self.in_think) else {
                break;
            };
            let tagged: String = self.tail.drain(..i + len).collect();
            self.emit(&mut out, &tagged[..i]);
            self.in_think = !self.in_think;
        }
        // hold back a tail that could still complete into a tag
        let tags = if self.in_think { CLOSES } else { OPENS };
        let lower = ascii_lower(&self.tail);
        let mut cut = self.tail.len();
        for tag in tags {
            for k in 1..tag.len().min(self.tail.len() + 1) {
                if lower.ends_with(&tag[..k]) {
                    cut = cut.min(self.tail.len() - k);
                }
            }
        }
        if cut > 0 {
            let text: String = self.tail.drain(..cut).collect();
            self.emit(&mut out, &text);
        }
        out
    }

    /// End of the stream: an unclosed reasoning block is dropped when
    /// hidden, shown as thinking when `show` is on. Either way it never
    /// becomes prose - matching [`strip_thinking`] for storage.
    pub fn finish(&mut self) -> Vec<ThinkChunk> {
        let mut out: Vec<ThinkChunk> = Vec::new();
        let tail = std::mem::take(&mut self.tail);
        self.emit(&mut out, &tail);
        self.in_think = false;
        out
    }

    fn emit(&self, out: &mut Vec<ThinkChunk>, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.in_think && !self.show {
            return;
        }
        if let Some(last) = out.last_mut() {
            if last.thinking == self.in_think {
                last.text.push_str(text);
                return;
            }
        }
        out.push(ThinkChunk {
            thinking: self.in_think,
            text: text.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const O: &str = "\x3cthink\x3e";
    const C: &str = "\x3c/think\x3e";

    #[test]
    fn strip_removes_complete_pairs() {
        let text = format!("say {O} hidden {C} ok");
        assert_eq!(strip_thinking(&text), "say  ok");
    }

    #[test]
    fn strip_is_case_insensitive_and_ignores_nested_opens() {
        let text = format!("x \x3cTHINK\x3e y {O} z \x3c/THINK\x3e w");
        assert_eq!(strip_thinking(&text), "x  w");
    }

    #[test]
    fn strip_unclosed_block_runs_to_the_end() {
        let text = format!("keep {O} gone");
        assert_eq!(strip_thinking(&text), "keep ");
        let text = format!("{O} gone");
        assert_eq!(strip_thinking(&text), "");
    }

    #[test]
    fn stray_close_tag_is_plain_text() {
        let text = format!("a {C} b");
        assert_eq!(strip_thinking(&text), format!("a {C} b"));
    }

    #[test]
    fn filter_hides_thinking_when_show_is_off() {
        let mut f = ThinkFilter::new(false);
        let text = format!("before {O} secret {C} after");
        let out = f.push(&text);
        let joined: String = out.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(joined, "before  after");
        assert!(out.iter().all(|c| !c.thinking));
    }

    #[test]
    fn filter_shows_thinking_tagged_when_show_is_on() {
        let mut f = ThinkFilter::new(true);
        let text = format!("hi {O} hmm {C} bye");
        let out = f.push(&text);
        assert_eq!(out.len(), 3);
        assert!(!out[0].thinking);
        assert!(out[1].thinking);
        assert_eq!(out[1].text, " hmm ");
        assert!(!out[2].thinking);
    }

    #[test]
    fn filter_handles_tags_split_across_deltas() {
        let mut f = ThinkFilter::new(false);
        let mut seen = String::new();
        for part in ["ab \x3cth", "ink\x3e se", "cret \x3c/th", "ink\x3e cd"] {
            for c in f.push(part) {
                seen.push_str(&c.text);
            }
        }
        for c in f.finish() {
            seen.push_str(&c.text);
        }
        assert_eq!(seen, "ab  cd");
    }

    #[test]
    fn filter_finish_drops_unclosed_thinking() {
        let mut f = ThinkFilter::new(false);
        let mut seen = String::new();
        let text = format!("prose {O} never ends");
        for c in f.push(&text) {
            seen.push_str(&c.text);
        }
        for c in f.finish() {
            seen.push_str(&c.text);
        }
        assert_eq!(seen, "prose ");
    }

    #[test]
    fn filter_keeps_partial_open_tag_that_never_completes() {
        let mut f = ThinkFilter::new(false);
        let mut seen = String::new();
        for c in f.push("value: \x3cth") {
            seen.push_str(&c.text);
        }
        for c in f.finish() {
            seen.push_str(&c.text);
        }
        assert_eq!(seen, "value: \x3cth");
    }
}
