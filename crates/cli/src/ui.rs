//! Terminal presentation: ANSI colors, English UI, Windows console setup.
//!
//! The interface is English (Claude-Code style). Persian/Arabic appears only
//! as *content* (user input, model output, files) and is passed through in
//! Unicode logical order - the terminal emulator does the BiDi rendering.
//!
//! Windows note: classic conhost has no per-glyph font fallback, so if the
//! console font lacks Persian/Arabic glyphs, ALL RTL text (including the
//! terminal's own echo of typed input) shows as boxes. We detect this via
//! GDI glyph lookup and offer to switch the console font, or point the
//! user to Windows Terminal which falls back to system fonts automatically.

use std::io::Write;

pub struct Ui {
    pub color: bool,
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
    pub fn new(color: bool) -> Ui {
        if color {
            setup_windows_console();
        }
        Ui { color }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("{code}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    pub fn banner(&self) {
        println!("{}", self.paint(BLUE, "nah - not-a-harness"));
        println!(
            "{}",
            self.paint(DIM, "lean local-LLM agent harness - text tool protocol")
        );
        println!(
            "{}\n",
            self.paint(DIM, "type /help for commands, /exit to quit")
        );
    }

    pub fn prompt(&self) {
        print!("{} ", self.paint(CYAN, ">"));
        flush();
    }

    pub fn delta(&self, text: &str) {
        // streamed model text - printed as-is so RTL content flows naturally
        print!("{text}");
        flush();
    }

    pub fn action(&self, label: &str) {
        println!(
            "{} {}",
            self.paint(MAGENTA, "tool:"),
            self.paint(DIM, label)
        );
    }

    pub fn tool_ok(&self, body: &str) {
        for line in body.lines().take(12) {
            println!("  {} {}", self.paint(DIM, "|"), self.paint(DIM, line));
        }
    }

    pub fn tool_err(&self, body: &str) {
        for line in body.lines().take(12) {
            println!("  {} {}", self.paint(RED, "|"), self.paint(RED, line));
        }
    }

    pub fn info(&self, text: &str) {
        println!("{}", self.paint(YELLOW, text));
    }

    pub fn error(&self, text: &str) {
        eprintln!("{} {}", self.paint(RED, "error:"), self.paint(RED, text));
    }

    pub fn ok(&self, text: &str) {
        println!("{} {}", self.paint(GREEN, "ok:"), text);
    }

    /// Read a y/N answer from stdin.
    pub fn ask_yes_no(&self, question: &str) -> bool {
        print!("{} ", self.paint(YELLOW, question));
        flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    /// Confirmation dialog for file writes/edits. Returns true to approve.
    pub fn confirm(&self, verb: &str, path: &str, preview: &str, is_new: bool) -> bool {
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
        println!("{}", self.paint(BOLD, "available models:"));
        for (name, detail) in models {
            if detail.is_empty() {
                println!("  - {name}");
            } else {
                println!("  - {name}  {}", self.paint(DIM, detail));
            }
        }
    }

    /// Warn (and offer a fix) when the console font cannot render Persian.
    /// No-op on other platforms and when the font is fine.
    pub fn rtl_font_notice(&self) {
        if rtl_glyphs_available() != Some(false) {
            return;
        }
        let font_desc = match console_font_name() {
            Some(name) => format!("current console font: '{name}'"),
            None => "current console font".to_string(),
        };
        println!();
        self.info(&format!(
            "note: {font_desc} has no Persian/Arabic glyphs - RTL text shows as boxes."
        ));
        self.info(
            "      this also affects the terminal's own echo of typed input (not a nah bug).",
        );
        self.info(
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
