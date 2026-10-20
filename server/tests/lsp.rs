//! End-to-end tests: the real binary, real stdio, real Graphviz.

mod common;

use base64::Engine as _;
use common::{decode_png, path_of, wait_for_file, Client, Options, SAMPLE};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn svg_options<'a>() -> roxmltree::ParsingOptions<'a> {
    // Graphviz SVG carries a DOCTYPE.
    roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    }
}

fn diagnostics_for<'a>(uri: &'a str) -> impl Fn(&Value) -> bool + 'a {
    move |n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == uri
}

#[test]
fn r1_open_shows_one_png_document() {
    let mut client = Client::start(Options::default());
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    client.pump(Duration::from_millis(500));

    let uris = client.show_document_uris();
    assert_eq!(
        uris.len(),
        1,
        "expected exactly one showDocument, got {uris:?}"
    );
    let shown = &uris[0];
    assert!(shown.starts_with("file://"), "{shown}");
    assert!(shown.ends_with("/flow.png"), "{shown}");
    let params = client.show_documents.lock().unwrap()[0].clone();
    assert_eq!(params["external"], json!(false));
    assert_eq!(params["takeFocus"], json!(true));

    let path = path_of(shown);
    assert!(
        path.starts_with(client.cache.path().canonicalize().unwrap())
            || path.starts_with(client.cache.path())
    );
    let image = decode_png(&std::fs::read(&path).unwrap());
    assert!(image.width > 10 && image.height > 10);
    // Three labelled nodes and two edges, not an empty canvas.
    let drawn = image.drawn(60.0);
    let total = (image.width * image.height) as usize;
    assert!(
        drawn > 500 && drawn < total / 2,
        "drawn pixels: {drawn} of {total}"
    );
    client.shutdown();
}

