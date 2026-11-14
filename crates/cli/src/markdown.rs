//! Streaming line-based CommonMark renderer for model output.
//!
//! The renderer is deliberately simple: one logical line in, zero or more
//! styled logical lines out. Blocks that need lookahead (tables) buffer;
//! everything else renders immediately, so a reply streams as it arrives.
//! Inline markup is only interpreted outside code spans and fences.

use crate::ui::{BLUE, BOLD, CYAN, DIM, RESET, YELLOW};

#[derive(PartialEq)]
enum State {
    Prose,
    Fence,
    Table,
}

pub struct Markdown {
    width: usize,
    color: bool,
    state: State,
    /// partial line assembled across deltas
    buf: String,
    /// held line waiting for a table separator on the next line
    table_wait: Option<String>,
    /// rows collected in Table state, header first
    rows: Vec<Vec<String>>,
    /// last emitted line was blank (collapse runs of blanks)
    last_blank: bool,
}

impl Markdown {
    pub fn new(width: usize) -> Markdown {
        Markdown {
            width: width.max(20),
            color: true,
            state: State::Prose,
            buf: String::new(),
            table_wait: None,
            rows: Vec::new(),
            last_blank: true,
        }
    }

    /// Render without ANSI codes (plain structure only).
    pub fn set_color(&mut self, color: bool) {
        self.color = color;
    }

