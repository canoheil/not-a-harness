//! Clean Ctrl+C handling.
//!
//! While a turn is running, Ctrl+C **stops the turn** (the shared stop
//! flag is set and the stream loop exits at the next token). At an idle
//! prompt it exits gracefully with a hint - the console uses the
//! canonical line editor (by design - the terminal renders RTL input
//! properly), so an interrupt cannot cancel just the current read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static TURN_RUNNING: AtomicBool = AtomicBool::new(false);

/// The process-wide stop flag. The session binds to this handle, so a
/// Ctrl+C (or the GUI's Stop button) reaches the running turn.
pub fn stop_handle() -> Arc<AtomicBool> {
    STOP.get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// Mark whether a turn is currently generating (Ctrl+C then cancels
/// instead of exiting).
pub fn set_turn_running(on: bool) {
    TURN_RUNNING.store(on, Ordering::SeqCst);
}

fn interrupt() {
    if TURN_RUNNING.load(Ordering::SeqCst) {
        if let Some(stop) = STOP.get() {
            stop.store(true, Ordering::SeqCst);
        }
        return;
    }
    exit_now();
}

#[cfg(unix)]
fn exit_now() {
    const MSG: &[u8] = b"\n^C interrupted - bye.\n";
    // Async-signal-safe: raw write + _exit only. Anything else
    // (println!, exit) could deadlock if the signal lands mid-allocation.
    unsafe {
        write(2, MSG.as_ptr().cast(), MSG.len());
        _exit(0);
    }
}

#[cfg(unix)]
pub fn install() {
    const SIGINT: i32 = 2;

    extern "C" fn handler(_sig: i32) {
        interrupt();
    }
    unsafe {
        signal(SIGINT, handler);
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    // signature must match the runtime symbol exactly (the std's own
    // declaration uses *const c_void) or rustc's
    // suspicious_runtime_symbol_definitions lint fires
    fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
    fn _exit(code: i32) -> !;
}

#[cfg(windows)]
fn exit_now() {
    eprintln!("\n^C interrupted - bye.");
    std::process::exit(0);
}

#[cfg(windows)]
pub fn install() {
    // Ctrl+C handlers run on their own thread, so normal code is fine.
    extern "system" fn handler(kind: u32) -> i32 {
        // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1
        if kind <= 1 {
            interrupt();
            return 1;
        }
        0
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
}
