//! Minimal std-only web GUI: a tiny local HTTP server plus an embedded
//! single-page UI (no frameworks, no external assets).
//!
//! `nah --gui` serves the interface on 127.0.0.1 (override with
//! `NAH_GUI_BIND`) and opens the browser. The engine (session, tools,
//! context) stays exactly the one the REPL uses; this module is only
//! transport and presentation.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};

use nah_core::config::{tools_dir, NahConfig};
use nah_core::error::CoreError;
use nah_core::provider::{connect, ChatOptions};
use nah_core::session::{Event, Session, SessionConfig};
use nah_core::tools::{ConfirmRequest, Mode, ToolKit};

use crate::ui::ToolBlockFilter;

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_JS: &str = include_str!("assets/app.js");
const STYLE_CSS: &str = include_str!("assets/style.css");

/// One confirmation waiting for the user's click.
#[derive(Debug, Clone)]
struct Pending {
    id: u64,
    verb: String,
    path: String,
    preview: String,
    is_new: bool,
}

/// Live status snapshot for `/api/state` (kept outside the engine lock).
#[derive(Debug, Clone, Default)]
struct Status {
    model: String,
    server: String,
    root: String,
    ctx_tokens: usize,
    usage_tokens: usize,
    messages: usize,
    stats: String,
}

#[derive(Default)]
struct Inner {
    pending: Vec<Pending>,
    answers: Vec<(u64, bool)>,
    next_id: u64, // starts at 1
    busy: bool,
    mode: String,
    status: Status,
}

/// Shared UI-side state: pending confirmations, status, busy flag.
/// Every field is safe to read while a turn is running.
struct UiState {
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl UiState {
    fn new() -> Self {
        let inner = Inner {
            next_id: 1,
            ..Default::default()
        };
        UiState {
            inner: Mutex::new(inner),
            cv: Condvar::new(),
        }
    }

    /// Register a confirmation and block until the user answers.
    fn ask(&self, req: &ConfirmRequest) -> bool {
        let id = {
            let mut inner = self.inner.lock().unwrap();
            let id = inner.next_id;
            inner.next_id += 1;
            inner.pending.push(Pending {
                id,
                verb: req.verb.clone(),
                path: req.path.clone(),
                preview: req.preview.clone(),
                is_new: req.is_new_file,
            });
            id
        };
        self.cv.notify_all();
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some(pos) = inner.answers.iter().position(|(i, _)| *i == id) {
                let (_, allow) = inner.answers.remove(pos);
                inner.pending.retain(|p| p.id != id);
                self.cv.notify_all();
                return allow;
            }
            inner = self.cv.wait(inner).unwrap();
        }
    }

    /// Resolve a pending confirmation (`POST /api/confirm`).
    fn answer(&self, id: u64, allow: bool) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.pending.iter().any(|p| p.id == id) {
            inner.answers.push((id, allow));
            self.cv.notify_all();
            true
        } else {
            false
        }
    }
}

/// The engine behind the GUI: one session, one tool kit.
struct Engine {
    session: Session<'static>,
}

// ---------------------------------------------------------------------------
// tiny JSON helpers (hand-rolled; the project is std-only)
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn state_json(state: &UiState) -> String {
    let inner = state.inner.lock().unwrap();
    let mut pending = String::new();
    for p in &inner.pending {
        if !pending.is_empty() {
            pending.push(',');
        }
        pending.push_str(&format!(
            "{{\"id\":{},\"verb\":\"{}\",\"path\":\"{}\",\"preview\":\"{}\",\"is_new\":{}}}",
            p.id,
            esc(&p.verb),
            esc(&p.path),
            esc(&p.preview),
            p.is_new
        ));
    }
    let s = &inner.status;
    format!(
        "{{\"model\":\"{}\",\"server\":\"{}\",\"root\":\"{}\",\"mode\":\"{}\",\
         \"ctx_tokens\":{},\"usage_tokens\":{},\"messages\":{},\"busy\":{},\
         \"stats\":\"{}\",\"pending\":[{}]}}",
        esc(&s.model),
        esc(&s.server),
        esc(&s.root),
        esc(&inner.mode),
        s.ctx_tokens,
        s.usage_tokens,
        s.messages,
        inner.busy,
        esc(&s.stats),
        pending
    )
}

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    body: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    // headers, byte by byte: requests are tiny and local
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body).ok()?;
    }
    Some(Request {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

/// Pull a flat `"key": "value"` / `"key": number` string field out of a
/// tiny request body. Good enough for our own fixed API shapes.
fn json_field(body: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let start = body.find(&pat)? + pat.len();
    let rest = body[start..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        // string value with basic unescaping
        let mut out = String::new();
        let mut chars = stripped.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some(other) => out.push(other),
                    None => break,
                },
                c => out.push(c),
            }
        }
        Some(out)
    } else {
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'))
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(body.as_bytes());
}

