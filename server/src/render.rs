//! DOT → SVG (system `dot` or the bundled Graphviz) → PNG.

use crate::settings::{RendererChoice, Settings};
use resvg::{tiny_skia, usvg};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

/// The largest SVG Graphviz may produce before the render is stopped.
pub const MAX_SVG_BYTES: usize = 64 << 20;
/// Overrides `MAX_SVG_BYTES` (used by the tests).
pub const MAX_SVG_ENV: &str = "GRAPHVIZ_PREVIEW_MAX_SVG_BYTES";
/// The most Graphviz may write to stderr before the render is stopped.
pub const MAX_STDERR_BYTES: usize = 1 << 20;
/// The largest preview we rasterize, in pixels (about 256 MB of RGBA).
const MAX_PIXELS: f32 = 64_000_000.0;
/// tiny-skia and most viewers cope with this per side.
const MAX_SIDE: f32 = 16_384.0;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Renderer {
    Dot(PathBuf),
    #[cfg(feature = "bundled")]
    Bundled,
}

impl Renderer {
    pub fn name(&self) -> &'static str {
        match self {
            Renderer::Dot(_) => "dot",
            #[cfg(feature = "bundled")]
            Renderer::Bundled => "bundled",
        }
    }
}

/// Whether this build embeds Graphviz.
pub const HAS_BUNDLED: bool = cfg!(feature = "bundled");

/// Picks the renderer for `settings` and explains the choice for the log.
pub fn resolve(settings: &Settings) -> Result<(Renderer, String), String> {
    let configured_dot = || -> Option<Result<PathBuf, String>> {
        let path = settings.dot_path.as_ref()?;
        Some(if path.is_file() {
            Ok(path.clone())
        } else {
            Err(format!("dot_path {} does not exist", path.display()))
        })
    };
    let dot_on_path = || which::which(dot_name()).ok();

    match settings.renderer {
        RendererChoice::Dot => match configured_dot() {
            Some(Ok(path)) => Ok((Renderer::Dot(path), "renderer setting \"dot\", dot_path setting".into())),
            Some(Err(e)) => Err(e),
            None => dot_on_path()
                .map(|p| (Renderer::Dot(p), "renderer setting \"dot\", found on PATH".into()))
                .ok_or_else(|| {
                    "renderer is \"dot\" but no dot executable is on PATH; install Graphviz or set dot_path".into()
                }),
        },
        RendererChoice::Bundled => bundled("renderer setting \"bundled\"".into()),
        RendererChoice::Auto => {
            let mut why = String::new();
            match configured_dot() {
                Some(Ok(path)) => return Ok((Renderer::Dot(path), "dot_path setting".into())),
                Some(Err(e)) => why = format!("{e}; "),
                None => {}
            }
            if let Some(path) = dot_on_path() {
                return Ok((Renderer::Dot(path), format!("{why}found on PATH")));
            }
            bundled(format!("{why}no dot on PATH"))
        }
    }
}

#[cfg(feature = "bundled")]
fn bundled(reason: String) -> Result<(Renderer, String), String> {
    Ok((Renderer::Bundled, reason))
}

#[cfg(not(feature = "bundled"))]
fn bundled(reason: String) -> Result<(Renderer, String), String> {
    Err(format!(
        "{reason}, and this build of graphviz-preview has no bundled Graphviz; install Graphviz or set dot_path"
    ))
}

fn dot_name() -> &'static str {
    if cfg!(windows) {
        "dot.exe"
    } else {
        "dot"
    }
}

/// Dark defaults in a Zed One Dark–like palette, as `(statement, attribute,
/// value)` triples.
pub const DARK_PALETTE: &[(&str, &str, &str)] = &[
    ("graph", "bgcolor", "#282c33"),
    ("graph", "color", "#a9afbc"),
    ("graph", "fontcolor", "#dce0e5"),
    ("graph", "fillcolor", "#2f343e"),
    ("node", "color", "#a9afbc"),
    ("node", "fontcolor", "#dce0e5"),
    ("node", "fillcolor", "#3b414d"),
    ("edge", "color", "#a9afbc"),
    ("edge", "fontcolor", "#dce0e5"),
];

