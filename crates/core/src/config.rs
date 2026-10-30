//! Configuration: CLI flags > environment > config files > defaults.
//!
//! Config file locations (later ones override earlier ones):
//!   1. `$NAH_CONFIG` (explicit path)
//!   2. `~/.config/nah/nah.conf` (written by the setup wizard)
//!   3. `./nah.conf` (per-project)

use std::path::{Path, PathBuf};

use crate::context::ContextConfig;
use crate::error::CoreError;
use crate::provider::{ChatOptions, ProviderKind};

#[derive(Debug, Clone)]
pub struct NahConfig {
    pub provider: ProviderKind,
    pub url: String,
    pub model: String,
    pub root: PathBuf,
    pub ctx_tokens: usize,
    pub history_messages: usize,
    pub max_open_files: usize,
    pub max_tool_bytes: usize,
    pub max_steps: usize,
    pub temperature: f32,
    pub auto_compact: bool,
    pub auto_approve: bool,
    pub color: bool,
    pub width: Option<usize>,
    /// Tool names disabled by the user (`/tools off ...`); everything else
    /// stays enabled.
    pub disabled_tools: Vec<String>,
}

impl Default for NahConfig {
    fn default() -> Self {
        NahConfig {
            provider: ProviderKind::Ollama,
            url: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            root: PathBuf::from("."),
            ctx_tokens: 4096,
            history_messages: 5,
            max_open_files: 3,
            max_tool_bytes: 32 * 1024,
            max_steps: 8,
            temperature: 0.2,
            auto_compact: true,
            auto_approve: false,
            color: true,
            width: None,
            disabled_tools: Vec::new(),
        }
    }
}

impl NahConfig {
    pub fn context_config(&self) -> ContextConfig {
        ContextConfig {
            ctx_tokens: self.ctx_tokens,
            history_messages: self.history_messages,
            max_open_files: self.max_open_files,
            auto_compact: self.auto_compact,
        }
    }

    pub fn chat_options(&self) -> ChatOptions {
        ChatOptions {
            num_ctx: self.ctx_tokens as u32,
            temperature: self.temperature,
        }
    }
}

pub const HELP: &str = "\
nah - lean harness for local LLMs (not-a-harness)

Usage:
  nah [options]          interactive REPL
  nah -p \"prompt\"        one-shot (non-interactive)
  nah --setup            run the setup wizard

Options:
  --model <name>         model name (default: qwen2.5-coder:7b)
  --url <url>            server URL (default: http://localhost:11434)
  --provider <kind>      ollama | openai   (default: ollama)
  --root <dir>           project root for file access (default: .)
  --ctx <tokens>         context budget / num_ctx (default: 4096)
  --history <n>          trailing messages always kept (default: 5)
  --steps <n>            max model<->tool round trips per turn (default: 8)
  --temperature <f>      sampling temperature (default: 0.2)
  --yes                  auto-approve file writes/edits and command runs
  --no-color             disable colors
  --no-compact           disable automatic history summarization
  --width <n>            override terminal width for text wrapping
  --version              print version
  --help                 show this help

Settings can also be stored in a config file (KEY = VALUE):
  ~/.config/nah/nah.conf   written by the setup wizard
  ./nah.conf               per-project overrides
or passed via environment: NAH_MODEL, NAH_URL, NAH_PROVIDER, NAH_CONFIG.
";

/// The user-level config path (`$NAH_CONFIG` or `~/.config/nah/nah.conf`).
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("NAH_CONFIG") {
        return PathBuf::from(p);
    }
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        return PathBuf::from(home)
            .join(".config")
            .join("nah")
            .join("nah.conf");
    }
    PathBuf::from("nah.conf")
}

/// The user-level custom tools directory (`~/.config/nah/tools/`).
/// Project-local tool files are intentionally unsupported (supply chain).
pub fn tools_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        return PathBuf::from(home)
            .join(".config")
            .join("nah")
            .join("tools");
    }
    PathBuf::from("nah-tools")
}

