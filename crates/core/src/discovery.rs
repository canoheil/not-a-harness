//! Auto-discovery of local model servers (Ollama, LM Studio, llama.cpp, ...).
//!
//! Scans a handful of well-known localhost ports with a short
//! `connect_timeout`, then *verifies* the identity of anything that answers
//! with a raw HTTP GET: `/api/tags` (Ollama) or `/v1/models` (OpenAI
//! compatible). An open port alone is never trusted - dev servers and
//! databases listen on these ports too.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::provider::ProviderKind;

/// Per-port connection budget. Loopback refuses closed ports instantly;
/// this cap only matters for a rare unresponsive socket.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Budget for the identity probe (local list endpoints answer in ms).
const IO_TIMEOUT: Duration = Duration::from_millis(500);

/// A port worth probing, with the service that usually lives there.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub port: u16,
    pub name: &'static str,
    /// Identity to try first (the usual occupant of the port).
    pub kind: ProviderKind,
}

/// A verified local provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discovered {
    pub name: &'static str,
    pub port: u16,
    pub kind: ProviderKind,
}

impl Discovered {
    /// Base URL to hand to `provider::connect`.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Well-known local ports, in display order.
pub const SCAN_PORTS: &[Candidate] = &[
    Candidate {
        port: 11434,
        name: "ollama",
        kind: ProviderKind::Ollama,
    },
    Candidate {
        port: 1234,
        name: "lm studio",
        kind: ProviderKind::OpenAiCompat,
    },
    Candidate {
        port: 8080,
        name: "llama.cpp",
        kind: ProviderKind::OpenAiCompat,
    },
    Candidate {
        port: 1337,
        name: "jan",
        kind: ProviderKind::OpenAiCompat,
    },
    Candidate {
        port: 8000,
        name: "vllm",
        kind: ProviderKind::OpenAiCompat,
    },
    Candidate {
        port: 5001,
        name: "koboldcpp",
        kind: ProviderKind::OpenAiCompat,
    },
];

/// Scan the well-known ports for local model servers.
pub fn discover() -> Vec<Discovered> {
    scan(SCAN_PORTS)
}

/// Scan an explicit candidate list (kept separate so tests can use
/// throwaway ports).
pub fn scan(ports: &[Candidate]) -> Vec<Discovered> {
    ports.iter().filter_map(probe).collect()
}

fn probe(c: &Candidate) -> Option<Discovered> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), c.port);
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    set_timeouts(&stream);

    // Try the expected identity first, then the other one - the service on
    // a port is not always the one that usually lives there.
    let (first_path, first_kind, second_path, second_kind) = match c.kind {
        ProviderKind::Ollama => (
            "/api/tags",
            ProviderKind::Ollama,
            "/v1/models",
            ProviderKind::OpenAiCompat,
        ),
        ProviderKind::OpenAiCompat => (
            "/v1/models",
            ProviderKind::OpenAiCompat,
            "/api/tags",
            ProviderKind::Ollama,
        ),
    };

    let kind = if http_ok(&mut stream, first_path) {
        Some(first_kind)
    } else {
        // fresh connection: the request above asked for `Connection: close`
        let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
        set_timeouts(&stream);
        http_ok(&mut stream, second_path).then_some(second_kind)
    }?;

    let name = if kind == c.kind {
        c.name
    } else {
        match kind {
            ProviderKind::Ollama => "ollama",
            ProviderKind::OpenAiCompat => "openai-compatible",
        }
    };
    Some(Discovered {
        name,
        port: c.port,
        kind,
    })
}

fn set_timeouts(stream: &TcpStream) {
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
}

