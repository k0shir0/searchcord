use searchcord_archive::media;
use searchcord_collector::{
    PageContext, Store,
    media_collection::{
        self, BATCH_SECONDS, FILE_SECONDS, Limits, MAX_BYTES, MediaCollector, PARALLEL_DOWNLOADS,
    },
    pacing::Control,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

const URL: &str = "https://cdn.discordapp.com/attachments/20/40/object.png?ex=123&hm=signature";

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
    body_delay: Duration,
    chunked: bool,
    declared: Option<usize>,
}
impl Reply {
    fn bytes(body: &[u8]) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "Image/PNG; ignored=charset".into())],
            body: body.to_vec(),
            delay: Duration::ZERO,
            body_delay: Duration::ZERO,
            chunked: false,
            declared: None,
        }
    }
}
struct State {
    replies: HashMap<String, Reply>,
    trace: Mutex<Vec<(String, Vec<String>)>>,
    active: AtomicUsize,
    maximum: AtomicUsize,
    stop: AtomicBool,
}
struct Fixture {
    state: Arc<State>,
    server: Option<thread::JoinHandle<()>>,
    directory: tempfile::TempDir,
    origin: String,
    path: PathBuf,
}
impl Fixture {
    fn new(replies: HashMap<String, Reply>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.db");
        std::fs::write(&path, b"synthetic path identity only").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(State {
            replies,
            trace: Mutex::new(vec![]),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let shared = state.clone();
        let server = thread::spawn(move || {
            let mut handlers = vec![];
            while !shared.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shared = shared.clone();
                        handlers.push(thread::spawn(move || serve(stream, &shared)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("fixture listener: {error}"),
                }
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        Self {
            state,
            server: Some(server),
            directory,
            origin,
            path,
        }
    }
    fn collector(&self, limits: Limits) -> MediaCollector {
        MediaCollector::for_loopback(&self.path, &self.origin, limits).unwrap()
    }
    fn trace(&self) -> Vec<(String, Vec<String>)> {
        self.state.trace.lock().unwrap().clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
    }
}
fn serve(mut stream: TcpStream, state: &State) {
    // Accepted sockets may inherit the listener's nonblocking mode on Windows
    // and macOS. This fixture's request parser uses blocking buffered reads.
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let path = first.split_whitespace().nth(1).unwrap_or("").to_owned();
    let mut headers = vec![];
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        headers.push(line.trim().to_owned());
    }
    state.trace.lock().unwrap().push((path.clone(), headers));
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.maximum.fetch_max(active, Ordering::SeqCst);
    let reply = state
        .replies
        .get(path.split('?').next().unwrap())
        .cloned()
        .unwrap_or_else(|| Reply {
            status: 404,
            ..Reply::bytes(b"")
        });
    thread::sleep(reply.delay);
    let mut response = format!("HTTP/1.1 {} fixture\r\nConnection: close\r\n", reply.status);
    for (key, value) in &reply.headers {
        response.push_str(&format!("{key}: {value}\r\n"));
    }
    if reply.chunked {
        response.push_str("Transfer-Encoding: chunked\r\n\r\n");
    } else {
        response.push_str(&format!(
            "Content-Length: {}\r\n\r\n",
            reply.declared.unwrap_or(reply.body.len())
        ));
    }
    if stream.write_all(response.as_bytes()).is_ok() {
        thread::sleep(reply.body_delay);
        if reply.chunked {
            for bytes in reply.body.chunks(257) {
                if write!(stream, "{:x}\r\n", bytes.len())
                    .and_then(|_| stream.write_all(bytes))
                    .and_then(|_| stream.write_all(b"\r\n"))
                    .is_err()
                {
                    break;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        } else {
            let _ = stream.write_all(&reply.body);
        }
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
}
fn without_time(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("observed_at");
    }
    value
}
fn no_temporary(path: &Path) {
    let directory = media::directory(path);
    if !directory.exists() {
        return;
    }
    for entry in std::fs::read_dir(directory).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".searchcord-media-")
        );
    }
}

fn python(operation: &str, request: Value) -> Value {
    // Mechanical production-function adapter. All scenario data/assertions stay in Rust.
    let script = r#"
import asyncio,json,sys
from pathlib import Path
sys.path.insert(0,sys.argv[1])
def offline(event,args):
    if event=='socket.connect' and isinstance(args[1],tuple) and args[1][0] in {'127.0.0.1','::1'}: return
    if event in {'socket.connect','socket.getaddrinfo','subprocess.Popen','os.system'}: raise RuntimeError('live network disabled')
sys.addaudithook(offline)
import media_store,httpx
from unittest.mock import patch
request=json.loads(sys.argv[3])
if sys.argv[2]=='urls': result=media_store.message_media_urls(request['messages'])
else:
    async def run():
        async def reply(req): return httpx.Response(request['status'],content=bytes(request['body']),headers=request.get('headers',{}))
        async with httpx.AsyncClient(transport=httpx.MockTransport(reply),follow_redirects=False,trust_env=False) as client:
            with patch.object(media_store,'MAX_BYTES',request.get('max_bytes',media_store.MAX_BYTES)):
                return await media_store.cache_media(request['path'],request['url'],client=client)
    result=asyncio.run(run())
print(json.dumps(result,ensure_ascii=False))
"#;
    let interpreter = std::env::var_os("SEARCHCORD_PARITY_PYTHON").unwrap_or("python".into());
    let reference = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("reference")
        .canonicalize()
        .unwrap();
    let output = Command::new(interpreter)
        .args(["-I", "-X", "utf8", "-c", script])
        .arg(&reference)
        .arg(operation)
        .arg(request.to_string())
        .current_dir(&reference)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn media_candidates_preserve_order_last_signature_and_supported_profile_assets() {
    let messages = json!([
        {"attachments":[{"url":URL},null,{"url":"https://external.test/a.png"},{"url":"https://cdn.discordapp.com/attachments/20/41/video.mp4"}],
         "embeds":[{"image":{"proxy_url":"https://media.discordapp.net/attachments/20/42/proxy.png","url":"https://external.test/no"},"thumbnail":{"url":"https://cdn.discordapp.com/attachments/20/43/thumb.jpg"},"video":{"url":"https://cdn.discordapp.com/attachments/20/41/video.mp4"}}],
         "author":{"id":"30","avatar":"a_hash","banner":"Banner"}},
        {"attachments":[{"url":"https://cdn.discordapp.com/attachments/20/40/object.png?hm=new&ex=456"}],"author":{"id":31,"avatar":"invalid_!"}},
        {"attachments":null,"embeds":[],"author":null},
        {"author":{"id":"١²","avatar":"Unicode"}}
    ]);
    let native = media_collection::message_media_urls(messages.as_array().unwrap());
    assert_eq!(json!(native), python("urls", json!({"messages":messages})));
    assert_eq!(native.len(), 7);
    assert!(native[0].contains("hm=new"));
    assert!(native[4].contains("avatars/30/a_hash.gif?size=256"));
    assert_eq!(
        (MAX_BYTES, FILE_SECONDS, BATCH_SECONDS, PARALLEL_DOWNLOADS),
        (25 * 1024 * 1024, 15, 30, 4)
    );
}

#[tokio::test]
async fn concurrent_collectors_publish_the_same_object_once() {
    let mut reply = Reply::bytes(b"shared archive object");
    reply.body_delay = Duration::from_millis(200);
    let fixture = Fixture::new(HashMap::from([(
        "/attachments/20/40/object.png".into(),
        reply,
    )]));
    // Independently constructed collectors must share ownership for this key.
    let first = fixture.collector(Limits::default());
    let second = fixture.collector(Limits::default());
    let (a, b) = tokio::join!(first.cache_media(URL, None), second.cache_media(URL, None));
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(fixture.state.trace.lock().unwrap().len(), 1);
    assert_eq!(media::status(&fixture.path, URL, true)["status"], "saved");
    no_temporary(&fixture.path);
}

#[tokio::test]
async fn native_cache_matches_python_manifest_digests_reuses_signature_and_repairs_damage() {
    let content: &[u8] = b"synthetic PNG bytes\0with no credentials";
    let fixture = Fixture::new(HashMap::from([(
        "/attachments/20/40/object.png".into(),
        Reply::bytes(content),
    )]));
    let collector = fixture.collector(Limits::default());
    let result = collector.cache_media(URL, None).await.unwrap();
    let python_path = fixture.directory.path().join("reference/archive.db");
    std::fs::create_dir_all(python_path.parent().unwrap()).unwrap();
    let reference = python(
        "cache",
        json!({"path":python_path,"url":URL,"status":200,"body":content,"headers":{"content-type":"Image/PNG; ignored=charset"}}),
    );
    assert_eq!(without_time(result.clone()), without_time(reference));
    assert_eq!(result["status"], "saved");
    assert_eq!(result["content_type"], "image/png");
    assert!(chrono_timestamp(result["observed_at"].as_str().unwrap()));
    assert_eq!(media::status(&fixture.path, URL, true), result);
    let key = media::media_key(URL).unwrap();
    let binary = media::directory(&fixture.path).join(format!("{key}.bin"));
    assert_eq!(std::fs::read(&binary).unwrap(), content);
    let cancelled = Control::default();
    cancelled.stop();
    assert_eq!(
        collector
            .cache_media(
                "https://cdn.discordapp.com/attachments/20/40/object.png?hm=replaced",
                Some(cancelled)
            )
            .await
            .unwrap(),
        result
    );
    assert_eq!(fixture.trace().len(), 1);
    std::fs::write(&binary, vec![0; content.len()]).unwrap();
    assert_eq!(
        collector.cache_media(URL, None).await.unwrap()["status"],
        "saved"
    );
    assert_eq!(std::fs::read(binary).unwrap(), content);
    assert_eq!(fixture.trace().len(), 2);
    for (_, headers) in fixture.trace() {
        assert!(headers.iter().all(|header| {
            !header.to_ascii_lowercase().starts_with("authorization:")
                && !header.to_ascii_lowercase().starts_with("cookie:")
        }));
    }
    no_temporary(&fixture.path);
}

fn chrono_timestamp(timestamp: &str) -> bool {
    timestamp.ends_with("+00:00") && timestamp.contains('T') && timestamp.contains('.')
}

#[tokio::test]
async fn failures_redirects_declared_and_streamed_caps_match_python_and_keep_message_identity() {
    let replies = HashMap::from([
        (
            "/attachments/20/40/object.png".into(),
            Reply {
                status: 404,
                ..Reply::bytes(b"")
            },
        ),
        (
            "/redirect".into(),
            Reply {
                status: 302,
                headers: vec![("Location".into(), "http://external.test/private".into())],
                ..Reply::bytes(b"")
            },
        ),
        (
            "/declared".into(),
            Reply {
                declared: Some(MAX_BYTES as usize + 1),
                ..Reply::bytes(b"")
            },
        ),
        (
            "/chunked".into(),
            Reply {
                chunked: true,
                ..Reply::bytes(&vec![1; 2048])
            },
        ),
        (
            "/broken".into(),
            Reply {
                declared: Some(10),
                ..Reply::bytes(b"short")
            },
        ),
    ]);
    let fixture = Fixture::new(replies);
    std::fs::remove_file(&fixture.path).unwrap();
    let mut store = Store::open(&fixture.path).unwrap();
    let original = json!({"id":"1450000000000000001","author":{"id":"30","username":"Alice"},"content":"message stays saved","attachments":[{"url":URL}]});
    store
        .save_page(
            std::slice::from_ref(&original),
            &PageContext {
                channel_id: "20",
                channel_name: "chat",
                ..Default::default()
            },
        )
        .unwrap();
    let collector = fixture.collector(Limits {
        max_bytes: 1024,
        ..Default::default()
    });
    for (url, reason) in [
        (URL, "http_404"),
        ("https://cdn.discordapp.com/redirect", "http_302"),
        ("https://cdn.discordapp.com/declared", "size_limit"),
        ("https://cdn.discordapp.com/chunked", "size_limit"),
        ("https://cdn.discordapp.com/broken", "network_error"),
    ] {
        let result = collector.cache_media(url, None).await.unwrap();
        assert_eq!(result["status"], "unavailable");
        assert_eq!(result["reason"], reason, "{result}");
        assert_eq!(media::status(&fixture.path, url, true), result);
        assert!(
            !media::directory(&fixture.path)
                .join(format!("{}.bin", media::media_key(url).unwrap()))
                .exists()
        );
    }
    let reference_path = fixture.directory.path().join("reference/archive.db");
    std::fs::create_dir_all(reference_path.parent().unwrap()).unwrap();
    let expected = python(
        "cache",
        json!({"path":reference_path,"url":URL,"status":404,"body":[],"max_bytes":1024}),
    );
    assert_eq!(
        without_time(media::status(&fixture.path, URL, true)),
        without_time(expected)
    );
    assert_eq!(
        store
            .read_message(1450000000000000001, 0, 100)
            .unwrap()
            .unwrap()["payload"],
        original
    );
    assert_eq!(fixture.trace().len(), 5); // redirects never reach a second origin
    assert_eq!(
        collector
            .cache_media("http://127.0.0.1/private", None)
            .await
            .unwrap(),
        json!({"status":"unavailable","reason":"unsupported_host"})
    );
    no_temporary(&fixture.path);
}

#[tokio::test]
async fn gzip_decoding_caps_decoded_bytes_and_records_decoded_digest() {
    let content = vec![b'x'; 2048];
    let mut gzip = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gzip.write_all(&content).unwrap();
    let compressed = gzip.finish().unwrap();
    let fixture = Fixture::new(HashMap::from([(
        "/compressed".into(),
        Reply {
            headers: vec![("Content-Encoding".into(), "gzip".into())],
            ..Reply::bytes(&compressed)
        },
    )]));
    let collector = fixture.collector(Limits {
        max_bytes: 1024,
        ..Default::default()
    });
    assert_eq!(
        collector
            .cache_media("https://cdn.discordapp.com/compressed", None)
            .await
            .unwrap()["reason"],
        "size_limit"
    );
    let collector = fixture.collector(Limits::default());
    let result = collector
        .cache_media("https://cdn.discordapp.com/compressed", None)
        .await
        .unwrap();
    assert_eq!(result["status"], "saved");
    assert_eq!(result["bytes"], 2048);
    assert_eq!(
        media::file_digest(&media::directory(&fixture.path).join(format!(
            "{}.bin",
            media::media_key("https://cdn.discordapp.com/compressed").unwrap()
        )))
        .unwrap(),
        result["sha256"]
    );
    no_temporary(&fixture.path);
}

#[tokio::test]
async fn batch_workers_share_deadline_cap_parallelism_and_cancel_without_partial_publication() {
    let replies = (0..8)
        .map(|n| {
            (
                format!("/slow/{n}"),
                Reply {
                    delay: Duration::from_millis(350),
                    ..Reply::bytes(b"late")
                },
            )
        })
        .collect();
    let fixture = Fixture::new(replies);
    let collector = fixture.collector(Limits {
        file_time: Duration::from_millis(160),
        batch_time: Duration::from_millis(160),
        ..Default::default()
    });
    let messages = json!([{"attachments":(0..8).map(|n|json!({"url":format!("https://cdn.discordapp.com/slow/{n}")})).collect::<Vec<_>>()}]);
    let results = collector
        .archive_message_media(messages.as_array().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(results.len(), 8);
    assert!(results.iter().all(|r| r["status"] == "unavailable"));
    assert_eq!(
        results
            .iter()
            .filter(|r| r["reason"] == "time_limit")
            .count(),
        4
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r["reason"] == "batch_time_limit")
            .count(),
        4
    );
    assert_eq!(fixture.state.maximum.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.trace().len(), 4);
    no_temporary(&fixture.path);
    let control = Control::default();
    control.stop();
    assert!(
        collector
            .archive_message_media(messages.as_array().unwrap(), Some(control.clone()))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        collector
            .cache_media("https://cdn.discordapp.com/cancelled", Some(control))
            .await
            .is_err()
    );
    assert_eq!(fixture.trace().len(), 4);
}

#[tokio::test]
async fn storage_failures_and_clear_media_keep_unowned_paths() {
    let fixture = Fixture::new(HashMap::new());
    let collector = fixture.collector(Limits {
        disk_free_bytes: Some(0),
        ..Default::default()
    });
    let result = collector.cache_media(URL, None).await.unwrap();
    assert_eq!(result["reason"], "disk_space");
    assert!(fixture.trace().is_empty());
    let directory = media::directory(&fixture.path);
    let key = media::media_key(URL).unwrap();
    std::fs::write(directory.join(format!("{key}.bin")), b"owned").unwrap();
    std::fs::write(directory.join(".searchcord-media-unused_1.tmp"), b"owned").unwrap();
    std::fs::write(directory.join("keep.txt"), b"user").unwrap();
    std::fs::create_dir(directory.join(format!("{}.bin", "a".repeat(64)))).unwrap();
    assert_eq!(media_collection::clear_media(&fixture.path).unwrap(), 3);
    assert_eq!(std::fs::read(directory.join("keep.txt")).unwrap(), b"user");
    assert!(directory.join(format!("{}.bin", "a".repeat(64))).is_dir());
    std::fs::remove_dir(directory.join(format!("{}.bin", "a".repeat(64)))).unwrap();
    std::fs::remove_file(directory.join("keep.txt")).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    std::fs::write(&directory, b"not a directory").unwrap();
    let failed = collector.cache_media(URL, None).await.unwrap();
    assert_eq!(failed["reason"], "storage_error");
    assert!(fixture.trace().is_empty());
    assert_eq!(std::fs::read(directory).unwrap(), b"not a directory");
    for origin in [
        "https://127.0.0.1:1",
        "http://localhost:1",
        "http://user:pass@127.0.0.1:1",
        "http://127.0.0.1:1/path",
        "http://127.0.0.1:1/?q=x",
        "http://example.com",
    ] {
        assert!(MediaCollector::for_loopback(&fixture.path, origin, Limits::default()).is_err());
    }
}

#[tokio::test]
async fn stopping_active_download_drains_worker_and_removes_temporary_binary() {
    let fixture = Fixture::new(HashMap::from([(
        "/cancel-active".into(),
        Reply {
            body_delay: Duration::from_millis(150),
            ..Reply::bytes(b"partial")
        },
    )]));
    let collector = fixture.collector(Limits::default());
    let control = Control::default();
    let task = {
        let control = control.clone();
        tokio::spawn(async move {
            collector
                .cache_media("https://cdn.discordapp.com/cancel-active", Some(control))
                .await
        })
    };
    for _ in 0..100 {
        if !fixture.trace().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(fixture.trace().len(), 1);
    tokio::time::sleep(Duration::from_millis(20)).await;
    control.stop();
    let result = task.await.unwrap();
    assert!(result.is_err());
    assert!(
        std::fs::read_dir(media::directory(&fixture.path))
            .unwrap()
            .next()
            .is_none()
    );
    no_temporary(&fixture.path);
}

#[tokio::test]
async fn paused_scrape_media_skips_downloads_while_other_media_collection_continues() {
    let fixture = Fixture::new(HashMap::from([
        ("/first".into(), Reply::bytes(b"first")),
        ("/second".into(), Reply::bytes(b"second")),
    ]));
    let collector = fixture.collector(Limits::default());
    let messages = json!([{"attachments":[
        {"url":"https://cdn.discordapp.com/first"},
        {"url":"https://cdn.discordapp.com/second"}
    ]}]);
    let control = Control::default();
    control.pause();
    assert!(
        collector
            .archive_scrape_media(messages.as_array().unwrap(), control.clone())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(fixture.trace().is_empty());
    let results = collector
        .archive_message_media(messages.as_array().unwrap(), Some(control))
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| result["status"] == "saved"));
    assert_eq!(fixture.trace().len(), 2);
    no_temporary(&fixture.path);
}

#[tokio::test]
async fn pausing_scrape_media_drains_active_download_without_starting_pending_downloads() {
    let fixture = Fixture::new(HashMap::from([
        (
            "/pause-active".into(),
            Reply {
                body_delay: Duration::from_millis(150),
                ..Reply::bytes(b"partial")
            },
        ),
        ("/pending".into(), Reply::bytes(b"pending")),
    ]));
    let collector = fixture.collector(Limits {
        parallel_downloads: 1,
        ..Default::default()
    });
    let messages = json!([{"attachments":[
        {"url":"https://cdn.discordapp.com/pause-active"},
        {"url":"https://cdn.discordapp.com/pending"}
    ]}]);
    let control = Control::default();
    let task = {
        let control = control.clone();
        tokio::spawn(async move {
            collector
                .archive_scrape_media(messages.as_array().unwrap(), control)
                .await
        })
    };
    for _ in 0..100 {
        if !fixture.trace().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(fixture.trace().len(), 1);
    control.pause();
    assert!(task.await.unwrap().is_err());
    assert_eq!(fixture.trace().len(), 1);
    assert!(
        std::fs::read_dir(media::directory(&fixture.path))
            .unwrap()
            .next()
            .is_none()
    );
    no_temporary(&fixture.path);
}

#[cfg(unix)]
#[tokio::test]
async fn media_directory_symlinks_cannot_escape_archive() {
    let fixture = Fixture::new(HashMap::new());
    let external = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(external.path(), media::directory(&fixture.path)).unwrap();
    assert_eq!(
        fixture
            .collector(Limits::default())
            .cache_media(URL, None)
            .await
            .unwrap()["reason"],
        "storage_error"
    );
    assert!(media_collection::clear_media(&fixture.path).is_err());
    assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 0);
    assert!(fixture.trace().is_empty());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_media_junction_cannot_escape_archive() {
    use std::os::windows::process::CommandExt;
    let fixture = Fixture::new(HashMap::new());
    let external = tempfile::tempdir().unwrap();
    let script = fixture.directory.path().join("create-junction.ps1");
    std::fs::write(&script,"param([string]$Link, [string]$Target)\n$ErrorActionPreference = 'Stop'\nNew-Item -ItemType Junction -Path $Link -Target $Target | Out-Null\n").unwrap();
    let output = Command::new("powershell.exe")
        .creation_flags(0x08000000)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg(media::directory(&fixture.path))
        .arg(external.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let key = media::media_key(URL).unwrap();
    std::fs::write(external.path().join(format!("{key}.bin")), b"outside").unwrap();
    std::fs::write(external.path().join(format!("{key}.json")),json!({"status":"saved","bytes":7,"sha256":media::file_digest(&external.path().join(format!("{key}.bin"))).unwrap()}).to_string()).unwrap();
    assert_eq!(
        fixture
            .collector(Limits::default())
            .cache_media(URL, None)
            .await
            .unwrap()["reason"],
        "storage_error"
    );
    assert!(media_collection::clear_media(&fixture.path).is_err());
    assert_eq!(
        std::fs::read(external.path().join(format!("{key}.bin"))).unwrap(),
        b"outside"
    );
    assert_eq!(std::fs::read_dir(external.path()).unwrap().count(), 2);
    assert!(fixture.trace().is_empty());
    std::fs::remove_dir(media::directory(&fixture.path)).unwrap();
}
