//! Word-boundary wrapping and display-width measurement.
//!
//! Why wrap ourselves? Terminals wrap long logical lines at arbitrary
//! points, which scrambles RTL paragraph flow (each half gets BiDi-reordered
//! independently and can even be displayed bottom-up). Emitting complete,
//! word-boundary lines keeps every line a clean BiDi paragraph segment -
//! the same technique TUIs like Claude Code use.

/// Resolve the terminal width: explicit override > NAH_WIDTH > COLUMNS >
/// platform query > 80.
pub fn terminal_width(override_w: Option<usize>) -> usize {
    if let Some(w) = override_w {
        return w.clamp(20, 500);
    }
    if let Some(w) = std::env::var("NAH_WIDTH").ok().and_then(|v| v.parse().ok()) {
        return w;
    }
    if let Some(w) = std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok()) {
        return w;
    }
    #[cfg(windows)]
    {
        if let Some(w) = windows_console_width() {
            return w;
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(w) = stty_width() {
            return w;
        }
    }
    80
}

#[cfg(windows)]
fn windows_console_width() -> Option<usize> {
    #[repr(C)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    struct ScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        max_window_size: Coord,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: i32) -> *mut core::ffi::c_void;
        fn GetConsoleScreenBufferInfo(
            h: *mut core::ffi::c_void,
            info: *mut ScreenBufferInfo,
        ) -> i32;
    }
    unsafe {
        let handle = GetStdHandle(-11);
        if handle.is_null() {
            return None;
        }
        let mut info: ScreenBufferInfo = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
            return None;
        }
        let width = info.window.right - info.window.left + 1;
        if width > 0 {
            Some(width as usize)
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
fn stty_width() -> Option<usize> {
    // `stty size` works when stdin is a tty; failures are fine.
    let out = std::process::Command::new("stty")
        .arg("size")
        .stdin(std::process::Stdio::inherit())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let cols = text.split_whitespace().nth(1)?;
    cols.parse().ok()
}

/// Display width of one character in terminal cells (approximation).
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    match c {
        // zero-width: ZWSP, ZWNJ, ZWJ, LRM/RLM, bidi overrides, word
        // joiner, variation selectors, combining marks
        '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{0300}'..='\u{036F}' => 0,
        _ => {
            if is_wide(u) {
                2
            } else {
                1
            }
        }
    }
}

fn is_wide(u: u32) -> bool {
    // coarse but practical: CJK + emoji blocks occupy two cells
    matches!(u,
        0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF
    )
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Wrap one logical line into lines of at most `width` display columns.
/// Breaks at spaces when possible, hard-splits overlong words.
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if display_width(line) <= width {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    // split keeping empty segments so multiple spaces survive-ish
    for (i, word) in line.split(' ').enumerate() {
        let sep = if i == 0 { 0 } else { 1 };
        let w = display_width(word);
        if current_w + sep + w > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            current_w = 0;
        }
        if w > width {
            // hard-split a very long word on char boundaries
            for c in word.chars() {
                let cw = char_width(c);
                if current_w + cw > width && !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                current.push(c);
                current_w += cw;
            }
        } else {
            if current_w > 0 {
                current.push(' ');
                current_w += 1;
            }
            current.push_str(word);
            current_w += w;
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Streaming wrapper: feed fragments as they arrive, take complete lines.
pub struct WordWrapper {
    pending: String,
    width: usize,
}

impl WordWrapper {
    pub fn new(width: usize) -> WordWrapper {
        WordWrapper {
            pending: String::new(),
            width: width.max(8),
        }
    }

    /// Push streamed text; `on_line` receives each completed display line.
    pub fn push(&mut self, text: &str, on_line: &mut dyn FnMut(&str)) {
        self.pending.push_str(text);
        while let Some(pos) = self.pending.find('\n') {
            let logical: String = self.pending.drain(..=pos).collect();
            let logical = logical.trim_end_matches('\n');
            for l in wrap_line(logical, self.width) {
                on_line(&l);
            }
        }
        // complete any fully-occupied logical line at word boundaries
        if display_width(&self.pending) > self.width {
            if let Some(cut) = self.pending.rfind(' ') {
                if cut > 0 {
                    let logical: String = self.pending.drain(..=cut).collect();
                    for l in wrap_line(logical.trim_end(), self.width) {
                        on_line(&l);
                    }
                }
            } else if !self.pending.is_empty() {
                // no space at all: the line is one huge word - emit complete chunks
                while display_width(&self.pending) > self.width {
                    let mut take = String::new();
                    let mut w = 0;
                    let mut rest = String::new();
                    let mut overflow = false;
                    for c in self.pending.chars() {
                        let cw = char_width(c);
                        if !overflow && w + cw <= self.width {
                            take.push(c);
                            w += cw;
                        } else {
                            overflow = true;
                            rest.push(c);
                        }
                    }
                    self.pending = rest;
                    on_line(&take);
                }
            }
        }
    }

    /// Emit whatever is left (end of the reply).
    pub fn flush(&mut self, on_line: &mut dyn FnMut(&str)) {
        if !self.pending.is_empty() {
            for l in wrap_line(&self.pending, self.width) {
                on_line(&l);
            }
            self.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_simple_words() {
        let lines = wrap_line("aaa bbb ccc ddd", 7);
        assert!(lines.len() >= 2);
        for l in &lines {
            assert!(display_width(l) <= 7, "{l}");
        }
        assert_eq!(lines.join(" ").split(' ').count(), 4);
    }

    #[test]
    fn wrap_persian_keeps_zwnj_words() {
        // words joined with ZWNJ must not be split at the ZWNJ
        let text = "سلام دنیا این یک متن فارسی است";
        let lines = wrap_line(text, 10);
        let rebuilt: Vec<&str> = lines.iter().map(|l| l.as_str()).collect();
        let joined = rebuilt.join(" ");
        assert_eq!(joined.split(' ').count(), text.split(' ').count());
        for l in &lines {
            assert!(display_width(l) <= 10, "{l} exceeds");
        }
    }

    #[test]
    fn zwnj_has_zero_width() {
        assert_eq!(char_width('\u{200c}'), 0);
        assert_eq!(display_width("می\u{200c}خواید"), 7);
    }

    #[test]
    fn emoji_width_two() {
        assert_eq!(char_width('😊'), 2);
    }

    #[test]
    fn hard_split_long_word() {
        let lines = wrap_line("abcdefghij", 4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrapper_streams_in_fragments() {
        let mut w = WordWrapper::new(10);
        let mut got = Vec::new();
        {
            let mut out = |s: &str| got.push(s.to_string());
            w.push("hello ", &mut out);
            w.push("beautiful ", &mut out); // completes "hello"
            w.push("world", &mut out); // completes "beautiful"
            w.flush(&mut out);
        }
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0], "hello");
        assert_eq!(got[1], "beautiful");
        assert_eq!(got[2], "world");
    }

    #[test]
    fn wrapper_handles_explicit_newlines() {
        let mut w = WordWrapper::new(40);
        let mut got = Vec::new();
        {
            let mut out = |s: &str| got.push(s.to_string());
            w.push("line one\nline two", &mut out);
            w.flush(&mut out);
        }
        assert_eq!(got, vec!["line one", "line two"]);
    }
}