// ---------------------------------------------------------------------------
// request handling
// ---------------------------------------------------------------------------

fn handle_client(mut stream: TcpStream, engine: Arc<Mutex<Engine>>, state: Arc<UiState>) {
    let Some(req) = read_request(&mut stream) else {
        return;
    };
    match (
        req.method.as_str(),
        req.path.split('?').next().unwrap_or(""),
    ) {
        ("GET", "/") => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML,
        ),
        ("GET", "/app.js") => respond(&mut stream, "200 OK", "application/javascript", APP_JS),
        ("GET", "/style.css") => respond(&mut stream, "200 OK", "text/css", STYLE_CSS),
        ("GET", "/api/state") => {
            let body = state_json(&state);
            respond(&mut stream, "200 OK", "application/json", &body);
        }
        ("POST", "/api/chat") => chat_handler(&mut stream, &req, &engine, &state),
        ("POST", "/api/confirm") => {
            let id = json_field(&req.body, "id")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let allow = json_field(&req.body, "allow")
                .map(|v| v == "true")
                .unwrap_or(false);
            if state.answer(id, allow) {
                respond(&mut stream, "200 OK", "application/json", "{\"ok\":true}");
            } else {
                respond(
                    &mut stream,
                    "404 Not Found",
                    "application/json",
                    "{\"ok\":false}",
                );
            }
        }
        ("POST", "/api/mode") => mode_handler(&mut stream, &req, &engine, &state),
        _ => respond(&mut stream, "404 Not Found", "text/plain", "not found"),
    }
}

fn mode_handler(
    stream: &mut TcpStream,
    req: &Request,
    engine: &Arc<Mutex<Engine>>,
    state: &Arc<UiState>,
) {
    let Some(mode) = json_field(&req.body, "mode").and_then(|m| Mode::parse(&m)) else {
        respond(
            stream,
            "400 Bad Request",
            "application/json",
            "{\"ok\":false}",
        );
        return;
    };
    if state.inner.lock().unwrap().busy {
        respond(
            stream,
            "409 Conflict",
            "application/json",
            "{\"ok\":false,\"busy\":true}",
        );
        return;
    }
    engine
        .lock()
        .unwrap()
        .session
        .tools
        .registry_mut()
        .set_mode(mode);
    state.inner.lock().unwrap().mode = mode.as_str().to_string();
    respond(stream, "200 OK", "application/json", "{\"ok\":true}");
}

fn chat_handler(
    stream: &mut TcpStream,
    req: &Request,
    engine: &Arc<Mutex<Engine>>,
    state: &Arc<UiState>,
) {
    let text = json_field(&req.body, "text").unwrap_or_default();
    if text.trim().is_empty() {
        respond(
            stream,
            "400 Bad Request",
            "application/json",
            "{\"ok\":false}",
        );
        return;
    }
    {
        let mut inner = state.inner.lock().unwrap();
        if inner.busy {
            respond(
                stream,
                "409 Conflict",
                "application/json",
                "{\"ok\":false,\"busy\":true}",
            );
            return;
        }
        inner.busy = true;
    }

    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.flush();

    // tool blocks stay hidden in the chat transcript (the tool cards
    // render the actions instead) - same filter the REPL uses
    let mut filter = ToolBlockFilter::new();
    let mut sse = |event: &str, data: &str| {
        let _ = write!(stream, "event: {event}\ndata: {data}\n\n");
        let _ = stream.flush();
    };

    let mut ask = |r: &ConfirmRequest| state.ask(r);
    let mut emit = |ev: &Event<'_>| match ev {
        Event::Delta(text) => {
            let visible = filter.push(text);
            if !visible.is_empty() {
                sse("delta", &format!("{{\"text\":\"{}\"}}", esc(&visible)));
            }
        }
        Event::Action(a) => {
            sse(
                "tool",
                &format!(
                    "{{\"verb\":\"{}\",\"path\":\"{}\"}}",
                    esc(&a.verb()),
                    esc(a.path())
                ),
            );
        }
        Event::ToolResult(o) => {
            sse(
                "result",
                &format!(
                    "{{\"verb\":\"{}\",\"path\":\"{}\",\"is_error\":{},\"body\":\"{}\"}}",
                    esc(&o.verb),
                    esc(&o.path),
                    o.is_error,
                    esc(&o.body)
                ),
            );
        }
        Event::Info(msg) => sse("info", &format!("{{\"text\":\"{}\"}}", esc(msg))),
        Event::TurnDone => {}
    };

    let result = {
        let mut engine = engine.lock().unwrap();
        engine.session.run_turn(&text, &mut ask, &mut emit)
    };

    // status snapshot for the footer
    let mut inner = state.inner.lock().unwrap();
    inner.busy = false;
    drop(inner);

    let engine = engine.lock().unwrap();
    let (usage, msgs) = engine.session.context_usage();
    {
        let mut inner = state.inner.lock().unwrap();
        inner.status.usage_tokens = usage;
        inner.status.messages = msgs;
    }
    drop(engine);

    match result {
        Ok(stats) => {
            let line = format!(
                "~{:.0} tok/s - {} tokens - {:.1}s",
                if stats.gen_secs > 0.0 {
                    stats.est_tokens as f64 / stats.gen_secs
                } else {
                    0.0
                },
                stats.est_tokens,
                stats.gen_secs
            );
            let payload = format!("{{\"stats\":\"{}\"}}", esc(&line));
            state.inner.lock().unwrap().status.stats = line;
            sse("done", &payload);
        }
        Err(e) => sse(
            "error",
            &format!("{{\"text\":\"{}\"}}", esc(&e.to_string())),
        ),
    }
}

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

