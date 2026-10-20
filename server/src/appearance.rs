//! Light or dark rendering.
//!
//! An extension cannot read Zed's theme, so `appearance: "auto"` follows the
//! operating system's light/dark setting, which is what Zed's default
//! `"theme": {"mode": "system"}` follows too.

use crate::settings::Appearance;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a detected OS appearance is reused before asking again.
const TTL: Duration = Duration::from_secs(2);

pub fn is_dark(appearance: Appearance) -> bool {
    match appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::Auto => os_is_dark(),
    }
}

static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
/// Set while a watcher keeps `CACHE` current, so it never needs a refresh.
static WATCHING: AtomicBool = AtomicBool::new(false);

fn os_is_dark() -> bool {
    static WARNED: std::sync::Once = std::sync::Once::new();

    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, dark)) = *cache {
        if at.elapsed() < TTL || WATCHING.load(Ordering::Relaxed) {
            return dark;
        }
    }
    let dark = match dark_light::detect() {
        Ok(dark_light::Mode::Dark) => true,
        Ok(_) => false,
        Err(e) => {
            WARNED.call_once(|| {
                crate::log(format!(
                    "appearance: could not read the OS light/dark setting ({e}); rendering light. Set \"appearance\" to choose."
                ))
            });
            false
        }
    };
    *cache = Some((Instant::now(), dark));
    dark
}

/// Calls `on_change` from a background thread whenever the OS switches
/// between light and dark. Returns false when the platform cannot report it.
pub fn watch(on_change: impl Fn(bool) + Send + 'static) -> bool {
    let watcher = match dark_light::subscribe() {
        Ok(watcher) => watcher,
        Err(e) => {
            crate::log(format!(
                "appearance: cannot watch the OS light/dark setting: {e}"
            ));
            return false;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("appearance".into())
        .spawn(move || {
            let mut last = dark_light::detect()
                .ok()
                .map(|m| m == dark_light::Mode::Dark);
            if let Some(dark) = last {
                *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), dark));
                WATCHING.store(true, Ordering::Relaxed);
            }
            for mode in watcher.iter() {
                let dark = mode == dark_light::Mode::Dark;
                *CACHE.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), dark));
                if last != Some(dark) {
                    on_change(dark);
                }
                last = Some(dark);
            }
            WATCHING.store(false, Ordering::Relaxed);
        });
    spawned.is_ok()
}
