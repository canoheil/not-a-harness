//! Configuration: CLI flags > environment > `nah.conf` key=value file > defaults.

use std::path::PathBuf;

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
nah — هارنس سبک مدل‌های لوکال (not-a-harness)

استفاده:
  nah [گزینه‌ها]

گزینه‌ها:
  --model <name>       نام مدل (پیش‌فرض: qwen2.5-coder:7b)
  --url <url>          آدرس سرور (پیش‌فرض: http://localhost:11434)
  --provider <kind>    ollama | openai  (پیش‌فرض: ollama)
  --root <dir>         ریشه‌ی پروژه برای دسترسی فایل‌ها (پیش‌فرض: .)
  --ctx <tokens>       بودجه‌ی کانتکست / num_ctx (پیش‌فرض: 4096)
  --history <n>        تعداد پیام‌های آخر که همیشه فرستاده می‌شوند (پیش‌فرض: 5)
  --steps <n>          حداکثر چرخه‌ی مدل↔ابزار در هر نوبت (پیش‌فرض: 8)
  --temperature <f>    دمای نمونه‌برداری (پیش‌فرض: 0.2)
  --yes                تأیید خودکار نوشتن/ویرایش فایل
  --no-color           غیرفعال کردن رنگ‌ها
  --no-compact         غیرفعال کردن خلاصه‌سازی خودکار
  --version            نسخه
  --help               این راهنما

همچنین می‌توانید کلیدهای بالا را در فایل nah.conf کنار برنامه یا در
NAH_MODEL / NAH_URL / NAH_PROVIDER تنظیم کنید.
";

/// Parse `KEY=VALUE` lines (comments start with `#`).
fn load_conf_file(path: &std::path::Path, cfg: &mut NahConfig) {
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
            _ => {}
        }
    }
}

/// Build config from argv + environment + config file.
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

    // config file next to the executable, then in cwd
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            load_conf_file(&dir.join("nah.conf"), &mut cfg);
        }
    }
    load_conf_file(std::path::Path::new("nah.conf"), &mut cfg);

    // CLI flags win
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let mut take_value = |name: &str| -> Result<String, CoreError> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| CoreError::Config(format!("گزینه‌ی {name} به مقدار نیاز دارد")))
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
                    CoreError::Config(format!("provider ناشناخته: '{v}' (ollama یا openai)"))
                })?;
            }
            "--root" => cfg.root = PathBuf::from(take_value("--root")?),
            "--ctx" => {
                cfg.ctx_tokens = take_value("--ctx")?
                    .parse()
                    .map_err(|_| CoreError::Config("--ctx باید عدد باشد".into()))?;
            }
            "--history" => {
                cfg.history_messages = take_value("--history")?
                    .parse()
                    .map_err(|_| CoreError::Config("--history باید عدد باشد".into()))?;
            }
            "--steps" => {
                cfg.max_steps = take_value("--steps")?
                    .parse()
                    .map_err(|_| CoreError::Config("--steps باید عدد باشد".into()))?;
            }
            "--temperature" => {
                cfg.temperature = take_value("--temperature")?
                    .parse()
                    .map_err(|_| CoreError::Config("--temperature باید عدد باشد".into()))?;
            }
            "--yes" | "-y" => cfg.auto_approve = true,
            "--no-color" => cfg.color = false,
            "--no-compact" => cfg.auto_compact = false,
            other => {
                return Err(CoreError::Config(format!(
                    "گزینه‌ی ناشناخته: '{other}' (برای راهنما: --help)"
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
        .map_err(|e| CoreError::Config(format!("ریشه‌ی پروژه معتبر نیست: {e}")))?;

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
}