#[test]
fn r2_live_change_rewrites_png_without_new_show_document() {
    let mut client = Client::start(Options {
        settings: json!({"render": "live", "debounce_ms": 300}),
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    let before = std::fs::read(&path).unwrap();

    let changed = SAMPLE.replace(
        "beta -> gamma;",
        "beta -> gamma;\n  gamma -> delta;\n  delta -> alpha;",
    );
    let sent = Instant::now();
    client.change(&uri, 2, &changed);
    let mut after = before.clone();
    while sent.elapsed() < Duration::from_secs(2) {
        client.pump(Duration::from_millis(20));
        after = std::fs::read(&path).unwrap_or_default();
        if !after.is_empty() && after != before {
            break;
        }
    }
    let took = sent.elapsed();
    assert_ne!(
        after,
        before,
        "PNG did not change within 2 s; log:\n{}",
        client.log()
    );
    assert!(
        took >= Duration::from_millis(300),
        "rendered before the debounce: {took:?}"
    );
    let image = decode_png(&after);
    let old = decode_png(&before);
    assert!(
        image.drawn(60.0) > old.drawn(60.0),
        "the new nodes were not drawn"
    );
    assert!(
        image.height > old.height,
        "the longer graph should be taller"
    );

    client.pump(Duration::from_millis(600));
    assert_eq!(
        client.show_document_uris().len(),
        1,
        "a live render sent another showDocument"
    );
    client.shutdown();
}

#[test]
fn save_mode_renders_on_save_only() {
    let mut client = Client::start(Options {
        settings: json!({"render": "save", "debounce_ms": 50}),
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    let before = std::fs::read(&path).unwrap();

    let changed = SAMPLE.replace("beta -> gamma;", "beta -> gamma;\n  gamma -> delta;");
    client.change(&uri, 2, &changed);
    client.pump(Duration::from_millis(800));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "rendered before the save"
    );

    client.notify(
        "textDocument/didSave",
        json!({"textDocument": {"uri": uri}}),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read(&path).unwrap() == before {
        assert!(
            Instant::now() < deadline,
            "no render after save; log:\n{}",
            client.log()
        );
        client.pump(Duration::from_millis(20));
    }
    assert!(client.log().contains("kind=save"));
    assert_eq!(client.show_document_uris().len(), 1);
    client.shutdown();
}

#[test]
fn r3_opening_twice_reuses_the_same_uri() {
    let mut client = Client::start(Options::default());
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    client.command("graphviz.openPreview", &uri);
    client.pump(Duration::from_millis(200));
    let uris = client.show_document_uris();
    assert_eq!(uris.len(), 2, "{uris:?}");
    assert_eq!(uris[0], uris[1]);

    // A file with the same name in another folder gets its own preview.
    std::fs::create_dir(client.workspace.path().join("other")).unwrap();
    let other = client.open("other/flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &other);
    let uris = client.show_document_uris();
    assert_eq!(uris.len(), 3);
    assert!(uris[2].ends_with("/flow.png"));
    assert_ne!(uris[2], uris[0]);
    client.shutdown();
}

#[test]
fn r4_dot_renderer() {
    let mut client = Client::start(Options {
        settings: json!({"renderer": "dot"}),
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    assert!(decode_png(&std::fs::read(path).unwrap()).drawn(60.0) > 500);
    let log = client.log();
    assert!(log.contains("renderer: engine=dot"), "{log}");
    assert!(log.contains("render: engine=dot"), "{log}");
    client.shutdown();
}

#[cfg(feature = "bundled")]
#[test]
fn r4_bundled_renderer_without_dot_on_path() {
    let empty = tempfile::tempdir().unwrap();
    assert!(
        which::which_in("dot", Some(empty.path()), empty.path()).is_err(),
        "the test PATH must not contain dot"
    );
    let mut client = Client::start(Options {
        settings: json!({"renderer": "bundled", "timeout_ms": 120000}),
        path_env: Some(empty.path().to_path_buf()),
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let uris = client.show_document_uris();
    assert_eq!(uris.len(), 1, "log:\n{}", client.log());
    assert!(decode_png(&std::fs::read(path_of(&uris[0])).unwrap()).drawn(60.0) > 500);

    client.command("graphviz.exportSvg", &uri);
    let svg_path = client.workspace.path().join("flow.svg");
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    let doc = roxmltree::Document::parse_with_options(&svg, svg_options()).expect("valid SVG");
    let labels: Vec<&str> = doc
        .descendants()
        .filter(|n| n.has_tag_name("text"))
        .filter_map(|n| n.text())
        .collect();
    for label in ["alpha", "beta", "gamma"] {
        assert!(labels.contains(&label), "{label} missing from {labels:?}");
    }
    // Graphviz messages from the WebAssembly build become diagnostics too.
    let broken = "// A small sample graph\ndigraph flow {\n  alpha -> beta;\n  beta -> ;\n}\n";
    let since = client.notifications.len();
    client.change(&uri, 2, broken);
    let note = client
        .wait_notification(since, Duration::from_secs(10), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        })
        .unwrap_or_else(|| panic!("no diagnostics; log:\n{}", client.log()));
    assert_eq!(
        note["params"]["diagnostics"][0]["range"]["start"]["line"],
        3
    );
    assert!(note["params"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .contains("syntax error in line 4"));

    let log = client.log();
    assert!(log.contains("renderer: engine=bundled"), "{log}");
    assert!(log.contains("render: engine=bundled"), "{log}");
    assert!(!log.contains("engine=dot"), "{log}");
    client.shutdown();
}

#[test]
fn r5_syntax_error_becomes_a_diagnostic_and_keeps_the_png() {
    let mut client = Client::start(Options::default());
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    let good = std::fs::read(&path).unwrap();

    // Line 4 (1-based) is broken.
    let broken = "// A small sample graph\ndigraph flow {\n  alpha -> beta;\n  beta -> ;\n}\n";
    let since = client.notifications.len();
    client.change(&uri, 2, broken);
    let note = client
        .wait_notification(since, Duration::from_secs(5), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        })
        .unwrap_or_else(|| panic!("no diagnostics; log:\n{}", client.log()));
    let diagnostics = note["params"]["diagnostics"].as_array().unwrap();
    let error = &diagnostics[0];
    assert_eq!(error["source"], "graphviz");
    assert_eq!(error["severity"], 1);
    assert_eq!(error["range"]["start"]["line"], 3, "{error}");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("syntax error in line 4"));
    assert_eq!(note["params"]["version"], 2);

    client.pump(Duration::from_millis(300));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        good,
        "the last good PNG must stay untouched"
    );

    // Fixing the error clears the diagnostics.
    let since = client.notifications.len();
    client.change(&uri, 3, SAMPLE);
    let cleared = client
        .wait_notification(since, Duration::from_secs(5), |n| {
            diagnostics_for(&uri)(n) && n["params"]["version"] == 3
        })
        .expect("diagnostics after the fix");
    assert_eq!(cleared["params"]["diagnostics"], json!([]));
    client.shutdown();
}

#[test]
fn r5_save_reports_diagnostics_without_a_preview() {
    let mut client = Client::start(Options {
        settings: json!({"render": "save"}),
        ..Options::default()
    });
    let uri = client.open("flow.dot", "digraph {\n  a -> }\n");
    let first = client
        .wait_notification(0, Duration::from_secs(5), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        })
        .expect("diagnostics on open");
    assert_eq!(
        first["params"]["diagnostics"][0]["range"]["start"]["line"],
        1
    );
    let since = client.notifications.len();
    client.change(&uri, 2, "digraph {\n  a -> b\n}\n");
    client.notify(
        "textDocument/didSave",
        json!({"textDocument": {"uri": uri}}),
    );
    let cleared = client
        .wait_notification(since, Duration::from_secs(5), diagnostics_for(&uri))
        .expect("diagnostics after save");
    assert_eq!(cleared["params"]["diagnostics"], json!([]));
    assert!(client.show_document_uris().is_empty());
    client.shutdown();
}

#[test]
fn r6_hover_on_the_header_returns_a_thumbnail() {
    let mut client = Client::start(Options::default());
    let uri = client.open("flow.dot", SAMPLE);
    let hover = client.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": {"line": 1, "character": 3}}),
    );
    let value = hover["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("{hover}"));
    assert_eq!(hover["contents"]["kind"], "markdown");
    let start =
        value.find("data:image/png;base64,").expect("data URI") + "data:image/png;base64,".len();
    let end = start + value[start..].find(')').expect("closing paren");
    let png = base64::engine::general_purpose::STANDARD
        .decode(&value[start..end])
        .expect("valid base64");
    let image = decode_png(&png);
    assert!(image.width <= 480, "thumbnail too wide: {}", image.width);
    assert!(image.drawn(60.0) > 200, "the thumbnail shows no graph");
    assert!(value.contains("Open Graphviz preview"));

    // Not on the header: nothing.
    let other = client.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": {"line": 2, "character": 3}}),
    );
    assert_eq!(other, Value::Null);
    // The comment line above the header: nothing either.
    let comment = client.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 3}}),
    );
    assert_eq!(comment, Value::Null);
    client.shutdown();
}

