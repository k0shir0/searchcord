//! Test-only parity driver. Scenarios and expectations belong in Rust tests.
pub mod performance;
use reqwest::{Method, blocking::Client};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    error::Error,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Python,
    Rust,
}

impl Backend {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "python" => Ok(Self::Python),
            "rust" => Ok(Self::Rust),
            _ => Err(format!("unknown parity backend: {value}").into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub kind: String,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpReply {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

impl HttpReply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("HTTP response must be JSON")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub event: String,
    pub id: Option<String>,
    pub data: String,
}

/// Parse completed SSE frames, retaining event names, IDs and multiline data.
pub fn parse_events(body: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut name = String::new();
    let mut id = None;
    let mut data = Vec::new();
    for line in body.lines() {
        if line.is_empty() {
            if !data.is_empty() {
                events.push(Event {
                    event: if name.is_empty() {
                        "message".into()
                    } else {
                        name.clone()
                    },
                    id: id.clone(),
                    data: data.join("\n"),
                });
            }
            name.clear();
            data.clear();
        } else if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => name = value.into(),
                "id" if !value.contains('\0') => id = Some(value.into()),
                "data" => data.push(value.to_owned()),
                _ => {}
            }
        }
    }
    events
}

/// Owns its child before its temporary directory so all handles close on drop.
pub struct Harness {
    backend: Backend,
    viewer: Option<Child>,
    child: Child,
    input: ChildStdin,
    output: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
    root: TempDir,
    client: Client,
    origin: Option<String>,
    pub traces: Vec<Trace>,
}

impl Harness {
    pub fn new() -> Result<Self> {
        let selection = std::env::var("SEARCHCORD_PARITY_BACKEND").unwrap_or("python".into());
        Self::with_backend(Backend::parse(&selection)?)
    }

    pub fn with_backend(backend: Backend) -> Result<Self> {
        if backend == Backend::Rust {
            return Err(
                "Rust backend is unimplemented for parity contracts; no contracts were skipped"
                    .into(),
            );
        }
        Self::fixture_backend(backend)
    }

    /// Only the viewer capability is native. Python remains an explicit fixture
    /// writer; unfinished collector contracts still reject native selection.
    pub fn new_viewer_fixture() -> Result<Self> {
        let backend =
            Backend::parse(&std::env::var("SEARCHCORD_PARITY_BACKEND").unwrap_or("python".into()))?;
        Self::fixture_backend(backend)
    }
    pub fn viewer_with_backend(backend: Backend) -> Result<Self> {
        Self::fixture_backend(backend)
    }
    fn fixture_backend(backend: Backend) -> Result<Self> {
        let root = tempfile::Builder::new()
            .prefix("searchcord-parity-")
            .tempdir()?;
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let python = std::env::var_os("SEARCHCORD_PARITY_PYTHON").unwrap_or("python".into());
        let client = Client::builder()
            .no_proxy() // Only numeric loopback URLs constructed below; never use an upstream proxy.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()?;
        let mut child = Command::new(python)
            .arg("-I")
            .args(["-X", "utf8"])
            .arg("-u")
            .arg(repo.join("crates/searchcord-parity/driver/reference.py"))
            .arg(root.path())
            .current_dir(&repo)
            .env("SEARCHCORD_DATA_DIR", root.path().join("archive"))
            .env(
                "SEARCHCORD_CREDENTIAL_FILE",
                root.path().join("credentials/synthetic.vault"),
            )
            .env(
                "SEARCHCORD_CREDENTIAL_KEY_FILE",
                root.path().join("credentials/synthetic.key"),
            )
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let harness = Self {
            backend,
            viewer: None,
            child,
            input,
            output,
            reader: Some(reader),
            root,
            client,
            origin: None,
            traces: Vec::new(),
        };
        let ready = harness.receive()?;
        if ready != json!({"ready":true,"protocol":1}) {
            return Err(format!("unexpected reference handshake: {ready}").into());
        }
        Ok(harness)
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }
    pub fn archive(&self) -> PathBuf {
        self.root().join("archive/synthetic.db")
    }
    pub fn vault(&self) -> PathBuf {
        self.root().join("credentials/synthetic.vault")
    }
    pub fn database(&self) -> Connection {
        Connection::open(self.archive()).expect("open disposable SQLite archive")
    }

    fn receive(&self) -> Result<Value> {
        let line = self.output.recv_timeout(Duration::from_secs(20))??;
        Ok(serde_json::from_str(&line)?)
    }

    /// Raw response, including the original Python exception class on failure.
    pub fn exchange(&mut self, request: Value) -> Value {
        if self.backend == Backend::Rust && request["op"] == "readonly_execute" {
            let archive = searchcord_archive::Archive::new(self.archive(), true);
            let session = archive.connect().expect("native readonly session");
            return match session
                .database()
                .execute_batch(request["sql"].as_str().unwrap())
            {
                Ok(()) => json!({"ok":true,"result":null}),
                Err(e) => {
                    json!({"ok":false,"error":{"type":"OperationalError","message":e.to_string()}})
                }
            };
        }
        self.traces.push(Trace {
            kind: "command".into(),
            data: request.clone(),
        });
        writeln!(self.input, "{request}").expect("write reference command");
        self.input.flush().expect("flush reference command");
        let reply = self
            .receive()
            .unwrap_or_else(|e| panic!("reference failed: {e}; traces: {:?}", self.traces));
        self.traces.push(Trace {
            kind: "reply".into(),
            data: reply.clone(),
        });
        reply
    }

