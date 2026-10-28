//! `nah` — terminal front-end.

mod repl;
mod ui;

use nah_core::config;
use nah_core::error::CoreError;
use nah_core::provider;

use ui::Ui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // one-shot mode: -p "prompt"
    let mut prompt: Option<String> = None;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-p" || args[i] == "--prompt" {
            i += 1;
            prompt = args.get(i).cloned();
            if prompt.is_none() {
                eprintln!("گزینه‌ی -p به متن نیاز دارد");
                std::process::exit(2);
            }
        } else {
            filtered.push(args[i].clone());
        }
        i += 1;
    }

    let cfg = match config::from_args(&filtered) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => return, // --help / --version
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    let ui = Ui::new(cfg.color);

    let provider = match provider::connect(cfg.provider, &cfg.url) {
        Ok(p) => p,
        Err(e) => {
            ui.error(&format!(
                "{e}\nمطمئن شوید سرور مدل اجراست (مثلاً `ollama serve`) یا --url درست است."
            ));
            std::process::exit(1);
        }
    };

    if let Some(p) = prompt {
        if let Err(e) = repl::run_oneshot(&ui, &cfg, provider, &p) {
            ui.error(&e.to_string());
            std::process::exit(1);
        }
        return;
    }

    let mut r = match repl::Repl::new(ui, provider, cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = r.run() {
        r.ui.error(&e.to_string());
        std::process::exit(1);
    }
}

// silence unused warning for CoreError in some cfgs
#[allow(dead_code)]
fn _e(_: CoreError) {}
