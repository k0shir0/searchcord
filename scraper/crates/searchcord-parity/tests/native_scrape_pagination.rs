//! Independent public-ID expectations for history, newer, and resumed gap stages.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::Response,
};
use searchcord_collector::{
    PageContext, Result, Store,
    discord::Discord,
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
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
const BASE: u64 = 1_450_000_000_000_000_000;
fn id(n: u64) -> String {
    (BASE + n).to_string()
}
fn message(n: u64) -> Value {
    json!({"id":id(n),"channel_id":"20","author":{"id":"30","username":"Alice"},"content":format!("message {n}"),"timestamp":"2026-01-01T12:00:00+00:00","attachments":[]})
}
fn source() -> Value {
    json!({"id":"20","name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
#[derive(Clone, Default)]
struct ClockFixture(Arc<AtomicU64>);
impl Clock for ClockFixture {
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
struct Fixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: Scrapes<ClockFixture>,
    writer: Writer,
    trace: Arc<Mutex<Vec<String>>>,
    replies: Arc<Mutex<VecDeque<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(seed: impl FnOnce(&mut Store), replies: Vec<Value>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.db");
        let mut store = Store::open(&path).unwrap();
        seed(&mut store);
        drop(store);
        let trace = Arc::new(Mutex::new(vec![]));
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        type FixtureState = (Arc<Mutex<Vec<String>>>, Arc<Mutex<VecDeque<Value>>>);
        async fn response(
            State((trace, replies)): State<FixtureState>,
            request: Request,
        ) -> Response<Body> {
            trace.lock().unwrap().push(request.uri().to_string());
            let value = replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected pagination request");
            Response::new(Body::from(value.to_string()))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(response)
            .with_state((trace.clone(), replies.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let clock = ClockFixture::default();
        let writer = Writer::open(path.clone()).await.unwrap();
        let client =
            Discord::for_loopback(Gate::open(path.clone(), clock).await.unwrap(), &origin).unwrap();
        let profiles = Profiles::new(path.clone(), client.clone());
        let owner = Scrapes::open(path.clone(), writer.clone(), client, profiles)
            .await
            .unwrap();
        Self {
            _directory: directory,
            path,
            owner,
            writer,
            trace,
            replies,
            server,
        }
    }
    async fn run(&self, limit: u64) -> Value {
        let job = self
            .owner
            .start(
                json!([source()]),
                limit,
                false,
                "account".into(),
                "synthetic-token".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = self.owner.snapshot(&job).await.unwrap();
                if snapshot["running"] == false {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
    fn messages(&self) -> Vec<String> {
        let db = rusqlite::Connection::open(&self.path).unwrap();
        db.prepare("SELECT CAST(id AS TEXT) FROM messages ORDER BY id DESC")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
    fn cursor(&self) -> Value {
        let db = rusqlite::Connection::open(&self.path).unwrap();
        db.query_row("SELECT newest_message_id,oldest_message_id,history_complete,pending_after_message_id,pending_before_message_id FROM scrape_cursors WHERE channel_id='20'",[],|r|Ok(json!({"newest":r.get::<_,String>(0)?,"oldest":r.get::<_,Option<String>>(1)?,"complete":r.get::<_,bool>(2)?,"after":r.get::<_,Option<String>>(3)?,"before":r.get::<_,Option<String>>(4)?}))).unwrap()
    }
}
fn context(history: bool) -> PageContext<'static> {
    PageContext {
        channel_id: "20",
        channel_name: "fixture",
        guild_id: Some("10"),
        guild_name: "Fixture",
        history_page: history,
        ..PageContext::default()
    }
}
#[tokio::test]
async fn complete_history_pages_keep_public_order_and_confirm_exhaustion() {
    let first: Vec<_> = (3..=102).rev().map(message).collect();
    let f = Fixture::new(|_| {}, vec![json!(first), json!([message(2), message(1)])]).await;
    let result = f.run(0).await;
    assert_eq!(result["total_messages"], 102);
    assert_eq!(result["completed"], 1);
    assert_eq!(result["last_event"]["failed_channels"], 0);
    assert_eq!(f.messages(), (1..=102).rev().map(id).collect::<Vec<_>>());
    assert_eq!(
        f.cursor(),
        json!({"newest":id(102),"oldest":id(1),"complete":true,"after":null,"before":null})
    );
    assert_eq!(
        *f.trace.lock().unwrap(),
        vec![
            "/api/v10/channels/20/messages?limit=100".to_owned(),
            format!("/api/v10/channels/20/messages?limit=100&before={}", id(3))
        ]
    );
    assert!(f.replies.lock().unwrap().is_empty());
    f.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn remaining_limit_counts_new_rows_while_duplicate_history_still_advances() {
    let f = Fixture::new(
        |store| {
            store
                .save_page(&[message(100), message(50)], &context(true))
                .unwrap();
            store.save_page(&[message(30)], &context(false)).unwrap();
        },
        vec![
            json!([message(110)]),
            json!([message(40), message(30)]),
            json!([message(20)]),
        ],
    )
    .await;
    let result = f.run(3).await;
    assert_eq!(result["total_messages"], 3);
    assert_eq!(result["rows"][0]["messages"], 3);
    assert_eq!(
        f.messages(),
        [110, 100, 50, 40, 30, 20]
            .into_iter()
            .map(id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        f.cursor(),
        json!({"newest":id(110),"oldest":id(20),"complete":false,"after":null,"before":null})
    );
    assert_eq!(
        *f.trace.lock().unwrap(),
        vec![
            format!("/api/v10/channels/20/messages?limit=3&after={}", id(100)),
            format!("/api/v10/channels/20/messages?limit=2&before={}", id(50)),
            format!("/api/v10/channels/20/messages?limit=1&before={}", id(30))
        ]
    );
    f.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn resumed_gap_drains_saved_anchor_before_fetching_newer_high_water_mark() {
    let f = Fixture::new(
        |store| {
            let mut context = context(true);
            context.history_complete = true;
            context.pending = Some((Some("1450000000000000100"), Some("1450000000000000130")));
            store
                .save_page(&[message(150), message(100)], &context)
                .unwrap();
        },
        vec![
            json!([message(120), message(110), message(100)]),
            json!([message(170), message(160)]),
        ],
    )
    .await;
    let result = f.run(0).await;
    assert_eq!(result["total_messages"], 4);
    assert_eq!(
        f.messages(),
        [170, 160, 150, 120, 110, 100]
            .into_iter()
            .map(id)
            .collect::<Vec<_>>()
    );
    let cursor = f.cursor();
    assert_eq!(cursor["newest"], id(170));
    assert_eq!(cursor["complete"], true);
    assert!(cursor["after"].is_null());
    assert!(cursor["before"].is_null());
    assert_eq!(
        *f.trace.lock().unwrap(),
        vec![
            format!("/api/v10/channels/20/messages?limit=100&before={}", id(130)),
            format!("/api/v10/channels/20/messages?limit=100&after={}", id(150))
        ]
    );
    f.writer.shutdown().await.unwrap();
}
