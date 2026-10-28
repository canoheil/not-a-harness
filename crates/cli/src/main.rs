//! `nah` - terminal front-end.

mod repl;
mod setup;
mod ui;

use std::io::IsTerminal;

use nah_core::config;
use nah_core::provider;

use ui::Ui;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // pre-pass: strip flags handled here (-p/--prompt, --setup)
    let mut prompt: Option<String> = None;
    let mut run_setup = false;
    let mut filtered: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--prompt" => {
                i += 1;
                prompt = args.get(i).cloned();
                if prompt.is_none() {
                    eprintln!("option -p needs a value");
                    std::process::exit(2);
                }
            }
            "--setup" => run_setup = true,
            other => filtered.push(other.to_string()),
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
    let mut cfg = cfg;

    // first-run setup: no config yet and the user is in an interactive terminal
    let explicit_connection = filtered
        .iter()
        .any(|a| matches!(a.as_str(), "--model" | "-m" | "--url" | "--provider"));
    let interactive = prompt.is_none() && std::io::stdin().is_terminal();
    if run_setup || (interactive && !explicit_connection && !config::config_exists()) {
        if let Err(e) = setup::run(&ui, &mut cfg) {
            ui.error(&e.to_string());
            std::process::exit(1);
        }
    } else if !config::config_exists() {
        ui.info("no config file found - using defaults (run `nah --setup` to configure)");
    }

    let provider = match provider::connect(cfg.provider, &cfg.url) {
        Ok(p) => p,
        Err(e) => {
            ui.error(&format!(
                "{e}\nmake sure the model server is running (e.g. `ollama serve`) \
                 or pass the right --url / --provider (see `nah --setup`)."
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
