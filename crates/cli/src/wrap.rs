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

/// Byte length of the ANSI escape sequence at the start of `s` (0 when
/// there is none). Covers CSI (`ESC [ ... final`) and OSC (`ESC ] ...
/// BEL` or `ESC \`) so styled text measures at its visible width.
pub fn ansi_len(s: &str) -> usize {
    let b = s.as_bytes();
    if b.first() != Some(&0x1b) {
        return 0;
    }
    match b.get(1) {
        Some(&b'[') => {
            let mut i = 2;
            while i < b.len() && !(0x40..=0x7E).contains(&b[i]) {
                i += 1;
            }
            (i + 1).min(b.len())
        }
        Some(&b']') => {
            let mut i = 2;
            while i < b.len() {
                if b[i] == 0x07 {
                    return i + 1;
                }
                if b[i] == 0x1b && b.get(i + 1) == Some(&0x5c) {
                    return i + 2;
                }
                i += 1;
            }
            b.len()
        }
        Some(_) => 2,
        None => 1,
    }
}

/// Visible width: escape sequences count zero.
pub fn display_width(s: &str) -> usize {
    let mut w = 0usize;
    let mut rest = s;
    while !rest.is_empty() {
        let esc = ansi_len(rest);
        if esc > 0 {
            rest = &rest[esc..];
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            w += char_width(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    w
}

/// The SGR style still active at the end of `s`: every escape since the
/// last reset. Used to carry styles across wrapped lines.
fn open_style(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while !rest.is_empty() {
        let esc = ansi_len(rest);
        if esc > 0 {
            let seq = &rest[..esc];
            rest = &rest[esc..];
            if seq == "\x1b[0m" || seq == "\x1b[m" {
                out.clear();
            } else {
                out.push_str(seq);
            }
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Fold the escapes of `word` into the running style state.
fn scan_style(style: &mut String, word: &str) {
    let mut rest = word;
    while !rest.is_empty() {
        let esc = ansi_len(rest);
        if esc > 0 {
            let seq = &rest[..esc];
            rest = &rest[esc..];
            if seq == "\x1b[0m" || seq == "\x1b[m" {
                style.clear();
            } else {
                style.push_str(seq);
            }
        } else {
            let c = rest.chars().next().unwrap_or(' ');
            rest = &rest[c.len_utf8()..];
        }
    }
}

/// One output line must be self-contained: close any style that is still
/// open at its end so colors never bleed into unrelated output.
fn sealed(line: &str, style: &str) -> String {
    if style.is_empty() {
        line.to_string()
    } else {
        format!("{line}\x1b[0m")
    }
}

/// Wrap one logical line into lines of at most `width` display columns.
/// Breaks at spaces when possible, hard-splits overlong words. ANSI
/// escapes are zero-width glue: never split across a break, and a style
/// opened before a wrap is closed at the line end and reopened on the
/// continuation so colors stay attached to their text.
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if display_width(line) <= width {
        let style = open_style(line);
        return vec![sealed(line, &style)];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    let mut style = String::new(); // style active at the end of `current`

    // split keeping empty segments so multiple spaces survive-ish
    for (i, word) in line.split(' ').enumerate() {
        let sep = if i == 0 { 0 } else { 1 };
        let w = display_width(word);
        if current_w + sep + w > width && !current.is_empty() {
            let line = sealed(&std::mem::take(&mut current), &style);
            out.push(line);
            current_w = 0;
            if !style.is_empty() {
                current.push_str(&style);
            }
        }
        if w > width {
            // hard-split a very long word on char boundaries
            let mut rest = word;
            while !rest.is_empty() {
                let esc = ansi_len(rest);
                if esc > 0 {
                    // escapes never split and never cost width
                    let seq = &rest[..esc];
                    rest = &rest[esc..];
                    current.push_str(seq);
                    scan_style(&mut style, seq);
                    continue;
                }
                let c = rest.chars().next().unwrap_or(' ');
                rest = &rest[c.len_utf8()..];
                let cw = char_width(c);
                if current_w + cw > width && !current.is_empty() {
                    let line = sealed(&std::mem::take(&mut current), &style);
                    out.push(line);
                    current_w = 0;
                    if !style.is_empty() {
                        current.push_str(&style);
                    }
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
            scan_style(&mut style, word);
        }
    }
    if !current.is_empty() {
        out.push(sealed(&current, &style));
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Streaming wrapper: feed fragments as they arrive, take complete lines.
/// A style that is open when a fragment ends is carried into the next
/// one, so markup split across deltas still renders correctly.
pub struct WordWrapper {
    pending: String,
    width: usize,
    carry: String,
}

impl WordWrapper {
    pub fn new(width: usize) -> WordWrapper {
        WordWrapper {
            pending: String::new(),
            width: width.max(8),
            carry: String::new(),
        }
    }

    /// Push streamed text; `on_line` receives each completed display line.
    pub fn push(&mut self, text: &str, on_line: &mut dyn FnMut(&str)) {
        self.pending.push_str(text);
        while let Some(pos) = self.pending.find('\n') {
            let logical: String = self.pending.drain(..=pos).collect();
            let logical = logical.trim_end_matches('\n');
            self.emit_logical(logical, on_line);
        }
        // complete any fully-occupied logical line at word boundaries
        if display_width(&self.pending) > self.width {
            if let Some(cut) = self.pending.rfind(' ') {
                if cut > 0 {
                    let logical: String = self.pending.drain(..=cut).collect();
                    self.emit_logical(logical.trim_end(), on_line);
                }
            } else if !self.pending.is_empty() {
                // no space at all: the line is one huge word - emit complete
                // chunks, keeping escape sequences uncut
                while display_width(&self.pending) > self.width {
                    let src = std::mem::take(&mut self.pending);
                    let mut take = String::new();
                    let mut tail = String::new();
                    let mut w = 0usize;
                    let mut overflow = false;
                    let mut i = 0usize;
                    while i < src.len() {
                        let esc = ansi_len(&src[i..]);
                        if esc > 0 {
                            let dst = if overflow { &mut tail } else { &mut take };
                            dst.push_str(&src[i..i + esc]);
                            i += esc;
                            continue;
                        }
                        let c = src[i..].chars().next().unwrap_or(' ');
                        let cw = char_width(c);
                        if !overflow && w + cw <= self.width {
                            take.push(c);
                            w += cw;
                        } else {
                            overflow = true;
                            tail.push(c);
                        }
                        i += c.len_utf8();
                    }
                    self.pending = tail;
                    on_line(&take);
                }
            }
        }
    }

    /// Wrap one logical chunk, carrying open styles across the boundary.
    fn emit_logical(&mut self, logical: &str, on_line: &mut dyn FnMut(&str)) {
        let input = format!("{}{}", self.carry, logical);
        self.carry = open_style(&input);
        for l in wrap_line(&input, self.width) {
            on_line(&l);
        }
    }

    /// Emit whatever is left (end of the reply).
    pub fn flush(&mut self, on_line: &mut dyn FnMut(&str)) {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.emit_logical(&pending, on_line);
        }
        self.carry.clear();
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
    fn ansi_len_covers_csi_and_osc() {
        for seq in [
            "\u{1b}[1m",
            "\u{1b}[38;2;1;2;3m",
            "\u{1b}[m",
            "\u{1b}]8;;http://x\u{7}",
            "\u{1b}]8;;http://x\u{1b}\\",
        ] {
            assert_eq!(super::ansi_len(seq), seq.len(), "seq {seq:?}");
        }
        assert_eq!(super::ansi_len("plain"), 0);
    }

    #[test]
    fn width_skips_escapes() {
        assert_eq!(super::display_width("\u{1b}[1mhi\u{1b}[0m"), 2);
        assert_eq!(super::display_width("\u{1b}]8;;http://x\u{7}link"), 4);
    }

    #[test]
    fn open_style_tracks_since_last_reset() {
        assert_eq!(super::open_style("plain"), "");
        assert_eq!(
            super::open_style("\u{1b}[1m\u{1b}[32mbold green"),
            "\u{1b}[1m\u{1b}[32m"
        );
        assert_eq!(
            super::open_style("\u{1b}[1mx\u{1b}[0m\u{1b}[33my"),
            "\u{1b}[33m"
        );
    }

    #[test]
    fn sealed_appends_reset_only_when_open() {
        assert_eq!(super::sealed("hi", ""), "hi");
        assert_eq!(
            super::sealed("\u{1b}[32mhi", "\u{1b}[32m"),
            "\u{1b}[32mhi\u{1b}[0m"
        );
    }

    #[test]
    fn styled_wrap_points_match_plain() {
        let plain = "the quick brown fox jumps over the lazy dog";
        let styled = format!("\u{1b}[1m{plain}\u{1b}[0m");
        let a = super::wrap_line(plain, 12);
        let b = super::wrap_line(&styled, 12);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(super::display_width(x), super::display_width(y));
        }
    }

    /// True when every escape sequence in the line terminates inside it.
    fn escapes_whole(l: &str) -> bool {
        let b = l.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == 0x1b {
                let mut j = i + 1;
                if j >= b.len() {
                    return false;
                }
                match b[j] {
                    b'[' => {
                        j += 1;
                        let mut found = false;
                        while j < b.len() {
                            if (0x40..=0x7E).contains(&b[j]) {
                                found = true;
                                break;
                            }
                            j += 1;
                        }
                        if !found {
                            return false;
                        }
                        j += 1;
                    }
                    b']' => {
                        j += 1;
                        let mut found = false;
                        while j < b.len() {
                            if b[j] == 0x07 {
                                found = true;
                                j += 1;
                                break;
                            }
                            if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == 0x5c {
                                found = true;
                                j += 2;
                                break;
                            }
                            j += 1;
                        }
                        if !found {
                            return false;
                        }
                    }
                    _ => j += 1,
                }
                i = j;
            } else {
                i += 1;
            }
        }
        true
    }

    #[test]
    fn wrapper_never_splits_escapes() {
        let mut w = super::WordWrapper::new(6);
        let mut lines = Vec::new();
        let mut sink = |l: &str| lines.push(l.to_string());
        // escapes land at and around the width boundary
        w.push("abcdefgh\u{1b}[32mxy\u{1b}[0m", &mut sink);
        w.flush(&mut sink);
        assert!(lines.len() >= 2, "expected wrapping: {lines:?}");
        for l in &lines {
            assert!(escapes_whole(l), "mid-escape split: {l:?}");
        }
        assert!(lines.iter().any(|l| l.contains("\u{1b}[32m")));
    }

    #[test]
    fn wrapper_carries_style_across_pushes() {
        let mut w = super::WordWrapper::new(10);
        let mut lines = Vec::new();
        let mut sink = |l: &str| lines.push(l.to_string());
        // the open green lands in one delta, the text and reset in later ones
        w.push("\u{1b}[32mhe", &mut sink);
        w.push("llo wo", &mut sink);
        w.push("rld\u{1b}[0m", &mut sink);
        w.flush(&mut sink);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "\u{1b}[32mhello\u{1b}[0m");
        assert_eq!(lines[1], "\u{1b}[32mworld\u{1b}[0m");
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
