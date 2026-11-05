//! Independent native versions of the nine durable_scrape contracts. Real
//! SQLite triggers block/fail accepted checkpoints; HTTP stays on loopback.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::Response,
};
use rusqlite::{Connection, functions::FunctionFlags};
use searchcord_collector::{
    PageContext, Result,
    accounts::Accounts,
    credentials::{Paths, Vault},
    discord::Discord,
    discovery::HttpError,
    pacing::{Clock, Gate},
    profiles::Profiles,
    scrapes::Scrapes,
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
use tokio::sync::{Notify, oneshot};
const BASE: i64 = 1_450_000_000_000_000_000;
#[derive(Clone, Default)]
struct FastClock(Arc<AtomicU64>);
impl Clock for FastClock {
    fn now(&self) -> f64 {
        1000.0 + self.0.load(Ordering::SeqCst) as f64 / 1000.0
    }
    fn wall(&self) -> f64 {
        1_700_000_000.0 + self.now()
    }
    fn uniform(&self, low: f64, _high: f64) -> Result<f64> {
        Ok(low)
    }
    async fn sleep(&self, seconds: f64) {
        self.0
            .fetch_add((seconds * 1000.0).ceil() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
fn channel(cid: &str) -> Value {
    json!({"id":cid,"name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
fn message(n: i64) -> Value {
    json!({"id":(BASE+n).to_string(),"channel_id":"20","author":{"id":"30","username":"fixture"},"content":"synthetic","attachments":[],"timestamp":"2026-01-01T00:00:00+00:00"})
}
fn context() -> PageContext<'static> {
    PageContext {
        channel_id: "20",
        channel_name: "fixture",
        guild_id: Some("10"),
        guild_name: "Fixture",
        history_page: true,
        ..PageContext::default()
    }
}
#[derive(Default)]
struct HttpBlock {
    entered: Notify,
    release: Notify,
}
struct Reply {
    body: Value,
    block: Option<Arc<HttpBlock>>,
}
impl Reply {
    fn json(body: Value) -> Self {
        Self { body, block: None }
    }
}
#[derive(Clone, Debug)]
struct Trace {
    path: String,
    query: String,
}
struct Mock {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<Trace>>,
    changed: Notify,
}
async fn serve(State(mock): State<Arc<Mock>>, request: Request) -> Response<Body> {
    assert_eq!(request.headers()["authorization"], "synthetic-token");
    mock.requests.lock().unwrap().push(Trace {
        path: request.uri().path().into(),
        query: request.uri().query().unwrap_or("").into(),
    });
    mock.changed.notify_waiters();
    let reply = mock
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| Reply::json(json!([])));
    if let Some(block) = reply.block {
        block.entered.notify_one();
        block.release.notified().await;
    }
    Response::new(Body::from(reply.body.to_string()))
}
struct Fixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    writer: Writer,
    client: Discord<FastClock>,
    profiles: Profiles<FastClock>,
    owner: Scrapes<FastClock>,
    mock: Arc<Mock>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic.db");
        let writer = Writer::open(path.clone()).await.unwrap();
        let mock = Arc::new(Mock {
            replies: Mutex::new(VecDeque::new()),
            requests: Mutex::new(vec![]),
            changed: Notify::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(serve).with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Discord::for_loopback(
            Gate::open(path.clone(), FastClock::default())
                .await
                .unwrap(),
            &origin,
        )
        .unwrap();
        let profiles = Profiles::new(path.clone(), client.clone());
        let owner = Scrapes::open(
            path.clone(),
            writer.clone(),
            client.clone(),
            profiles.clone(),
        )
        .await
        .unwrap();
        Self {
            _directory: directory,
            path,
            writer,
            client,
            profiles,
            owner,
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
    async fn start(&self, limit: u64, profiles: bool, channels: Value) -> String {
        self.owner
            .start(
                channels,
                limit,
                profiles,
                "account".into(),
                "synthetic-token".into(),
            )
            .await
            .unwrap()
    }
    async fn reopen(&self) -> Scrapes<FastClock> {
        Scrapes::open(
            self.path.clone(),
            self.writer.clone(),
            self.client.clone(),
            self.profiles.clone(),
        )
        .await
        .unwrap()
    }
    async fn until(
        &self,
        owner: &Scrapes<FastClock>,
        id: &str,
        key: &str,
        expected: Value,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = owner.snapshot(id).await.unwrap();
                if state[key] == expected {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap()
    }
    fn count(&self, table: &str) -> i64 {
        assert!(["messages", "scrape_cursors", "profile_details"].contains(&table));
        Connection::open(&self.path)
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn saved(&self, id: &str) -> Value {
        let raw: String = Connection::open(&self.path)
            .unwrap()
            .query_row(
                "SELECT state_json FROM collection_jobs WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("synthetic-token"));
        serde_json::from_str(&raw).unwrap()
    }
    async fn checkpoint_gate(&self, point: Point, failures: usize) -> CommitGate {
        let (entered, ready) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let count = attempts.clone();
        self.writer.write(move|store|{
            let entered=Mutex::new(Some(entered));
            store.connection().create_scalar_function("lifecycle_checkpoint_gate",0,FunctionFlags::SQLITE_UTF8,move|_|{
                let attempt=count.fetch_add(1,Ordering::SeqCst);
                if attempt==0{let _=entered.lock().unwrap().take().unwrap().send(());wait.recv().map_err(|e|rusqlite::Error::UserFunctionError(e.into()))?;}
                if attempt<failures{return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),None));}
                Ok(0i64)
            })?;
            let (operation,condition)=match point{
                Point::Initial=>("INSERT","NEW.kind='scrape'"),
                Point::Progress=>("UPDATE","NEW.kind='scrape' AND json_extract(NEW.state_json,'$.last_event.type')='progress'"),
                Point::Pause=>("UPDATE","NEW.kind='scrape' AND json_extract(NEW.state_json,'$.pause_requested')=1 AND json_extract(OLD.state_json,'$.pause_requested')=0"),
                Point::Resume=>("UPDATE","NEW.kind='scrape' AND json_extract(NEW.state_json,'$.pause_requested')=0 AND json_extract(OLD.state_json,'$.pause_requested')=1"),
                Point::Terminal=>("UPDATE","NEW.kind='scrape' AND json_extract(NEW.state_json,'$.running')=0"),
            };
            store.connection().execute_batch(&format!("DROP TRIGGER IF EXISTS temp.lifecycle_gate;CREATE TEMP TRIGGER lifecycle_gate BEFORE {operation} ON main.collection_jobs WHEN {condition} BEGIN SELECT lifecycle_checkpoint_gate();END;"))?;Ok(Value::Null)
        }).await.unwrap();
        CommitGate {
            ready: Some(ready),
            release: Some(release),
            attempts,
        }
    }
    async fn finish(&self) {
        self.owner.suspend().await.unwrap();
        self.writer.shutdown().await.unwrap();
    }
}
enum Point {
    Initial,
    Progress,
    Pause,
    Resume,
    Terminal,
}
struct CommitGate {
    ready: Option<oneshot::Receiver<()>>,
    release: Option<std::sync::mpsc::Sender<()>>,
    attempts: Arc<AtomicUsize>,
}
impl CommitGate {
    async fn entered(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.ready.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
    fn release(&mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }
}
fn old_checkpoint() -> Value {
    json!({"running":true,"cancelled":false,"pause_requested":false,"paused":false,"channels":[channel("20")],"limit":3,"harvest_profiles":false,"index":1,"total_messages":2,"completed":0,"rows":[{"type":"progress","channel":"fixture","messages":2,"total_messages":2}],"last_event":{"type":"progress","channel":"fixture","messages":2,"total_messages":2},"next_index":0,"failed_channels":0,"credential_id":"account","profile_auth_failed":null,"channel_state":{"stage":"history","messages":2,"profiles":0,"pending_profiles":[],"anchor":null,"before":(BASE+40).to_string(),"newest":null,"oldest":null,"history_complete":false,"resuming_pending":false}})
}
async fn seed_old(f: &Fixture) {
    f.writer
        .write(|store| {
            store.save_page(&[message(41), message(40)], &context())?;
            store.connection().execute(
                "INSERT INTO collection_jobs VALUES ('old-scrape','scrape',?,0)",
                [old_checkpoint().to_string()],
            )?;
            Ok(Value::Null)
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn checkpoint_busy_failure_rolls_back_page_cursor_and_projection_then_retries_same_page() {
    let f = Fixture::new().await;
    let mut gate = f.checkpoint_gate(Point::Progress, 1).await;
    f.replies([Reply::json(json!([message(41), message(40)]))]);
    let id = f.start(3, false, json!([channel("20")])).await;
    gate.entered().await;
    assert_eq!(f.count("messages"), 0);
    assert_eq!(f.count("scrape_cursors"), 0);
    assert_eq!(f.saved(&id)["total_messages"], 0);
    let owner = f.owner.clone();
    let job = id.clone();
    let pause = tokio::spawn(async move { owner.pause(&job).await });
    gate.release();
    pause.await.unwrap().unwrap();
    let paused = f.until(&f.owner, &id, "paused", json!(true)).await;
    assert_eq!(paused["total_messages"], 2);
    assert_eq!(paused["rows"][0]["messages"], 2);
    assert_eq!(f.saved(&id)["rows"], paused["rows"]);
    let cursor: (String, String, i64) = Connection::open(&f.path)
        .unwrap()
        .query_row(
            "SELECT newest_message_id,oldest_message_id,history_complete FROM scrape_cursors",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        cursor,
        ((BASE + 41).to_string(), (BASE + 40).to_string(), 1)
    );
    assert_eq!(gate.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(f.requests().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn stop_joins_accepted_page_before_clear_and_cannot_recreate_cleared_state() {
    let f = Fixture::new().await;
    let mut gate = f.checkpoint_gate(Point::Progress, 0).await;
    f.replies([Reply::json(json!([message(41), message(40)]))]);
    let id = f.start(0, false, json!([channel("20")])).await;
    gate.entered().await;
    let owner = f.owner.clone();
    let job = id.clone();
    let stop = tokio::spawn(async move { owner.stop(&job).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!stop.is_finished());
    assert_eq!(f.count("messages"), 0);
    gate.release();
    stop.await.unwrap().unwrap();
    let state = f.owner.snapshot(&id).await.unwrap();
    assert_eq!(state["total_messages"], 2);
    assert_eq!(state["running"], false);
    assert_eq!(f.count("messages"), 2);
    assert!(f.owner.active().await.is_empty());
    f.writer.write(|store|{store.connection().execute_batch("DELETE FROM messages;DELETE FROM scrape_cursors;DELETE FROM collection_jobs WHERE kind='scrape';")?;Ok(Value::Null)}).await.unwrap();
    f.writer.barrier().await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(f.count("messages"), 0);
    assert_eq!(
        Connection::open(&f.path)
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM collection_jobs WHERE kind='scrape'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    f.finish().await;
}

#[tokio::test]
async fn shutdown_joins_page_and_recovers_remaining_channel_budget_and_queue() {
    let f = Fixture::new().await;
    let mut gate = f.checkpoint_gate(Point::Progress, 0).await;
    f.replies([Reply::json(json!(
        (101..=200).rev().map(message).collect::<Vec<_>>()
    ))]);
    let id = f
        .start(101, false, json!([channel("20"), channel("21")]))
        .await;
    gate.entered().await;
    let owner = f.owner.clone();
    let shutdown = tokio::spawn(async move { owner.suspend().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!shutdown.is_finished());
    gate.release();
    shutdown.await.unwrap().unwrap();
    let recovered = f.reopen().await;
    let snapshot = recovered.snapshot(&id).await.unwrap();
    assert_eq!(snapshot["paused"], true);
    assert_eq!(snapshot["total_messages"], 100);
    assert_eq!(snapshot["rows"][0]["messages"], 100);
    assert_eq!(snapshot["channels"], json!([channel("20"), channel("21")]));
    f.replies([Reply::json(json!([message(100)]))]);
    recovered
        .resume(&id, "account", "synthetic-token".into())
        .await
        .unwrap();
    let done = f.until(&recovered, &id, "running", json!(false)).await;
    assert_eq!(done["total_messages"], 101);
    assert_eq!(done["completed"], 2);
    assert_eq!(
        f.requests()[1].query,
        format!("limit=1&before={}", BASE + 101)
    );
    assert_eq!(f.requests()[2].path, "/api/v10/channels/21/messages");
    assert_eq!(f.count("messages"), 101);
    recovered.suspend().await.unwrap();
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn recovery_restores_pending_profiles_before_remaining_page_budget() {
    let f = Fixture::new().await;
    let block = Arc::new(HttpBlock::default());
    f.replies([Reply {
        body: json!((101..=200).rev().map(message).collect::<Vec<_>>()),
        block: Some(block.clone()),
    }]);
    let id = f.start(101, true, json!([channel("20")])).await;
    block.entered.notified().await;
    f.owner.pause(&id).await.unwrap();
    block.release.notify_one();
    f.until(&f.owner, &id, "paused", json!(true)).await;
    f.owner.suspend().await.unwrap();
    assert_eq!(
        f.saved(&id)["channel_state"]["pending_profiles"],
        json!(["30"])
    );
    let recovered = f.reopen().await;
    assert_eq!(
        recovered
            .start(
                json!([channel("20")]),
                1,
                false,
                "account".into(),
                "synthetic-token".into()
            )
            .await
            .unwrap_err()
            .downcast_ref::<HttpError>()
            .unwrap()
            .status,
        409
    );
    f.replies([
        Reply::json(json!({"user":{"id":"30","username":"fixture"}})),
        Reply::json(json!([message(100)])),
    ]);
    recovered
        .resume(&id, "account", "synthetic-token".into())
        .await
        .unwrap();
    let done = f.until(&recovered, &id, "running", json!(false)).await;
    assert_eq!(done["total_messages"], 101);
    assert_eq!(f.count("profile_details"), 1);
    assert_eq!(
        f.requests()
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/api/v10/channels/20/messages",
            "/api/v10/users/30/profile",
            "/api/v10/channels/20/messages"
        ]
    );
    assert_eq!(
        f.requests()[2].query,
        format!("limit=1&before={}", BASE + 101)
    );
    recovered.suspend().await.unwrap();
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn literal_old_checkpoint_requires_original_account_and_keeps_remaining_budget() {
    let f = Fixture::new().await;
    seed_old(&f).await;
    let recovered = f.reopen().await;
    assert_eq!(
        recovered.snapshot("old-scrape").await.unwrap()["recovered"],
        true
    );
    assert_eq!(
        recovered
            .resume("old-scrape", "different", "synthetic-token".into())
            .await
            .unwrap_err()
            .downcast_ref::<HttpError>()
            .unwrap()
            .status,
        409
    );
    assert!(f.requests().is_empty());
    assert_eq!(f.saved("old-scrape"), old_checkpoint());
    f.replies([Reply::json(json!([message(39)]))]);
    recovered
        .resume("old-scrape", "account", "synthetic-token".into())
        .await
        .unwrap();
    assert_eq!(
        f.until(&recovered, "old-scrape", "running", json!(false))
            .await["total_messages"],
        3
    );
    assert_eq!(
        f.requests()[0].query,
        format!("limit=1&before={}", BASE + 40)
    );
    recovered.suspend().await.unwrap();
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn account_change_between_resume_and_worker_rechecks_binding_and_remains_paused() {
    // Isolate credential path overrides in a child process, including during
    // parallel test runs. The child only opens its own synthetic sealed vault.
    if std::env::var_os("SEARCHCORD_LIFECYCLE_ACCOUNT_CHILD").is_none() {
        let directory = tempfile::tempdir().unwrap();
        let vault = directory.path().join("synthetic.vault");
        let key = directory.path().join("synthetic.key");
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "account_change_between_resume_and_worker_rechecks_binding_and_remains_paused",
                    "--nocapture",
                ])
                .env("SEARCHCORD_LIFECYCLE_ACCOUNT_CHILD", "1")
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
    let f = Fixture::new().await;
    seed_old(&f).await;
    let accounts = Accounts::new(f.path.clone(), f.writer.clone());
    let recovered = f.reopen().await.with_accounts(accounts);
    recovered.pause("old-scrape").await.unwrap();
    let save_vault = |path: PathBuf, active: &'static str| {
        tokio::task::spawn_blocking(move || {
            Vault::platform(Paths::for_database(&path).unwrap()).save(&json!([{"id":"account","token":"synthetic-token","label":"Account"},{"id":"other","token":"synthetic-other","label":"Other"}]),&json!(active)).unwrap();
        })
    };
    save_vault(f.path.clone(), "account").await.unwrap();
    let mut gate = f.checkpoint_gate(Point::Resume, 0).await;
    let owner = recovered.clone();
    let resume = tokio::spawn(async move {
        owner
            .resume("old-scrape", "account", "synthetic-token".into())
            .await
    });
    gate.entered().await;
    save_vault(f.path.clone(), "other").await.unwrap();
    gate.release();
    resume.await.unwrap().unwrap();
    let state = f
        .until(&recovered, "old-scrape", "paused", json!(true))
        .await;
    assert_eq!(state["running"], true);
    assert_eq!(
        state["last_event"]["message"],
        "Select this job's saved account before resuming."
    );
    assert!(f.requests().is_empty());
    assert_eq!(f.saved("old-scrape")["credential_id"], "account");
    recovered.stop("old-scrape").await.unwrap();
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn simultaneous_starts_reserve_channel_before_initial_checkpoint_commits() {
    let f = Fixture::new().await;
    let block = Arc::new(HttpBlock::default());
    f.replies([Reply {
        body: json!([]),
        block: Some(block.clone()),
    }]);
    let mut gate = f.checkpoint_gate(Point::Initial, 0).await;
    let owner = f.owner.clone();
    let first = tokio::spawn(async move {
        owner
            .start(
                json!([channel("20")]),
                0,
                false,
                "account".into(),
                "synthetic-token".into(),
            )
            .await
    });
    gate.entered().await;
    let owner = f.owner.clone();
    let second = tokio::spawn(async move {
        owner
            .start(
                json!([channel("20")]),
                0,
                false,
                "account".into(),
                "synthetic-token".into(),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());
    gate.release();
    let id = first.await.unwrap().unwrap();
    let conflict = second.await.unwrap().unwrap_err();
    assert_eq!(conflict.downcast_ref::<HttpError>().unwrap().status, 409);
    assert_eq!(f.owner.active().await.len(), 1);
    f.owner.stop(&id).await.unwrap();
    block.release.notify_one();
    f.finish().await;
}

#[tokio::test]
async fn shutdown_during_initial_checkpoint_keeps_recoverable_job_without_requests() {
    let f = Fixture::new().await;
    let mut gate = f.checkpoint_gate(Point::Initial, 0).await;
    let owner = f.owner.clone();
    let start = tokio::spawn(async move {
        owner
            .start(
                json!([channel("20")]),
                0,
                false,
                "account".into(),
                "synthetic-token".into(),
            )
            .await
    });
    gate.entered().await;
    let owner = f.owner.clone();
    let shutdown = tokio::spawn(async move { owner.suspend().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!shutdown.is_finished());
    gate.release();
    let id = start.await.unwrap().unwrap();
    shutdown.await.unwrap().unwrap();
    assert_eq!(f.saved(&id)["paused"], true);
    assert_eq!(f.saved(&id)["total_messages"], 0);
    assert!(f.requests().is_empty());
    let recovered = f.reopen().await;
    assert_eq!(recovered.snapshot(&id).await.unwrap()["paused"], true);
    recovered.stop(&id).await.unwrap();
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn resume_after_shutdown_barrier_cannot_launch_worker() {
    let f = Fixture::new().await;
    seed_old(&f).await;
    let recovered = f.reopen().await;
    recovered.suspend().await.unwrap();
    let before = f.saved("old-scrape");
    let error = recovered
        .resume("old-scrape", "account", "synthetic-token".into())
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<HttpError>().unwrap().status, 409);
    assert_eq!(f.saved("old-scrape"), before);
    assert!(f.requests().is_empty());
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_pause_and_resume_callers_complete_flags_and_request_admission() {
    let f = Fixture::new().await;
    let block = Arc::new(HttpBlock::default());
    f.replies([Reply {
        body: json!((101..=200).rev().map(message).collect::<Vec<_>>()),
        block: Some(block.clone()),
    }]);
    let id = f.start(101, false, json!([channel("20")])).await;
    block.entered.notified().await;
    let mut pause_gate = f.checkpoint_gate(Point::Pause, 0).await;
    let owner = f.owner.clone();
    let job = id.clone();
    let pause = tokio::spawn(async move { owner.pause(&job).await });
    pause_gate.entered().await;
    pause.abort();
    assert!(pause.await.unwrap_err().is_cancelled());
    pause_gate.release();
    f.writer.barrier().await.unwrap();
    block.release.notify_one();
    let paused = f.until(&f.owner, &id, "paused", json!(true)).await;
    assert_eq!(paused["total_messages"], 100);
    assert_eq!(f.requests().len(), 1);
    let mut resume_gate = f.checkpoint_gate(Point::Resume, 0).await;
    f.replies([Reply::json(json!([message(100)]))]);
    let owner = f.owner.clone();
    let job = id.clone();
    let resume = tokio::spawn(async move {
        owner
            .resume(&job, "account", "synthetic-token".into())
            .await
    });
    resume_gate.entered().await;
    resume.abort();
    assert!(resume.await.unwrap_err().is_cancelled());
    resume_gate.release();
    assert_eq!(
        f.until(&f.owner, &id, "running", json!(false)).await["total_messages"],
        101
    );
    assert_eq!(f.requests().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn cancelled_start_still_owns_initial_checkpoint_and_can_be_stopped() {
    let f = Fixture::new().await;
    let block = Arc::new(HttpBlock::default());
    f.replies([Reply {
        body: json!([]),
        block: Some(block.clone()),
    }]);
    let mut gate = f.checkpoint_gate(Point::Initial, 0).await;
    let owner = f.owner.clone();
    let start = tokio::spawn(async move {
        owner
            .start(
                json!([channel("20")]),
                0,
                false,
                "account".into(),
                "synthetic-token".into(),
            )
            .await
    });
    gate.entered().await;
    start.abort();
    assert!(start.await.unwrap_err().is_cancelled());
    gate.release();
    let id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let active = f.owner.active().await;
            if let Some(job) = active.first() {
                break job["job_id"].as_str().unwrap().to_owned();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.owner.stop(&id).await.unwrap();
    assert_eq!(f.saved(&id)["running"], false);
    f.finish().await;
}

#[tokio::test]
async fn failed_terminal_checkpoint_keeps_job_controls_and_channel_reservation_after_retention() {
    let f = Fixture::new().await;
    let mut gate = f.checkpoint_gate(Point::Terminal, 1).await;
    f.replies([Reply::json(json!([message(1)]))]);
    let id = f.start(1, false, json!([channel("20")])).await;
    let entered = tokio::time::timeout(Duration::from_secs(5), gate.ready.take().unwrap()).await;
    assert!(
        entered.is_ok(),
        "terminal checkpoint not reached: saved={}, requests={:?}",
        f.saved(&id),
        f.requests()
    );
    entered.unwrap().unwrap();
    gate.release();
    f.writer.barrier().await.unwrap();
    f.owner.pause(&id).await.unwrap();
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;
    let state = f.owner.snapshot(&id).await.unwrap();
    assert_eq!(state["running"], true);
    assert_eq!(state["pause_requested"], true);
    assert_eq!(state["total_messages"], 1);
    assert_eq!(f.saved(&id)["running"], true);
    assert_eq!(f.owner.active().await.len(), 1);
    let error = f
        .owner
        .start(
            json!([channel("20")]),
            0,
            false,
            "account".into(),
            "synthetic-token".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<HttpError>().unwrap().status, 409);
    f.owner.stop(&id).await.unwrap();
    assert_eq!(f.saved(&id)["running"], false);
    assert_eq!(f.saved(&id)["cancelled"], true);
    assert_eq!(f.requests().len(), 1);
    tokio::time::resume();
    f.finish().await;
}

#[tokio::test]
async fn non_busy_pacing_checkpoint_failure_is_terminal_without_request_retry() {
    let f = Fixture::new().await;
    f.writer.write(|store| {
        store.connection().execute_batch("CREATE TRIGGER lifecycle_reject_pacing BEFORE INSERT ON collection_jobs WHEN NEW.kind='pacing' BEGIN SELECT RAISE(ABORT,'fixture non-busy checkpoint failure');END;")?;
        Ok(Value::Null)
    }).await.unwrap();
    f.replies([Reply::json(json!([message(1)]))]);
    let id = f.start(1, false, json!([channel("20")])).await;
    let done = f.until(&f.owner, &id, "running", json!(false)).await;
    assert_eq!(done["last_event"]["failed_channels"], 1);
    assert_eq!(done["total_messages"], 0);
    assert_eq!(f.requests().len(), 1);
    assert_eq!(f.count("messages"), 0);
    assert_eq!(f.count("scrape_cursors"), 0);
    f.finish().await;
}

#[tokio::test]
async fn cancelled_writer_shutdown_caller_still_joins_accepted_work_on_next_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic.db");
    let writer = Writer::open(path.clone()).await.unwrap();
    let (entered, ready) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = writer.clone();
    let accepted = tokio::spawn(async move {
        worker
            .write(move |store| {
                let _ = entered.send(());
                wait.recv()?;
                store
                    .connection()
                    .execute("INSERT INTO authors(id,name) VALUES ('30','accepted')", [])?;
                Ok(Value::Null)
            })
            .await
    });
    ready.await.unwrap();
    let worker = writer.clone();
    let shutdown = tokio::spawn(async move { worker.shutdown().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    let worker = writer.clone();
    let next = tokio::spawn(async move { worker.shutdown().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let joined = next.is_finished();
    release.send(()).unwrap();
    accepted.await.unwrap().unwrap();
    next.await.unwrap().unwrap();
    assert!(
        !joined,
        "the second shutdown must join the first shutdown owner"
    );
    assert_eq!(
        Connection::open(path)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM authors", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
