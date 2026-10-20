//! Turns Graphviz stderr into LSP diagnostics.
//!
//! Graphviz reports problems as lines starting with `Error:` or `Warning:`;
//! some messages continue on the following lines. Only syntax errors carry a
//! file line (`syntax error in line N`). For the rest we look for the name the
//! message mentions (a node, a shape, a colour) in the source, and fall back
//! to the graph declaration line.

use crate::document;
use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

pub const SOURCE: &str = "graphviz";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub severity: Severity,
    /// The message text without the `Error:`/`Warning:` prefix, continuation
    /// lines joined with newlines.
    pub text: String,
}

/// Splits Graphviz stderr into messages. Lines before the first prefixed
/// line are kept as errors so nothing Graphviz says is lost.
pub fn parse_stderr(stderr: &str) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    for raw in stderr.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Error:") {
            messages.push(Message {
                severity: Severity::Error,
                text: rest.trim().to_string(),
            });
        } else if let Some(rest) = line.strip_prefix("Warning:") {
            messages.push(Message {
                severity: Severity::Warning,
                text: rest.trim().to_string(),
            });
        } else if let Some(last) = messages.last_mut() {
            last.text.push('\n');
            last.text.push_str(line.trim());
        } else {
            messages.push(Message {
                severity: Severity::Error,
                text: line.trim().to_string(),
            });
        }
    }
    messages
}

/// Where in the source a message points, before it becomes a `Range`.
#[derive(Debug, PartialEq, Eq)]
enum Anchor {
    /// Zero-based line, plus an optional token to highlight on that line.
    Line(u32, Option<String>),
    /// A name to search for in the whole document.
    Token(String),
    /// No location: the graph declaration line.
    Header,
}

fn anchor(message: &Message) -> Anchor {
    let first_line = message.text.lines().next().unwrap_or_default();

    // An unterminated string is reported where scanning stopped (often the
    // end of the file); the continuation line says where it started.
    if let Some(start) = message
        .text
        .lines()
        .find_map(|l| l.strip_prefix("String starting:"))
    {
        let snippet: String = start.chars().take(24).collect();
        if !snippet.is_empty() {
            return Anchor::Token(snippet);
        }
    }

    // HTML label errors count lines inside the label; point at the owner.
    if let Some(owner) = message
        .text
        .lines()
        .find_map(|l| l.strip_prefix("in label of "))
    {
        if let Some(name) = owner.split_whitespace().nth(1) {
            return Anchor::Token(unquote(name));
        }
    }

    if first_line.starts_with("<stdin>:") || first_line.contains("syntax error") {
        if let Some(line) = number_after(first_line, "line ") {
            let near = between(first_line, "near '", "'");
            return Anchor::Line(line.saturating_sub(1), near);
        }
    }

    if let Some(shape) = first_line.split("unknown shape ").nth(1) {
        return Anchor::Token(unquote(shape.trim()));
    }
    if let Some(rest) = first_line.strip_prefix("node ") {
        if let Some(name) = rest.split([',', ' ']).next() {
            if !name.is_empty() {
                return Anchor::Token(unquote(name));
            }
        }
    }
    if let Some(color) = first_line.strip_suffix(" is not a known color.") {
        return Anchor::Token(unquote(color.trim()));
    }
    if let Some(layout) = between(first_line, "Layout type: \"", "\"") {
        return Anchor::Token(layout);
    }
    Anchor::Header
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '\'').to_string()
}

fn between(s: &str, start: &str, end: &str) -> Option<String> {
    let from = s.find(start)? + start.len();
    let len = s[from..].find(end)?;
    let inner = &s[from..from + len];
    (!inner.is_empty()).then(|| inner.to_string())
}

