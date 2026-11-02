//! Synthetic invite fixtures only. The clock advances without real 30-second waits.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::Response,
};
use rusqlite::{Connection, params};
use searchcord_collector::{
    Result, Store,
    discord::Discord,
    discovery::HttpError,
    invites::{Invites, parse_invites},
    pacing::{Clock, Gate},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct VirtualClock(Arc<AtomicU64>);
impl Clock for VirtualClock {
    fn now(&self) -> f64 {
        1000.0 + self.0.load(Ordering::SeqCst) as f64 / 1_000_000.0
    }
    fn wall(&self) -> f64 {
        1_700_000_000.0 + self.now()
    }
    fn uniform(&self, low: f64, _high: f64) -> Result<f64> {
        Ok(low)
    }
    async fn sleep(&self, seconds: f64) {
        self.0
            .fetch_add((seconds * 1_000_000.0).ceil() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
struct Reply {
    status: u16,
    body: Value,
    elapsed: f64,
    hold: bool,
}
fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body,
        elapsed: 0.0,
        hold: false,
    }
}
struct Fixture {
    clock: VirtualClock,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Value>>,
    entered: Notify,
}
async fn handler(State(f): State<Arc<Fixture>>, request: Request) -> Response<Body> {
    f.requests.lock().unwrap().push(json!({"method":request.method().as_str(),"path":request.uri().path(),"at":f.clock.now(),"token":request.headers()["authorization"].to_str().unwrap()}));
    let r = f
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected HTTP request");
    f.clock
        .0
        .fetch_add((r.elapsed * 1_000_000.0) as u64, Ordering::SeqCst);
    f.entered.notify_one();
    if r.hold {
        std::future::pending::<()>().await;
    }
    Response::builder()
        .status(r.status)
        .body(Body::from(r.body.to_string()))
        .unwrap()
}
async fn setup(
    replies: Vec<Reply>,
) -> (
    tempfile::TempDir,
    PathBuf,
    Invites<VirtualClock>,
    Arc<Fixture>,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    drop(Store::open(&path).unwrap());
    let clock = VirtualClock::default();
    let fixture = Arc::new(Fixture {
        clock: clock.clone(),
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
        entered: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let client = Discord::for_loopback(gate, &origin).unwrap();
    let invites = Invites::open(path.clone(), client, Arc::new(clock))
        .await
        .unwrap();
    (dir, path, invites, fixture, server)
}
fn http(error: Box<dyn std::error::Error + Send + Sync>, status: u16, detail: &str) {
    let error = error.downcast::<HttpError>().unwrap();
    assert_eq!(error.status, status);
    assert_eq!(error.detail, detail);
}
fn guild(name: &str) -> Reply {
    reply(200, json!({"guild":{"id":"10","name":name}}))
}
fn seed(path: &PathBuf, clock: &VirtualClock, job: Value) {
    Connection::open(path).unwrap().execute("INSERT INTO collection_jobs(id,kind,state_json,updated_at) VALUES ('invite-queue','invite',?,0) ON CONFLICT(id) DO UPDATE SET state_json=excluded.state_json", params![json!({"job":job,"next_attempt_at":clock.wall()+30.0}).to_string()]).unwrap();
}

#[test]
fn parser_matches_real_python_for_links_wrappers_errors_and_limits() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let cases = vec![
        "https://discord.gg/Abc Abc;discord.com/invite/Def".to_owned(),
        "<HTTPS://WWW.CANARY.DISCORDAPP.COM/invite/Ab_C-1/?x=1#ok>. 'abc' abc ABC".into(),
        ", ; invalid/link".into(),
        "[] () ...".into(),
        "".into(),
        "x/y".into(),
        "discord.gg/a/b".into(),
        "https://example.com/a".into(),
        "a".repeat(100),
        "a".repeat(101),
        (0..200)
            .map(|i| format!("c{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
        (0..201)
            .map(|i| format!("c{i}"))
            .collect::<Vec<_>>()
            .join(" "),
    ];
    let script = r#"import ast,json,re,sys
tree=ast.parse(open('invite_api.py',encoding='utf-8').read())
nodes=[n for n in tree.body if isinstance(n,ast.FunctionDef) and n.name=='parse_invites' or isinstance(n,ast.Assign) and any(isinstance(t,ast.Name) and t.id in ('CODE','LINK') for t in n.targets)]
scope={'re':re};exec(compile(ast.Module(body=nodes,type_ignores=[]),'invite_api.py','exec'),scope)
out=[]
for text in json.load(sys.stdin):
 try: out.append({'codes':scope['parse_invites'](text)})
 except ValueError as e: out.append({'error':str(e)})
print(json.dumps(out))"#;
    let python = std::env::var_os("SEARCHCORD_PARITY_PYTHON").unwrap_or("python".into());
    let mut child = Command::new(python)
        .arg("-I")
        .arg("-c")
        .arg(script)
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(&cases).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
    let native: Vec<Value> = cases
        .iter()
        .map(|text| match parse_invites(text) {
            Ok(codes) => json!({"codes":codes}),
            Err(error) => json!({"error":error.to_string()}),
        })
        .collect();
    assert_eq!(json!(native), expected);
}

#[tokio::test]
async fn successes_spacing_rate_limits_and_snapshot_copies() {
    let (_dir, path, service, fixture, server) = setup(vec![
        guild("First"),
        reply(200, json!({})),
        reply(429, json!({"retry_after":45})),
        guild("Second"),
        reply(204, json!({})),
        guild("Third"),
        reply(201, json!({"pending":true})),
    ])
    .await;
    let (initial, mut events) = service.subscribe().await;
    assert_eq!(initial, json!({"job":null}));
    service
        .start(
            "First Second Third",
            Some("account".into()),
            "synthetic-token".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), service.barrier())
        .await
        .unwrap()
        .unwrap();
    let mut snapshot = service.status().await;
    assert_eq!(snapshot["job"]["status"], "complete");
    assert_eq!(snapshot["job"]["running"], false);
    assert_eq!(snapshot["job"]["wait_seconds"], 0);
    assert_eq!(snapshot["job"]["items"][1]["attempts"], 1);
    assert_eq!(
        snapshot["job"]["items"][2]["message"],
        "Joined; complete server screening in Discord."
    );
    snapshot["job"]["items"][0]["code"] = json!("mutated");
    assert_eq!(service.status().await["job"]["items"][0]["code"], "First");
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 7);
    let at = |i: usize| requests[i]["at"].as_f64().unwrap();
    assert!(at(2) - at(1) >= 29.999);
    assert!(at(3) - at(2) >= 44.999);
    assert!(at(6) - at(4) >= 29.999);
    assert!(requests.iter().all(|r| r["token"] == "synthetic-token"));
    let saved: String = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE id='invite-queue'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!saved.contains("synthetic-token"));
    server.abort();
}

#[tokio::test]
async fn slow_preflights_failed_joins_and_retry_cap_keep_spacing() {
    let mut slow = guild("Slow");
    slow.elapsed = 45.0;
    let mut replies = vec![
        slow,
        reply(403, json!({"code":40007})),
        reply(200, json!({"guild":null})),
    ];
    replies.extend((0..6).map(|_| reply(429, json!({"retry_after":"NaN"}))));
    let (_dir, _path, service, fixture, server) = setup(replies).await;
    service
        .start("Slow NoGuild Limited", None, "fixture".into())
        .await
        .unwrap();
    service.barrier().await.unwrap();
    let state = service.status().await;
    assert_eq!(
        state["job"]["items"][0]["message"],
        "This account is banned from the invited server."
    );
    assert_eq!(
        state["job"]["items"][1]["message"],
        "This invite does not identify a server."
    );
    assert_eq!(state["job"]["items"][2]["attempts"], 6);
    assert_eq!(state["job"]["items"][2]["status"], "failed");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 9);
    assert!(requests[1]["at"].as_f64().unwrap() - requests[0]["at"].as_f64().unwrap() >= 45.0);
    assert!(requests[2]["at"].as_f64().unwrap() - requests[1]["at"].as_f64().unwrap() >= 29.999);
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn account_errors_and_captchas_block_without_processing_later_items() {
    for (status, body) in [
        (403, json!({"code":340015})),
        (400, json!({"captcha_key":null})),
        (401, json!({})),
        (400, json!({"code":20001})),
        (400, json!({"code":30001})),
        (400, json!({"code":40002})),
        (400, json!({"code":50014})),
    ] {
        let (_dir, _path, service, fixture, server) = setup(vec![reply(status, body)]).await;
        service
            .start("blocked later", None, "fixture".into())
            .await
            .unwrap();
        service.barrier().await.unwrap();
        let state = service.status().await;
        assert_eq!(state["job"]["status"], "blocked");
        assert_eq!(state["job"]["running"], false);
        assert_eq!(state["job"]["items"][1]["status"], "pending");
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
        server.abort();
    }
}

#[tokio::test]
async fn interrupted_join_recovers_paused_binds_account_and_never_replays_uncertain() {
    let mut held = guild("Held");
    held.hold = true;
    let (_dir, path, service, fixture, server) =
        setup(vec![held, guild("Pending"), reply(204, json!({}))]).await;
    let started = service
        .start(
            "Interrupted Pending",
            Some("account".into()),
            "original".into(),
        )
        .await
        .unwrap();
    let id = started["job"]["id"].as_str().unwrap();
    fixture.entered.notified().await;
    http(
        service
            .start("other", None, "other".into())
            .await
            .unwrap_err(),
        409,
        "An invite queue is already running.",
    );
    service.shutdown(true).await.unwrap();
    assert_eq!(
        service.status().await["job"]["items"][0]["status"],
        "uncertain"
    );
    let gate = Gate::open(path.clone(), fixture.clock.clone())
        .await
        .unwrap();
    // Reuse the same bound listener origin reconstructed from an explicit fixture field.
    let unused = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", unused.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let resumed_server = tokio::spawn(async move {
        axum::serve(unused, app).await.unwrap();
    });
    let restored = Invites::open(
        path.clone(),
        Discord::for_loopback(gate, &origin).unwrap(),
        Arc::new(fixture.clock.clone()),
    )
    .await
    .unwrap();
    assert_eq!(restored.status().await["job"]["paused"], true);
    http(
        restored
            .resume(id, Some("different".into()), "other".into())
            .await
            .unwrap_err(),
        409,
        "Select the account used to start this queue before resuming.",
    );
    restored
        .resume(id, Some("account".into()), "original".into())
        .await
        .unwrap();
    restored.barrier().await.unwrap();
    let state = restored.status().await;
    assert_eq!(state["job"]["items"][0]["status"], "uncertain");
    assert_eq!(state["job"]["items"][1]["status"], "joined");
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["path"] == "/api/v10/invites/Interrupted")
            .count(),
        1
    );
    http(
        restored.stop("absent").await.unwrap_err(),
        404,
        "Invite queue not found.",
    );
    assert_eq!(restored.stop(id).await.unwrap()["job"]["status"], "stopped");
    server.abort();
    resumed_server.abort();
}