#[test]
fn r7_exports_are_valid_and_light_even_in_dark_mode() {
    let mut client = Client::start(Options {
        settings: json!({"appearance": "dark"}),
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    let since = client.notifications.len();
    client.command("graphviz.exportSvg", &uri);
    client.command("graphviz.exportPng", &uri);

    let svg_path = client.workspace.path().join("flow.svg");
    let png_path = client.workspace.path().join("flow.png");
    let svg = std::fs::read_to_string(&svg_path).expect("flow.svg next to the source");
    let doc = roxmltree::Document::parse_with_options(&svg, svg_options()).expect("valid SVG");
    assert!(doc.root_element().has_tag_name("svg"));
    assert!(!svg.contains("#282c33"), "export used the dark palette");

    let image = decode_png(&std::fs::read(&png_path).expect("flow.png next to the source"));
    assert!(
        image.luma(0, 0) > 240.0,
        "export background is not light: {:?}",
        image.pixel(0, 0)
    );
    // Graphviz's default black lines and text on that background.
    let dark_ink = image.count(|p| common::luma(p) < 100.0);
    assert!(dark_ink > 500, "no dark strokes: {dark_ink}");

    let messages: Vec<String> = client.notifications[since..]
        .iter()
        .filter(|n| n["method"] == "window/showMessage")
        .map(|n| {
            n["params"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(
        messages.contains(&"Exported flow.svg".to_string()),
        "{messages:?}"
    );
    assert!(
        messages.contains(&"Exported flow.png".to_string()),
        "{messages:?}"
    );
    client.shutdown();
}

#[test]
fn r8_dark_appearance_and_file_colours() {
    let mut client = Client::start(Options {
        settings: json!({"appearance": "dark"}),
        ..Options::default()
    });
    let uri = client.open("dark.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    let image = decode_png(&std::fs::read(&path).unwrap());
    assert!(
        image.luma(0, 0) < 80.0,
        "corner is not dark: {:?}",
        image.pixel(0, 0)
    );
    assert_eq!(image.pixel(0, 0)[3], 255);
    // Light lines and text on the dark background.
    let light_ink = image.count(|p| common::luma(p) > 150.0);
    assert!(light_ink > 500, "no light strokes: {light_ink}");
    assert_eq!(
        image.count(|p| common::luma(p) < 30.0),
        0,
        "black ink on dark"
    );

    let white = SAMPLE.replace("digraph flow {", "digraph flow {\n  bgcolor=white;");
    let uri = client.open("white.dot", &white);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[1]);
    let bytes = wait_for_file(&path, Duration::from_secs(5)).unwrap();
    let image = decode_png(&bytes);
    assert_eq!(
        image.pixel(0, 0),
        [255, 255, 255, 255],
        "the file's bgcolor must win"
    );
    assert!(
        image.drawn(30.0) > 500,
        "nothing drawn on the white background"
    );
    client.shutdown();
}

#[test]
fn code_actions_offer_the_three_commands() {
    let mut client = Client::start(Options::default());
    let uri = client.open("flow.dot", SAMPLE);
    let actions = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}},
            "context": {"diagnostics": []},
        }),
    );
    let titles: Vec<&str> = actions
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Open Graphviz preview",
            "Export graph as SVG",
            "Export graph as PNG"
        ]
    );
    let commands: Vec<&str> = actions
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["command"]["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        commands,
        [
            "graphviz.openPreview",
            "graphviz.exportSvg",
            "graphviz.exportPng"
        ]
    );
    assert_eq!(actions[0]["command"]["arguments"], json!([uri]));
    assert!(actions[0].get("edit").is_none());

    // Format-on-save style requests for specific kinds get nothing.
    let filtered = client.request(
        "textDocument/codeAction",
        json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
            "context": {"diagnostics": [], "only": ["source.organizeImports"]},
        }),
    );
    assert_eq!(filtered, json!([]));
    client.shutdown();
}

