//! Shared error type for the core engine.

use std::fmt;

#[derive(Debug)]
pub enum CoreError {
    Http(String),
    Json(String),
    Provider(String),
    Tool(String),
    Io(String),
    Config(String),
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::Http(m) => write!(f, "HTTP error: {m}"),
            CoreError::Json(m) => write!(f, "JSON error: {m}"),
            CoreError::Provider(m) => write!(f, "Provider error: {m}"),
            CoreError::Tool(m) => write!(f, "Tool error: {m}"),
            CoreError::Io(m) => write!(f, "I/O error: {m}"),
            CoreError::Config(m) => write!(f, "Config error: {m}"),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io(e.to_string())
    }
}