/// The source with dark defaults inserted as the first statements of the
/// graph body, on the same line as its `{` so no line moves.
///
/// Statements that come later in the file override them, which is what
/// lets a file's own colours win. Graphviz's `-G`/`-N`/`-E` flags cannot be
/// used for this: `-G` replaces the file's graph attributes and `-N`/`-E`
/// replace its `node [...]`/`edge [...]` defaults.
pub fn with_dark_defaults(source: &str) -> Option<String> {
    let at = crate::document::body_start(source)?;
    let mut statements = String::new();
    for kind in ["graph", "node", "edge"] {
        statements.push(' ');
        statements.push_str(kind);
        statements.push_str(" [");
        let attrs: Vec<String> = DARK_PALETTE
            .iter()
            .filter(|(k, _, _)| *k == kind)
            .map(|(_, name, value)| format!("{name}=\"{value}\""))
            .collect();
        statements.push_str(&attrs.join(" "));
        statements.push_str("];");
    }
    statements.push(' ');
    Some(format!("{}{}{}", &source[..at], statements, &source[at..]))
}

/// Graphviz arguments for one render: the output format and an optional
/// layout engine. Without one, Graphviz honours the file's `layout=`.
pub fn graphviz_args(engine: Option<&str>) -> Vec<String> {
    let mut args = vec!["-Tsvg".to_string()];
    if let Some(engine) = engine {
        args.push(format!("-K{engine}"));
    }
    args
}

pub struct SvgOutput {
    /// The SVG when Graphviz exited successfully and produced output.
    pub svg: Option<Vec<u8>>,
    pub stderr: String,
    pub timed_out: bool,
    /// Why there is no SVG when Graphviz itself did not say: it could not
    /// be started, its output was too large, or it crashed. A full sentence.
    pub failure: Option<String>,
}

impl SvgOutput {
    fn failed(message: String) -> Self {
        SvgOutput {
            svg: None,
            stderr: String::new(),
            timed_out: false,
            failure: Some(message),
        }
    }
}

/// The largest SVG a render may produce.
pub fn max_svg_bytes() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var(MAX_SVG_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(MAX_SVG_BYTES)
    })
}

static CANCELLED: AtomicBool = AtomicBool::new(false);

/// Stops every render in progress and any later one: running `dot`
/// processes are killed and the bundled Graphviz is interrupted. Used when
/// the server shuts down.
pub fn cancel_all() {
    CANCELLED.store(true, Ordering::SeqCst);
    #[cfg(feature = "bundled")]
    crate::bundled::interrupt();
}

pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// When a render must be finished, checked between its stages.
#[derive(Clone, Copy, Debug)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn after(timeout: Duration) -> Self {
        Deadline(Instant::now() + timeout)
    }

    pub fn remaining(self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }

    /// True once the time is up or the server is shutting down.
    pub fn passed(self) -> bool {
        cancelled() || Instant::now() >= self.0
    }
}

/// Runs Graphviz on `source` within `timeout`. Returns the output and the
/// deadline the later stages must keep: for the bundled Graphviz the budget
/// starts once the render's turn comes, not while it waits for it.
pub fn render_svg(
    renderer: &Renderer,
    source: &str,
    args: &[String],
    timeout: Duration,
) -> (SvgOutput, Deadline) {
    if cancelled() {
        return (
            SvgOutput::failed("The server is shutting down.".into()),
            Deadline::after(Duration::ZERO),
        );
    }
    match renderer {
        Renderer::Dot(path) => {
            let deadline = Deadline::after(timeout);
            (run_dot(path, source, args, deadline), deadline)
        }
        #[cfg(feature = "bundled")]
        Renderer::Bundled => crate::bundled::run(source, args, timeout),
    }
}