    pub fn command(&mut self, request: Value) -> Value {
        let reply = self.exchange(request);
        assert_eq!(reply["ok"], true, "reference error: {reply}");
        reply["result"].clone()
    }

    pub fn init(&mut self) -> Value {
        self.command(json!({"op":"init"}))
    }

    pub fn start_viewer(&mut self, archive: &str, immutable: bool, window: Option<u64>) {
        if self.backend == Backend::Rust {
            if let Some(mut child) = self.viewer.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let binary = std::env::var_os("SEARCHCORD_PARITY_RUST_VIEWER")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join(format!("searchcord-search{}", std::env::consts::EXE_SUFFIX))
                });
            let mut cmd = Command::new(binary);
            cmd.arg("--db")
                .arg(self.root().join("archive").join(archive))
                .args(["--port", "0"]);
            if immutable {
                cmd.arg("--snapshot");
            }
            if let Some(window) = window {
                cmd.env("SEARCHCORD_VIEWER_SCAN_WINDOW", window.to_string());
            }
            let mut child = cmd
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("build native viewer before running native HTTP contracts");
            let stdout = child.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            let reader = thread::spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
                let _ = tx.send(result);
            });
            self.viewer = Some(child);
            let line = rx
                .recv_timeout(Duration::from_secs(15))
                .expect("native viewer readiness timeout")
                .unwrap();
            reader.join().unwrap();
            let origin = line
                .trim()
                .strip_prefix("Searchcord: ")
                .expect("native viewer readiness URL");
            assert!(origin.starts_with("http://127.0.0.1:"));
            self.origin = Some(origin.to_owned());
            return;
        }
        let reply = self.command(
            json!({"op":"start_viewer","archive":archive,"immutable":immutable,"window":window}),
        );
        let port = reply["port"].as_u64().expect("viewer port");
        assert!((1..=65535).contains(&port));
        self.origin = Some(format!("http://127.0.0.1:{port}"));
    }

    pub fn start_collector(&mut self) {
        let reply = self.command(json!({"op":"job","action":"start_http"}));
        let port = reply["port"].as_u64().expect("collector port");
        assert!((1..=65535).contains(&port));
        self.origin = Some(format!("http://127.0.0.1:{port}"));
    }

    pub fn origin(&self) -> &str {
        self.origin.as_deref().expect("start HTTP first")
    }

    /// Read a bounded prefix of an infinite production SSE stream, then close it.
    pub fn event_prefix(&mut self, path: &str, count: usize) -> Vec<Event> {
        assert!(
            path.starts_with('/') && !path.starts_with("//") && !path.contains(['\\', '#', '?'])
        );
        assert!((1..=100).contains(&count));
        let response = self
            .client
            .get(format!("{}{path}", self.origin()))
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        let mut reader = BufReader::new(response);
        let mut body = String::new();
        loop {
            let mut line = String::new();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "SSE ended before requested events"
            );
            body.push_str(&line);
            let events = parse_events(&body);
            if events.len() >= count {
                for event in &events {
                    self.traces.push(Trace {
                        kind: "event".into(),
                        data: serde_json::to_value(event).unwrap(),
                    });
                }
                return events;
            }
        }
    }

    pub fn request(&mut self, method: &str, path: &str, params: &[(&str, String)]) -> HttpReply {
        assert!(
            path.starts_with('/') && !path.starts_with("//") && !path.contains(['\\', '#', '?']),
            "only local paths without query strings"
        );
        let url = format!(
            "{}{path}",
            self.origin.as_ref().expect("start viewer first")
        );
        self.traces.push(Trace {
            kind: "request".into(),
            data: json!({"method":method,"path":path,"params":params}),
        });
        let response = self
            .client
            .request(Method::from_bytes(method.as_bytes()).unwrap(), url)
            .query(params)
            .send()
            .expect("local HTTP request");
        let reply = HttpReply {
            status: response.status().as_u16(),
            headers: response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
                .collect(),
            body: response.text().expect("response body"),
        };
        self.traces.push(Trace {
            kind: "response".into(),
            data: serde_json::to_value(&reply).unwrap(),
        });
        reply
    }

    pub fn get(&mut self, path: &str, params: &[(&str, String)]) -> HttpReply {
        self.request("GET", path, params)
    }

    pub fn events(&mut self, path: &str) -> Vec<Event> {
        let reply = self.get(path, &[]);
        assert_eq!(reply.status, 200);
        assert!(reply.headers["content-type"].starts_with("text/event-stream"));
        let events = parse_events(&reply.body);
        for event in &events {
            self.traces.push(Trace {
                kind: "event".into(),
                data: serde_json::to_value(event).unwrap(),
            });
        }
        events
    }

    /// Explicitly export synthetic traces into this test's isolated directory.
    pub fn write_trace(&self) -> Result<PathBuf> {
        let path = self.root().join("trace.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&self.traces)?)?;
        Ok(path)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(mut child) = self.viewer.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = writeln!(self.input, "{{\"op\":\"shutdown\"}}");
        let _ = self.input.flush();
        let deadline = Instant::now() + Duration::from_secs(3);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
