//! First-run setup wizard: discover local providers (or enter one by
//! hand), test the connection, list available models and choose a default.

use nah_core::config::{self, NahConfig};
use nah_core::discovery;
use nah_core::error::CoreError;
use nah_core::provider::{self, LlmProvider, ProviderKind};

use crate::ui::{flush, Ui};

pub fn run(ui: &Ui, cfg: &mut NahConfig) -> Result<(), CoreError> {
    println!("\n== nah setup ==\n");
    println!("Connect nah to your local model server.\n");

    // ---- 0-2) discovery / connection details / connection test ----
    let models = configure_connection(ui, cfg)?;

    // ---- 3) choose default model ----
    choose_model(ui, cfg, &models);

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

/// Discover local model servers or ask for the connection details, then
/// test the connection. Returns the models the server offers.
///
/// Shared by the setup wizard and the `/provider` REPL command.
pub fn configure_connection(
    ui: &Ui,
    cfg: &mut NahConfig,
) -> Result<Vec<(String, String)>, CoreError> {
    // ---- 0) auto-discovery of local providers ----
    let mut from_discovery = false;
    print!("scanning for local providers... ");
    flush();
    let found = discovery::discover();
    match found.len() {
        0 => {
            println!();
            ui.info("no local providers found - enter the connection details manually.");
        }
        1 => {
            let d = &found[0];
            println!();
            println!(
                "found 1 active service: {} (port {}) - using it",
                d.name, d.port
            );
            cfg.provider = d.kind;
            cfg.url = d.base_url();
            from_discovery = true;
        }
        n => {
            println!();
            println!("found {n} active services:");
            for (i, d) in found.iter().enumerate() {
                println!("  [{}] {} (port {})", i + 1, d.name, d.port);
            }
            println!("  [{}] enter URL manually", n + 1);
            loop {
                let answer = ask(&format!("select a provider to connect (1-{})", n + 1));
                match parse_selection(&answer, n) {
                    Some(Selection::Provider(i)) => {
                        cfg.provider = found[i].kind;
                        cfg.url = found[i].base_url();
                        from_discovery = true;
                        break;
                    }
                    Some(Selection::Manual) => break,
                    None => ui.info("pick a number from the list."),
                }
            }
        }
    }

    // ---- 1) provider type + default URL (manual path only) ----
    if !from_discovery {
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
    }

    // ---- 2) server URL (with connection test loop) ----
    let default_url = match cfg.provider {
        ProviderKind::Ollama => "http://localhost:11434",
        ProviderKind::OpenAiCompat => "http://localhost:8080",
    };
    if !from_discovery
        && (cfg.url.is_empty()
            || cfg.url == NahConfig::default().url && cfg.provider != ProviderKind::Ollama)
    {
        cfg.url = default_url.to_string();
    }

    let mut models: Vec<(String, String)> = Vec::new();
    let mut ask_url = !from_discovery;
    loop {
        if ask_url {
            let answer = ask(&format!("Server URL [{default_url}]"));
            if !answer.trim().is_empty() {
                cfg.url = answer.trim().trim_end_matches('/').to_string();
            }
        }
        ask_url = true; // retries always re-ask the URL

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
    Ok(models)
}

/// Choose the default model from a server listing (or ask for a name).
/// Shared by the setup wizard and the `/provider` REPL command.
pub fn choose_model(ui: &Ui, cfg: &mut NahConfig, models: &[(String, String)]) {
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
}

/// The discovery list choice: a discovered provider, or manual entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Provider(usize),
    Manual,
}

fn parse_selection(input: &str, count: usize) -> Option<Selection> {
    let t = input.trim().to_ascii_lowercase();
    if t.is_empty() {
        return Some(Selection::Provider(0));
    }
    if t == "m" || t == "manual" {
        return Some(Selection::Manual);
    }
    let n: usize = t.parse().ok()?;
    if n >= 1 && n <= count {
        Some(Selection::Provider(n - 1))
    } else if n == count + 1 {
        Some(Selection::Manual)
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_by_number() {
        assert_eq!(parse_selection("1", 2), Some(Selection::Provider(0)));
        assert_eq!(parse_selection("2", 2), Some(Selection::Provider(1)));
        assert_eq!(parse_selection("3", 2), Some(Selection::Manual));
    }

    #[test]
    fn selection_default_and_manual() {
        assert_eq!(parse_selection("", 2), Some(Selection::Provider(0)));
        assert_eq!(parse_selection("manual", 2), Some(Selection::Manual));
        assert_eq!(parse_selection("m", 2), Some(Selection::Manual));
    }

    #[test]
    fn selection_out_of_range_is_rejected() {
        assert_eq!(parse_selection("0", 2), None);
        assert_eq!(parse_selection("4", 2), None);
        assert_eq!(parse_selection("abc", 2), None);
    }
}
