//! `graphviz-preview`: a language server that renders Graphviz DOT files to
//! PNG previews and asks the editor to show them.

mod appearance;
#[cfg(feature = "bundled")]
mod bundled;
mod cache;
mod diagnostics;
mod document;
#[cfg(feature = "bundled")]
mod engine_config;
mod render;
mod server;
mod settings;

use std::process::ExitCode;

/// Writes a log line to stderr, which Zed shows in the language server log.
pub fn log(message: impl AsRef<str>) {
    eprintln!("[graphviz-preview] {}", message.as_ref());
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--version" | "-V") => {
            println!("graphviz-preview {}", env!("CARGO_PKG_VERSION"));
            #[cfg(feature = "bundled")]
            println!("bundled Graphviz wasm sha256 {}", bundled::wasm_sha256());
            #[cfg(not(feature = "bundled"))]
            println!("built without bundled Graphviz");
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("graphviz-preview {}", env!("CARGO_PKG_VERSION"));
            println!("A language server for Graphviz previews. It speaks LSP over stdio and is");
            println!("started by the Zed Graphviz extension; there is nothing to run by hand.");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    log(format!(
        "starting version {} (bundled Graphviz: {})",
        env!("CARGO_PKG_VERSION"),
        if render::HAS_BUNDLED { "yes" } else { "no" }
    ));
    match server::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(format!("fatal: {e}"));
            ExitCode::FAILURE
        }
    }
}