    /// Feed streamed text; `on_line` receives each rendered logical line.
    pub fn push(&mut self, text: &str, on_line: &mut dyn FnMut(&str)) {
        self.buf.push_str(text);
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            self.handle_line(line, on_line);
        }
    }

    /// Emit the buffered partial line without closing open blocks
    /// (used at style transitions mid-stream).
    pub fn flush_partial(&mut self, on_line: &mut dyn FnMut(&str)) {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            let line = line.trim_end_matches(['\n', '\r']);
            self.handle_line(line, on_line);
        }
    }

    /// End of the message: flush everything and reset block state.
    pub fn finish(&mut self, on_line: &mut dyn FnMut(&str)) {
        self.flush_partial(on_line);
        if self.state == State::Table {
            self.emit_table(on_line);
        }
        if self.state == State::Fence {
            self.emit_border('\u{2514}', '\u{2500}', on_line);
        }
        self.state = State::Prose;
    }

    // ------------------------------------------------------------------
    // block machine
    // ------------------------------------------------------------------

    fn handle_line(&mut self, line: &str, on_line: &mut dyn FnMut(&str)) {
        let t = line.trim();

        // a table separator resolves the held candidate line as a header
        if self.state == State::Prose && self.table_wait.is_some() {
            if is_table_sep(t) {
                let head = self.table_wait.take().unwrap_or_default();
                self.rows.clear();
                self.rows.push(split_row(&head));
                self.state = State::Table;
                return;
            }
            let head = self.table_wait.take().unwrap_or_default();
            self.paragraph(&head, on_line);
            // fall through: the current line still needs handling
        }

        match self.state {
            State::Fence => {
                if t.starts_with("```") {
                    self.emit_border('\u{2514}', '\u{2500}', on_line);
                    self.state = State::Prose;
                } else {
                    self.emit_code(line, on_line);
                }
            }
            State::Table => {
                if t.is_empty() || !t.contains('|') {
                    self.emit_table(on_line);
                    self.state = State::Prose;
                    self.handle_line(line, on_line);
                } else {
                    self.rows.push(split_row(line));
                }
            }
            State::Prose => {
                if t.starts_with("```") {
                    self.emit_border('\u{250c}', '\u{2500}', on_line);
                    self.state = State::Fence;
                } else if t.is_empty() {
                    self.blank(on_line);
                } else if is_hr(t) {
                    self.hr(on_line);
                } else if let Some(rest) = t.strip_prefix('>') {
                    self.blockquote(rest.trim_start(), on_line);
                } else if let Some((bullet, rest)) = parse_list(t) {
                    self.list_item(bullet, rest, line, on_line);
                } else if let Some((level, text)) = parse_heading(t) {
                    self.heading(level, text, on_line);
                } else if t.contains('|') {
                    self.table_wait = Some(line.to_string());
                } else {
                    self.paragraph(line, on_line);
                }
            }
        }
    }

    fn blank(&mut self, on_line: &mut dyn FnMut(&str)) {
        if !self.last_blank {
            on_line("");
            self.last_blank = true;
        }
    }

    fn styled(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    fn heading(&mut self, level: usize, text: &str, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        let body = self.inline(text);
        let code = if level <= 2 { BLUE } else { CYAN };
        on_line(&self.styled(&format!("{BOLD}{code}"), &body));
    }

    fn hr(&mut self, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        let line = "\u{2500}".repeat(self.width.min(60));
        on_line(&self.styled(DIM, &line));
    }

    fn blockquote(&mut self, text: &str, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        let body = self.inline(text);
        let bar = self.styled(DIM, "\u{2502} ");
        on_line(&format!("{bar}{}", self.styled(DIM, &body)));
    }

    fn list_item(&mut self, bullet: Bullet, rest: &str, raw: &str, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        let indent = raw.len() - raw.trim_start().len();
        let indent = " ".repeat((indent / 2) * 2);
        let body = self.inline(rest);
        match bullet {
            Bullet::Dash => on_line(&format!("{indent}\u{2022} {body}")),
            Bullet::Task(done) => {
                let mark = if done { "\u{2611} " } else { "\u{2610} " };
                on_line(&format!("{indent}{mark}{body}"));
            }
            Bullet::Num(n) => {
                let num = self.styled(BOLD, &format!("{n}."));
                on_line(&format!("{indent}{num} {body}"));
            }
        }
    }

    fn paragraph(&mut self, line: &str, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        on_line(&self.inline(line.trim()));
    }

    // ------------------------------------------------------------------
    // code panels and tables
    // ------------------------------------------------------------------

    fn emit_border(&mut self, left: char, fill: char, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        let w = self.width.min(60).saturating_sub(1);
        on_line(&self.styled(DIM, &format!("{left}{}", fill.to_string().repeat(w))));
    }

    fn emit_code(&mut self, line: &str, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        let bar = self.styled(DIM, "\u{2502} ");
        for l in wrap_plain(line, self.width.saturating_sub(2).max(8)) {
            on_line(&format!("{bar}{l}"));
        }
    }

    fn emit_table(&mut self, on_line: &mut dyn FnMut(&str)) {
        self.last_blank = false;
        if self.rows.is_empty() {
            return;
        }
        let cols = self.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 0 {
            self.rows.clear();
            return;
        }
        // column width = widest cell, shrunk when the table would overflow
        let mut widths = vec![0usize; cols];
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(crate::wrap::display_width(cell));
            }
        }
        let avail = self.width.saturating_sub(3 * cols + 1).max(cols * 3);
        let total: usize = widths.iter().sum();
        if total > avail {
            let each = avail / cols;
            for w in widths.iter_mut() {
                *w = (*w).min(each.max(3));
            }
        }
        let bar = self.styled(DIM, "\u{2502}");
        let join = self.styled(DIM, "\u{2502}");
        for (ri, row) in self.rows.iter().enumerate() {
            let mut cells = Vec::new();
            for (i, w) in widths.iter().enumerate() {
                let cell = row.get(i).map(String::as_str).unwrap_or("");
                let c = if crate::wrap::display_width(cell) > *w {
                    truncate_to(cell, *w)
                } else {
                    pad_to(cell, *w)
                };
                cells.push(if ri == 0 { self.styled(BOLD, &c) } else { c });
            }
            on_line(&format!("{bar} {} {bar}", cells.join(&format!(" {join} "))));
            if ri == 0 {
                let sep: Vec<String> = widths.iter().map(|w| "\u{2500}".repeat(*w + 2)).collect();
                on_line(&self.styled(DIM, &format!("\u{251c}{}\u{2524}", sep.join("\u{253c}"))));
            }
        }
        self.rows.clear();
    }

    // ------------------------------------------------------------------
    // inline markup
    // ------------------------------------------------------------------

    /// Render inline markdown: `code`, **bold**, *italic*, ~~strike~~,
    /// [text](url). Underscore emphasis is deliberately ignored (it
    /// appears inside identifiers far more often than as markup).
    fn inline(&self, s: &str) -> String {
        let mut out = String::new();
        let b = s.as_bytes();
        let mut i = 0usize;
        let mut bold = false;
        let mut italic = false;
        while i < b.len() {
            // code span: atomic, nothing inside is interpreted
            if b[i] == b'`' {
                let end = s[i + 1..].find('`').map(|p| i + 1 + p);
                if let Some(end) = end {
                    let inner = &s[i + 1..end];
                    out.push_str(&self.styled(YELLOW, inner));
                    i = end + 1;
                    continue;
                }
            }
            // [text](url) -> text (url) with a dim url
            if b[i] == b'[' {
                if let Some((text, url, len)) = parse_link(&s[i..]) {
                    out.push_str(&self.inline(text));
                    out.push(' ');
                    out.push_str(&self.styled(DIM, &format!("({url})")));
                    i += len;
                    continue;
                }
            }
            // ~~strikethrough~~
            if s[i..].starts_with("~~") {
                if let Some(end) = s[i + 2..].find("~~") {
                    out.push_str(&self.styled(DIM, &s[i + 2..i + 2 + end]));
                    i += 2 + end + 2;
                    continue;
                }
            }
            // **bold** (word-boundary rules)
            if s[i..].starts_with("**") {
                let next = s[i + 2..].chars().next();
                if !bold && next.is_some_and(|c| !c.is_whitespace()) {
                    bold = true;
                    out.push_str(if self.color { BOLD } else { "" });
                    i += 2;
                    continue;
                }
                if bold && i > 0 && !s[..i].ends_with(char::is_whitespace) {
                    bold = false;
                    out.push_str(if self.color { RESET } else { "" });
                    i += 2;
                    continue;
                }
            }
            // *italic* (word-boundary rules, never mid-word)
            if b[i] == b'*' && !s[i..].starts_with("**") {
                let prev_ws = i == 0 || s[..i].ends_with(char::is_whitespace);
                let next = s[i + 1..].chars().next();
                if !italic && prev_ws && next.is_some_and(|c| !c.is_whitespace()) {
                    italic = true;
                    out.push_str(if self.color { "\u{1b}[3m" } else { "" });
                    i += 1;
                    continue;
                }
                if italic && !s[..i].ends_with(char::is_whitespace) {
                    italic = false;
                    out.push_str(if self.color { RESET } else { "" });
                    i += 1;
                    continue;
                }
            }
            let c = s[i..].chars().next().unwrap_or(' ');
            out.push(c);
            i += c.len_utf8();
        }
        // close any unterminated span so color never leaks past the line
        if self.color && (bold || italic) {
            out.push_str(RESET);
        }
        out
    }
}