/// Is there any config file? (used to decide whether to run first-time setup)
pub fn config_exists() -> bool {
    config_path().exists() || Path::new("nah.conf").exists()
}

/// Persist the wizard result (or any runtime change) to the user config file.
pub fn save(path: &Path, cfg: &NahConfig) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CoreError::Io(format!("cannot create {}: {e}", parent.display())))?;
    }
    let text = format!(
        "# nah configuration (edit or run `nah --setup`)\n\
         provider = {}\n\
         url = \"{}\"\n\
         model = \"{}\"\n\
         ctx = {}\n\
         history = {}\n\
         temperature = {}\n\
         auto_approve = {}\n\
         disabled_tools = \"{}\"\n",
        cfg.provider.as_str(),
        cfg.url,
        cfg.model,
        cfg.ctx_tokens,
        cfg.history_messages,
        cfg.temperature,
        cfg.auto_approve,
        cfg.disabled_tools.join(","),
    );
    std::fs::write(path, text)
        .map_err(|e| CoreError::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(())
}

/// Parse `KEY=VALUE` lines (comments start with `#`).
pub fn load_conf_file(path: &Path, cfg: &mut NahConfig) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.as_str() {
            "provider" => {
                if let Some(p) = ProviderKind::parse(value) {
                    cfg.provider = p;
                }
            }
            "url" => cfg.url = value.to_string(),
            "model" => cfg.model = value.to_string(),
            "root" => cfg.root = PathBuf::from(value),
            "ctx" | "ctx_tokens" => {
                if let Ok(n) = value.parse() {
                    cfg.ctx_tokens = n;
                }
            }
            "history" | "history_messages" => {
                if let Ok(n) = value.parse() {
                    cfg.history_messages = n;
                }
            }
            "steps" | "max_steps" => {
                if let Ok(n) = value.parse() {
                    cfg.max_steps = n;
                }
            }
            "temperature" => {
                if let Ok(f) = value.parse() {
                    cfg.temperature = f;
                }
            }
            "auto_approve" | "yes" => {
                cfg.auto_approve = matches!(value, "1" | "true" | "yes");
            }
            "color" => {
                cfg.color = matches!(value, "1" | "true" | "yes");
            }
            "auto_compact" | "compact" => {
                cfg.auto_compact = matches!(value, "1" | "true" | "yes");
            }
            "disabled_tools" => {
                cfg.disabled_tools = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
}

/// Build config from argv + environment + config files.
/// Returns `Ok(None)` when the process should exit (help/version).
pub fn from_args(args: &[String]) -> Result<Option<NahConfig>, CoreError> {
    let mut cfg = NahConfig::default();

    // environment
    if let Ok(v) = std::env::var("NAH_MODEL") {
        cfg.model = v;
    }
    if let Ok(v) = std::env::var("NAH_URL") {
        cfg.url = v;
    }
    if let Ok(v) = std::env::var("NAH_PROVIDER") {
        if let Some(p) = ProviderKind::parse(&v) {
            cfg.provider = p;
        }
    }

    // config files (user-level first, per-project overrides)
    load_conf_file(&config_path(), &mut cfg);
    load_conf_file(Path::new("nah.conf"), &mut cfg);

    // CLI flags win
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut take_value = |name: &str| -> Result<String, CoreError> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| CoreError::Config(format!("option {name} needs a value")))
        };
        match arg {
            "--help" | "-h" | "help" => {
                println!("{HELP}");
                return Ok(None);
            }
            "--version" | "-V" => {
                println!("nah {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--model" | "-m" => cfg.model = take_value("--model")?,
            "--url" => cfg.url = take_value("--url")?,
            "--provider" => {
                let v = take_value("--provider")?;
                cfg.provider = ProviderKind::parse(&v).ok_or_else(|| {
                    CoreError::Config(format!("unknown provider '{v}' (use ollama or openai)"))
                })?;
            }
            "--root" => cfg.root = PathBuf::from(take_value("--root")?),
            "--ctx" => {
                cfg.ctx_tokens = take_value("--ctx")?
                    .parse()
                    .map_err(|_| CoreError::Config("--ctx must be a number".into()))?;
            }
            "--history" => {
                cfg.history_messages = take_value("--history")?
                    .parse()
                    .map_err(|_| CoreError::Config("--history must be a number".into()))?;
            }
            "--steps" => {
                cfg.max_steps = take_value("--steps")?
                    .parse()
                    .map_err(|_| CoreError::Config("--steps must be a number".into()))?;
            }
            "--temperature" => {
                cfg.temperature = take_value("--temperature")?
                    .parse()
                    .map_err(|_| CoreError::Config("--temperature must be a number".into()))?;
            }
            "--yes" | "-y" => cfg.auto_approve = true,
            "--no-color" => cfg.color = false,
            "--no-compact" => cfg.auto_compact = false,
            "--width" => {
                cfg.width = Some(
                    take_value("--width")?
                        .parse()
                        .map_err(|_| CoreError::Config("--width must be a number".into()))?,
                );
            }
            other => {
                return Err(CoreError::Config(format!(
                    "unknown option '{other}' (try --help)"
                )));
            }
        }
        i += 1;
    }

    if std::env::var("NO_COLOR").is_ok() {
        cfg.color = false;
    }

    // normalize root to an absolute path
    cfg.root = cfg
        .root
        .canonicalize()
        .map_err(|e| CoreError::Config(format!("invalid project root: {e}")))?;

    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse() {
        let args: Vec<String> = ["--model", "m1", "--ctx", "2048", "--yes"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cfg = from_args(&args).unwrap().unwrap();
        assert_eq!(cfg.model, "m1");
        assert_eq!(cfg.ctx_tokens, 2048);
        assert!(cfg.auto_approve);
    }

    #[test]
    fn unknown_flag_errors() {
        let args: Vec<String> = ["--bogus"].iter().map(|s| s.to_string()).collect();
        assert!(from_args(&args).is_err());
    }

    #[test]
    fn conf_file_keys() {
        let dir = std::env::temp_dir().join("nah_test_conf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("nah.conf"),
            "# comment\nmodel = \"abc:1b\"\nhistory = 7\nauto_approve = true\n",
        )
        .unwrap();
        let mut cfg = NahConfig::default();
        load_conf_file(&dir.join("nah.conf"), &mut cfg);
        assert_eq!(cfg.model, "abc:1b");
        assert_eq!(cfg.history_messages, 7);
        assert!(cfg.auto_approve);
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join("nah_test_save");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sub").join("nah.conf");
        let cfg = NahConfig {
            provider: ProviderKind::OpenAiCompat,
            url: "http://localhost:9999".into(),
            model: "test-model:3b".into(),
            ctx_tokens: 2048,
            ..Default::default()
        };
        save(&path, &cfg).unwrap();

        let mut loaded = NahConfig::default();
        load_conf_file(&path, &mut loaded);
        assert_eq!(loaded.provider, ProviderKind::OpenAiCompat);
        assert_eq!(loaded.url, "http://localhost:9999");
        assert_eq!(loaded.model, "test-model:3b");
        assert_eq!(loaded.ctx_tokens, 2048);
    }

    #[test]
    fn disabled_tools_roundtrip() {
        let dir = std::env::temp_dir().join("nah_cfg_tools_t1");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nah.conf");
        let mut cfg = NahConfig {
            disabled_tools: vec!["LIST_DIR".to_string(), "RUN_COMMAND".to_string()],
            ..Default::default()
        };
        save(&path, &cfg).unwrap();
        let mut loaded = NahConfig::default();
        load_conf_file(&path, &mut loaded);
        assert_eq!(loaded.disabled_tools, vec!["LIST_DIR", "RUN_COMMAND"]);
        // empty list survives too
        cfg.disabled_tools.clear();
        save(&path, &cfg).unwrap();
        let mut loaded = NahConfig {
            disabled_tools: vec!["X".to_string()],
            ..Default::default()
        };
        load_conf_file(&path, &mut loaded);
        assert!(loaded.disabled_tools.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
