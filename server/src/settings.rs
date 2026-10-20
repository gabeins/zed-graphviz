//! User settings, read from `lsp.graphviz-preview.settings` in Zed.
//!
//! Parsing is lenient: unknown keys are ignored and invalid values fall back
//! to the default with a log line, so a typo never stops the server.

use serde_json::Value;
use std::path::PathBuf;

/// The key the extension wraps the settings object in.
pub const SECTION: &str = "graphviz-preview";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererChoice {
    Auto,
    Dot,
    Bundled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderMode {
    Live,
    Save,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub renderer: RendererChoice,
    pub dot_path: Option<PathBuf>,
    /// Layout engine passed as `-K<engine>`; `None` respects the file's `layout=`.
    pub engine: Option<String>,
    pub render: RenderMode,
    pub debounce_ms: u64,
    pub timeout_ms: u64,
    pub scale: f32,
    pub appearance: Appearance,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            renderer: RendererChoice::Auto,
            dot_path: None,
            engine: None,
            render: RenderMode::Live,
            debounce_ms: 300,
            timeout_ms: 10_000,
            scale: 2.0,
            appearance: Appearance::Auto,
        }
    }
}

/// Layout engines Graphviz ships; anything else would make `dot` exit with an error.
const ENGINES: &[&str] = &[
    "dot",
    "neato",
    "fdp",
    "sfdp",
    "circo",
    "twopi",
    "osage",
    "patchwork",
];

impl Settings {
    /// Builds settings from either `{"graphviz-preview": {...}}` or the bare
    /// settings object. Returns the settings and one warning per rejected value.
    pub fn from_json(value: &Value) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut warnings = Vec::new();
        let obj = match unwrap_section(value) {
            Some(Value::Object(obj)) => obj,
            Some(Value::Null) | None => return (settings, warnings),
            Some(other) => {
                warnings.push(format!("settings must be an object, got {other}"));
                return (settings, warnings);
            }
        };

        for (key, value) in obj {
            match key.as_str() {
                "renderer" => match value.as_str() {
                    Some("auto") => settings.renderer = RendererChoice::Auto,
                    Some("dot") => settings.renderer = RendererChoice::Dot,
                    Some("bundled") => settings.renderer = RendererChoice::Bundled,
                    _ => warnings.push(invalid(key, value, "\"auto\", \"dot\" or \"bundled\"")),
                },
                "dot_path" => match value {
                    Value::String(s) if !s.trim().is_empty() => {
                        settings.dot_path = Some(PathBuf::from(s.trim()))
                    }
                    Value::String(_) | Value::Null => settings.dot_path = None,
                    _ => warnings.push(invalid(key, value, "a path string")),
                },
                "engine" => match value {
                    Value::String(s) if s.is_empty() => settings.engine = None,
                    Value::String(s) if ENGINES.contains(&s.as_str()) => {
                        settings.engine = Some(s.clone())
                    }
                    Value::Null => settings.engine = None,
                    _ => warnings.push(invalid(
                        key,
                        value,
                        &format!("one of {}", ENGINES.join(", ")),
                    )),
                },
                "render" => match value.as_str() {
                    Some("live") => settings.render = RenderMode::Live,
                    Some("save") => settings.render = RenderMode::Save,
                    _ => warnings.push(invalid(key, value, "\"live\" or \"save\"")),
                },
                "debounce_ms" => match value.as_u64() {
                    Some(ms) if ms <= 10_000 => settings.debounce_ms = ms,
                    _ => warnings.push(invalid(key, value, "milliseconds between 0 and 10000")),
                },
                "timeout_ms" => match value.as_u64() {
                    Some(ms) if (100..=600_000).contains(&ms) => settings.timeout_ms = ms,
                    _ => warnings.push(invalid(key, value, "milliseconds between 100 and 600000")),
                },
                "scale" => match value.as_f64() {
                    Some(scale) if (0.25..=8.0).contains(&scale) => settings.scale = scale as f32,
                    _ => warnings.push(invalid(key, value, "a number between 0.25 and 8")),
                },
                "appearance" => match value.as_str() {
                    Some("auto") => settings.appearance = Appearance::Auto,
                    Some("light") => settings.appearance = Appearance::Light,
                    Some("dark") => settings.appearance = Appearance::Dark,
                    _ => warnings.push(invalid(key, value, "\"auto\", \"light\" or \"dark\"")),
                },
                _ => warnings.push(format!("unknown setting \"{key}\" ignored")),
            }
        }
        (settings, warnings)
    }
}

fn unwrap_section(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(obj) if obj.contains_key(SECTION) => obj.get(SECTION),
        other => Some(other),
    }
}

fn invalid(key: &str, value: &Value, expected: &str) -> String {
    format!("invalid value {value} for \"{key}\" (expected {expected}); using the default")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_for_null_and_empty() {
        assert_eq!(Settings::from_json(&Value::Null).0, Settings::default());
        assert_eq!(Settings::from_json(&json!({})).0, Settings::default());
    }

    #[test]
    fn accepts_wrapped_and_bare_objects() {
        let wrapped = json!({"graphviz-preview": {"renderer": "dot", "scale": 1.5}});
        let bare = json!({"renderer": "dot", "scale": 1.5});
        let (a, wa) = Settings::from_json(&wrapped);
        let (b, wb) = Settings::from_json(&bare);
        assert_eq!(a, b);
        assert!(wa.is_empty() && wb.is_empty());
        assert_eq!(a.renderer, RendererChoice::Dot);
        assert_eq!(a.scale, 1.5);
    }

    #[test]
    fn invalid_values_keep_defaults_and_warn() {
        let (s, warnings) = Settings::from_json(&json!({
            "renderer": "cairo",
            "engine": "nonsense",
            "debounce_ms": -1,
            "scale": 100,
            "appearance": "sepia",
        }));
        assert_eq!(s, Settings::default());
        assert_eq!(warnings.len(), 5);
    }

    #[test]
    fn parses_every_setting() {
        let (s, warnings) = Settings::from_json(&json!({
            "renderer": "bundled",
            "dot_path": "/usr/local/bin/dot",
            "engine": "neato",
            "render": "save",
            "debounce_ms": 50,
            "timeout_ms": 2000,
            "scale": 3,
            "appearance": "dark",
        }));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(s.renderer, RendererChoice::Bundled);
        assert_eq!(s.dot_path, Some(PathBuf::from("/usr/local/bin/dot")));
        assert_eq!(s.engine.as_deref(), Some("neato"));
        assert_eq!(s.render, RenderMode::Save);
        assert_eq!(s.debounce_ms, 50);
        assert_eq!(s.timeout_ms, 2000);
        assert_eq!(s.scale, 3.0);
        assert_eq!(s.appearance, Appearance::Dark);
    }
}
