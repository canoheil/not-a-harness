//! `nah` desktop: a native window around the built-in web GUI.
//!
//! The shell starts the exact same GUI server the browser version uses
//! (`nah --gui`) on a free loopback port and points a native webview at
//! it - one code path, one interface, no browser chrome.

use std::net::TcpListener;

use tauri::{Url, WebviewUrl, WebviewWindowBuilder};

fn main() {
    // Same configuration as the terminal front-end; first-run setup is
    // the GUI settings panel (provider, URL, model, root, ...).
    let cfg = match nah_core::config::from_args(&[]) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => std::process::exit(0),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // Start the embedded GUI server on a free loopback port. It dies
    // with the process when the window closes.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("cannot bind a local port");
    let port = listener.local_addr().expect("local address").port();
    std::thread::spawn(move || {
        let _ = nah_cli::gui::serve_listener(listener, cfg);
    });

    let url = Url::parse(&format!("http://127.0.0.1:{port}")).expect("valid local url");
    tauri::Builder::default()
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("nah")
                .inner_size(1180.0, 800.0)
                .min_inner_size(720.0, 520.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the nah desktop shell");
}
