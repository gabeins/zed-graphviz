//! Compiles the bundled Graphviz (`graphviz-wasm/graphviz.wasm`) to native
//! code for the target, so the server ships only wasmtime's runtime and not
//! its compiler.

use std::path::Path;

#[cfg(feature = "bundled")]
#[path = "src/engine_config.rs"]
mod engine_config;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=src/engine_config.rs");
    if std::env::var_os("CARGO_FEATURE_BUNDLED").is_none() {
        return;
    }
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let wasm = Path::new(&manifest_dir)
        .join("graphviz-wasm")
        .join("graphviz.wasm");
    println!("cargo::rerun-if-changed={}", wasm.display());
    if !wasm.is_file() {
        eprintln!();
        eprintln!("error: {} is missing.", wasm.display());
        eprintln!();
        eprintln!("The `bundled` feature (on by default) embeds Graphviz compiled to WebAssembly.");
        eprintln!("Build it first with:");
        eprintln!();
        eprintln!("    server/graphviz-wasm/build.sh");
        eprintln!();
        eprintln!("or build a server that only uses the system `dot` with:");
        eprintln!();
        eprintln!("    cargo build --no-default-features");
        eprintln!();
        std::process::exit(1);
    }
    #[cfg(feature = "bundled")]
    if let Err(e) = precompile(&wasm) {
        eprintln!("error: could not compile {}: {e}", wasm.display());
        std::process::exit(1);
    }
}

#[cfg(feature = "bundled")]
fn precompile(wasm: &Path) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;

    let bytes = std::fs::read(wasm).map_err(|e| e.to_string())?;
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("cargo::rustc-env=GRAPHVIZ_WASM_SHA256={sha}");

    // Compile for the target being built, which differs from the machine
    // running this script when cross-compiling. With an explicit target,
    // Cranelift uses that architecture's baseline CPU features instead of
    // the build machine's, so the code runs on any CPU of the target.
    let target = std::env::var("TARGET").map_err(|e| e.to_string())?;
    let mut config = wasmtime::Config::new();
    config.target(&target).map_err(|e| e.to_string())?;
    engine_config::configure(&mut config);
    let engine = wasmtime::Engine::new(&config).map_err(|e| e.to_string())?;
    let compiled = engine
        .precompile_module(&bytes)
        .map_err(|e| e.to_string())?;

    let out =
        PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?).join("graphviz.cwasm");
    std::fs::write(&out, compiled).map_err(|e| e.to_string())?;
    Ok(())
}
