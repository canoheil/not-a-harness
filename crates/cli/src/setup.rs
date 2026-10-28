//! First-run setup wizard: pick a provider, enter its URL, test the
//! connection, list available models and choose a default.

use nah_core::config::{self, NahConfig};
use nah_core::error::CoreError;
use nah_core::provider::{self, LlmProvider, ProviderKind};

use crate::ui::{flush, Ui};

pub fn run(ui: &Ui, cfg: &mut NahConfig) -> Result<(), CoreError> {
    println!("\n== nah setup ==\n");
    println!("Connect nah to your local model server.\n");

    // ---- 1) provider type ----
    loop {
        let def = cfg.provider.as_str();
        let answer = ask(&format!(
            "Provider type - [1] ollama  [2] openai-compatible (llama.cpp, LM Studio, vLLM) [{def}]"
        ));
        let picked = match answer.trim().to_ascii_lowercase().as_str() {
            "" => Some(cfg.provider),
            "1" | "ollama" => Some(ProviderKind::Ollama),
            "2" | "openai" | "openai-compatible" | "openai-compat" => {
                Some(ProviderKind::OpenAiCompat)
            }
            _ => None,
        };
        if let Some(p) = picked {
            cfg.provider = p;
            break;
        }
        ui.info("please answer 1, 2, ollama or openai.");
    }

    // ---- 2) server URL (with connection test loop) ----
    let default_url = match cfg.provider {
        ProviderKind::Ollama => "http://localhost:11434",
        ProviderKind::OpenAiCompat => "http://localhost:8080",
    };
    if cfg.url.is_empty()
        || cfg.url == NahConfig::default().url && cfg.provider != ProviderKind::Ollama
    {
        cfg.url = default_url.to_string();
    }

    let mut models: Vec<(String, String)> = Vec::new();
    loop {
        let answer = ask(&format!("Server URL [{default_url}]"));
        if !answer.trim().is_empty() {
            cfg.url = answer.trim().trim_end_matches('/').to_string();
        }

        print!("Connecting to {} ... ", cfg.url);
        flush();
        match probe(cfg) {
            Ok(list) => {
                println!("ok");
                models = list;
                break;
            }
            Err(e) => {
                println!("failed");
                ui.error(&e.to_string());
                ui.info(&hint_for(&e, cfg.provider));
                let again = ask("Try another URL? [Y/n] (n = skip the connection test)");
                if matches!(again.trim().to_ascii_lowercase().as_str(), "n" | "no") {
                    break;
                }
            }
        }
    }

    // ---- 3) choose default model ----
    if !models.is_empty() {
        println!();
        for (i, (name, detail)) in models.iter().enumerate() {
            if detail.is_empty() {
                println!("  [{:2}] {name}", i + 1);
            } else {
                println!("  [{:2}] {name}  ({detail})", i + 1);
            }
        }
        loop {
            let def_idx = models
                .iter()
                .position(|(n, _)| *n == cfg.model)
                .map(|i| i + 1)
                .unwrap_or(1);
            let answer = ask(&format!(
                "Choose default model (number or name) [{def_idx}]"
            ));
            let answer = answer.trim();
            let picked = if answer.is_empty() {
                Some(models[def_idx - 1].0.clone())
            } else if let Ok(n) = answer.parse::<usize>() {
                models.get(n.wrapping_sub(1)).map(|(name, _)| name.clone())
            } else {
                models
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(answer))
                    .map(|(name, _)| name.clone())
            };
            match picked {
                Some(name) => {
                    cfg.model = name;
                    break;
                }
                None => ui.info("pick a number from the list, or type an exact model name."),
            }
        }
    } else {
        // no model list available - ask for a name directly
        let answer = ask("Model name [qwen2.5-coder:7b]");
        if !answer.trim().is_empty() {
            cfg.model = answer.trim().to_string();
        }
    }

    // ---- 4) context budget ----
    loop {
        let answer = ask(&format!("Context window in tokens [{}]", cfg.ctx_tokens));
        if answer.trim().is_empty() {
            break;
        }
        if let Ok(n) = answer.trim().parse::<usize>() {
            if n >= 512 {
                cfg.ctx_tokens = n;
                break;
            }
        }
        ui.info("enter a number >= 512, or press Enter for the default.");
    }

    // ---- 5) save ----
    let path = config::config_path();
    config::save(&path, cfg)?;
    ui.ok(&format!(
        "setup complete - config saved to {}",
        path.display()
    ));
    ui.info(&format!(
        "using provider '{}' at {} with model '{}'",
        cfg.provider.as_str(),
        cfg.url,
        cfg.model
    ));
    println!();
    Ok(())
}

fn probe(cfg: &NahConfig) -> Result<Vec<(String, String)>, CoreError> {
    let mut p: Box<dyn LlmProvider> = provider::connect(cfg.provider, &cfg.url)?;
    let models = p.list_models()?;
    Ok(models.into_iter().map(|m| (m.name, m.detail)).collect())
}

fn hint_for(e: &CoreError, kind: ProviderKind) -> String {
    let text = e.to_string();
    if text.contains("404") {
        match kind {
            ProviderKind::Ollama => "the server answered but has no /api/chat - if this is \
                 llama.cpp, LM Studio or vLLM, choose provider 'openai' instead."
                .to_string(),
            ProviderKind::OpenAiCompat => "the server has no /v1/chat/completions - if this is \
                 Ollama, choose provider 'ollama' instead."
                .to_string(),
        }
    } else if text.contains("connect") {
        "is the server running? (e.g. `ollama serve`, or start llama-server / LM Studio)"
            .to_string()
    } else {
        "check the URL and that the server is running.".to_string()
    }
}

fn ask(question: &str) -> String {
    print!("{question}: ");
    flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if !answer.ends_with('\n') {
        println!();
    }
    answer.trim_end().to_string()
}