#[test]
fn render_timeout_stops_graphviz_and_reports_it() {
    // A dense graph that takes dot far longer than the 200 ms budget.
    let text = dense_graph();
    let mut client = Client::start(Options {
        settings: json!({"timeout_ms": 200, "renderer": "dot"}),
        ..Options::default()
    });
    let started = Instant::now();
    let uri = client.open("big.dot", &text);
    let note = client
        .wait_notification(0, Duration::from_secs(10), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        })
        .unwrap_or_else(|| panic!("no timeout diagnostic; log:\n{}", client.log()));
    assert!(started.elapsed() < Duration::from_secs(5));
    let message = note["params"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.contains("did not finish within 200 ms"),
        "{message}"
    );
    client.shutdown();
}

/// A graph that makes Graphviz recurse deeply (and takes it many seconds).
fn dense_graph() -> String {
    dense(400)
}

/// `n` nodes, each linked to the next eleven: dot needs about a second for
/// 120 nodes, a few for 200 and over ten for 400.
fn dense(n: usize) -> String {
    let mut text = String::from("digraph big {\n");
    for i in 0..n {
        for j in (i + 1)..(i + 12).min(n) {
            text.push_str(&format!("  n{i} -> n{j};\n"));
        }
    }
    text.push_str("}\n");
    text
}