/// Send `GET <path>` and report whether the status code is 2xx.
fn http_ok(stream: &mut TcpStream, path: &str) -> bool {
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() || stream.flush().is_err() {
        return false;
    }
    // Loop until the status line is complete (it arrives in one segment on
    // loopback, but be strict anyway).
    let mut buf = [0u8; 256];
    let mut got = 0;
    while got < buf.len() && !buf[..got].contains(&b'\n') {
        match stream.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(_) => return false,
        }
    }
    std::str::from_utf8(&buf[..got])
        .ok()
        .and_then(|text| text.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .is_some_and(|code| (200..300).contains(&code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// Fake local server: answers each connection with the status mapped
    /// for its request path (status 0 = garbage bytes).
    fn spawn(routes: &'static [(&'static str, u16)], conns: usize) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(conns) {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 1024];
                let mut got = 0;
                while got < buf.len() && !buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf[got..]) {
                        Ok(0) => break,
                        Ok(n) => got += n,
                        Err(_) => break,
                    }
                }
                let text = String::from_utf8_lossy(&buf[..got]).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("");
                let status = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, s)| *s)
                    .unwrap_or(404);
                let response = if status == 0 {
                    "not http at all\r\n\r\n".to_string()
                } else {
                    format!("HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (port, handle)
    }

    #[test]
    fn ollama_identity_is_verified() {
        let (port, _h) = spawn(&[("/api/tags", 200)], 1);
        let found = scan(&[Candidate {
            port,
            name: "ollama",
            kind: ProviderKind::Ollama,
        }]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProviderKind::Ollama);
        assert_eq!(found[0].name, "ollama");
        assert_eq!(found[0].port, port);
        assert_eq!(found[0].base_url(), format!("http://127.0.0.1:{port}"));
    }

    #[test]
    fn openai_identity_is_verified() {
        let (port, _h) = spawn(&[("/v1/models", 200)], 1);
        let found = scan(&[Candidate {
            port,
            name: "lm studio",
            kind: ProviderKind::OpenAiCompat,
        }]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProviderKind::OpenAiCompat);
        assert_eq!(found[0].name, "lm studio");
    }

    #[test]
    fn open_port_without_identity_is_rejected() {
        let (port, _h) = spawn(&[("/api/tags", 404), ("/v1/models", 404)], 2);
        let found = scan(&[Candidate {
            port,
            name: "ollama",
            kind: ProviderKind::Ollama,
        }]);
        assert!(found.is_empty());
    }

    #[test]
    fn unexpected_identity_uses_generic_name() {
        // something non-Ollama on the Ollama port: /api/tags fails, /v1/models works
        let (port, _h) = spawn(&[("/api/tags", 404), ("/v1/models", 200)], 2);
        let found = scan(&[Candidate {
            port,
            name: "ollama",
            kind: ProviderKind::Ollama,
        }]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, ProviderKind::OpenAiCompat);
        assert_eq!(found[0].name, "openai-compatible");
    }

    #[test]
    fn garbage_response_is_rejected() {
        let (port, _h) = spawn(&[("/api/tags", 0), ("/v1/models", 0)], 2);
        let found = scan(&[Candidate {
            port,
            name: "ollama",
            kind: ProviderKind::Ollama,
        }]);
        assert!(found.is_empty());
    }

    #[test]
    fn closed_port_is_skipped() {
        // grab a port and free it again - nothing should answer on it
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let found = scan(&[Candidate {
            port,
            name: "ollama",
            kind: ProviderKind::Ollama,
        }]);
        assert!(found.is_empty());
    }

    #[test]
    fn candidates_are_reported_in_order() {
        let (p1, _h1) = spawn(&[("/api/tags", 200)], 1);
        let (p2, _h2) = spawn(&[("/v1/models", 200)], 1);
        let found = scan(&[
            Candidate {
                port: p1,
                name: "ollama",
                kind: ProviderKind::Ollama,
            },
            Candidate {
                port: p2,
                name: "lm studio",
                kind: ProviderKind::OpenAiCompat,
            },
        ]);
        assert_eq!(
            found.iter().map(|d| d.port).collect::<Vec<_>>(),
            vec![p1, p2]
        );
    }
}
