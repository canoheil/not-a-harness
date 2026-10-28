//! Ollama provider: `POST /api/chat` (NDJSON stream) + `GET /api/tags`.

use crate::error::CoreError;
use crate::http;
use crate::json::Json;

use super::{ChatMessage, ChatOptions, ChatRequest, LlmProvider, ModelInfo};

pub struct OllamaProvider {
    base: String,
}

impl OllamaProvider {
    pub fn new(base_url: &str) -> Result<Self, CoreError> {
        let base = base_url.trim().trim_end_matches('/').to_string();
        // validate eagerly so bad URLs fail at startup
        http::parse_url(&format!("{base}/api/tags"))?;
        Ok(OllamaProvider { base })
    }
}

fn messages_to_json(messages: &[ChatMessage], options: &ChatOptions) -> Json {
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
        (
            "options".into(),
            Json::Obj(vec![
                ("num_ctx".into(), Json::num(options.num_ctx as f64)),
                ("temperature".into(), Json::num(options.temperature as f64)),
            ]),
        ),
    ])
}

fn format_size(bytes: f64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", UNITS[i])
}

impl LlmProvider for OllamaProvider {
    fn list_models(&mut self) -> Result<Vec<ModelInfo>, CoreError> {
        let body = http::get_string(&format!("{}/api/tags", self.base))?;
        let parsed = Json::parse(&body).map_err(|e| CoreError::Json(e.to_string()))?;
        let mut out = Vec::new();
        if let Some(models) = parsed.get("models").and_then(|m| m.as_arr()) {
            for m in models {
                let name = m
                    .get("name")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.get("model").and_then(|v| v.as_str()))
                    .unwrap_or("?")
                    .to_string();
                let size = m.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let details = m.get("details");
                let quant = details
                    .and_then(|d| d.get("quantization_level"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let params = details
                    .and_then(|d| d.get("parameter_size"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let mut parts = vec![format_size(size)];
                if !params.is_empty() {
                    parts.push(params.to_string());
                }
                if !quant.is_empty() {
                    parts.push(quant.to_string());
                }
                out.push(ModelInfo {
                    name,
                    detail: parts.join(" · "),
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
        let mut payload = messages_to_json(req.messages, req.options);
        // set model name
        if let Json::Obj(entries) = &mut payload {
            entries.insert(0, ("model".into(), Json::str(req.model)));
        }
        let body = payload.to_string();

        let (status, lines) = http::post_json_stream(&format!("{}/api/chat", self.base), &body)?;
        if status == 404 {
            return Err(CoreError::Provider(
                "the server has no /api/chat - if this is llama.cpp, LM Studio or vLLM, \
                 use provider 'openai' instead (nah --setup)"
                    .to_string(),
            ));
        }
        if status != 200 {
            return Err(CoreError::Provider(format!(
                "/api/chat returned HTTP {status}"
            )));
        }

        let mut full = String::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let parsed = Json::parse(&line)
                .map_err(|e| CoreError::Json(format!("bad NDJSON from Ollama: {e}")))?;
            if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
                return Err(CoreError::Provider(format!("Ollama error: {err}")));
            }
            if let Some(delta) = parsed
                .get("message")
                .and_then(|m| m.get("content"))
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
