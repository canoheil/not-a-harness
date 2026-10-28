//! Terminal presentation: ANSI colors, English UI, RTL-safe rendering.
//!
//! The interface is English (Claude-Code style). Persian/Arabic appears
//! only as *content* (user input, model output, files). Two techniques keep
//! it display-clean in any modern terminal:
//!
//! 1. We wrap every printed line at word boundaries ourselves (see
//!    `wrap.rs`) so the terminal never splits RTL paragraphs mid-flow -
//!    each line we emit is one clean BiDi segment, top-down in logical
//!    order, exactly like polished TUIs do.
//! 2. Text is passed through in Unicode logical order; the terminal does
//!    the BiDi reordering and Arabic shaping at render time.
//!
//! Windows: we enable VT processing and UTF-8 (CP65001). In classic
//! conhost (no font fallback) a font without Persian glyphs shows RTL as
//! boxes - we detect that and offer a font switch. Windows Terminal does
//! its own font fallback, so the warning is skipped there.

use std::io::Write;
use std::sync::Mutex;

use crate::spinner::Spinner;
use crate::wrap::{terminal_width, wrap_line, WordWrapper};

pub struct Ui {
    pub color: bool,
    pub width: usize,
    stream: Mutex<WordWrapper>,
    spinner: Mutex<Option<Spinner>>,
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";

impl Ui {
    pub fn new(color: bool, width: Option<usize>) -> Ui {
        if color {
            setup_windows_console();
        }
        let width = terminal_width(width);
        Ui {
            color,
            width,
            stream: Mutex::new(WordWrapper::new(width)),
            spinner: Mutex::new(None),
        }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    // ------------------------------------------------------------------
    // spinner control
    // ------------------------------------------------------------------

    /// Start (or restart) the activity spinner for a model turn.
    pub fn spinner_on(&self) {
        let mut slot = self.spinner.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(Spinner::start(self.color));
        }
    }

    /// Stop the spinner and leave the line clean.
    pub fn spinner_off(&self) {
        let mut slot = self.spinner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = slot.take() {
            s.stop();
        }
    }

    /// Record output activity (erases the spinner frame).
    fn touch(&self) {
        let slot = self.spinner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = slot.as_ref() {
            s.activity();
        }
    }

    // ------------------------------------------------------------------
    // printing (everything goes through word-wrap)
    // ------------------------------------------------------------------

    /// Print one complete logical line, wrapped to the terminal width.
    pub fn line(&self, text: &str) {
        self.touch();
        for l in wrap_line(text, self.width) {
            println!("{l}");
        }
    }

    /// Print one pre-colored logical line (color codes excluded from wrap).
    pub fn line_colored(&self, code: &str, text: &str) {
        self.touch();
        if self.color {
            for l in wrap_line(text, self.width) {
                println!("{code}{l}{RESET}");
            }
        } else {
            self.line(text);
        }
    }

    /// Streamed model text: fragments arrive over time; complete wrapped
    /// lines are printed as soon as they are ready.
    pub fn delta(&self, text: &str) {
        self.touch();
        let mut lines: Vec<String> = Vec::new();
        {
            let mut stream = self.stream.lock().unwrap_or_else(|e| e.into_inner());
            let mut collect = |l: &str| lines.push(l.to_string());
            stream.push(text, &mut collect);
        }
        for l in lines {
            println!("{l}");
        }
    }

    /// Flush the tail of the streamed reply (end of turn).
    pub fn stream_flush(&self) {
        let mut lines: Vec<String> = Vec::new();
        {
            let mut stream = self.stream.lock().unwrap_or_else(|e| e.into_inner());
            let mut collect = |l: &str| lines.push(l.to_string());
            stream.flush(&mut collect);
        }
        if !lines.is_empty() {
            self.touch();
            for l in lines {
                println!("{l}");
            }
        }
    }

    // ------------------------------------------------------------------
    // UI chrome
    // ------------------------------------------------------------------

    pub fn banner(&self) {
        println!(
            "{}",
            self.paint(BLUE, &format!("{}{}", BOLD, "nah - not-a-harness"))
        );
        self.line_colored(DIM, "lean local-LLM agent harness - text tool protocol");
        self.line_colored(DIM, "type /help for commands, /exit to quit");
        println!();
    }

    pub fn divider(&self) {
        self.touch();
        let line = "─".repeat(self.width.min(60));
        println!("{}", self.paint(DIM, &line));
    }

    pub fn prompt(&self) {
        self.touch();
        print!("{} ", self.paint(CYAN, ">"));
        flush();
    }

    pub fn action(&self, label: &str) {
        self.touch();
        self.line_colored(MAGENTA, &format!("tool: {label}"));
    }

    pub fn tool_ok(&self, body: &str) {
        self.touch();
        for line in body.lines().take(12) {
            self.line_colored(DIM, &format!("| {line}"));
        }
    }