// ----------------------------------------------------------------------
// helpers
// ----------------------------------------------------------------------

enum Bullet {
    Dash,
    Num(usize),
    Task(bool),
}

fn parse_heading(t: &str) -> Option<(usize, &str)> {
    let hashes = t.len() - t.trim_start_matches('#').len();
    if hashes == 0 || hashes > 6 || !t[hashes..].starts_with(' ') {
        return None;
    }
    Some((hashes, t[hashes..].trim()))
}

fn parse_list(t: &str) -> Option<(Bullet, &str)> {
    if let Some(rest) = t.strip_prefix("- [ ] ") {
        return Some((Bullet::Task(false), rest));
    }
    if let Some(rest) = t.strip_prefix("- [x] ") {
        return Some((Bullet::Task(true), rest));
    }
    if let Some(rest) = t.strip_prefix("- [X] ") {
        return Some((Bullet::Task(true), rest));
    }
    if let Some(rest) = t.strip_prefix("- ") {
        return Some((Bullet::Dash, rest));
    }
    if let Some(rest) = t.strip_prefix("* ") {
        return Some((Bullet::Dash, rest));
    }
    if let Some(rest) = t.strip_prefix("+ ") {
        return Some((Bullet::Dash, rest));
    }
    // ordered: digits then '.' or ')'
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = &t[digits.len()..];
        if (rest.starts_with(". ") || rest.starts_with(") ")) && digits.len() <= 9 {
            if let Ok(n) = digits.parse::<usize>() {
                return Some((Bullet::Num(n), &rest[2..]));
            }
        }
    }
    None
}