#[cfg(feature = "bundled")]
#[test]
fn bundled_graphviz_has_the_stack_for_deep_recursion_and_is_bounded() {
    // With wasmtime's default 512 KiB stack this graph traps with a stack
    // overflow after about two seconds; it must instead keep running until
    // the timeout stops it.
    let empty = tempfile::tempdir().unwrap();
    let mut client = Client::start(Options {
        settings: json!({"renderer": "bundled", "timeout_ms": 4000}),
        path_env: Some(empty.path().to_path_buf()),
        ..Options::default()
    });
    let uri = client.open("big.dot", &dense_graph());
    let note = client
        .wait_notification(0, Duration::from_secs(60), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        })
        .unwrap_or_else(|| panic!("no diagnostic; log:\n{}", client.log()));
    let message = note["params"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.contains("did not finish within 4000 ms"),
        "{message}"
    );
    client.shutdown();
}

#[test]
fn output_over_the_cap_is_a_diagnostic_and_keeps_the_png() {
    let mut client = Client::start(Options {
        settings: json!({"debounce_ms": 50}),
        env: vec![("GRAPHVIZ_PREVIEW_MAX_SVG_BYTES", "20000".to_string())],
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    let good = std::fs::read(&path).unwrap();

    // Two hundred labelled nodes make far more than 20000 bytes of SVG.
    let mut big = String::from("digraph flow {\n");
    for i in 0..200 {
        big.push_str(&format!("  n{i} [label=\"node number {i}\"];\n"));
    }
    big.push_str("}\n");
    let since = client.notifications.len();
    client.change(&uri, 2, &big);
    let note = client
        .wait_notification(since, Duration::from_secs(10), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|d| !d.is_empty())
        })
        .unwrap_or_else(|| panic!("no diagnostic; log:\n{}", client.log()));
    let message = note["params"]["diagnostics"][0]["message"]
        .as_str()
        .unwrap();
    assert!(
        message.contains("Graphviz wrote more than 19 KiB of SVG"),
        "{message}"
    );
    client.pump(Duration::from_millis(200));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        good,
        "the last good PNG must stay"
    );
    client.shutdown();
}

/// Process ids of the children of `parent`, from `ps`.
#[cfg(unix)]
fn children_of(parent: u32) -> Vec<u32> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            (ppid == parent).then_some(pid)
        })
        .collect()
}

