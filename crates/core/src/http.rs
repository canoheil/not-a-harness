//! Minimal blocking HTTP/1.1 client over std::net — no dependencies.
//!
//! Scope (deliberate, documented in README):
//! * `http://` only. Local inference servers (Ollama, llama.cpp, LM Studio)
//!   speak plain HTTP on localhost, which is what this harness targets.
//! * `Connection: close` per request keeps parsing trivial and predictable.
//! * Streaming body reads (chunked or until-EOF) so the UI can render
//!   tokens as the model produces them (NDJSON / SSE).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::error::CoreError;

#[derive(Debug, Clone)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(input: &str) -> Result<Url, CoreError> {
    let input = input.trim().trim_end_matches('/');
    let (scheme, rest) = match input.split_once("://") {
        Some(pair) => pair,
        None => ("http", input),
    };
    if scheme != "http" {
        return Err(CoreError::Http(format!(
            "only http:// is supported in v1 (got '{scheme}://'). \
             Local servers like Ollama/llama.cpp work over plain HTTP."
        )));
    }
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| CoreError::Http(format!("bad port in '{input}'")))?,
        ),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err(CoreError::Http("empty host".into()));
    }
    Ok(Url { host, port, path })
}

pub struct Request {
    pub method: &'static str,
    pub url: Url,
    pub content_type: Option<String>,
    pub body: Option<String>,
}

pub struct Response {
    pub status: u16,
    reader: BodyReader,
}

impl Response {
    /// Read the whole body into a String (dechunking if needed).
    pub fn read_all(&mut self) -> Result<String, CoreError> {
        let mut buf = String::new();
        self.reader
            .read_to_string(&mut buf)
            .map_err(|e| CoreError::Http(format!("read body: {e}")))?;
        Ok(buf)
    }

    /// Stream the body line by line (for NDJSON / SSE).
    pub fn lines(self) -> ResponseLines {
        ResponseLines {
            inner: BufReader::new(self.reader),
        }
    }
}

pub struct ResponseLines {
    inner: BufReader<BodyReader>,
}

impl Iterator for ResponseLines {
    type Item = Result<String, CoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = String::new();
        match self.inner.read_line(&mut line) {
            Ok(0) => None,
            Ok(_) => {
                while line.ends_with('\n') || line.ends_with('\r') {
                    line.pop();
                }
                Some(Ok(line))
            }
            Err(e) => Some(Err(CoreError::Http(format!("stream read: {e}")))),
        }
    }
}

pub fn send(req: Request) -> Result<Response, CoreError> {
    let addr = format!("{}:{}", req.url.host, req.url.port);
    let stream = TcpStream::connect(&addr)
        .map_err(|e| CoreError::Http(format!("connect to {addr}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_secs(600))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(60))).ok();

    let body = req.body.unwrap_or_default();
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\n",
        req.method, req.url.path, req.url.host
    );
    if let Some(ct) = &req.content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));

    let mut stream = stream;
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|e| CoreError::Http(format!("send request: {e}")))?;

    // ---- read status line + headers ----
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| CoreError::Http(format!("read status: {e}")))?;
    if status_line.is_empty() {
        return Err(CoreError::Http("empty response from server".into()));
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| CoreError::Http(format!("bad status line: {status_line:?}")))?;

    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| CoreError::Http(format!("read header: {e}")))?;
        if n <= 2 {
            break; // blank line = end of headers
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        } else if lower.starts_with("content-length:") {
            content_length = lower
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse::<usize>().ok());
        }
    }

    let body_reader = if chunked {
        BodyReader::Chunked {
            reader,
            remaining_in_chunk: 0,
            done: false,
        }
    } else if let Some(len) = content_length {
        BodyReader::ContentLength {
            reader,
            remaining: len,
        }
    } else {
        BodyReader::UntilEof { reader }
    };

    Ok(Response {
        status,
        reader: body_reader,
    })
}