fn is_hr(t: &str) -> bool {
    (t.chars().all(|c| c == '-') && t.len() >= 3)
        || (t.chars().all(|c| c == '*') && t.len() >= 3)
        || (t.chars().all(|c| c == '_') && t.len() >= 3)
}

fn is_table_sep(t: &str) -> bool {
    let t = t.trim_matches('|');
    !t.is_empty()
        && t.split('|').all(|c| {
            let c = c.trim();
            !c.is_empty() && c.chars().all(|c| c == '-' || c == ':' || c == ' ') && c.contains('-')
        })
}

fn split_row(line: &str) -> Vec<String> {
    let t = line.trim().trim_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn parse_link(s: &str) -> Option<(&str, &str, usize)> {
    let close = s.find(']')?;
    let text = &s[1..close];
    let rest = &s[close + 1..];
    if !rest.starts_with('(') {
        return None;
    }
    let end = rest.find(')')?;
    let url = &rest[1..end];
    Some((text, url, close + 1 + end + 1))
}

fn pad_to(cell: &str, w: usize) -> String {
    let mut s = cell.to_string();
    while crate::wrap::display_width(&s) < w {
        s.push(' ');
    }
    s
}

fn truncate_to(cell: &str, w: usize) -> String {
    let mut s = String::new();
    let mut width = 0;
    for c in cell.chars() {
        let cw = crate::wrap::char_width(c);
        if width + cw > w.saturating_sub(1) {
            break;
        }
        s.push(c);
        width += cw;
    }
    s.push('\u{2026}');
    s
}

/// Plain wrap for code lines: no ANSI in code, so a simple width split
/// with a hanging indent is enough.
fn wrap_plain(line: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    if crate::wrap::display_width(line) <= width {
        return vec![line.to_string()];
    }
    crate::wrap::wrap_line(line, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(text: &str) -> Vec<String> {
        let mut md = Markdown::new(40);
        let mut out = Vec::new();
        {
            let mut sink = |l: &str| out.push(l.to_string());
            md.push(text, &mut sink);
            md.finish(&mut sink);
        }
        out
    }

    #[test]
    fn headings_are_styled_and_hash_free() {
        let out = render("# Title\n\n### Sub\n");
        assert_eq!(out[0], format!("{BOLD}{BLUE}Title{RESET}"));
        assert_eq!(out[2], format!("{BOLD}{CYAN}Sub{RESET}"));
    }

    #[test]
    fn fenced_code_is_panelled_and_raw() {
        let out = render("```rust\n**x**\n```\n");
        assert!(out[0].contains('\u{250c}'), "{:?}", out[0]);
        assert!(out[1].contains('\u{2502}'), "{:?}", out[1]);
        assert!(out[1].contains("**x**"), "{:?}", out[1]);
        assert!(out[2].contains('\u{2514}'), "{:?}", out[2]);
    }

    #[test]
    fn fence_left_open_closes_at_finish() {
        let out = render("```\ncode");
        assert!(out.last().unwrap().contains('\u{2514}'), "{out:?}");
    }

    #[test]
    fn lists_bullets_numbers_and_tasks() {
        let out = render("- one\n2. two\n- [ ] todo\n- [x] done\n");
        assert_eq!(out[0], "\u{2022} one");
        assert_eq!(out[1], format!("{} two", styled_bold_num("2")));
        assert_eq!(out[2], "\u{2610} todo");
        assert_eq!(out[3], "\u{2611} done");
    }

    fn styled_bold_num(n: &str) -> String {
        format!("{BOLD}{n}.{RESET}")
    }

    #[test]
    fn blockquote_is_barred_and_dim() {
        let out = render("> quoted\n");
        assert_eq!(out[0], format!("{DIM}\u{2502} {RESET}{DIM}quoted{RESET}"));
    }

    #[test]
    fn hr_fills_the_width() {
        let out = render("---\n");
        assert_eq!(out[0], format!("{DIM}{}\u{1b}[0m", "\u{2500}".repeat(40)));
    }

    #[test]
    fn table_aligns_columns() {
        let out = render("| Name | Qty |\n| --- | ---: |\n| a | 1 |\n| bbbb | 22 |\ndone\n");
        // header, separator, two rows, then the trailing paragraph
        assert_eq!(out.len(), 5, "{out:?}");
        assert!(out[0].contains('\u{2502}'), "{:?}", out[0]);
        assert!(out[1].contains('\u{253c}'), "{:?}", out[1]);
        assert!(out[2].contains('\u{2502}'), "{:?}", out[2]);
        assert_eq!(out[4], "done");
    }

    #[test]
    fn inline_markup_renders() {
        let out = render("`code` and **bold** and *it* and ~~gone~~ and [x](http://y)\n");
        let l = &out[0];
        assert!(l.contains(&format!("{YELLOW}code{RESET}")), "{l}");
        assert!(l.contains(&format!("{BOLD}bold{RESET}")), "{l}");
        assert!(l.contains(&format!("\u{1b}[3mit{RESET}")), "{l}");
        assert!(l.contains(&format!("{DIM}gone{RESET}")), "{l}");
        assert!(l.contains(&format!("x {DIM}(http://y){RESET}")), "{l}");
    }

    #[test]
    fn underscores_are_never_emphasis() {
        let out = render("snake_case_name stays\n");
        assert!(!out[0].contains('\u{1b}'), "{:?}", out[0]);
    }

    #[test]
    fn blank_lines_collapse() {
        let out = render("a\n\n\n\nb\n");
        assert_eq!(out, vec!["a", "", "b"]);
    }

    #[test]
    fn no_color_mode_has_no_escapes() {
        let mut md = Markdown::new(40);
        md.set_color(false);
        let mut out = Vec::new();
        {
            let mut sink = |l: &str| out.push(l.to_string());
            md.push("# T\n**b** `c` [x](u)\n", &mut sink);
            md.finish(&mut sink);
        }
        for l in &out {
            assert!(!l.contains('\u{1b}'), "{l:?}");
        }
        assert_eq!(out[0], "T");
    }

    #[test]
    fn streamed_slices_match_single_push() {
        let text = "# H\n\nsome **bold** words that go on\n\n- a\n- b\n";
        let one = render(text);
        let mut md = Markdown::new(40);
        let mut out = Vec::new();
        {
            let mut sink = |l: &str| out.push(l.to_string());
            for ch in [
                "#",
                " H\n\nsom",
                "e **bo",
                "ld** wor",
                "ds that go on\n\n- a\n- b\n",
            ] {
                md.push(ch, &mut sink);
            }
            md.finish(&mut sink);
        }
        assert_eq!(one, out);
    }

    #[test]
    fn flush_partial_emits_line_without_closing_fence() {
        let mut md = Markdown::new(40);
        let mut out: Vec<String> = Vec::new();
        md.push("```\npar", &mut |l: &str| out.push(l.to_string()));
        md.flush_partial(&mut |l: &str| out.push(l.to_string()));
        let last = out.last().cloned().unwrap_or_default();
        assert!(last.contains("par"), "{last:?}");
        assert!(!last.contains('\u{2514}'), "fence closed early");
        md.push("tial\ntail\n```\n", &mut |l: &str| out.push(l.to_string()));
        md.finish(&mut |l: &str| out.push(l.to_string()));
        assert!(out.last().unwrap().contains('\u{2514}'), "{out:?}");
    }
}