/// Whether `pid` is a live (not zombie) process.
#[cfg(unix)]
fn running(pid: u32) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .expect("ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

#[cfg(unix)]
#[test]
fn exit_stops_a_running_dot() {
    let mut client = Client::start(Options {
        settings: json!({"renderer": "dot", "timeout_ms": 120000}),
        ..Options::default()
    });
    // dot needs well over ten seconds for this graph.
    client.open("big.dot", &dense_graph());
    let deadline = Instant::now() + Duration::from_secs(10);
    let dot = loop {
        if let Some(&pid) = children_of(client.pid()).first() {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "dot never started; log:\n{}",
            client.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(running(dot));
    client.exit();
    let deadline = Instant::now() + Duration::from_secs(1);
    while running(dot) {
        assert!(
            Instant::now() < deadline,
            "dot (pid {dot}) outlived the server; log:\n{}",
            client.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_render_from_before_a_reopen_is_not_applied() {
    // A small scale keeps drawing cheap; dot's layout is what is slow.
    let mut client = Client::start(Options {
        settings: json!({"renderer": "dot", "debounce_ms": 50, "scale": 0.25}),
        ..Options::default()
    });
    let uri = client.open("flow.dot", SAMPLE);
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);

    // Start a slow live render, then close and reopen the file with other
    // text while it runs.
    client.change(&uri, 2, &dense(150));
    client.pump(Duration::from_millis(300));
    client.close(&uri);
    let reopened = client.open("flow.dot", "digraph flow { reopened }\n");
    assert_eq!(reopened, uri);
    client.command("graphviz.openPreview", &uri);
    let after_reopen = std::fs::read(&path).unwrap();

    // Let the slow render finish; its result belongs to the closed document.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !client.log().contains("kind=live") {
        assert!(
            Instant::now() < deadline,
            "the slow render never finished; log:\n{}",
            client.log()
        );
        client.pump(Duration::from_millis(50));
    }
    client.pump(Duration::from_millis(200));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        after_reopen,
        "a result from before the reopen replaced the preview; log:\n{}",
        client.log()
    );
    client.shutdown();
}

#[test]
fn back_to_back_settings_changes_end_on_the_latest() {
    // Drawing at 0.75 takes much longer than at 0.25, so without revision
    // checks the first, outdated render would finish last and win.
    let settings = |scale: f64| json!({"renderer": "dot", "scale": scale, "appearance": "light"});
    let mut client = Client::start(Options {
        settings: settings(0.25),
        ..Options::default()
    });
    let uri = client.open("dense.dot", &dense(120));
    client.command("graphviz.openPreview", &uri);
    let path = path_of(&client.show_document_uris()[0]);
    let width = decode_png(&std::fs::read(&path).unwrap()).width;

    let renders_before = client.log().matches("kind=live").count();
    client.set_settings(settings(0.75));
    client.set_settings(settings(0.25));
    // Wait for both re-renders (or the skip of the first) to settle.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let log = client.log();
        let settled =
            log.matches("kind=live").count() + log.matches("skipped superseded live").count();
        if settled >= renders_before + 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "renders did not settle; log:\n{log}"
        );
        client.pump(Duration::from_millis(50));
    }
    client.pump(Duration::from_millis(300));
    let image = decode_png(&std::fs::read(&path).unwrap());
    assert_eq!(
        image.width, width,
        "the preview does not reflect the latest settings"
    );
    client.shutdown();
}

#[test]
fn images_outside_the_workspace_are_not_drawn() {
    let elsewhere = tempfile::tempdir().unwrap();
    let image = elsewhere.path().join("secret.png");
    // A 20x20 opaque red PNG.
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 20, 20);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[255, 0, 0, 255].repeat(400))
            .unwrap();
    }
    std::fs::write(&image, &png_bytes).unwrap();

    let mut client = Client::start(Options {
        settings: json!({"renderer": "dot", "appearance": "light"}),
        ..Options::default()
    });
    let text = format!(
        "digraph g {{\n  a [shape=box label=\"\" image=\"{}\"];\n}}\n",
        image.display()
    );
    let since = client.notifications.len();
    let uri = client.open("img.dot", &text);
    client.command("graphviz.openPreview", &uri);
    let note = client
        .wait_notification(since, Duration::from_secs(10), |n| {
            diagnostics_for(&uri)(n)
                && n["params"]["diagnostics"].as_array().is_some_and(|d| {
                    d.iter()
                        .any(|d| d["message"].as_str().unwrap_or("").contains("outside"))
                })
        })
        .unwrap_or_else(|| panic!("no warning; log:\n{}", client.log()));
    let warning = note["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["message"].as_str().unwrap().contains("outside"))
        .unwrap()
        .clone();
    assert_eq!(warning["severity"], 2);
    let shown = decode_png(&std::fs::read(path_of(&client.show_document_uris()[0])).unwrap());
    let red = (0..shown.height)
        .flat_map(|y| (0..shown.width).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let [r, g, b, _] = shown.pixel(x, y);
            r > 200 && g < 60 && b < 60
        })
        .count();
    assert_eq!(red, 0, "the outside image was drawn");
    client.shutdown();
}

