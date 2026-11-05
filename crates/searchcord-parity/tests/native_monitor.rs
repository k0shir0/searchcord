//! Disposable monitor archives, a controlled poll clock and numeric loopback.
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, Response},
    routing::get,
};
use rusqlite::Connection;
use searchcord_collector::{
    PageContext, Result, Store,
    accounts::Accounts,
    credentials::{Paths, Vault},
    discord::Discord,
    discovery::HttpError,
    media_collection::{Limits, MediaCollector},
    monitor::{Monitors, POLL_SECONDS},
    pacing::{Clock, Gate},
    writer::Writer,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore, broadcast};
const BASE: i64 = 1_450_000_000_000_000_000;

struct ClockState {
    elapsed: AtomicU64,
    sleeps: AtomicUsize,
    changed: Notify,
    ticks: Semaphore,
}
impl Default for ClockState {
    fn default() -> Self {
        Self {
            elapsed: AtomicU64::new(0),
            sleeps: AtomicUsize::new(0),
            changed: Notify::new(),
            ticks: Semaphore::new(0),
        }
    }
}
#[derive(Clone, Default)]
struct TestClock(Arc<ClockState>);
impl Clock for TestClock {
    fn now(&self) -> f64 {
        1000.0 + self.0.elapsed.load(Ordering::SeqCst) as f64 / 1000.0
    }
    fn wall(&self) -> f64 {
        1_700_000_000.0 + self.now()
    }
    fn uniform(&self, low: f64, _high: f64) -> Result<f64> {
        Ok(low)
    }
    async fn sleep(&self, seconds: f64) {
        if seconds == POLL_SECONDS {
            self.0.sleeps.fetch_add(1, Ordering::SeqCst);
            self.0.changed.notify_waiters();
            self.0.ticks.acquire().await.unwrap().forget();
        }
        self.0
            .elapsed
            .fetch_add((seconds * 1000.0).ceil() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
struct Reply {
    status: u16,
    body: String,
}
impl Reply {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}
#[derive(Clone, Debug)]
struct Trace {
    path: String,
    query: String,
    saved: i64,
    last_id: Value,
}
struct MockState {
    path: PathBuf,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Trace>>,
}
async fn serve(State(state): State<Arc<MockState>>, request: Request<Body>) -> Response<Body> {
    assert_eq!(request.headers()["authorization"], "synthetic-token");
    let db = Connection::open(&state.path).unwrap();
    let saved = db
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    let last_id = db
        .query_row(
            "SELECT json_extract(state_json,'$.last_id') FROM collection_jobs WHERE id='20'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .unwrap_or(None);
    state.requests.lock().unwrap().push(Trace {
        path: request.uri().path().into(),
        query: request.uri().query().unwrap_or("").into(),
        saved,
        last_id: json!(last_id),
    });
    let reply = state
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| Reply::json(404, json!({})));
    Response::builder()
        .status(reply.status)
        .header("content-type", "application/json")
        .body(Body::from(reply.body))
        .unwrap()
}
struct Fixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    writer: Writer,
    client: Discord<TestClock>,
    clock: TestClock,
    monitors: Monitors<TestClock>,
    mock: Arc<MockState>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn channel() -> Value {
    json!({"id":"20","name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
fn checkpoint(last: i64) -> Value {
    json!({"channel_name":"fixture","guild_id":"10","guild_name":"Fixture","credential_id":"account","running":true,"paused":false,"last_id":(BASE+last).to_string(),"pending_before":null,"pending_anchor":null,"pending_newest":null})
}
fn message(offset: i64, content: &str) -> Value {
    json!({"id":(BASE+offset).to_string(),"channel_id":"20","timestamp":"2026-01-01T00:00:00+00:00","author":{"id":"30","username":"alice","global_name":"Alice","avatar":"a_avatar1"},"content":content,"attachments":[],"unknown":{"kept":true}})
}
fn context() -> PageContext<'static> {
    PageContext {
        channel_id: "20",
        channel_name: "fixture",
        guild_id: Some("10"),
        guild_name: "Fixture",
        ..PageContext::default()
    }
}
impl Fixture {
    async fn new(messages: Vec<Value>, state: Option<Value>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic.db");
        let mut store = Store::open(&path).unwrap();
        if !messages.is_empty() {
            store.save_page(&messages, &context()).unwrap();
        }
        if let Some(state) = state {
            store
                .connection()
                .execute(
                    "INSERT INTO collection_jobs VALUES ('20','monitor',?,0)",
                    [state.to_string()],
                )
                .unwrap();
        }
        drop(store);
        let mock = Arc::new(MockState {
            path: path.clone(),
            replies: Mutex::new(VecDeque::new()),
            requests: Mutex::new(vec![]),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/api/v10/channels/{cid}/messages", get(serve))
            .route("/api/v10/channels/{cid}/messages/{mid}", get(serve))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let clock = TestClock::default();
        let client = Discord::for_loopback(
            Gate::open(path.clone(), clock.clone()).await.unwrap(),
            &origin,
        )
        .unwrap();
        let writer = Writer::open(path.clone()).await.unwrap();
        let monitors = Monitors::open(
            path.clone(),
            writer.clone(),
            client.clone(),
            Arc::new(clock.clone()),
        )
        .await
        .unwrap();
        Self {
            _directory: directory,
            path,
            writer,
            client,
            clock,
            monitors,
            mock,
            server,
        }
    }
    fn replies(&self, replies: impl IntoIterator<Item = Reply>) {
        self.mock.replies.lock().unwrap().extend(replies);
    }
    fn requests(&self) -> Vec<Trace> {
        self.mock.requests.lock().unwrap().clone()
    }
    fn count(&self, sql: &str) -> i64 {
        Connection::open(&self.path)
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }
    fn state(&self) -> Value {
        let raw: String = Connection::open(&self.path)
            .unwrap()
            .query_row(
                "SELECT state_json FROM collection_jobs WHERE kind='monitor' AND id='20'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("synthetic-token"));
        serde_json::from_str(&raw).unwrap()
    }
    async fn idle(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = self.clock.0.changed.notified();
                if self.clock.0.sleeps.load(Ordering::SeqCst) >= count {
                    break;
                }
                changed.await;
            }
        })
        .await
        .expect("monitor reaches next poll boundary");
    }
    async fn tick(&self) {
        let next = self.clock.0.sleeps.load(Ordering::SeqCst) + 1;
        self.clock.0.ticks.add_permits(1);
        self.idle(next).await;
    }
    async fn start(&self) -> Value {
        self.monitors
            .start(
                json!([channel()]),
                "account".into(),
                "synthetic-token".into(),
            )
            .await
            .unwrap()
    }
    async fn finish(&self) {
        self.monitors.stop(vec![]).await.unwrap();
        self.writer.shutdown().await.unwrap();
    }
}
async fn receive(receiver: &mut broadcast::Receiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn anchors_without_archiving_then_commits_observations_before_broadcast_and_updates_edits() {
    let f = Fixture::new(vec![], None).await;
    assert_eq!(f.monitors.status().await, json!({"channels":[]}));
    let (initial, mut events) = f.monitors.subscribe().await;
    assert!(initial.is_empty());
    f.replies([Reply::json(200, json!([message(1, "anchor")]))]);
    assert_eq!(f.start().await, json!({"active":["20"]}));
    f.idle(1).await;
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 0);
    assert_eq!(f.state()["last_id"], (BASE + 1).to_string());
    assert_eq!(f.requests()[0].query, "limit=1");
    let start = json!({"type":"monitor_start","channel_id":"20","channel":"fixture","guild":"Fixture","guild_id":"10"});
    assert_eq!(receive(&mut events).await, start);
    assert_eq!(f.monitors.subscribe().await.0, vec![start]);
    let mut observed = message(2, "new message");
    observed["attachments"] = json!([
        {"url":"https://cdn.discordapp.com/attachments/20/40/image.png"},
        {"url":"https://cdn.discordapp.com/attachments/20/41/noextension","content_type":"image/webp"},
        {"url":"https://cdn.discordapp.com/attachments/20/42/file.zip"}]);
    f.replies([Reply::json(200, json!([observed]))]);
    f.tick().await;
    let incoming = receive(&mut events).await;
    assert_eq!(
        incoming,
        json!({"type":"message","id":(BASE+2).to_string(),"channel_id":"20","channel":"fixture","guild_id":"10","guild":"Fixture","author_id":"30","author":"Alice","avatar_url":"https://cdn.discordapp.com/avatars/30/a_avatar1.gif?size=64","content":"new message","timestamp":"2026-01-01T00:00:00+00:00","attachments":[format!("/api/images/{}/0",BASE+2),format!("/api/images/{}/1",BASE+2)]})
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(f.state()["last_id"], (BASE + 2).to_string());
    assert!(f.state()["pending_before"].is_null());
    assert_eq!(f.requests()[1].saved, 0);
    assert_eq!(f.requests()[1].last_id, (BASE + 1).to_string());
    f.replies([Reply::json(200, json!([message(2, "edited")]))]);
    f.tick().await;
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(f.count("SELECT COUNT(*) FROM message_revisions"), 2);
    let content: String = Connection::open(&f.path)
        .unwrap()
        .query_row("SELECT content FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(content, "edited");
    assert_eq!(
        f.monitors.stop(vec!["20".into()]).await.unwrap(),
        json!({"stopped":["20"]})
    );
    assert_eq!(
        receive(&mut events).await,
        json!({"type":"monitor_stop","channel_id":"20","channel":"fixture"})
    );
    assert_eq!(
        f.count("SELECT COUNT(*) FROM collection_jobs WHERE kind='monitor'"),
        0
    );
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn gap_cursor_recovers_paused_binds_account_and_requires_exact_unknown_message_confirmation()
{
    let f = Fixture::new(vec![message(1, "anchor")], Some(checkpoint(1))).await;
    assert!(f.requests().is_empty());
    assert_eq!(f.monitors.status().await["channels"][0]["paused"], true);
    assert_eq!(f.monitors.subscribe().await.0[0]["type"], "monitor_paused");
    let wrong = f
        .monitors
        .start(json!([channel()]), "other".into(), "synthetic-token".into())
        .await
        .unwrap_err();
    assert_eq!(wrong.downcast_ref::<HttpError>().unwrap().status, 409);
    f.start().await;
    f.idle(1).await;
    let first: Vec<_> = (52..=101).rev().map(|n| message(n, "newest")).collect();
    f.replies([Reply::json(200, json!(first))]);
    f.tick().await;
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 51);
    let state = f.state();
    assert_eq!(state["last_id"], (BASE + 1).to_string());
    assert_eq!(state["pending_before"], (BASE + 52).to_string());
    assert_eq!(state["pending_anchor"], (BASE + 1).to_string());
    assert_eq!(state["pending_newest"], (BASE + 101).to_string());
    f.monitors.suspend().await.unwrap();
    assert_eq!(f.state()["paused"], true);
    let resumed = Monitors::open(
        f.path.clone(),
        f.writer.clone(),
        f.client.clone(),
        Arc::new(f.clock.clone()),
    )
    .await
    .unwrap();
    assert_eq!(resumed.status().await["channels"][0]["paused"], true);
    assert_eq!(resumed.subscribe().await.0[0]["type"], "monitor_paused");
    let sleeps = f.clock.0.sleeps.load(Ordering::SeqCst);
    resumed
        .start(
            json!([channel()]),
            "account".into(),
            "synthetic-token".into(),
        )
        .await
        .unwrap();
    f.idle(sleeps + 1).await;
    let second: Vec<_> = (2..=51).rev().map(|n| message(n, "gap")).collect();
    f.replies([
        Reply::json(200, json!(second)),
        Reply::json(200, json!([message(1, "anchor")])),
    ]);
    f.tick().await;
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 101);
    assert_eq!(f.state()["last_id"], (BASE + 1).to_string());
    f.tick().await;
    assert_eq!(f.state()["last_id"], (BASE + 101).to_string());
    assert!(f.state()["pending_before"].is_null());
    let requests = f.requests();
    assert_eq!(requests[0].query, "limit=50");
    assert_eq!(requests[1].query, format!("limit=50&before={}", BASE + 52));
    assert_eq!(requests[2].query, format!("limit=50&before={}", BASE + 2));
    for body in [json!({}), json!({"code":"10008"}), json!({"code":10008.0})] {
        f.replies([Reply::json(200, json!([])), Reply::json(404, body)]);
        f.tick().await;
        assert_eq!(
            f.count("SELECT COUNT(*) FROM message_payloads WHERE deleted_at IS NOT NULL"),
            0
        );
    }
    f.replies([
        Reply::json(200, json!([])),
        Reply::json(404, json!({"code":10008})),
    ]);
    f.tick().await;
    assert_eq!(
        f.count("SELECT COUNT(*) FROM message_payloads WHERE deleted_at IS NOT NULL"),
        1
    );
    assert!(
        f.requests()
            .last()
            .unwrap()
            .path
            .ends_with(&(BASE + 101).to_string())
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM message_revisions WHERE event='deleted' AND source='upstream_not_found'"),1);
    resumed.stop(vec![]).await.unwrap();
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn page_and_checkpoint_rollback_together_and_bad_polls_do_not_advance_progress() {
    let f = Fixture::new(vec![message(1, "anchor")], Some(checkpoint(1))).await;
    f.start().await;
    f.idle(1).await;
    let (_, mut events) = f.monitors.subscribe().await;
    let db = Connection::open(&f.path).unwrap();
    db.execute_batch(&format!("CREATE TRIGGER fixture_reject_monitor BEFORE UPDATE ON collection_jobs WHEN NEW.id='20' AND json_extract(NEW.state_json,'$.last_id')='{}' BEGIN SELECT RAISE(ABORT,'fixture checkpoint rejected'); END;",BASE+2)).unwrap();
    f.replies([Reply::json(200, json!([message(2, "must roll back")]))]);
    f.tick().await;
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 1);
    assert_eq!(f.state()["last_id"], (BASE + 1).to_string());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    db.execute_batch("DROP TRIGGER fixture_reject_monitor")
        .unwrap();
    f.replies([
        Reply::json(503, json!({})),
        Reply::json(200, json!({})),
        Reply {
            status: 200,
            body: "not JSON".into(),
        },
    ]);
    for _ in 0..3 {
        f.tick().await;
        assert_eq!(f.state()["last_id"], (BASE + 1).to_string());
    }
    f.replies([Reply::json(200, json!([message(2, "committed")]))]);
    f.tick().await;
    assert_eq!(receive(&mut events).await["content"], "committed");
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 2);
    assert_eq!(f.state()["last_id"], (BASE + 2).to_string());
    assert_eq!(f.monitors.status().await["channels"][0]["paused"], false);
    f.finish().await;
}

#[tokio::test]
async fn missing_message_probe_saves_edits_and_only_confirmed_deletion_removes_future_probes() {
    let f = Fixture::new(
        vec![
            message(1, "one"),
            message(2, "old two"),
            message(3, "three"),
        ],
        Some(checkpoint(3)),
    )
    .await;
    f.start().await;
    f.idle(1).await;
    let (_, mut events) = f.monitors.subscribe().await;
    let visible = json!([message(3, "three"), message(1, "one")]);
    f.replies([
        Reply::json(200, visible.clone()),
        Reply::json(200, message(2, "observed edit")),
    ]);
    f.tick().await;
    let content: String = Connection::open(&f.path)
        .unwrap()
        .query_row("SELECT content FROM messages WHERE id=?", [BASE + 2], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(content, "observed edit");
    assert_eq!(f.state()["last_id"], (BASE + 3).to_string());
    assert_eq!(f.count("SELECT COUNT(*) FROM message_revisions"), 4);
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(f.requests()[1].path.ends_with(&(BASE + 2).to_string()));
    f.replies([
        Reply::json(200, visible.clone()),
        Reply::json(404, json!({"code":10008})),
    ]);
    f.tick().await;
    assert_eq!(
        f.count("SELECT COUNT(*) FROM message_payloads WHERE deleted_at IS NOT NULL"),
        1
    );
    let requests = f.requests().len();
    f.replies([Reply::json(200, visible)]);
    f.tick().await;
    assert_eq!(f.requests().len(), requests + 1);
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 3);
    f.finish().await;
}

#[tokio::test]
async fn failed_anchor_pauses_and_resumes_and_stop_joins_accepted_writes() {
    let f = Fixture::new(vec![], None).await;
    f.replies([Reply::json(401, json!({}))]);
    f.start().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if f.monitors.status().await["channels"][0]["paused"] == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.requests().len(), 1);
    assert_eq!(f.state()["paused"], true);
    f.replies([Reply::json(200, json!([]))]);
    f.start().await;
    f.idle(1).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let writer = f.writer.clone();
    let accepted = tokio::spawn(async move {
        writer
            .write(move |store| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                store.connection().execute(
                    "INSERT INTO settings VALUES ('synthetic-accepted','kept')",
                    [],
                )?;
                Ok(Value::Null)
            })
            .await
    });
    entered_rx.await.unwrap();
    let monitors = f.monitors.clone();
    let stop = tokio::spawn(async move { monitors.stop(vec![]).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!stop.is_finished());
    release_tx.send(()).unwrap();
    accepted.await.unwrap().unwrap();
    assert_eq!(stop.await.unwrap().unwrap(), json!({"stopped":["20"]}));
    assert_eq!(
        f.count("SELECT COUNT(*) FROM settings WHERE key='synthetic-accepted'"),
        1
    );
    assert_eq!(
        f.count("SELECT COUNT(*) FROM collection_jobs WHERE kind='monitor'"),
        0
    );
    assert_eq!(f.monitors.status().await, json!({"channels":[]}));
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn worker_rechecks_selected_account_and_missing_credentials_before_any_network() {
    if std::env::var_os("SEARCHCORD_MONITOR_ACCOUNT_CHILD").is_none() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("synthetic.vault");
        let key = directory.path().join("synthetic.key");
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "worker_rechecks_selected_account_and_missing_credentials_before_any_network",
                    "--nocapture",
                ])
                .env("SEARCHCORD_MONITOR_ACCOUNT_CHILD", "1")
                .env("SEARCHCORD_CREDENTIAL_FILE", vault)
                .env("SEARCHCORD_CREDENTIAL_KEY_FILE", key)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(std::env::var_os("SEARCHCORD_CREDENTIAL_FILE").is_some());
    assert!(std::env::var_os("SEARCHCORD_CREDENTIAL_KEY_FILE").is_some());
    let f = Fixture::new(vec![], None).await;
    let monitors = f
        .monitors
        .clone()
        .with_accounts(Accounts::new(f.path.clone(), f.writer.clone()));
    let tokens = json!([{"id":"account","token":"synthetic-token","label":"Account"},{"id":"other","token":"synthetic-other","label":"Other"}]);
    let vault = Vault::platform(Paths::for_database(&f.path).unwrap());
    vault.save(&tokens, &json!("other")).unwrap();
    monitors
        .start(
            json!([channel()]),
            "account".into(),
            "synthetic-token".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if monitors.status().await["channels"][0]["paused"] == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(f.requests().is_empty());
    assert_eq!(f.state()["credential_id"], "account");
    vault.clear().unwrap();
    monitors
        .start(
            json!([channel()]),
            "account".into(),
            "synthetic-token".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if monitors.status().await["channels"][0]["paused"] == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(f.requests().is_empty());
    vault.save(&tokens, &json!("account")).unwrap();
    f.replies([Reply::json(200, json!([]))]);
    monitors
        .start(
            json!([channel()]),
            "account".into(),
            "ignored-caller-token".into(),
        )
        .await
        .unwrap();
    f.idle(1).await;
    assert_eq!(f.requests().len(), 1);
    assert_eq!(monitors.status().await["channels"][0]["paused"], false);
    f.finish().await;
}

#[tokio::test]
async fn failed_final_checkpoint_leaves_exited_worker_paused_and_resumable_in_memory() {
    let f = Fixture::new(vec![], None).await;
    f.writer.write(|store| {
        store.connection().execute_batch("CREATE TRIGGER reject_monitor_pause BEFORE UPDATE ON collection_jobs WHEN NEW.kind='monitor' AND json_extract(NEW.state_json,'$.paused')=1 BEGIN SELECT RAISE(ABORT,'fixture pause write failure');END;")?;
        Ok(Value::Null)
    }).await.unwrap();
    f.replies([Reply::json(401, json!({}))]);
    f.start().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if f.monitors.status().await["channels"][0]["paused"] == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.state()["paused"], false);
    assert!(f.state()["last_id"].is_null());
    assert_eq!(f.requests().len(), 1);
    f.writer
        .write(|store| {
            store
                .connection()
                .execute_batch("DROP TRIGGER reject_monitor_pause")?;
            Ok(Value::Null)
        })
        .await
        .unwrap();
    f.replies([Reply::json(200, json!([]))]);
    f.start().await;
    f.idle(1).await;
    assert_eq!(f.monitors.status().await["channels"][0]["paused"], false);
    f.finish().await;
}

#[tokio::test]
async fn media_caches_after_identity_commit_before_broadcast_and_failed_binary_keeps_message() {
    struct MediaState {
        path: PathBuf,
        entered: Notify,
        release: Notify,
    }
    async fn media_response(
        State(state): State<Arc<MediaState>>,
        request: Request<Body>,
    ) -> Response<Body> {
        assert!(!request.headers().contains_key("authorization"));
        if request.uri().path().ends_with("failed.png") {
            return Response::builder().status(503).body(Body::empty()).unwrap();
        }
        assert_eq!(
            Connection::open(&state.path)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        state.entered.notify_one();
        state.release.notified().await;
        Response::builder()
            .header("content-type", "image/png")
            .body(Body::from("synthetic image bytes"))
            .unwrap()
    }
    let f = Fixture::new(vec![], None).await;
    let media_state = Arc::new(MediaState {
        path: f.path.clone(),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .fallback(media_response)
        .with_state(media_state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    f.monitors.clone().with_media(
        MediaCollector::for_loopback(
            &f.path,
            &origin,
            Limits {
                disk_free_bytes: Some(2 * 1024 * 1024 * 1024),
                ..Limits::default()
            },
        )
        .unwrap(),
    );
    f.replies([Reply::json(200, json!([message(1, "anchor")]))]);
    f.start().await;
    f.idle(1).await;
    let (_, mut events) = f.monitors.subscribe().await;
    let url = "https://cdn.discordapp.com/attachments/20/40/saved.png";
    let mut observed = message(2, "saved with media");
    observed["attachments"] = json!([{"url":url}]);
    observed["author"]["avatar"] = Value::Null;
    f.replies([Reply::json(200, json!([observed]))]);
    f.clock.0.ticks.add_permits(1);
    media_state.entered.notified().await;
    assert_eq!(f.state()["last_id"], (BASE + 2).to_string());
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    media_state.release.notify_one();
    f.idle(2).await;
    assert_eq!(receive(&mut events).await["content"], "saved with media");
    assert_eq!(
        searchcord_archive::media::status(&f.path, url, true)["status"],
        "saved"
    );
    let failed_url = "https://cdn.discordapp.com/attachments/20/41/failed.png";
    let mut observed = message(3, "identity survives media failure");
    observed["attachments"] = json!([{"url":failed_url}]);
    observed["author"]["avatar"] = Value::Null;
    f.replies([Reply::json(200, json!([observed]))]);
    f.tick().await;
    assert_eq!(
        receive(&mut events).await["content"],
        "identity survives media failure"
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 2);
    assert_eq!(f.state()["last_id"], (BASE + 3).to_string());
    assert_eq!(
        searchcord_archive::media::status(&f.path, failed_url, true)["reason"],
        "http_503"
    );
    f.finish().await;
    server.abort();
}

#[tokio::test]
async fn slow_subscriber_is_bounded_and_cannot_block_monitor_commits() {
    let f = Fixture::new(vec![message(1, "anchor")], Some(checkpoint(1))).await;
    let (_, mut events) = f.monitors.subscribe().await;
    f.start().await;
    f.idle(1).await;
    assert_eq!(receive(&mut events).await["type"], "monitor_start");
    for index in 0..11 {
        let high = 551 - index * 50;
        let page: Vec<_> = (high - 49..=high)
            .rev()
            .map(|id| message(id, "bounded events"))
            .collect();
        f.replies([Reply::json(200, json!(page))]);
        f.tick().await;
    }
    assert_eq!(f.count("SELECT COUNT(*) FROM messages"), 551);
    assert!(matches!(
        events.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    assert_eq!(f.monitors.status().await["channels"][0]["paused"], false);
    f.finish().await;
}