/// Process ids of running `dot` children, for tests and the log.
static CHILDREN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Ids of the `dot` processes currently running.
pub fn running_children() -> Vec<u32> {
    CHILDREN.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

enum Stop {
    Exited(std::process::ExitStatus),
    TimedOut,
    Cancelled,
    TooMuchOutput,
}

fn run_dot(path: &Path, source: &str, args: &[String], deadline: Deadline) -> SvgOutput {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    limit_memory(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return SvgOutput::failed(format!(
                "Graphviz could not run: could not start {}: {e}",
                path.display()
            ))
        }
    };
    let pid = child.id();
    CHILDREN.lock().unwrap_or_else(|p| p.into_inner()).push(pid);

    // Feed stdin and drain both pipes on their own threads so a large graph
    // cannot deadlock on a full pipe. A pipe that goes over its cap raises
    // `overflow`, and the process is killed.
    let stdin = child.stdin.take();
    let input = source.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            // A broken pipe here means dot exited early; its stderr says why.
            let _ = stdin.write_all(&input);
        }
    });
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child
        .stdout
        .take()
        .map(|pipe| drain(pipe, max_svg_bytes(), overflow.clone()));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| drain(pipe, MAX_STDERR_BYTES, overflow.clone()));

    // Wait in short slices so a cap, the deadline or a shutdown stops the
    // process promptly.
    let stop = loop {
        let slice = deadline.remaining().min(Duration::from_millis(50));
        match child.wait_timeout(slice) {
            Ok(Some(status)) => break Stop::Exited(status),
            Ok(None) => {}
            Err(e) => {
                crate::log(format!("waiting for {} failed: {e}", path.display()));
                break Stop::Cancelled;
            }
        }
        if overflow.load(Ordering::SeqCst) {
            break Stop::TooMuchOutput;
        }
        if cancelled() {
            break Stop::Cancelled;
        }
        if deadline.passed() {
            break Stop::TimedOut;
        }
    };
    if !matches!(stop, Stop::Exited(_)) {
        let _ = child.kill();
        let _ = child.wait();
    }
    CHILDREN
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|&p| p != pid);
    let _ = writer.join();
    let stdout = stdout.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let too_much = too_much_output;

    match stop {
        Stop::Exited(_) if overflow.load(Ordering::SeqCst) => SvgOutput {
            stderr,
            ..SvgOutput::failed(too_much())
        },
        Stop::Exited(status) => SvgOutput {
            svg: (status.success() && !stdout.is_empty()).then_some(stdout),
            stderr,
            timed_out: false,
            failure: None,
        },
        Stop::TimedOut => SvgOutput {
            svg: None,
            stderr,
            timed_out: true,
            failure: None,
        },
        Stop::TooMuchOutput => SvgOutput {
            stderr,
            ..SvgOutput::failed(too_much())
        },
        Stop::Cancelled => SvgOutput::failed("The render was cancelled.".into()),
    }
}

/// The message for a render whose output went over the caps.
pub fn too_much_output() -> String {
    format!(
        "Graphviz wrote more than {} of SVG (or 1 MiB of messages); the render was stopped.",
        human_size(max_svg_bytes())
    )
}

fn human_size(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} KiB", bytes >> 10)
    }
}

/// Address space a native `dot` may use, on Linux.
#[cfg(target_os = "linux")]
pub const DOT_MEMORY_LIMIT: libc::rlim_t = 2 << 30;

/// The `(soft, hard)` limits to set: the inherited ones lowered to `cap`,
/// never raised (raising a hard limit is not allowed and would fail).
#[cfg(any(target_os = "linux", test))]
pub fn clamp_limits<T: Ord + Copy>(soft: T, hard: T, cap: T) -> (T, T) {
    (soft.min(cap), hard.min(cap))
}

/// Caps the address space of a native `dot` on Linux. Other platforms have
/// no equivalent per-process limit that is safe to set here, so there `dot`
/// is bounded only by the render timeout.
#[cfg(target_os = "linux")]
fn limit_memory(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // The child inherits the server's limits, so read them here, where a
    // problem can be logged, and only apply the result in the child.
    let mut inherited = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes the struct it is given.
    if unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut inherited) } != 0 {
        crate::log(format!(
            "warning: could not read the address-space limit ({}); dot runs without a memory cap",
            std::io::Error::last_os_error()
        ));
        return;
    }
    let (soft, hard) = clamp_limits(inherited.rlim_cur, inherited.rlim_max, DOT_MEMORY_LIMIT);
    let limit = libc::rlimit {
        rlim_cur: soft,
        rlim_max: hard,
    };
    // SAFETY: the closure runs in the child between fork and exec and only
    // calls setrlimit, which is async-signal-safe. Lowering limits is always
    // permitted; if it still fails, dot runs uncapped rather than not at all.
    unsafe {
        command.pre_exec(move || {
            let _ = libc::setrlimit(libc::RLIMIT_AS, &limit);
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn limit_memory(_command: &mut Command) {}

/// Reads `pipe` to the end, keeping at most `cap` bytes; raises `overflow`
/// and stops reading when there is more.
fn drain(
    mut pipe: impl Read + Send + 'static,
    cap: usize,
    overflow: Arc<AtomicBool>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() + n > cap {
                        overflow.store(true, Ordering::SeqCst);
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
        }
        buf
    })
}

/// System fonts, loaded once. Generic families fall back to installed fonts
/// so text never silently disappears on a machine without Times or Arial.
pub fn fontdb() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let started = Instant::now();
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        let families: Vec<String> = db
            .faces()
            .flat_map(|f| f.families.iter().map(|(name, _)| name.clone()))
            .collect();
        let has = |name: &str| families.iter().any(|f| f.eq_ignore_ascii_case(name));
        let pick = |candidates: &[&str]| -> Option<String> {
            candidates
                .iter()
                .find(|c| has(c))
                .map(|c| c.to_string())
                .or_else(|| families.first().cloned())
        };
        if let Some(f) = pick(&[
            "Times New Roman",
            "Times",
            "DejaVu Serif",
            "Liberation Serif",
            "Noto Serif",
        ]) {
            db.set_serif_family(f);
        }
        if let Some(f) = pick(&[
            "Arial",
            "Helvetica",
            "DejaVu Sans",
            "Liberation Sans",
            "Noto Sans",
        ]) {
            db.set_sans_serif_family(f);
        }
        if let Some(f) = pick(&[
            "Courier New",
            "Courier",
            "Menlo",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
        ]) {
            db.set_monospace_family(f);
        }
        crate::log(format!(
            "fonts: {} faces loaded in {} ms",
            db.len(),
            started.elapsed().as_millis()
        ));
        Arc::new(db)
    })
    .clone()
}

