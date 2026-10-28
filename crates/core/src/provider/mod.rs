//! Provider abstraction: talk to a local inference server and stream text back.

pub mod ollama;
pub mod openai_compat;

use crate::error::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage {
            role: Role::System,
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: Role::User,
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    /// Human-readable size summary, e.g. "4.7 GB · Q4_K_M · 7.6B".
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ChatOptions {
    /// Context window passed to the server (Ollama `options.num_ctx`).
    pub num_ctx: u32,
    pub temperature: f32,
}

impl Default for ChatOptions {
    fn default() -> Self {
        ChatOptions {
            num_ctx: 4096,
            temperature: 0.2,
        }
    }
}

pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: &'a [ChatMessage],
    pub options: &'a ChatOptions,
}

/// A local model backend (Ollama, llama.cpp server, ...).
pub trait LlmProvider {
    /// Which models does the server expose?
    fn list_models(&mut self) -> Result<Vec<ModelInfo>, CoreError>;

    /// Run one chat completion, calling `on_delta` with each text fragment
    /// as it streams in. Returns the full assembled text.
    fn chat_stream(
        &mut self,
        req: &ChatRequest<'_>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CoreError>;
}

/// Pick a provider implementation by name / URL heuristic.
pub fn connect(kind: ProviderKind, base_url: &str) -> Result<Box<dyn LlmProvider>, CoreError> {
    match kind {
        ProviderKind::Ollama => Ok(Box::new(ollama::OllamaProvider::new(base_url)?)),
        ProviderKind::OpenAiCompat => Ok(Box::new(openai_compat::OpenAiCompatProvider::new(
            base_url,
        )?)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Ollama,
    OpenAiCompat,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Option<ProviderKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ollama" => Some(ProviderKind::Ollama),
            "openai" | "openai-compat" | "llama.cpp" | "llamacpp" => {
                Some(ProviderKind::OpenAiCompat)
            }
            _ => None,
        }
    }
}
