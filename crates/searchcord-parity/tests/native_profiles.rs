//! Native profile collection uses disposable archives and numeric loopback only.
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, Response},
    routing::get,
};
use rusqlite::{Connection, params};
use searchcord_collector::{
    Result, Store,
    discord::Discord,
    pacing::{Clock, Control, Gate, Options},
    profiles::{ProfileError, Profiles, RetryInfo},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};

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
#[derive(Default)]
struct Block {
    entered: Notify,
    release: Notify,
}
struct Reply {
    status: u16,
    body: String,
    block: Option<Arc<Block>>,
    retry_after: Option<&'static str>,
}
impl Reply {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            block: None,
            retry_after: None,
        }
    }
}
#[derive(Clone, Debug)]
struct RequestTrace {
    uid: String,
    query: String,
    saved_before: i64,
}
struct MockState {
    path: PathBuf,
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<RequestTrace>>,
}
async fn serve(State(state): State<Arc<MockState>>, request: Request<Body>) -> Response<Body> {
    assert_eq!(request.headers()["authorization"], "synthetic-token");
    let uid = request.uri().path().split('/').nth(4).unwrap().to_owned();
    let saved_before = count(&state.path);
    state.requests.lock().unwrap().push(RequestTrace {
        uid: uid.clone(),
        query: request.uri().query().unwrap_or("").into(),
        saved_before,
    });
    let reply = state
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| Reply::json(200, payload(&uid, "default")));
    if let Some(block) = reply.block {
        block.entered.notify_one();
        block.release.notified().await;
    }
    let mut response = Response::builder()
        .status(reply.status)
        .header("content-type", "application/json");
    if let Some(value) = reply.retry_after {
        response = response.header("retry-after", value);
    }
    response.body(Body::from(reply.body)).unwrap()
}
struct Fixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    profiles: Profiles<FastClock>,
    state: Arc<MockState>,
    server: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(ids: &[&str]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic.db");
        let store = Store::open(&path).unwrap();
        for uid in ids {
            store
                .connection()
                .execute(
                    "INSERT INTO authors(id,name) VALUES (?,?)",
                    params![uid, format!("author-{uid}")],
                )
                .unwrap();
        }
        drop(store);
        let state = Arc::new(MockState {
            path: path.clone(),
            replies: Mutex::new(VecDeque::new()),
            requests: Mutex::new(vec![]),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/api/v10/users/{uid}/profile", get(serve))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let discord = Discord::for_loopback(
            Gate::open(path.clone(), FastClock::default())
                .await
                .unwrap(),
            &origin,
        )
        .unwrap();
        Self {
            _directory: directory,
            profiles: Profiles::new(path.clone(), discord),
            path,
            state,
            server,
        }
    }
    fn replies(&self, replies: impl IntoIterator<Item = Reply>) {
        self.state.replies.lock().unwrap().extend(replies);
    }
    fn requests(&self) -> Vec<RequestTrace> {
        self.state.requests.lock().unwrap().clone()
    }
    async fn done(&self) -> searchcord_collector::profiles::BackfillState {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let state = self.profiles.state();
                if !state.running {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("backfill finishes")
    }
}
fn payload(uid: &str, name: &str) -> Value {
    json!({"user":{"id":uid,"username":name,"global_name":"Héllo","avatar":"avatar1","banner":"banner1","bot":false,"accent_color":0,"public_flags":64},
        "user_profile":{"bio":"complete bio","pronouns":"they/them","banner":"detailbanner"},
        "presence":{"status":"idle","activities":[{"type":4,"state":"saved status"}]},
        "unknown":{"nested":[1,false,null,"雪"],"float":1e-7},"connected_accounts":[{"type":"github","name":"synthetic"}]})
}
fn count(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM profile_details", [], |r| r.get(0))
        .unwrap()
}
fn saved(path: &Path, uid: &str) -> Value {
    let raw: String = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT payload FROM profile_details WHERE user_id=?",
            [uid],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
fn status(err: &(dyn std::error::Error + Send + Sync + 'static)) -> u16 {
    err.downcast_ref::<ProfileError>().unwrap().status
}

#[tokio::test]
async fn manual_fetch_refresh_preserve_complete_payload_and_failed_requests_leave_saved_data() {
    let f = Fixture::new(&["30"]).await;
    assert!(!f.profiles.state().running);
    let original = payload("30", "saved");
    f.replies([
        Reply::json(200, original.clone()),
        Reply::json(503, json!({})),
        Reply::json(200, payload("31", "wrong")),
        Reply {
            status: 200,
            body: "not JSON".into(),
            block: None,
            retry_after: None,
        },
        Reply::json(200, json!([])),
    ]);
    assert_eq!(
        f.profiles
            .fetch("30", "synthetic-token", Control::default())
            .await
            .unwrap(),
        original
    );
    assert_eq!(saved(&f.path, "30"), original);
    let db = Connection::open(&f.path).unwrap();
    let fields: (String, String, String, i64, i64, String) = db.query_row("SELECT username,avatar_hash,banner_hash,accent_color,bot,fetched_at FROM profiles WHERE user_id='30'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    assert_eq!(
        (&fields.0, &fields.1, &fields.2, fields.3, fields.4),
        (
            &"saved".to_owned(),
            &"avatar1".to_owned(),
            &"banner1".to_owned(),
            0,
            0
        )
    );
    assert!(fields.5.ends_with("+00:00"));
    for expected in [503, 502, 502, 502] {
        let err = f
            .profiles
            .fetch("30", "synthetic-token", Control::default())
            .await
            .unwrap_err();
        assert_eq!(status(err.as_ref()), expected);
        assert_eq!(saved(&f.path, "30"), original);
    }
    let before = f.requests().len();
    for uid in [
        "",
        "0",
        "-1",
        "a",
        "9223372036854775808",
        "12345678901234567890",
    ] {
        assert_eq!(
            status(
                f.profiles
                    .fetch(uid, "synthetic-token", Control::default())
                    .await
                    .unwrap_err()
                    .as_ref()
            ),
            400
        );
    }
    assert_eq!(
        status(
            f.profiles
                .fetch("99", "synthetic-token", Control::default())
                .await
                .unwrap_err()
                .as_ref()
        ),
        404
    );
    assert_eq!(f.requests().len(), before);
    assert!(
        f.requests()
            .iter()
            .all(|r| r.query == "with_mutual_guilds=true&with_mutual_friends_count=false")
    );
    db.execute_batch("CREATE TRIGGER fixture_fail BEFORE INSERT ON profile_details BEGIN SELECT RAISE(ABORT,'fixture save rejected'); END;").unwrap();
    assert!(
        f.profiles
            .fetch("30", "synthetic-token", Control::default())
            .await
            .is_err()
    );
    assert_eq!(saved(&f.path, "30"), original);
    assert_eq!(
        db.query_row(
            "SELECT username FROM profiles WHERE user_id='30'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "saved"
    );
}

#[tokio::test]
async fn native_saves_are_readable_by_python_and_python_saves_skip_native_collection() {
    let f = Fixture::new(&["30"]).await;
    let original = payload("30", "native");
    f.replies([Reply::json(200, original.clone())]);
    f.profiles
        .fetch("30", "synthetic-token", Control::default())
        .await
        .unwrap();
    let path = f.path.clone();
    let python = tokio::task::spawn_blocking(move || {
        let script = "import sys,json,sqlite3,asyncio;sys.path.insert(0,sys.argv[1]);from profile_store import read_profile;from profile_api import save_profile;import aiosqlite\nwith sqlite3.connect(sys.argv[2]) as db: print(json.dumps(read_profile(db,'30'),ensure_ascii=False))\nasync def write():\n async with aiosqlite.connect(sys.argv[2]) as db:\n  payload={'user':{'id':'30','username':'python'},'python_unknown':{'kept':True}}\n  await save_profile(db,'30',payload['user'],payload,'2026-01-01T00:00:00+00:00');await db.commit()\nasyncio.run(write())";
        std::process::Command::new(std::env::var_os("SEARCHCORD_PARITY_PYTHON").unwrap_or("python".into()))
            .args(["-I","-X","utf8","-c",script]).arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .arg(path).env("PYTHONDONTWRITEBYTECODE","1").output().unwrap()
    }).await.unwrap();
    assert!(
        python.status.success(),
        "{}",
        String::from_utf8_lossy(&python.stderr)
    );
    let profile: Value = serde_json::from_slice(&python.stdout).unwrap();
    assert_eq!(profile["details"], original);
    assert_eq!(profile["bio"], "complete bio");
    assert_eq!(profile["observation_kind"], "profile_fetch");
    assert!(
        !f.profiles
            .collect_missing("30", "synthetic-token", Options::default(), None)
            .await
            .unwrap()
    );
    assert_eq!(saved(&f.path, "30")["python_unknown"], json!({"kept":true}));
    assert_eq!(f.requests().len(), 1);
}

#[tokio::test]
async fn retryable_responses_stay_on_same_user_and_stop_interrupts_retry_wait() {
    let f = Fixture::new(&["30"]).await;
    let retried = Arc::new(Mutex::new(Vec::<RetryInfo>::new()));
    f.replies([
        Reply::json(408, json!({})),
        Reply::json(200, payload("30", "retried")),
    ]);
    let info = retried.clone();
    f.profiles
        .collect(
            "30",
            "synthetic-token",
            Options::default(),
            Some(Arc::new(move |v| info.lock().unwrap().push(v))),
        )
        .await
        .unwrap();
    assert_eq!(
        f.requests()
            .iter()
            .map(|v| v.uid.as_str())
            .collect::<Vec<_>>(),
        ["30", "30"]
    );
    let info = retried.lock().unwrap();
    assert_eq!(
        (
            info[0].attempt,
            info[0].wait_seconds,
            info[0].reason.as_str()
        ),
        (1, 2.0, "HTTP 408")
    );
    drop(info);
    for http in [401, 403, 404] {
        f.replies([Reply::json(http, json!({}))]);
        assert_eq!(
            status(
                f.profiles
                    .collect("30", "synthetic-token", Options::default(), None)
                    .await
                    .unwrap_err()
                    .as_ref()
            ),
            http
        );
    }
    let old = saved(&f.path, "30");
    for http in [425, 429, 500, 502, 503, 599] {
        f.replies([Reply::json(http, json!({"retry_after":7}))]);
        let retrying = Arc::new(Notify::new());
        let notify = retrying.clone();
        let options = Options::default();
        let control = options.control.clone();
        let profiles = f.profiles.clone();
        let task = tokio::spawn(async move {
            profiles
                .collect(
                    "30",
                    "synthetic-token",
                    options,
                    Some(Arc::new(move |info| {
                        assert_eq!(info.wait_seconds, 7.0);
                        notify.notify_one();
                    })),
                )
                .await
        });
        retrying.notified().await;
        control.stop();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(saved(&f.path, "30"), old);
    }
    let mut rate_limited = Reply::json(429, json!({"retry_after":"not-a-number"}));
    rate_limited.retry_after = Some("99");
    f.replies([rate_limited]);
    let err = f
        .profiles
        .fetch("30", "synthetic-token", Control::default())
        .await
        .unwrap_err();
    assert_eq!(err.downcast_ref::<ProfileError>().unwrap().retry_after, 0.0);
}

#[tokio::test]
async fn shared_exclusion_rechecks_missing_and_cancels_waiters() {
    let f = Fixture::new(&["30", "31"]).await;
    let block = Arc::new(Block::default());
    let mut reply = Reply::json(200, payload("30", "first"));
    reply.block = Some(block.clone());
    f.replies([reply]);
    let profiles = f.profiles.clone();
    let first = tokio::spawn(async move {
        profiles
            .fetch("30", "synthetic-token", Control::default())
            .await
    });
    block.entered.notified().await;
    let profiles = f.profiles.clone();
    let missing = tokio::spawn(async move {
        profiles
            .collect_missing("30", "synthetic-token", Options::default(), None)
            .await
    });
    let options = Options::default();
    let control = options.control.clone();
    let profiles = f.profiles.clone();
    let waiter = tokio::spawn(async move {
        profiles
            .collect_missing("31", "synthetic-token", options, None)
            .await
    });
    tokio::task::yield_now().await;
    control.stop();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(f.requests().len(), 1);
    block.release.notify_one();
    first.await.unwrap().unwrap();
    assert!(!missing.await.unwrap().unwrap());
    f.profiles
        .fetch("31", "synthetic-token", Control::default())
        .await
        .unwrap();
    assert_eq!(f.requests()[1].saved_before, 1);
    assert_eq!(count(&f.path), 2);
    let paused = Control::default();
    paused.pause();
    let profiles = f.profiles.clone();
    let stop = paused.clone();
    let task = tokio::spawn(async move { profiles.fetch("30", "synthetic-token", paused).await });
    tokio::task::yield_now().await;
    stop.stop();
    assert!(task.await.unwrap().is_err());
    assert_eq!(f.requests().len(), 2);
}

#[tokio::test]
async fn dropped_call_keeps_exclusion_until_save_and_explicit_stop_cancels_active_request() {
    let f = Fixture::new(&["30", "31"]).await;
    let block = Arc::new(Block::default());
    let mut reply = Reply::json(200, payload("30", "accepted"));
    reply.block = Some(block.clone());
    f.replies([reply]);
    let profiles = f.profiles.clone();
    let caller = tokio::spawn(async move {
        profiles
            .fetch("30", "synthetic-token", Control::default())
            .await
    });
    block.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let profiles = f.profiles.clone();
    let successor = tokio::spawn(async move {
        profiles
            .collect_missing("30", "synthetic-token", Options::default(), None)
            .await
    });
    block.release.notify_one();
    assert!(!successor.await.unwrap().unwrap());
    assert_eq!(count(&f.path), 1);
    assert_eq!(f.requests().len(), 1);

    let block = Arc::new(Block::default());
    let mut reply = Reply::json(200, payload("31", "stopped"));
    reply.block = Some(block.clone());
    f.replies([reply]);
    let profiles = f.profiles.clone();
    let control = Control::default();
    let stop = control.clone();
    let active =
        tokio::spawn(async move { profiles.fetch("31", "synthetic-token", control).await });
    block.entered.notified().await;
    stop.stop();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), active)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(count(&f.path), 1);
    block.release.notify_one();
    assert!(
        f.profiles
            .collect_missing("31", "synthetic-token", Options::default(), None)
            .await
            .unwrap()
    );
    assert_eq!(count(&f.path), 2);
}

#[tokio::test]
async fn backfill_is_stopped_on_startup_exclusive_bounded_and_refreshes_only_stale_profiles() {
    let ids: Vec<String> = (1000..1102).map(|v| v.to_string()).collect();
    let f = Fixture::new(&ids.iter().map(String::as_str).collect::<Vec<_>>()).await;
    assert_eq!(f.profiles.state().status, "idle");
    assert!(f.requests().is_empty());
    let block = Arc::new(Block::default());
    let mut reply = Reply::json(200, payload("1000", "first"));
    reply.block = Some(block.clone());
    f.replies([reply]);
    assert!(
        f.profiles
            .start("synthetic-token", false)
            .await
            .unwrap()
            .running
    );
    block.entered.notified().await;
    assert_eq!(
        status(
            f.profiles
                .start("synthetic-token", false)
                .await
                .unwrap_err()
                .as_ref()
        ),
        409
    );
    block.release.notify_one();
    let done = f.done().await;
    assert_eq!(
        (done.saved, done.failed, done.status.as_str()),
        (102, 0, "Complete.")
    );
    let requests = f.requests();
    assert_eq!(requests.len(), 102);
    assert!(
        requests
            .iter()
            .enumerate()
            .all(|(i, v)| v.saved_before == i as i64)
    );
    f.profiles.start("synthetic-token", false).await.unwrap();
    assert_eq!(f.done().await.saved, 0);
    assert_eq!(f.requests().len(), 102);
    Connection::open(&f.path).unwrap().execute("UPDATE profile_details SET fetched_at='2000-01-01T00:00:00+00:00' WHERE user_id IN ('1000','1101')",[]).unwrap();
    f.profiles.start("synthetic-token", true).await.unwrap();
    assert_eq!(f.done().await.saved, 2);
    assert_eq!(
        f.requests()[102..]
            .iter()
            .map(|v| v.uid.as_str())
            .collect::<Vec<_>>(),
        ["1000", "1101"]
    );
}

#[tokio::test]
async fn backfill_stops_on_auth_failure_continues_unavailable_and_joins_immediate_stop() {
    let f = Fixture::new(&["30", "31", "32"]).await;
    f.replies([Reply::json(403, json!({})), Reply::json(401, json!({}))]);
    f.profiles.start("synthetic-token", false).await.unwrap();
    let done = f.done().await;
    assert_eq!((done.saved, done.failed), (0, 2));
    assert_eq!(done.last_error.as_deref(), Some("HTTP 401 for user 31"));
    assert!(done.status.starts_with("Stopped after Discord HTTP 401"));
    assert_eq!(f.requests().len(), 2);
    let before = f.requests().len();
    f.profiles.start("synthetic-token", false).await.unwrap();
    let stopped = f.profiles.stop().await;
    assert!(!stopped.running);
    assert!(stopped.cancelled);
    assert_eq!(f.requests().len(), before);
    f.profiles.start("synthetic-token", false).await.unwrap();
    assert_eq!(f.done().await.saved, 3);
    assert_eq!(count(&f.path), 3);
}