/// Where the SVG may load images from.
#[derive(Clone, Debug, Default)]
pub struct ImagePolicy {
    /// Relative image paths are resolved against this directory (the
    /// document's folder).
    pub base: Option<PathBuf>,
    /// Images must be inside this folder (the workspace folder holding the
    /// document, else the document's folder). `None` allows no files.
    pub root: Option<PathBuf>,
}

/// A rasterized preview.
pub struct Raster {
    pub png: Vec<u8>,
    /// Image references the policy refused to load.
    pub refused_images: Vec<String>,
}

pub enum RasterError {
    TimedOut,
    Failed(String),
}

/// Rasterizes `svg` at `scale`, optionally capping the width in pixels.
/// Huge graphs are scaled down to stay within `MAX_PIXELS`. The deadline is
/// checked between parsing, drawing and encoding.
pub fn svg_to_png(
    svg: &[u8],
    scale: f32,
    max_width: Option<f32>,
    images: &ImagePolicy,
    deadline: Deadline,
) -> Result<Raster, RasterError> {
    if svg.len() > max_svg_bytes() {
        return Err(RasterError::Failed(format!(
            "the SVG is larger than {}",
            human_size(max_svg_bytes())
        )));
    }
    if deadline.passed() {
        return Err(RasterError::TimedOut);
    }
    let refused = Arc::new(Mutex::new(Vec::new()));
    let options = usvg::Options {
        resources_dir: images.base.clone(),
        fontdb: fontdb(),
        image_href_resolver: image_resolver(images.root.clone(), refused.clone()),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(svg, &options)
        .map_err(|e| RasterError::Failed(format!("could not read Graphviz SVG: {e}")))?;
    if deadline.passed() {
        return Err(RasterError::TimedOut);
    }
    let (w, h) = (tree.size().width(), tree.size().height());
    let mut scale = scale;
    if let Some(max) = max_width {
        scale = scale.min(max / w.max(1.0));
    }
    scale = scale
        .min((MAX_PIXELS / (w * h).max(1.0)).sqrt())
        .min(MAX_SIDE / w.max(1.0))
        .min(MAX_SIDE / h.max(1.0));
    let width = (w * scale).ceil().max(1.0) as u32;
    let height = (h * scale).ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| RasterError::Failed(format!("cannot allocate a {width}x{height} image")))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    if deadline.passed() {
        return Err(RasterError::TimedOut);
    }
    let png = pixmap
        .encode_png()
        .map_err(|e| RasterError::Failed(format!("PNG encoding failed: {e}")))?;
    // Drawing and encoding cannot be interrupted; a result that comes in
    // late is dropped like any other overrun.
    if deadline.passed() {
        return Err(RasterError::TimedOut);
    }
    let refused_images = std::mem::take(&mut *refused.lock().unwrap_or_else(|p| p.into_inner()));
    Ok(Raster {
        png,
        refused_images,
    })
}

