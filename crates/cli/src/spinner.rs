//! A rotating activity indicator shown while the model is thinking or
//! generating, so slow local models never look "frozen".
//!
//! The spinner only paints after ~500ms of silence and clears itself the
//! moment real output starts, so it never fights with streamed text.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const IDLE_AFTER: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(90);

struct Shared {
    stop: AtomicBool,
    frame_on_screen: AtomicBool,
    last_activity: Mutex<Instant>,
    label: &'static str,
    color: bool,
}

pub struct Spinner {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Spinner {
    pub fn start(color: bool) -> Spinner {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            frame_on_screen: AtomicBool::new(false),
            last_activity: Mutex::new(Instant::now()),
            label: "thinking...",
            color,
        });
        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::spawn(move || {
            let mut frame = 0usize;
            loop {
                std::thread::sleep(TICK);
                if thread_shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                let idle = thread_shared
                    .last_activity
                    .lock()
                    .map(|t| t.elapsed() >= IDLE_AFTER)
                    .unwrap_or(true);
                if !idle {
                    continue;
                }
                // draw one frame; hold it until the next activity/stop
                let mut out = std::io::stdout();
                let symbol = FRAMES[frame % FRAMES.len()];
                frame += 1;
                let text = if thread_shared.color {
                    format!("\r\x1b[2m{symbol} {}\x1b[0m", thread_shared.label)
                } else {
                    format!("\r{symbol} {}", thread_shared.label)
                };
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
                thread_shared.frame_on_screen.store(true, Ordering::SeqCst);
            }
        });
        Spinner {
            shared,
            handle: Some(handle),
        }
    }

    /// Record output activity and erase the spinner frame if visible.
    pub fn activity(&self) {
        if let Ok(mut t) = self.shared.last_activity.lock() {
            *t = Instant::now();
        }
        self.clear_frame();
    }

    fn clear_frame(&self) {
        if self.shared.frame_on_screen.swap(false, Ordering::SeqCst) {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\r\x1b[2K");
            let _ = out.flush();
        }
    }

    /// Stop the spinner thread and make sure no frame is left on screen.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.clear_frame();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_starts_and_stops_cleanly() {
        let s = Spinner::start(false);
        s.activity();
        s.stop();
    }

    #[test]
    fn spinner_survives_rapid_activity() {
        let s = Spinner::start(true);
        for _ in 0..50 {
            s.activity();
        }
        s.stop();
    }
}