#[tokio::test]
async fn raw_python_checkpoint_joining_state_recovers_without_network() {
    let (_dir, path, service, fixture, server) = setup(vec![]).await;
    seed(
        &path,
        &fixture.clock,
        json!({"id":"saved","running":true,"paused":false,"status":"running","credential_id":"original","message":"Checking","items":[{"code":"A","status":"joining","message":"Checking"},{"code":"B","status":"waiting","message":"Waiting"}]}),
    );
    service.shutdown(true).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let gate = Gate::open(path.clone(), fixture.clock.clone())
        .await
        .unwrap();
    let restored = Invites::open(
        path,
        Discord::for_loopback(gate, &origin).unwrap(),
        Arc::new(fixture.clock.clone()),
    )
    .await
    .unwrap();
    let state = restored.status().await;
    assert_eq!(state["job"]["items"][0]["status"], "uncertain");
    assert_eq!(state["job"]["items"][1]["status"], "waiting");
    assert_eq!(state["job"]["wait_seconds"], 30);
    assert!(fixture.requests.lock().unwrap().is_empty());
    restored.shutdown(false).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn cancelled_start_caller_cannot_abandon_accepted_checkpoint_or_shutdown_join() {
    let mut held = guild("Held");
    held.hold = true;
    let (_dir, path, service, _fixture, server) = setup(vec![held]).await;
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .start("fixture", Some("account".into()), "synthetic-token".into())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.status().await["job"].is_null() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    start.abort();
    let _ = start.await;
    let shutdown = {
        let service = service.clone();
        tokio::spawn(async move { service.shutdown(true).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!shutdown.is_finished());
    blocker.execute_batch("COMMIT").unwrap();
    shutdown.await.unwrap().unwrap();
    let raw: String = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE id='invite-queue'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let saved: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(saved["job"]["paused"], true);
    assert_eq!(saved["job"]["running"], true);
    assert!(!raw.contains("synthetic-token"));
    server.abort();
}