/// Serve the web GUI until the process is stopped.
pub fn serve(cfg: NahConfig, port: u16) -> Result<(), CoreError> {
    let provider = connect(cfg.provider, &cfg.url)?;
    let mut tools = ToolKit::new(cfg.root.clone(), cfg.max_tool_bytes);
    for w in tools.load_custom_tools(&tools_dir()) {
        eprintln!("[warn] {w}");
    }
    if cfg.auto_approve {
        tools.registry_mut().set_mode(Mode::Auto);
    }
    let mode = tools.registry().mode().as_str().to_string();
    let session = Session::new(
        provider,
        SessionConfig {
            model: cfg.model.clone(),
            options: ChatOptions {
                num_ctx: cfg.ctx_tokens as u32,
                temperature: cfg.temperature,
            },
            max_steps: cfg.max_steps,
        },
        tools,
        nah_core::context::ContextManager::new(cfg.context_config()),
    );
    let engine = Arc::new(Mutex::new(Engine { session }));
    let state = Arc::new(UiState::new());
    {
        let mut inner = state.inner.lock().unwrap();
        inner.mode = mode;
        inner.status = Status {
            model: cfg.model.clone(),
            server: cfg.url.clone(),
            root: cfg.root.display().to_string(),
            ctx_tokens: cfg.ctx_tokens,
            usage_tokens: 0,
            messages: 0,
            stats: String::new(),
        };
    }

    let bind = std::env::var("NAH_GUI_BIND").unwrap_or_else(|_| "127.0.0.1".to_string());
    let listener = TcpListener::bind((bind.as_str(), port))
        .map_err(|e| CoreError::Tool(format!("cannot bind GUI server on port {port}: {e}")))?;
    let url = format!("http://127.0.0.1:{port}");
    println!("nah GUI listening at {url}");
    open_browser(&url);

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let engine = Arc::clone(&engine);
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            handle_client(stream, engine, state);
        });
    }
    Ok(())
}

/// Best-effort browser launch; a failure is never fatal.
fn open_browser(url: &str) {
    #[cfg(windows)]
    let ok = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .status()
        .is_ok();
    #[cfg(target_os = "macos")]
    let ok = std::process::Command::new("open").arg(url).status().is_ok();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let ok = std::process::Command::new("xdg-open")
        .arg(url)
        .status()
        .is_ok();
    if !ok {
        eprintln!("(could not open a browser automatically - open {url} manually)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_field_reads_strings_numbers_and_bools() {
        let body = r#"{"id":3,"allow":true,"text":"hi\nthere"}"#;
        assert_eq!(json_field(body, "id").as_deref(), Some("3"));
        assert_eq!(json_field(body, "allow").as_deref(), Some("true"));
        assert_eq!(json_field(body, "text").as_deref(), Some("hi\nthere"));
        assert_eq!(json_field(body, "missing"), None);
    }

    #[test]
    fn esc_makes_json_safe() {
        assert_eq!(esc("a\"b"), "a\\\"b");
        assert_eq!(esc("x\ny"), "x\\ny");
        assert!(!esc("\u{1}").contains('\u{1}'));
    }

    #[test]
    fn state_json_is_well_formed() {
        let state = UiState::new();
        let json = state_json(&state);
        assert!(json.contains("\"mode\":"));
        assert!(json.ends_with(",\"pending\":[]}"));
        // answer for an unknown id is rejected
        assert!(!state.answer(99, true));
    }

    #[test]
    fn ask_blocks_until_answered() {
        let state = Arc::new(UiState::new());
        let req = ConfirmRequest {
            verb: "WRITE_FILE".into(),
            path: "x".into(),
            preview: String::new(),
            is_new_file: true,
        };
        let worker = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || state.ask(&req))
        };
        // wait until it registers
        let id = loop {
            let inner = state.inner.lock().unwrap();
            if let Some(p) = inner.pending.first() {
                break p.id;
            }
            drop(inner);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(state.answer(id, true));
        assert!(worker.join().unwrap());
    }
}
