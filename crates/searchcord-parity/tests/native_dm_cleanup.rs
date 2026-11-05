//! DM cleanup tests use synthetic archives, loopback HTTP and a controlled clock.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::Response,
};
use rusqlite::Connection;
use searchcord_collector::{
    PageContext, Result, Store,
    discord::Discord,
    discovery::HttpError,
    dm_cleanup::DmCleanup,
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
    hold: bool,
}
fn reply(status: u16, body: Value) -> Reply {
    Reply {
        status,
        body,
        hold: false,
    }
}
struct Fixture {
    clock: VirtualClock,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Value>>,
    entered: Notify,
    release: Notify,
}
async fn handler(State(f): State<Arc<Fixture>>, request: Request) -> Response<Body> {
    f.requests.lock().unwrap().push(json!({"method":request.method().as_str(),"path":request.uri().path(),"query":request.uri().query(),"token":request.headers()["authorization"].to_str().unwrap(),"at":f.clock.now()}));
    let r = f
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected HTTP request");
    if r.hold {
        f.entered.notify_one();
        f.release.notified().await;
    }
    Response::builder()
        .status(r.status)
        .body(Body::from(r.body.to_string()))
        .unwrap()
}
fn message(id: i64, author: &str) -> Value {
    json!({"id":id.to_string(),"channel_id":"20","author":{"id":author,"username":"Fixture"},"content":"synthetic","timestamp":"2026-01-01T00:00:00+00:00","attachments":[]})
}
async fn setup(
    messages: &[Value],
    replies: Vec<Reply>,
) -> (
    tempfile::TempDir,
    PathBuf,
    DmCleanup<VirtualClock>,
    Arc<Fixture>,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let mut store = Store::open(&path).unwrap();
    store
        .save_page(
            messages,
            &PageContext {
                channel_id: "20",
                channel_name: "Fixture",
                ..Default::default()
            },
        )
        .unwrap();
    drop(store);
    let clock = VirtualClock::default();
    let fixture = Arc::new(Fixture {
        clock: clock.clone(),
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(handler).with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let client = Discord::for_loopback(gate, &origin).unwrap();
    (
        dir,
        path.clone(),
        DmCleanup::new(path, client, Arc::new(clock)),
        fixture,
        server,
    )
}
fn http(error: Box<dyn std::error::Error + Send + Sync>, status: u16, detail: &str) {
    let e = error.downcast::<HttpError>().unwrap();
    assert_eq!(e.status, status);
    assert_eq!(e.detail, detail);
}
async fn finish(service: &DmCleanup<VirtualClock>, id: &str) -> Value {
    finish_with_timeout(service, id, Duration::from_secs(30)).await
}
async fn finish_with_timeout(
    service: &DmCleanup<VirtualClock>,
    id: &str,
    timeout: Duration,
) -> Value {
    tokio::time::timeout(timeout, service.barrier(id))
        .await
        .unwrap()
        .unwrap();
    service.state(id).await.unwrap()["terminal"].clone()
}

#[tokio::test]
async fn own_messages_only_and_confirmed_missing_have_distinct_tombstones() {
    let messages = vec![message(103, "31"), message(102, "30"), message(101, "30")];
    let (_dir, path, service, fixture, server) = setup(
        &messages,
        vec![
            reply(200, json!({"id":"30"})),
            reply(200, json!(messages)),
            reply(404, json!({"code":10008})),
            reply(204, json!({})),
        ],
    )
    .await;
    let started = service
        .start("20", "original-account".into())
        .await
        .unwrap();
    let id = started["job_id"].as_str().unwrap();
    let (initial, _events) = service.subscribe(id).await.unwrap();
    assert_eq!(initial["channel_id"], "20");
    assert_eq!(initial["cancelled"], false);
    let terminal = finish(&service, id).await;
    assert_eq!(
        terminal,
        json!({"type":"complete","deleted":1,"failed":0,"already_missing":1,"scanned":3})
    );
    let requests = fixture.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[2]["path"], "/api/v10/channels/20/messages/102");
    assert_eq!(requests[3]["path"], "/api/v10/channels/20/messages/101");
    assert!(requests.iter().all(|r| r["token"] == "original-account"));
    let db = Connection::open(&path).unwrap();
    let mut stmt=db.prepare("SELECT message_id,source FROM message_revisions WHERE event='deleted' ORDER BY message_id").unwrap();
    let revisions = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        revisions,
        vec![
            (101, "confirmed_delete".into()),
            (102, "confirmed_missing".into())
        ]
    );
    assert_eq!(service.stop(id).await.unwrap(), json!({"ok":true}));
    assert_eq!(service.subscribe(id).await.unwrap().0["terminal"], terminal);
    fixture.clock.0.fetch_add(61_000_000, Ordering::SeqCst);
    http(service.state(id).await.unwrap_err(), 404, "Job not found");
    server.abort();
}