#[cfg(feature = "bundled")]
fn bundled_client(timeout_ms: u64) -> (Client, tempfile::TempDir) {
    let empty = tempfile::tempdir().unwrap();
    let client = Client::start(Options {
        settings: json!({"renderer": "bundled", "timeout_ms": timeout_ms}),
        path_env: Some(empty.path().to_path_buf()),
        ..Options::default()
    });
    (client, empty)
}

#[cfg(feature = "bundled")]
#[test]
fn a_bundled_render_waiting_its_turn_keeps_its_full_budget() {
    let (mut client, _path) = bundled_client(2000);
    let small = client.open("small.dot", SAMPLE);
    client
        .wait_notification(0, Duration::from_secs(30), diagnostics_for(&small))
        .expect("diagnostics for the small graph");

    // Two slow graphs take the bundled Graphviz in turn, each for its full
    // 2 s budget; the preview of the small graph has to wait for both.
    client.open("big1.dot", &dense_graph());
    client.open("big2.dot", &dense_graph());
    client.pump(Duration::from_millis(300));
    let started = Instant::now();
    client.command("graphviz.openPreview", &small);
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(3000),
        "did not wait: {waited:?}"
    );

    let uris = client.show_document_uris();
    assert_eq!(
        uris.len(),
        1,
        "no preview after waiting; log:\n{}",
        client.log()
    );
    let image = decode_png(&std::fs::read(path_of(&uris[0])).unwrap());
    assert!(image.drawn(60.0) > 500);
    client.shutdown();
}

#[cfg(feature = "bundled")]
#[test]
fn exit_is_prompt_with_bundled_renders_waiting() {
    let (mut client, _path) = bundled_client(120_000);
    // One slow render runs and a second waits for its turn.
    client.open("big1.dot", &dense_graph());
    client.open("big2.dot", &dense_graph());
    client.pump(Duration::from_millis(500));
    let started = Instant::now();
    client.exit();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(5), "exit took {took:?}");
}

/// A 20x20 opaque PNG of one colour.
fn solid_png(rgb: [u8; 3]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 20, 20);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[rgb[0], rgb[1], rgb[2], 255].repeat(400))
            .unwrap();
    }
    bytes
}

#[test]
fn hover_thumbnails_are_cached_per_folder() {
    let mut client = Client::start(Options {
        settings: json!({"renderer": "dot", "appearance": "light"}),
        ..Options::default()
    });
    // The same text in two folders. The image is inside the first folder,
    // so only the first document may draw it.
    let red = client.workspace.path().join("red");
    let blue = client.workspace.path().join("blue");
    std::fs::create_dir(&red).unwrap();
    std::fs::create_dir(&blue).unwrap();
    let picture = red.join("pic.png");
    std::fs::write(&picture, solid_png([255, 0, 0])).unwrap();
    let text = format!(
        "digraph g {{\n  n [shape=box label=\"\" image=\"{}\"];\n}}\n",
        picture.display()
    );
    let mut thumbnails = Vec::new();
    for folder in ["red", "blue"] {
        let uri = client.open(&format!("{folder}/g.dot"), &text);
        let hover = client.request(
            "textDocument/hover",
            json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 2}}),
        );
        let value = hover["contents"]["value"].as_str().unwrap().to_string();
        let start = value.find("base64,").unwrap() + "base64,".len();
        let end = start + value[start..].find(')').unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(&value[start..end])
            .unwrap();
        thumbnails.push(decode_png(&png));
    }
    let red_pixels = |image: &common::Image| image.count(|p| p[0] > 200 && p[1] < 60 && p[2] < 60);
    assert!(
        red_pixels(&thumbnails[0]) > 50,
        "the image is missing in its own folder"
    );
    assert_eq!(
        red_pixels(&thumbnails[1]),
        0,
        "the other folder got the first folder's cached thumbnail"
    );
    client.shutdown();
}
