//! `nah-core` — the engine behind `nah` (not-a-harness).
//!
//! A dependency-free (std-only) agent harness for local LLMs:
//! * providers for Ollama and any OpenAI-compatible local server;
//! * a *text* tool protocol (models emit fenced blocks, not JSON);
//! * a context manager that fits a 4K window (working set + rolling summary);
//! * an agent loop with confirmation-gated file writes.

pub mod config;
pub mod context;
pub mod error;
pub mod http;
pub mod json;
pub mod protocol;
pub mod provider;
pub mod session;
pub mod tools;
