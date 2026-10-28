//! Terminal presentation: ANSI colors, RTL-safe printing, Windows VT setup.
//!
//! RTL policy (v1): we never reorder text ourselves — Unicode logical order
//! goes to the terminal as-is and the emulator (Windows Terminal, …) does
//! the BiDi rendering. We only choose alignment and colors.

#![allow(dead_code)]

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
            enable_ansi_windows();
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
        let art = format!(
            "{}\n{}\n{}\n",
            self.paint(BLUE, "  nah — not-a-harness"),
            self.paint(
                DIM,
                "  هارنس سبک مدل‌های لوکال · پروتکل ابزار متنی · RTL-friendly"
            ),
            self.paint(DIM, "  /help برای راهنما · /exit برای خروج"),
        );
        print!("{art}");
        flush();
    }

    pub fn prompt(&self) {
        // ASCII prompt keeps the input line bidi-stable in every terminal;
        // Persian is used for all output text (rendered RTL by the terminal).
        print!("{} ", self.paint(CYAN, ">"));
        flush();
    }

    pub fn user_echo(&self, text: &str) {
        println!("{} {}", self.paint(GREEN, "شما:"), text);
    }

    pub fn delta(&self, text: &str) {
        // streamed model text — print raw so RTL flows naturally
        print!("{text}");
        flush();
    }

    pub fn turn_start(&self) {
        // marker printed before the model starts streaming
    }

    pub fn action(&self, label: &str) {
        println!(
            "{} {}",
            self.paint(MAGENTA, "◀ ابزار:"),
            self.paint(DIM, label)
        );
    }

    pub fn tool_ok(&self, body: &str) {
        for line in body.lines().take(12) {
            println!("  {} {}", self.paint(DIM, "│"), self.paint(DIM, line));
        }
    }

    pub fn tool_err(&self, body: &str) {
        for line in body.lines().take(12) {
            println!("  {} {}", self.paint(RED, "│"), self.paint(RED, line));
        }
    }

    pub fn info(&self, text: &str) {
        println!("{}", self.paint(YELLOW, text));
    }

    pub fn error(&self, text: &str) {
        eprintln!("{} {}", self.paint(RED, "خطا:"), self.paint(RED, text));
    }

    pub fn ok(&self, text: &str) {
        println!("{} {}", self.paint(GREEN, "✓"), text);
    }

    /// Confirmation dialog for file writes/edits. Returns true to approve.
    pub fn confirm(&self, verb: &str, path: &str, preview: &str, is_new: bool) -> bool {
        let kind = if is_new {
            "ایجاد فایل جدید"
        } else {
            "تغییر فایل"
        };
        println!(
            "\n{} {} → {}",
            self.paint(BOLD, &format!("{kind}:")),
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
        print!("{} ", self.paint(YELLOW, "تأیید می‌کنید؟ (y/N)"));
        flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "بله"
        )
    }

    pub fn models(&self, models: &[(String, String)]) {
        if models.is_empty() {
            self.info("هیچ مدلی روی سرور پیدا نشد.");
            return;
        }
        println!("{}", self.paint(BOLD, "مدل‌های موجود:"));
        for (name, detail) in models {
            if detail.is_empty() {
                println!("  • {name}");
            } else {
                println!("  • {name}  {}", self.paint(DIM, detail));
            }
        }
    }
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// Windows legacy consoles hide ANSI support behind a flag; ask for it
/// via raw kernel32 FFI (no extra crate).
#[cfg(windows)]
fn enable_ansi_windows() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: i32) -> *mut core::ffi::c_void;
        fn GetConsoleMode(h: *mut core::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: *mut core::ffi::c_void, mode: u32) -> i32;
    }
    unsafe {
        const STD_OUTPUT_HANDLE: i32 = -11;
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle.is_null() {
            return;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_ansi_windows() {}