/// GET helper returning the full body; non-2xx becomes an error with body excerpt.
pub fn get_string(url: &str) -> Result<String, CoreError> {
    let url = parse_url(url)?;
    let mut resp = send(Request {
        method: "GET",
        url,
        content_type: None,
        body: None,
    })?;
    let body = resp.read_all()?;
    if !(200..300).contains(&resp.status) {
        return Err(CoreError::Http(format!(
            "GET failed with status {}: {}",
            resp.status,
            body.chars().take(300).collect::<String>()
        )));
    }
    Ok(body)
}

/// POST JSON helper returning (status, body).
pub fn post_json(url: &str, body: &str) -> Result<(u16, String), CoreError> {
    let url = parse_url(url)?;
    let mut resp = send(Request {
        method: "POST",
        url,
        content_type: Some("application/json".to_string()),
        body: Some(body.to_string()),
    })?;
    let text = resp.read_all()?;
    Ok((resp.status, text))
}

/// POST JSON that streams its response body; lines are yielded as they arrive.
pub fn post_json_stream(url: &str, body: &str) -> Result<(u16, ResponseLines), CoreError> {
    let url = parse_url(url)?;
    let resp = send(Request {
        method: "POST",
        url,
        content_type: Some("application/json".to_string()),
        body: Some(body.to_string()),
    })?;
    Ok((resp.status, resp.lines()))
}

// ---------------------------------------------------------------------------
// Body decoding
// ---------------------------------------------------------------------------

enum BodyReader {
    ContentLength {
        reader: BufReader<TcpStream>,
        remaining: usize,
    },
    Chunked {
        reader: BufReader<TcpStream>,
        remaining_in_chunk: usize,
        done: bool,
    },
    UntilEof {
        reader: BufReader<TcpStream>,
    },
}

impl Read for BodyReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            BodyReader::ContentLength { reader, remaining } => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let want = buf.len().min(*remaining);
                let n = reader.read(&mut buf[..want])?;
                *remaining -= n;
                Ok(n)
            }
            BodyReader::UntilEof { reader } => reader.read(buf),
            BodyReader::Chunked {
                reader,
                remaining_in_chunk,
                done,
            } => {
                loop {
                    if *done {
                        return Ok(0);
                    }
                    if *remaining_in_chunk == 0 {
                        // read next chunk-size line (and skip trailers after last chunk)
                        let mut line = String::new();
                        let n = reader.read_line(&mut line)?;
                        if n == 0 {
                            *done = true;
                            return Ok(0);
                        }
                        let size_str = line.split(';').next().unwrap_or("").trim();
                        let size = usize::from_str_radix(size_str, 16).unwrap_or(0);
                        if size == 0 {
                            // consume trailer lines until blank
                            loop {
                                let mut t = String::new();
                                let m = reader.read_line(&mut t)?;
                                if m <= 2 {
                                    break;
                                }
                            }
                            *done = true;
                            return Ok(0);
                        }
                        *remaining_in_chunk = size;
                    }
                    let want = buf.len().min(*remaining_in_chunk);
                    let n = reader.read(&mut buf[..want])?;
                    *remaining_in_chunk -= n;
                    if *remaining_in_chunk == 0 {
                        // consume trailing CRLF of this chunk
                        let mut crlf = [0u8; 2];
                        let mut got = 0;
                        while got < 2 {
                            match reader.read(&mut crlf[got..])? {
                                0 => break,
                                k => got += k,
                            }
                        }
                    }
                    if n > 0 {
                        return Ok(n);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_urls() {
        let u = parse_url("http://localhost:11434/api/chat").unwrap();
        assert_eq!(u.host, "localhost");
        assert_eq!(u.port, 11434);
        assert_eq!(u.path, "/api/chat");

        let u = parse_url("http://127.0.0.1:8080").unwrap();
        assert_eq!(u.path, "/");

        let u = parse_url("127.0.0.1:8080/x").unwrap();
        assert_eq!(u.port, 8080);

        assert!(parse_url("https://example.com").is_err());
        assert!(parse_url("http://host:notaport").is_err());
    }
}