    pub fn tool_err(&self, body: &str) {
        self.touch();
        for line in body.lines().take(12) {
            self.line_colored(RED, &format!("| {line}"));
        }
    }

    pub fn info(&self, text: &str) {
        self.touch();
        self.line_colored(YELLOW, text);
    }

    pub fn error(&self, text: &str) {
        self.touch();
        self.stream_flush();
        eprintln!("{}", self.paint(RED, &format!("error: {text}")));
    }

    pub fn ok(&self, text: &str) {
        self.touch();
        self.line_colored(GREEN, &format!("ok: {text}"));
    }

    /// Read a y/N answer from stdin (spinner is paused around the prompt).
    pub fn ask_yes_no(&self, question: &str) -> bool {
        self.spinner_off();
        print!("{} ", self.paint(YELLOW, question));
        flush();
        let mut answer = String::new();
        let ok = std::io::stdin().read_line(&mut answer).is_ok();
        self.spinner_on();
        ok && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    /// Confirmation dialog for file writes/edits. Returns true to approve.
    pub fn confirm(&self, verb: &str, path: &str, preview: &str, is_new: bool) -> bool {
        self.spinner_off();
        let kind = if is_new { "create file" } else { "modify file" };
        println!(
            "\n{} {} -> {}",
            self.paint(BOLD, kind),
            self.paint(CYAN, verb),
            self.paint(CYAN, path)
        );
        for line in preview.lines().take(60) {
            let colored = if line.starts_with('+') {
                self.paint(GREEN, line)
            } else if line.starts_with('-') {
                self.paint(RED, line)
            } else if line.starts_with("...") {
                self.paint(DIM, line)
            } else {
                line.to_string()
            };
            println!("  {colored}");
        }
        self.ask_yes_no("Apply this change? [y/N]")
    }

    pub fn models(&self, models: &[(String, String)]) {
        if models.is_empty() {
            self.info("no models found on the server.");
            return;
        }
        self.line_colored(BOLD, "available models:");
        for (name, detail) in models {
            if detail.is_empty() {
                self.line(&format!("  - {name}"));
            } else {
                self.line_colored(DIM, &format!("  - {name}  ({detail})"));
            }
        }
    }

    /// Warn (and offer a fix) when the console font cannot render Persian.
    /// Skipped in Windows Terminal (it does per-glyph font fallback) and on
    /// other platforms. No-op when the font is fine.
    pub fn rtl_font_notice(&self) {
        if in_windows_terminal() {
            return; // font fallback makes RTL render fine here
        }
        if rtl_glyphs_available() != Some(false) {
            return;
        }
        let font_desc = match console_font_name() {
            Some(name) => format!("current console font: '{name}'"),
            None => "current console font".to_string(),
        };
        println!();
        self.line_colored(
            YELLOW,
            &format!("note: {font_desc} has no Persian/Arabic glyphs - RTL text shows as boxes."),
        );
        self.line_colored(
            YELLOW,
            "      this also affects the terminal's own echo of typed input (not a nah bug).",
        );
        self.line_colored(
            YELLOW,
            "      recommended: run nah inside Windows Terminal - it falls back to system fonts.",
        );
        if self.ask_yes_no("try switching this console's font to 'Segoe UI' now? [y/N]") {
            if set_console_font("Segoe UI") && rtl_glyphs_available() == Some(true) {
                self.ok("console font switched to 'Segoe UI'.");
            } else {
                self.info("could not switch the font automatically.");
                self.info("open the console Properties > Font and pick one with Persian");
                self.info("support (e.g. Segoe UI, Vazirmatn Mono, DejaVu Sans Mono).");
            }
        }
    }
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// True when running inside Windows Terminal (font fallback available).
pub fn in_windows_terminal() -> bool {
    std::env::var_os("WT_SESSION").is_some() || std::env::var_os("WT_PROFILE_ID").is_some()
}

// ---------------------------------------------------------------------------
// Windows console plumbing (UTF-8 code page, VT mode, font glyph checks)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use core::ffi::c_void;

    #[repr(C)]
    pub struct Coord {
        pub x: i16,
        pub y: i16,
    }

    #[repr(C)]
    pub struct ConsoleFontInfoEx {
        pub cb_size: u32,
        pub n_font: u32,
        pub dw_font_size: Coord,
        pub family: u32,
        pub weight: u32,
        pub face_name: [u16; 32], // LF_FACESIZE
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetStdHandle(n: i32) -> *mut c_void;
        pub fn GetConsoleMode(h: *mut c_void, mode: *mut u32) -> i32;
        pub fn SetConsoleMode(h: *mut c_void, mode: u32) -> i32;
        pub fn SetConsoleOutputCP(cp: u32) -> i32;
        pub fn SetConsoleCP(cp: u32) -> i32;
        pub fn GetCurrentConsoleFontEx(
            h: *mut c_void,
            max_window: i32,
            info: *mut ConsoleFontInfoEx,
        ) -> i32;
        pub fn SetCurrentConsoleFontEx(
            h: *mut c_void,
            max_window: i32,
            info: *const ConsoleFontInfoEx,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        pub fn CreateFontW(
            height: i32,
            width: i32,
            escapement: i32,
            orientation: i32,
            weight: i32,
            italic: u32,
            underline: u32,
            strikeout: u32,
            charset: u32,
            output_precision: u32,
            clip_precision: u32,
            quality: u32,
            pitch_family: u32,
            face: *const u16,
        ) -> *mut c_void;
        pub fn SelectObject(hdc: *mut c_void, obj: *mut c_void) -> *mut c_void;
        pub fn DeleteObject(obj: *mut c_void) -> i32;
        pub fn GetGlyphIndicesW(
            hdc: *mut c_void,
            text: *const u16,
            count: i32,
            indices: *mut u16,
            flags: u32,
        ) -> u32;
    }

    #[link(name = "user32")]
    extern "system" {
        pub fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        pub fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
    }

    pub const STD_OUTPUT_HANDLE: i32 = -11;
    pub const CP_UTF8: u32 = 65001;
    pub const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    pub const GGI_MARK_NONEXISTING_GLYPHS: u32 = 0x0001;
    pub const GDI_ERROR: u32 = 0xFFFF_FFFF;

    pub fn current_font_info() -> Option<ConsoleFontInfoEx> {
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle.is_null() {
                return None;
            }
            let mut info = ConsoleFontInfoEx {
                cb_size: std::mem::size_of::<ConsoleFontInfoEx>() as u32,
                n_font: 0,
                dw_font_size: Coord { x: 0, y: 0 },
                family: 0,
                weight: 0,
                face_name: [0; 32],
            };
            if GetCurrentConsoleFontEx(handle, 0, &mut info) == 0 {
                return None;
            }
            Some(info)
        }
    }
}

/// Enable ANSI colors + UTF-8 output on Windows consoles.
#[cfg(windows)]
fn setup_windows_console() {
    use win::*;
    unsafe {
        SetConsoleOutputCP(CP_UTF8);
        SetConsoleCP(CP_UTF8);
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() {
            return;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn setup_windows_console() {}

/// Can the current console font render Persian/Arabic letters?
/// `true` = yes, `false` = glyphs missing, `None` = could not determine.
#[cfg(windows)]
pub fn rtl_glyphs_available() -> Option<bool> {
    use win::*;
    let info = current_font_info()?;
    unsafe {
        let hdc = GetDC(core::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let font = CreateFontW(
            0,
            0,
            0,
            0,
            0, // FW_DONTCARE
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            0,
            0,
            info.face_name.as_ptr(),
        );
        if font.is_null() {
            ReleaseDC(core::ptr::null_mut(), hdc);
            return None;
        }
        let previous = SelectObject(hdc, font);
        // Persian-specific letters (پ چ ژ گ ک ی) - distinct from plain Arabic
        let sample: [u16; 6] = [0x067E, 0x0686, 0x0698, 0x06AF, 0x06A9, 0x06CC];
        let mut indices = [0u16; 6];
        let result = GetGlyphIndicesW(
            hdc,
            sample.as_ptr(),
            sample.len() as i32,
            indices.as_mut_ptr(),
            GGI_MARK_NONEXISTING_GLYPHS,
        );
        SelectObject(hdc, previous);
        DeleteObject(font);
        ReleaseDC(core::ptr::null_mut(), hdc);
        if result == GDI_ERROR {
            return None;
        }
        // with GGI_MARK_NONEXISTING_GLYPHS, missing glyphs are 0xFFFF
        Some(!indices.contains(&0xFFFF))
    }
}

#[cfg(not(windows))]
pub fn rtl_glyphs_available() -> Option<bool> {
    None
}

/// Name of the active console font (Windows only).
#[cfg(windows)]
pub fn console_font_name() -> Option<String> {
    let info = win::current_font_info()?;
    let len = info
        .face_name
        .iter()
        .position(|&u| u == 0)
        .unwrap_or(info.face_name.len());
    if len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&info.face_name[..len]))
}

#[cfg(not(windows))]
pub fn console_font_name() -> Option<String> {
    None
}

/// Try to change this console window's font (classic conhost only;
/// Windows Terminal ignores it and has font fallback anyway).
#[cfg(windows)]
fn set_console_font(name: &str) -> bool {
    use win::*;
    let Some(mut info) = current_font_info() else {
        return false;
    };
    let mut face = [0u16; 32];
    for (slot, unit) in face.iter_mut().zip(name.encode_utf16()) {
        *slot = unit;
    }
    info.face_name = face;
    info.family = 0; // FF_DONTCARE
    info.weight = 400; // FW_NORMAL
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() {
            return false;
        }
        SetCurrentConsoleFontEx(handle, 0, &info) != 0
    }
}

#[cfg(not(windows))]
fn set_console_font(_name: &str) -> bool {
    false
}
