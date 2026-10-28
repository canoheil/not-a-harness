//! OpenAI-compatible provider (`/v1/chat/completions`, SSE stream).
//!
//! Works with llama.cpp's `llama-server`, LM Studio, vLLM, text-generation-webui
//! and anything else that speaks the OpenAI HTTP dialect over plain HTTP.

use crate::error::CoreError;
use crate::http;
use crate::json::Json;

use super::{ChatMessage, ChatRequest, LlmProvider, ModelInfo};

pub struct OpenAiCompatProvider {
    base: String,
}

impl OpenAiCompatProvider {
    pub fn new(base_url: &str) -> Result<Self, CoreError> {
        let base = base_url.trim().trim_end_matches('/').to_string();
        http::parse_url(&format!("{base}/v1/models"))?;
        Ok(OpenAiCompatProvider { base })
    }
}

fn messages_to_json(messages: &[ChatMessage], temperature: f32) -> Json {
    let msgs = Json::Arr(
        messages
            .iter()
            .map(|m| {
                Json::Obj(vec![
                    ("role".into(), Json::str(m.role.as_str())),
                    ("content".into(), Json::str(&m.content)),
                ])
            })
            .collect(),
    );
    Json::Obj(vec![
        ("messages".into(), msgs),
        ("stream".into(), Json::Bool(true)),
        ("temperature".into(), Json::num(temperature as f64)),
    ])
}

impl LlmProvider for OpenAiCompatProvider {
    fn list_models(&mut self) -> Result<Vec<ModelInfo>, CoreError> {
        let body = http::get_string(&format!("{}/v1/models", self.base))?;
        let parsed = Json::parse(&body).map_err(|e| CoreError::Json(e.to_string()))?;
        let mut out = Vec::new();
        if let Some(items) = parsed.get("data").and_then(|d| d.as_arr()) {
            for m in items {
                let name = m
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                out.push(ModelInfo {
                    name,
                    detail: String::new(),
                });
            }
        }
        Ok(out)
    }

    fn chat_stream(
        &mut self,
        req: &ChatRequest<'_>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String, CoreError> {
        let mut payload = messages_to_json(req.messages, req.options.temperature);
        if let Json::Obj(entries) = &mut payload {
            entries.insert(0, ("model".into(), Json::str(req.model)));
        }
        let body = payload.to_string();

        let (status, lines) =
            http::post_json_stream(&format!("{}/v1/chat/completions", self.base), &body)?;
        if status == 404 {
            return Err(CoreError::Provider(
                "the server has no /v1/chat/completions - if this is Ollama, \
                 use provider 'ollama' instead (nah --setup)"
                    .to_string(),
            ));
        }
        if status != 200 {
            return Err(CoreError::Provider(format!(
                "/v1/chat/completions returned HTTP {status}"
            )));
        }

        let mut full = String::new();
        for line in lines {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line == "data: [DONE]" {
                break;
            }
            let payload = match line.strip_prefix("data: ") {
                Some(p) => p,
                None => {
                    // some servers emit bare JSON lines instead of SSE
                    if line.starts_with('{') {
                        line
                    } else {
                        continue;
                    }
                }
            };
            let parsed =
                Json::parse(payload).map_err(|e| CoreError::Json(format!("bad SSE chunk: {e}")))?;
            if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
                return Err(CoreError::Provider(format!("server error: {err}")));
            }
            if let Some(delta) = parsed
                .get("choices")
                .and_then(|c| c.as_arr())
                .and_then(|c| c.first())
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !delta.is_empty() {
                    full.push_str(delta);
                    on_delta(delta);
                }
            }
        }
        Ok(full)
    }
}
