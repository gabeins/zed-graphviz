//! Small lexical helpers over DOT source text.

/// Zero-based line of the first graph declaration (`strict`, `graph` or
/// `digraph`, case-insensitive), skipping whitespace, `//` and `/* */`
/// comments and `#` lines. `None` when the first statement is something else.
pub fn header_line(text: &str) -> Option<u32> {
    let start = skip_trivia(text.as_bytes(), 0, true);
    let word = word_at(text, start);
    ["strict", "graph", "digraph"]
        .iter()
        .any(|k| word.eq_ignore_ascii_case(k))
        .then(|| text[..start].matches('\n').count() as u32)
}

/// Byte offset just after the `{` that opens the first graph's body, so
/// statements can be inserted there without moving any line. `None` when
/// the text does not start with a well-formed graph header.
pub fn body_start(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = skip_trivia(bytes, 0, true);
    let mut keyword = word_at(text, i);
    if keyword.eq_ignore_ascii_case("strict") {
        i = skip_trivia(bytes, i + keyword.len(), false);
        keyword = word_at(text, i);
    }
    if !(keyword.eq_ignore_ascii_case("graph") || keyword.eq_ignore_ascii_case("digraph")) {
        return None;
    }
    i = skip_trivia(bytes, i + keyword.len(), false);
    if bytes.get(i) != Some(&b'{') {
        i = skip_id(bytes, i)?;
        i = skip_trivia(bytes, i, false);
    }
    (bytes.get(i) == Some(&b'{')).then_some(i + 1)
}

fn word_at(text: &str, i: usize) -> &str {
    let rest = text.get(i..).unwrap_or_default();
    let len = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    &rest[..len]
}

/// Skips whitespace and comments (and `#` lines when `at_start`).
fn skip_trivia(bytes: &[u8], mut i: usize, at_start: bool) -> usize {
    let mut line_start = at_start;
    if i == 0 && bytes.starts_with("\u{feff}".as_bytes()) {
        i = 3;
    }
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                line_start = true;
                i += 1;
            }
            b' ' | b'\t' | b'\r' | 0x0c => i += 1,
            b'#' if line_start => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                line_start = false;
            }
            _ => break,
        }
    }
    i
}

/// Skips a DOT ID: a quoted string, an HTML string or a bare word/number.
fn skip_id(bytes: &[u8], mut i: usize) -> Option<usize> {
    match *bytes.get(i)? {
        b'"' => {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => return Some(i + 1),
                    _ => i += 1,
                }
            }
            None
        }
        b'<' => {
            let mut depth = 0usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        c if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-' || c >= 0x80 => {
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'_'
                    || bytes[i] == b'.'
                    || bytes[i] == b'-'
                    || bytes[i] >= 0x80)
            {
                i += 1;
            }
            Some(i)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_at(text: &str) -> Option<String> {
        body_start(text).map(|at| format!("{}|{}", &text[..at], &text[at..]))
    }

    #[test]
    fn body_start_handles_header_forms() {
        assert_eq!(insert_at("digraph {}").as_deref(), Some("digraph {|}"));
        assert_eq!(insert_at("digraph G {}").as_deref(), Some("digraph G {|}"));
        assert_eq!(
            insert_at("// c\n# x\nstrict Graph \"a \\\" {b\" /* { */\n{\n}").as_deref(),
            Some("// c\n# x\nstrict Graph \"a \\\" {b\" /* { */\n{|\n}")
        );
        assert_eq!(
            insert_at("digraph <x<b>y</b>> {}").as_deref(),
            Some("digraph <x<b>y</b>> {|}")
        );
        assert_eq!(
            insert_at("graph g_1.2 {a}").as_deref(),
            Some("graph g_1.2 {|a}")
        );
    }

    #[test]
    fn body_start_rejects_malformed_headers() {
        assert_eq!(body_start(""), None);
        assert_eq!(body_start("node [a=b]"), None);
        assert_eq!(body_start("digraph \"open {"), None);
        assert_eq!(body_start("digraph G H {}"), None);
        assert_eq!(body_start("digraph G"), None);
    }

    #[test]
    fn finds_the_declaration_after_comments() {
        assert_eq!(header_line("digraph G {}"), Some(0));
        assert_eq!(header_line("\n\n  graph {}"), Some(2));
        assert_eq!(
            header_line("// a\n/* b\n c */\n# d\nStrict DiGraph {}"),
            Some(4)
        );
        assert_eq!(header_line("/* x */ digraph {}"), Some(0));
        assert_eq!(header_line("\u{feff}digraph {}"), Some(0));
        assert_eq!(body_start("\u{feff}digraph {}"), Some(12));
    }

    #[test]
    fn rejects_non_graph_first_statements() {
        assert_eq!(header_line(""), None);
        assert_eq!(header_line("// only a comment"), None);
        assert_eq!(header_line("node [shape=box]"), None);
        assert_eq!(header_line("graphs {}"), None);
    }
}
