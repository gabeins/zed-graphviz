//! A minimal LSP client that drives the real `graphviz-preview` binary over
//! stdio, the way Zed does: it answers `window/showDocument` with success and
//! `workspace/configuration` with the test's settings.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SAMPLE: &str =
    "// A small sample graph\ndigraph flow {\n  alpha -> beta;\n  beta -> gamma;\n}\n";

pub struct Client {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    incoming: Receiver<Value>,
    /// Every notification received so far, in order.
    pub notifications: Vec<Value>,
    /// Params of every `window/showDocument` request, in order.
    pub show_documents: Arc<Mutex<Vec<Value>>>,
    pub stderr: Arc<Mutex<String>>,
    /// What `workspace/configuration` answers, like Zed's current settings.
    settings: Arc<Mutex<Value>>,
    next_id: i64,
    pub cache: tempfile::TempDir,
    pub workspace: tempfile::TempDir,
}

pub struct Options {
    pub settings: Value,
    /// Replaces PATH for the server process.
    pub path_env: Option<PathBuf>,
    /// Extra environment variables for the server process.
    pub env: Vec<(&'static str, String)>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            settings: json!({}),
            path_env: None,
            env: Vec::new(),
        }
    }
}

fn write_message(stdin: &Mutex<ChildStdin>, message: &Value) {
    let body = message.to_string();
    let mut stdin = stdin.lock().unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length: ") {
            length = v.parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

impl Client {
    pub fn start(options: Options) -> Client {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_graphviz-preview"));
        command
            .env("GRAPHVIZ_PREVIEW_CACHE_DIR", cache.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = &options.path_env {
            command.env("PATH", path);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn graphviz-preview");
        let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();

        let stderr = Arc::new(Mutex::new(String::new()));
        {
            let stderr = stderr.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr_pipe).lines().map_while(Result::ok) {
                    let mut s = stderr.lock().unwrap();
                    s.push_str(&line);
                    s.push('\n');
                }
            });
        }

        let show_documents = Arc::new(Mutex::new(Vec::new()));
        let settings = Arc::new(Mutex::new(options.settings.clone()));
        let (tx, incoming) = mpsc::channel();
        {
            let stdin = stdin.clone();
            let show_documents = show_documents.clone();
            let settings = settings.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                while let Some(message) = read_message(&mut reader) {
                    let is_request = message.get("method").is_some() && message.get("id").is_some();
                    if !is_request {
                        if tx.send(message).is_err() {
                            break;
                        }
                        continue;
                    }
                    let id = message["id"].clone();
                    let result = match message["method"].as_str() {
                        Some("window/showDocument") => {
                            show_documents
                                .lock()
                                .unwrap()
                                .push(message["params"].clone());
                            json!({"success": true})
                        }
                        Some("workspace/configuration") => {
                            let items = message["params"]["items"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default();
                            Value::Array(
                                items
                                    .iter()
                                    .map(|item| match item["section"].as_str() {
                                        Some("graphviz-preview") => {
                                            settings.lock().unwrap().clone()
                                        }
                                        _ => Value::Null,
                                    })
                                    .collect(),
                            )
                        }
                        _ => Value::Null,
                    };
                    write_message(
                        &stdin,
                        &json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    );
                }
            });
        }

        let mut client = Client {
            child,
            stdin,
            incoming,
            notifications: Vec::new(),
            show_documents,
            stderr,
            settings,
            next_id: 0,
            cache,
            workspace,
        };
        let root = url(client.workspace.path());
        client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root,
                "capabilities": {
                    "workspace": {"configuration": true, "didChangeConfiguration": {"dynamicRegistration": false}},
                    "window": {"showDocument": {"support": true}},
                    "textDocument": {"hover": {"contentFormat": ["markdown"]}},
                },
                "initializationOptions": {"graphviz-preview": options.settings},
            }),
        );
        client.notify("initialized", json!({}));
        client
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Changes the settings the way Zed does: the new value is pushed and
    /// also returned by later `workspace/configuration` requests.
    pub fn set_settings(&mut self, settings: Value) {
        *self.settings.lock().unwrap() = settings.clone();
        self.notify(
            "workspace/didChangeConfiguration",
            json!({"settings": {"graphviz-preview": settings}}),
        );
    }

    pub fn close(&mut self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": uri}}),
        );
    }

    /// Sends `shutdown` and `exit` and waits for the process to end.
    pub fn exit(&mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                assert!(status.success(), "server exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("server did not exit after shutdown/exit");
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        write_message(
            &self.stdin,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        );
    }

    /// Sends a request and waits for its result. Panics on an error response.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        write_message(
            &self.stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let message = self
                .next_message(deadline)
                .unwrap_or_else(|| panic!("no response to {method}; server log:\n{}", self.log()));
            if message.get("id") == Some(&json!(id)) {
                if let Some(error) = message.get("error") {
                    panic!("{method} failed: {error}; server log:\n{}", self.log());
                }
                return message.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }

    fn next_message(&mut self, deadline: Instant) -> Option<Value> {
        let timeout = deadline.saturating_duration_since(Instant::now());
        match self.incoming.recv_timeout(timeout) {
            Ok(message) => {
                if message.get("id").is_none() {
                    self.notifications.push(message.clone());
                }
                Some(message)
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Collects notifications for `duration`.
    pub fn pump(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while self.next_message(deadline).is_some() {}
    }

    /// Waits for a notification matching `pred`, looking at ones already
    /// received after index `since` first.
    pub fn wait_notification(
        &mut self,
        since: usize,
        timeout: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Option<Value> {
        if let Some(found) = self.notifications.iter().skip(since).find(|n| pred(n)) {
            return Some(found.clone());
        }
        let deadline = Instant::now() + timeout;
        while let Some(message) = self.next_message(deadline) {
            if message.get("id").is_none() && pred(&message) {
                return Some(message);
            }
        }
        None
    }

    pub fn log(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    pub fn show_document_uris(&self) -> Vec<String> {
        self.show_documents
            .lock()
            .unwrap()
            .iter()
            .map(|p| p["uri"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Writes `text` to `name` in the workspace and opens it. Returns its URI.
    pub fn open(&mut self, name: &str, text: &str) -> String {
        let path = self.workspace.path().join(name);
        std::fs::write(&path, text).unwrap();
        let uri = url(&path);
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "dot", "version": 1, "text": text}}),
        );
        uri
    }

    pub fn change(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
    }

    pub fn command(&mut self, command: &str, uri: &str) -> Value {
        self.request(
            "workspace/executeCommand",
            json!({"command": command, "arguments": [uri]}),
        )
    }

    pub fn shutdown(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                assert!(status.success(), "server exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("server did not exit after shutdown/exit");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

pub fn path_of(uri: &str) -> PathBuf {
    url::Url::parse(uri).unwrap().to_file_path().unwrap()
}

/// Decoded PNG: width, height and RGBA pixels.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Pixels that satisfy `pred`.
    pub fn count(&self, pred: impl Fn([u8; 4]) -> bool) -> usize {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|&&p| pred(p))
            .count()
    }

    /// Pixels that differ from the background (the top-left pixel) by more
    /// than `by` in luma: the drawn nodes, edges and text.
    pub fn drawn(&self, by: f32) -> usize {
        let background = self.luma(0, 0);
        self.count(|p| (luma(p) - background).abs() > by)
    }

    pub fn luma(&self, x: u32, y: u32) -> f32 {
        let [r, g, b, _] = self.pixel(x, y);
        0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
    }
}

pub fn luma([r, g, b, _]: [u8; 4]) -> f32 {
    0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
}

/// Decodes a PNG with the `png` crate (independent of the encoder used by
/// the server). Panics if the bytes are not a valid, non-empty PNG.
pub fn decode_png(bytes: &[u8]) -> Image {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().expect("PNG header");
    let mut buf = vec![0; reader.output_buffer_size().expect("PNG size")];
    let info = reader.next_frame(&mut buf).expect("PNG frame");
    assert!(info.width > 0 && info.height > 0, "empty PNG");
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => data
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        other => panic!("unexpected PNG colour type {other:?}"),
    };
    Image {
        width: info.width,
        height: info.height,
        rgba,
    }
}

/// Reads a file, polling until it exists.
pub fn wait_for_file(path: &Path, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                return Some(bytes);
            }
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
