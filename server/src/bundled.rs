//! Graphviz compiled to WebAssembly (WASI preview 1), run with wasmtime.
//!
//! The module behaves like `dot`: arguments on the command line, DOT on
//! stdin, SVG on stdout, messages on stderr and a non-zero exit on error.
//! It gets no filesystem, network or environment access.
//!
//! `build.rs` compiles the module to native code for the target ahead of
//! time, so the server only needs wasmtime's runtime to load it.

use crate::engine_config;
use crate::render::{self, Deadline, SvgOutput};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use wasmtime::{Config, Engine, InstancePre, Linker, Module, ResourceLimiter, Store, Trap};
use wasmtime_wasi::p1::WasiP1Ctx;
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

/// The module compiled by `build.rs`.
static COMPILED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/graphviz.cwasm"));

/// Linear memory a single render may use.
pub const MAX_MEMORY: usize = 1 << 30;

/// The SHA-256 of the embedded `graphviz.wasm`, as hex.
pub fn wasm_sha256() -> &'static str {
    env!("GRAPHVIZ_WASM_SHA256")
}

/// Caps linear memory and remembers whether the cap was what stopped a
/// render, so the user gets a clear message instead of a generic failure.
struct MemoryCap {
    max: usize,
    hit: bool,
}

impl ResourceLimiter for MemoryCap {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max {
            self.hit = true;
            return Ok(false);
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

struct State {
    wasi: WasiP1Ctx,
    memory: MemoryCap,
}

struct Bundled {
    engine: Engine,
    pre: InstancePre<State>,
    /// Renders run one at a time: the timeout bumps the engine-wide epoch,
    /// which would otherwise interrupt a neighbouring render too.
    lock: Mutex<()>,
}

static INSTANCE: OnceLock<Result<Bundled, String>> = OnceLock::new();

fn instance() -> Result<&'static Bundled, String> {
    INSTANCE
        .get_or_init(|| {
            load().map_err(|e| {
                let msg = format!("bundled Graphviz failed to load: {e}");
                crate::log(&msg);
                msg
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn load() -> wasmtime::Result<Bundled> {
    let started = Instant::now();
    let mut config = Config::new();
    engine_config::configure(&mut config);
    let engine = Engine::new(&config)?;
    // SAFETY: `COMPILED` was produced by `Engine::precompile_module` in
    // build.rs, with the same wasmtime version (pinned in Cargo.toml) and the
    // same `engine_config::configure` settings, for this target. wasmtime
    // also checks the version and settings and refuses a mismatch.
    let module = unsafe { Module::deserialize(&engine, COMPILED) }?;
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s: &mut State| &mut s.wasi)?;
    let pre = linker.instantiate_pre(&module)?;
    crate::log(format!(
        "bundled Graphviz: loaded {} KiB of compiled code in {} ms",
        COMPILED.len() / 1024,
        started.elapsed().as_millis()
    ));
    Ok(Bundled {
        engine,
        pre,
        lock: Mutex::new(()),
    })
}

/// Interrupts the render in progress, if any (used at shutdown; later
/// renders see the cancellation before they start).
pub fn interrupt() {
    if let Some(Ok(bundled)) = INSTANCE.get() {
        bundled.engine.increment_epoch();
    }
}

/// Loads the module ahead of the first render.
pub fn warm_up() {
    let _ = instance();
}

/// Runs the bundled Graphviz. Renders take turns; the `timeout` budget
/// starts when this render's turn comes, and the returned deadline is the
/// one the rest of the pipeline must keep.
pub fn run(source: &str, args: &[String], timeout: Duration) -> (SvgOutput, Deadline) {
    run_with_memory(source, args, timeout, MAX_MEMORY)
}

fn run_with_memory(
    source: &str,
    args: &[String],
    timeout: Duration,
    max_memory: usize,
) -> (SvgOutput, Deadline) {
    let failed = |msg: String| SvgOutput {
        svg: None,
        stderr: String::new(),
        timed_out: false,
        failure: Some(msg),
    };
    let bundled = match instance() {
        Ok(b) => b,
        Err(e) => return (failed(e), Deadline::after(Duration::ZERO)),
    };
    let _guard = bundled
        .lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let deadline = Deadline::after(timeout);
    // A shutdown while this render waited for its turn.
    if render::cancelled() {
        return (failed("The render was cancelled.".into()), deadline);
    }
    (
        execute(bundled, source, args, deadline, max_memory),
        deadline,
    )
}

fn execute(
    bundled: &Bundled,
    source: &str,
    args: &[String],
    deadline: Deadline,
    max_memory: usize,
) -> SvgOutput {
    let max_svg = render::max_svg_bytes();
    let stdout = MemoryOutputPipe::new(max_svg);
    let stderr = MemoryOutputPipe::new(render::MAX_STDERR_BYTES);
    let mut builder = WasiCtxBuilder::new();
    builder
        .stdin(MemoryInputPipe::new(source.as_bytes().to_vec()))
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .arg("dot")
        .args(args);
    let state = State {
        wasi: builder.build_p1(),
        memory: MemoryCap {
            max: max_memory,
            hit: false,
        },
    };
    let mut store = Store::new(&bundled.engine, state);
    store.limiter(|s| &mut s.memory);
    store.set_epoch_deadline(1);
    // A shutdown interrupts by advancing the epoch, which only reaches a
    // store whose deadline is already armed. One that came in before this
    // point would be missed, so look again now that it is armed.
    if render::cancelled() {
        return SvgOutput {
            svg: None,
            stderr: String::new(),
            timed_out: false,
            failure: Some("The render was cancelled.".into()),
        };
    }

    // Interrupt the guest when the timeout passes; dropping `done` first
    // stops the timer.
    let (done, wait) = mpsc::channel::<()>();
    let engine = bundled.engine.clone();
    let timeout = deadline.remaining();
    let timer = std::thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
            engine.increment_epoch();
            true
        } else {
            false
        }
    });

    let result = bundled.pre.instantiate(&mut store).and_then(|instance| {
        let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
        start.call(&mut store, ())
    });
    drop(done);
    let timed_out = timer.join().unwrap_or(false);
    let memory_hit = store.data().memory.hit;
    drop(store);

    let stdout = stdout.contents().to_vec();
    let stderr_bytes = stderr.contents();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    // A full pipe refuses further writes, which Graphviz reports as an error.
    if stdout.len() >= max_svg || stderr_bytes.len() >= render::MAX_STDERR_BYTES {
        return SvgOutput {
            svg: None,
            stderr,
            timed_out: false,
            failure: Some(render::too_much_output()),
        };
    }
    let exit = match result {
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<I32Exit>() {
            Some(exit) => exit.0,
            None => {
                let interrupted = e.downcast_ref::<Trap>() == Some(&Trap::Interrupt);
                if interrupted && !timed_out {
                    return SvgOutput {
                        svg: None,
                        stderr,
                        timed_out: false,
                        failure: Some("The render was cancelled.".into()),
                    };
                }
                if interrupted {
                    crate::log(format!(
                        "bundled Graphviz: stopped after {} ms",
                        timeout.as_millis()
                    ));
                    return SvgOutput {
                        svg: None,
                        stderr,
                        timed_out: true,
                        failure: None,
                    };
                }
                crate::log(format!("bundled Graphviz trapped: {e:?}"));
                return SvgOutput {
                    svg: None,
                    stderr,
                    timed_out: false,
                    failure: Some(failure_message(&e, memory_hit, max_memory)),
                };
            }
        },
    };
    let svg = (exit == 0 && !stdout.is_empty()).then_some(stdout);
    let failure = (svg.is_none() && memory_hit).then(|| memory_message(max_memory));
    if let Some(message) = &failure {
        crate::log(format!("bundled Graphviz: exit status {exit}: {message}"));
    }
    SvgOutput {
        svg,
        stderr,
        timed_out: false,
        failure,
    }
}

fn memory_message(max_memory: usize) -> String {
    format!(
        "The built-in Graphviz needs more than {} MiB of memory for this graph and was stopped.",
        max_memory >> 20
    )
}

/// A message for a render that ended in a trap rather than an exit status.
fn failure_message(error: &wasmtime::Error, memory_hit: bool, max_memory: usize) -> String {
    if memory_hit {
        return memory_message(max_memory);
    }
    match error.downcast_ref::<Trap>() {
        Some(Trap::StackOverflow) => {
            "The built-in Graphviz ran out of stack space on this graph; the system dot may still render it."
                .to_string()
        }
        Some(Trap::MemoryOutOfBounds) => {
            "The built-in Graphviz crashed (out-of-bounds memory access); the system dot may still render this graph."
                .to_string()
        }
        Some(Trap::UnreachableCodeReached) => {
            "The built-in Graphviz stopped on an internal error.".to_string()
        }
        Some(trap) => format!("The built-in Graphviz crashed: {trap}."),
        None => format!("The built-in Graphviz failed: {}.", error.root_cause()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "digraph { alpha -> beta }";

    fn args() -> Vec<String> {
        vec!["-Tsvg".to_string()]
    }

    /// Runs `f` on a thread with as much stack as the server's render threads.
    fn on_render_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(4 * engine_config::WASM_STACK)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn precompiled_module_loads_and_renders() {
        let out = on_render_thread(|| run(SAMPLE, &args(), Duration::from_secs(30)).0);
        assert!(out.failure.is_none(), "{:?}", out.failure);
        let svg = String::from_utf8(out.svg.expect("an SVG")).unwrap();
        assert!(
            svg.contains(">alpha</text>") && svg.contains(">beta</text>"),
            "{svg}"
        );
    }

    #[test]
    fn memory_cap_stops_the_render_with_a_clear_message() {
        // The module starts with about 8.2 MiB (its 8 MiB C stack plus
        // data); 9 MiB leaves too little heap for a few thousand edges.
        let mut text = String::from("digraph {\n");
        for i in 0..2000 {
            text.push_str(&format!("  n{i} -> n{};\n", i + 1));
        }
        text.push('}');
        let out = on_render_thread(move || {
            run_with_memory(&text, &args(), Duration::from_secs(60), 9 << 20).0
        });
        assert!(out.svg.is_none());
        let failure = out.failure.expect("a failure message");
        assert!(failure.contains("more than 9 MiB of memory"), "{failure}");
    }

    #[test]
    fn traps_become_readable_messages() {
        let message = |trap: Trap| failure_message(&wasmtime::Error::from(trap), false, MAX_MEMORY);
        assert!(message(Trap::StackOverflow).contains("ran out of stack space"));
        assert!(message(Trap::MemoryOutOfBounds).contains("out-of-bounds memory access"));
        assert!(message(Trap::UnreachableCodeReached).contains("internal error"));
        let capped = failure_message(
            &wasmtime::Error::from(Trap::UnreachableCodeReached),
            true,
            MAX_MEMORY,
        );
        assert!(capped.contains("more than 1024 MiB of memory"), "{capped}");
    }
}
