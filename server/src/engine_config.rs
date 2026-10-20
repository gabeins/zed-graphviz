//! wasmtime settings shared by `build.rs`, which compiles the bundled
//! Graphviz ahead of time, and the server, which loads that compiled code.
//! A precompiled module only loads into an engine configured the same way,
//! so both sides take their settings from here.

/// Native stack the guest may use. Graphviz recurses deeply on large graphs
/// (rank assignment, for one); wasmtime's 512 KiB default overflows where a
/// native `dot` with an 8 MiB stack does not. Render threads are spawned
/// with room for this (see `server::RENDER_STACK`).
pub const WASM_STACK: usize = 16 << 20;

/// Applies the settings that compiled code depends on.
pub fn configure(config: &mut wasmtime::Config) {
    // Lets a timer interrupt a render that runs too long.
    config.epoch_interruption(true);
    config.max_wasm_stack(WASM_STACK);
    // Only used by asynchronous calls, which the server does not make, but
    // wasmtime requires it to be at least the wasm stack size.
    config.async_stack_size(2 * WASM_STACK);
}