/// Loads `data:` images and image files inside `root`; refuses the rest and
/// records them in `refused`.
fn image_resolver(
    root: Option<PathBuf>,
    refused: Arc<Mutex<Vec<String>>>,
) -> usvg::ImageHrefResolver<'static> {
    let root = root.and_then(|r| r.canonicalize().ok());
    let load = usvg::ImageHrefResolver::default_string_resolver();
    usvg::ImageHrefResolver {
        resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(move |href: &str, options: &usvg::Options| {
            let path = href.strip_prefix("file://").unwrap_or(href);
            let path = options.get_abs_path(Path::new(path));
            let inside = match (&root, path.canonicalize()) {
                (Some(root), Ok(path)) => path.starts_with(root),
                _ => false,
            };
            if inside {
                load(href, options)
            } else {
                refused
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(href.to_string());
                None
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_set_the_format_and_optional_engine() {
        assert_eq!(graphviz_args(None), vec!["-Tsvg"]);
        assert_eq!(graphviz_args(Some("neato")), vec!["-Tsvg", "-Kneato"]);
    }

    #[test]
    fn dark_defaults_are_inserted_without_moving_lines() {
        let source = "// c\ndigraph G {\n  a -> b\n}\n";
        let dark = with_dark_defaults(source).unwrap();
        assert_eq!(dark.lines().count(), source.lines().count());
        let header = dark.lines().nth(1).unwrap();
        assert!(
            header.starts_with("digraph G { graph [bgcolor=\"#282c33\""),
            "{header}"
        );
        assert!(header
            .contains("node [color=\"#a9afbc\" fontcolor=\"#dce0e5\" fillcolor=\"#3b414d\"];"));
        assert!(header.contains("edge [color=\"#a9afbc\" fontcolor=\"#dce0e5\"];"));
        assert_eq!(dark.lines().nth(2), Some("  a -> b"));
        assert_eq!(with_dark_defaults("node [x=y]"), None);
    }

    fn dot() -> Renderer {
        Renderer::Dot(which::which(dot_name()).expect("the tests need Graphviz `dot` on PATH"))
    }

    fn svg_of(source: &str) -> String {
        let (out, _) = render_svg(
            &dot(),
            source,
            &graphviz_args(None),
            Duration::from_secs(10),
        );
        String::from_utf8(
            out.svg
                .unwrap_or_else(|| panic!("dot failed: {}", out.stderr)),
        )
        .unwrap()
    }

    #[test]
    fn file_colours_win_over_dark_defaults_in_real_dot() {
        let plain = svg_of(&with_dark_defaults("digraph { a -> b }").unwrap());
        assert!(plain.contains(r##"<polygon fill="#282c33""##), "{plain}");
        assert!(plain.contains(r##"stroke="#a9afbc""##));
        assert!(plain.contains(r##"fill="#dce0e5""##), "text colour");

        let own = svg_of(
            &with_dark_defaults(
                "digraph {\n bgcolor=white\n node [color=red fontcolor=blue]\n edge [color=green]\n a -> b\n c [color=orange]\n}",
            )
            .unwrap(),
        );
        assert!(
            own.contains(r##"<polygon fill="white""##),
            "graph attribute: {own}"
        );
        assert!(own.contains(r##"stroke="red""##), "node default");
        assert!(own.contains(r##"fill="blue""##), "node fontcolor");
        assert!(own.contains(r##"stroke="green""##), "edge default");
        assert!(own.contains(r##"stroke="orange""##), "node attribute");
        assert!(!own.contains("#a9afbc"), "a dark default leaked: {own}");
    }

    #[test]
    fn explicit_dot_renderer_reports_a_missing_dot_path() {
        let settings = Settings {
            renderer: RendererChoice::Dot,
            dot_path: Some(PathBuf::from("/definitely/not/here/dot")),
            ..Settings::default()
        };
        let err = resolve(&settings).unwrap_err();
        assert!(err.contains("/definitely/not/here/dot"), "{err}");
    }

    fn png(svg: &[u8], scale: f32, max_width: Option<f32>) -> tiny_skia::Pixmap {
        let deadline = Deadline::after(Duration::from_secs(10));
        let raster = svg_to_png(svg, scale, max_width, &ImagePolicy::default(), deadline)
            .unwrap_or_else(|_| panic!("rasterizing failed"));
        assert_eq!(&raster.png[..8], b"\x89PNG\r\n\x1a\n");
        tiny_skia::Pixmap::decode_png(&raster.png).unwrap()
    }

    #[test]
    fn png_respects_scale_and_width_cap() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><rect width="100" height="50" fill="white"/></svg>"#;
        let decoded = png(svg, 2.0, None);
        assert_eq!((decoded.width(), decoded.height()), (200, 100));
        let decoded = png(svg, 2.0, Some(50.0));
        assert_eq!((decoded.width(), decoded.height()), (50, 25));
    }

    #[test]
    fn a_passed_deadline_stops_rasterizing() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;
        let passed = Deadline::after(Duration::ZERO);
        let result = svg_to_png(svg, 1.0, None, &ImagePolicy::default(), passed);
        assert!(matches!(result, Err(RasterError::TimedOut)));
    }

    #[test]
    fn images_outside_the_root_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // A 1x1 red PNG.
        let red = tiny_skia::Pixmap::from_vec(
            vec![255, 0, 0, 255],
            tiny_skia::IntSize::from_wh(1, 1).unwrap(),
        )
        .unwrap()
        .encode_png()
        .unwrap();
        std::fs::write(root.path().join("in.png"), &red).unwrap();
        std::fs::write(outside.path().join("out.png"), &red).unwrap();
        let svg = |href: &str| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{href}"/></svg>"#
            )
        };
        let policy = ImagePolicy {
            base: Some(root.path().to_path_buf()),
            root: Some(root.path().to_path_buf()),
        };
        let deadline = Deadline::after(Duration::from_secs(10));
        let draw = |href: &str| match svg_to_png(svg(href).as_bytes(), 1.0, None, &policy, deadline)
        {
            Ok(raster) => raster,
            Err(_) => panic!("rasterizing failed"),
        };

        let inside = draw("in.png");
        assert!(inside.refused_images.is_empty());
        let pixel = tiny_skia::Pixmap::decode_png(&inside.png)
            .unwrap()
            .pixel(5, 5)
            .unwrap();
        assert_eq!((pixel.red(), pixel.alpha()), (255, 255));

        let href = outside.path().join("out.png").display().to_string();
        let refused = draw(&href);
        assert_eq!(refused.refused_images, vec![href]);
        let pixel = tiny_skia::Pixmap::decode_png(&refused.png)
            .unwrap()
            .pixel(5, 5)
            .unwrap();
        assert_eq!(pixel.alpha(), 0, "the outside image was drawn");
        // `..` cannot climb out of the root either.
        let climbing = format!(
            "../{}/out.png",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        assert_eq!(draw(&climbing).refused_images.len(), 1);
    }

    #[test]
    fn drain_stops_at_the_cap() {
        let flag = Arc::new(AtomicBool::new(false));
        let kept = drain(
            std::io::Cursor::new(vec![0u8; 3 << 20]),
            1 << 20,
            flag.clone(),
        )
        .join()
        .unwrap();
        assert!(kept.len() <= 1 << 20);
        assert!(flag.load(Ordering::SeqCst));

        let flag = Arc::new(AtomicBool::new(false));
        let kept = drain(std::io::Cursor::new(vec![0u8; 1000]), 1 << 20, flag.clone())
            .join()
            .unwrap();
        assert_eq!(kept.len(), 1000);
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn memory_limits_are_lowered_never_raised() {
        let cap: u64 = 2 << 30;
        // Unlimited (RLIM_INFINITY is all ones on Linux): both get the cap.
        assert_eq!(clamp_limits(u64::MAX, u64::MAX, cap), (cap, cap));
        // A lower inherited hard limit is kept, so setrlimit cannot fail.
        assert_eq!(clamp_limits(1 << 30, 1 << 30, cap), (1 << 30, 1 << 30));
        assert_eq!(clamp_limits(1 << 29, u64::MAX, cap), (1 << 29, cap));
        assert_eq!(clamp_limits(u64::MAX, 3 << 30, cap), (cap, cap));
        let (soft, hard) = clamp_limits(1 << 30, 3 << 29, cap);
        assert!(soft <= hard);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_dot_runs_under_a_memory_limit() {
        // The same setup run_dot uses, applied to a shell that reports the
        // limit (in KiB).
        let mut command = Command::new("sh");
        command.args(["-c", "ulimit -v"]);
        limit_memory(&mut command);
        let out = command.output().unwrap();
        let reported = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(reported, (DOT_MEMORY_LIMIT / 1024).to_string());
    }
}
