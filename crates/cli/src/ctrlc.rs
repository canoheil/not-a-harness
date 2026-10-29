//! Clean exit on Ctrl+C instead of a bare process kill.
//!
//! The REPL uses the console's canonical line editor (by design - the
//! terminal renders RTL input properly), so an interrupt cannot cancel
//! just the current read. Instead we exit gracefully with a hint.

#[cfg(unix)]
pub fn install() {
    const MSG: &[u8] = b"\n^C interrupted - bye.\n";
    const SIGINT: i32 = 2;

    // Async-signal-safe handler: raw write + _exit only. Anything else
    // (println!, exit) could deadlock if the signal lands mid-allocation.
    extern "C" fn handler(_sig: i32) {
        unsafe {
            write(2, MSG.as_ptr().cast(), MSG.len());
            _exit(0);
        }
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
pub fn install() {
    // Ctrl+C handlers run on their own thread, so normal code is fine.
    extern "system" fn handler(kind: u32) -> i32 {
        // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1
        if kind <= 1 {
            eprintln!("\n^C interrupted - bye.");
            std::process::exit(0);
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