fn number_after(s: &str, marker: &str) -> Option<u32> {
    let from = s.find(marker)? + marker.len();
    let digits: String = s[from..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Converts messages to diagnostics against `text`.
pub fn to_diagnostics(messages: &[Message], text: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = text.lines().collect();
    let header = document::header_line(text).unwrap_or(0);
    messages
        .iter()
        .map(|message| {
            let range = match anchor(message) {
                Anchor::Line(line, near) => {
                    let line = line.min(lines.len().saturating_sub(1) as u32);
                    near.and_then(|token| token_range_on_line(&lines, line, &token))
                        .unwrap_or_else(|| line_range(&lines, line))
                }
                Anchor::Token(token) => {
                    find_token(&lines, &token).unwrap_or_else(|| line_range(&lines, header))
                }
                Anchor::Header => line_range(&lines, header),
            };
            Diagnostic {
                range,
                severity: Some(match message.severity {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                }),
                source: Some(SOURCE.to_string()),
                message: message.text.clone(),
                ..Default::default()
            }
        })
        .collect()
}

/// A diagnostic not produced by Graphviz itself (timeouts, missing renderer).
pub fn header_diagnostic(text: &str, severity: DiagnosticSeverity, message: String) -> Diagnostic {
    let lines: Vec<&str> = text.lines().collect();
    let header = document::header_line(text).unwrap_or(0);
    Diagnostic {
        range: line_range(&lines, header),
        severity: Some(severity),
        source: Some(SOURCE.to_string()),
        message,
        ..Default::default()
    }
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

fn line_range(lines: &[&str], line: u32) -> Range {
    let content = lines.get(line as usize).copied().unwrap_or_default();
    let start = utf16_len(&content[..content.len() - content.trim_start().len()]);
    let end = utf16_len(content.trim_end());
    Range::new(
        Position::new(line, start.min(end)),
        Position::new(line, end),
    )
}

fn token_range_on_line(lines: &[&str], line: u32, token: &str) -> Option<Range> {
    let content = lines.get(line as usize)?;
    let at = find_word(content, token)?;
    let start = utf16_len(&content[..at]);
    Some(Range::new(
        Position::new(line, start),
        Position::new(line, start + utf16_len(token)),
    ))
}

fn find_token(lines: &[&str], token: &str) -> Option<Range> {
    (0..lines.len() as u32).find_map(|line| token_range_on_line(lines, line, token))
}

/// Byte offset of `token` in `line` where it is not part of a longer identifier.
fn find_word(line: &str, token: &str) -> Option<usize> {
    if token.is_empty() {
        return None;
    }
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(pos) = line[from..].find(token) {
        let at = from + pos;
        let end = at + token.len();
        let before_ok = token.starts_with(|c: char| !is_ident(c))
            || !line[..at].chars().next_back().is_some_and(is_ident);
        let after_ok = token.ends_with(|c: char| !is_ident(c))
            || !line[end..].chars().next().is_some_and(is_ident);
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + token.chars().next().map_or(1, char::len_utf8);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(stderr: &str, text: &str) -> Vec<Diagnostic> {
        to_diagnostics(&parse_stderr(stderr), text)
    }

    #[test]
    fn syntax_error_lands_on_reported_line_and_token() {
        let text = "digraph {\n  a -> b\n  c -> }\n";
        let d = diag("Error: <stdin>: syntax error in line 3 near '}'\n", text);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d[0].source.as_deref(), Some("graphviz"));
        assert_eq!(d[0].range.start, Position::new(2, 7));
        assert_eq!(d[0].range.end, Position::new(2, 8));
        assert_eq!(d[0].message, "<stdin>: syntax error in line 3 near '}'");
    }

    #[test]
    fn unterminated_string_points_at_its_start() {
        let text = "digraph {\n a [label=\"unterminated]\n}\n";
        let stderr = "Error: <stdin>: syntax error in line 4 scanning a quoted string (missing endquote? longer than 16384?)\nString starting:\"unterminated]\n}\n\n";
        let d = diag(stderr, text);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].range.start.line, 1);
        assert!(d[0].message.contains("String starting:"));
    }

    #[test]
    fn warnings_find_the_name_they_mention() {
        let text =
            "digraph {\n  label=\"x\"\n  a [shape=foo]\n  b [color=notacolor]\n  a:x -> b\n}\n";
        let d = diag(
            "Warning: using box for unknown shape foo\nWarning: notacolor is not a known color.\nWarning: node a, port x unrecognized\n",
            text,
        );
        assert_eq!(d.len(), 3);
        assert!(d
            .iter()
            .all(|d| d.severity == Some(DiagnosticSeverity::WARNING)));
        assert_eq!(d[0].range.start, Position::new(2, 11));
        assert_eq!(d[1].range.start, Position::new(3, 11));
        assert_eq!(d[2].range.start, Position::new(2, 2));
    }

    #[test]
    fn html_label_error_points_at_the_node() {
        let text = "digraph {\n  x\n  a [label=<<b>x</c>>]\n}\n";
        let stderr = "Error: mismatched tag in line 1 \n... x</c> ...\nin label of node a\n";
        let d = diag(stderr, text);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].range.start, Position::new(2, 2));
        assert!(d[0].message.ends_with("in label of node a"));
    }

    #[test]
    fn layout_error_and_unlocated_messages() {
        let text = "// comment\ndigraph G {\n  layout=foo; a\n}\n";
        let stderr = "Error: Layout type: \"foo\" not recognized. Use one of: circo dot\nError: Layout was not done.  Missing layout plugins? \n";
        let d = diag(stderr, text);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].range.start, Position::new(2, 9));
        // No location: the graph declaration line, not the comment.
        assert_eq!(d[1].range.start, Position::new(1, 0));
        assert_eq!(d[1].range.end, Position::new(1, 11));
    }

    #[test]
    fn unprefixed_output_is_kept() {
        let msgs = parse_stderr("something odd\nWarning: careful\n");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].severity, Severity::Error);
        assert_eq!(msgs[1].text, "careful");
    }

    #[test]
    fn crlf_and_out_of_range_lines_are_safe() {
        let text = "digraph {\r\n a -> \r\n";
        let d = diag("Error: <stdin>: syntax error in line 99 near 'x'\r\n", text);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].range.start.line, 1);
    }

    #[test]
    fn word_search_respects_identifier_boundaries() {
        assert_eq!(find_word("ab a", "a"), Some(3));
        assert_eq!(find_word("xa_a a", "a"), Some(5));
        assert_eq!(find_word("\"q\" ", "\"q"), Some(0));
        assert_eq!(find_word("none", "x"), None);
    }
}