#[tokio::test]
async fn unknown_channel_generic_404_string_code_and_wrong_author_do_not_tombstone() {
    let messages = vec![
        message(105, "31"),
        message(104, "30"),
        message(103, "30"),
        message(102, "30"),
        message(101, "30"),
    ];
    let (_dir, path, service, _fixture, server) = setup(
        &messages,
        vec![
            reply(200, json!({"id":"30"})),
            reply(200, json!(messages)),
            reply(404, json!({"code":10003})),
            reply(404, json!({"code":"10008"})),
            reply(404, json!([])),
            reply(200, json!({})),
        ],
    )
    .await;
    let started = service.start("20", "fixture".into()).await.unwrap();
    let id = started["job_id"].as_str().unwrap();
    assert_eq!(
        finish(&service, id).await,
        json!({"type":"complete","deleted":0,"failed":4,"already_missing":0,"scanned":5})
    );
    let count: i64 = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM message_payloads WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    server.abort();
}

#[tokio::test]
async fn transient_retries_backoff_and_429_cooldown_precede_confirmed_deletion() {
    let messages = vec![message(101, "30")];
    let (_dir, _path, service, fixture, server) = setup(
        &messages,
        vec![
            reply(200, json!({"id":"30"})),
            reply(200, json!(messages)),
            reply(503, json!({})),
            reply(425, json!({})),
            reply(429, json!({"retry_after":20})),
            reply(204, json!({})),
        ],
    )
    .await;
    let started = service.start("20", "fixture".into()).await.unwrap();
    let id = started["job_id"].as_str().unwrap();
    let (_initial, mut events) = service.subscribe(id).await.unwrap();
    assert_eq!(finish(&service, id).await["deleted"], 1);
    let mut warnings = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event["type"] == "warning" {
            warnings.push(event["message"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(
        warnings,
        vec![
            "Retrying delete 101 (1/3).",
            "Retrying delete 101 (2/3).",
            "Retrying delete 101 (3/3)."
        ]
    );
    let trace = fixture.requests.lock().unwrap();
    assert_eq!(trace.len(), 6);
    let at = |i: usize| trace[i]["at"].as_f64().unwrap();
    assert!(at(3) - at(2) >= 0.999);
    assert!(at(4) - at(3) >= 1.999);
    assert!(at(5) - at(4) >= 19.999);
    drop(trace);
    server.abort();
}

#[tokio::test]
async fn retry_cap_and_auth_denial_emit_exact_terminal_counts() {
    for denial in [false, true] {
        let messages = vec![message(102, "30"), message(101, "30")];
        let mut replies = vec![reply(200, json!({"id":"30"})), reply(200, json!(messages))];
        if denial {
            replies.push(reply(403, json!({})));
        } else {
            replies.extend((0..8).map(|_| reply(500, json!({}))));
        }
        let (_dir, _path, service, fixture, server) = setup(&messages, replies).await;
        let started = service.start("20", "fixture".into()).await.unwrap();
        let id = started["job_id"].as_str().unwrap();
        let terminal = finish(&service, id).await;
        if denial {
            assert_eq!(
                terminal,
                json!({"type":"error","message":"Discord denied deletion: HTTP 403","deleted":0,"failed":1,"already_missing":0,"scanned":2})
            );
            assert_eq!(fixture.requests.lock().unwrap().len(), 3);
        } else {
            assert_eq!(terminal["failed"], 2);
            assert_eq!(terminal["type"], "complete");
            assert_eq!(fixture.requests.lock().unwrap().len(), 10);
        }
        server.abort();
    }
}

#[tokio::test]
async fn invalid_inputs_duplicate_running_jobs_cancelled_requests_and_shutdown_never_restart() {
    let mut held = reply(200, json!({"id":"30"}));
    held.hold = true;
    let (_dir, path, service, fixture, server) = setup(&[], vec![held]).await;
    for id in [
        "",
        "0",
        "9223372036854775808",
        "abc",
        "12345678901234567890",
    ] {
        http(
            service.start(id, "fixture".into()).await.unwrap_err(),
            422,
            "Channel ID must be a Discord ID",
        );
    }
    let started = service.start("20", "fixture".into()).await.unwrap();
    let id = started["job_id"].as_str().unwrap();
    fixture.entered.notified().await;
    http(
        service.start("20", "other".into()).await.unwrap_err(),
        409,
        "This DM is already being cleared",
    );
    http(
        service.stop("absent").await.unwrap_err(),
        404,
        "Job not found",
    );
    service.stop(id).await.unwrap();
    let terminal = finish(&service, id).await;
    assert_eq!(terminal["type"], "cancelled");
    assert_eq!(terminal["scanned"], 0);
    service.shutdown().await.unwrap();
    http(
        service.start("20", "fixture".into()).await.unwrap_err(),
        503,
        "Deletion jobs are shutting down.",
    );
    let count: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM collection_jobs WHERE kind='dm-clear'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let gate = Gate::open(path.clone(), fixture.clock.clone())
        .await
        .unwrap();
    let fresh = DmCleanup::new(
        path,
        Discord::for_loopback(gate, "http://127.0.0.1:1").unwrap(),
        Arc::new(fixture.clock.clone()),
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    fresh.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn full_pages_use_oldest_cursor_and_stalled_progress_is_bounded() {
    let messages: Vec<_> = (1..=100).rev().map(|n| message(n, "30")).collect();
    let mut replies = vec![reply(200, json!({"id":"30"})), reply(200, json!(messages))];
    // 100 messages x 4 failed attempts emits 500 events, then another full page.
    replies.extend((0..400).map(|_| reply(500, json!({}))));
    replies.push(reply(200, json!(messages)));
    replies.extend((0..400).map(|_| reply(500, json!({}))));
    replies.push(reply(200, json!(messages)));
    replies.extend((0..400).map(|_| reply(500, json!({}))));
    replies.push(reply(200, json!([])));
    let (_dir, _path, service, fixture, server) = setup(&messages, replies).await;
    let started = service.start("20", "fixture".into()).await.unwrap();
    let id = started["job_id"].as_str().unwrap();
    let (_, mut events) = service.subscribe(id).await.unwrap();
    // This stress case performs 1,205 real loopback requests and durable pacing
    // writes. Allow hosted Windows disk latency without changing retry policy,
    // event capacity, request count, cursor assertions or production deadlines.
    let terminal = finish_with_timeout(&service, id, Duration::from_secs(120)).await;
    assert_eq!(terminal["failed"], 300);
    assert_eq!(terminal["scanned"], 300);
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
    ));
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 1205);
    assert_eq!(requests[402]["query"], "limit=100&before=1");
    assert_eq!(requests[803]["query"], "limit=100&before=1");
    drop(requests);
    server.abort();
}

#[tokio::test]
async fn shutdown_joins_accepted_deletion_and_tombstone_even_if_barrier_caller_is_cancelled() {
    let messages = vec![message(101, "30")];
    let mut deletion = reply(204, json!({}));
    deletion.hold = true;
    let (_dir, path, service, fixture, server) = setup(
        &messages,
        vec![
            reply(200, json!({"id":"30"})),
            reply(200, json!(messages)),
            deletion,
        ],
    )
    .await;
    let started = service.start("20", "fixture".into()).await.unwrap();
    let id = started["job_id"].as_str().unwrap().to_owned();
    fixture.entered.notified().await;
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    fixture.release.notify_one();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let waiter = {
        let service = service.clone();
        let id = id.clone();
        tokio::spawn(async move { service.barrier(&id).await })
    };
    tokio::task::yield_now().await;
    waiter.abort();
    let _ = waiter.await;
    let shutdown = {
        let service = service.clone();
        tokio::spawn(async move { service.shutdown().await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!shutdown.is_finished());
    blocker.execute_batch("COMMIT").unwrap();
    shutdown.await.unwrap().unwrap();
    let db = Connection::open(path).unwrap();
    let source: String = db
        .query_row(
            "SELECT source FROM message_revisions WHERE event='deleted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, "confirmed_delete");
    server.abort();
}

#[tokio::test]
async fn malformed_pages_and_status_failures_emit_reference_errors() {
    for (replies, message) in [
        (
            vec![reply(401, json!({}))],
            "Failed to fetch user: HTTP 401",
        ),
        (
            vec![reply(200, json!({"id":"30"})), reply(403, json!({}))],
            "Could not read messages: HTTP 403",
        ),
        (
            vec![reply(200, json!({"id":"30"})), reply(200, json!({}))],
            "Discord returned an invalid message page",
        ),
    ] {
        let (_dir, _path, service, _fixture, server) = setup(&[], replies).await;
        let started = service.start("20", "fixture".into()).await.unwrap();
        let terminal = finish(&service, started["job_id"].as_str().unwrap()).await;
        assert_eq!(terminal["type"], "error");
        assert_eq!(terminal["message"], message);
        server.abort();
    }
}
