//! Terminal presentation: ANSI colors, English UI, Windows console setup.
//!
//! The interface is English (Claude-Code style). Persian/Arabic appears only
//! as *content* (user input, model output, files) and is passed through in
//! Unicode logical order — the terminal emulator does the BiDi rendering.
//! On Windows we switch the console to UTF-8 (CP65001) so non-ASCII text
//! is not mangled into `?` by legacy code pages.

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
        print!("{} ", self.paint(YELLOW, "Apply this change? [y/N]"));
        flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
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
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// Windows legacy consoles need two things: ANSI (VT) processing enabled,
/// and the UTF-8 code page so byte output is not mangled into `?`.
#[cfg(windows)]
fn setup_windows_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: i32) -> *mut core::ffi::c_void;
        fn GetConsoleMode(h: *mut core::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: *mut core::ffi::c_void, mode: u32) -> i32;
        fn SetConsoleOutputCP(cp: u32) -> i32;
        fn SetConsoleCP(cp: u32) -> i32;
    }
    unsafe {
        const STD_OUTPUT_HANDLE: i32 = -11;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
        // UTF-8 code page: keeps Persian/Unicode text intact in legacy consoles
        const CP_UTF8: u32 = 65001;
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
