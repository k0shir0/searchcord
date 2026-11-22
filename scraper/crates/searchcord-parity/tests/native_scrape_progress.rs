use axum::{Router, body::Body, extract::State, http::Response};
use searchcord_collector::{
    Result, Store,
    discord::Discord,
    pacing::{Clock, Gate},
    profiles::Profiles,
    scrapes::Scrapes,
    writer::Writer,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
#[derive(Default)]
struct Time {
    millis: AtomicU64,
    blocked: AtomicBool,
    entered: Notify,
    release: Notify,
}
#[derive(Clone)]
struct ControlledClock(Arc<Time>);
impl Clock for ControlledClock {
    fn now(&self) -> f64 {
        1000.0 + self.0.millis.load(Ordering::SeqCst) as f64 / 1000.0
    }
    fn wall(&self) -> f64 {
        1_700_000_000.0 + self.now()
    }
    fn uniform(&self, low: f64, _high: f64) -> Result<f64> {
        Ok(low)
    }
    async fn sleep(&self, seconds: f64) {
        if self.0.blocked.load(Ordering::SeqCst) {
            self.0.entered.notify_one();
            self.0.release.notified().await;
        }
        self.0
            .millis
            .fetch_add((seconds * 1000.0).ceil() as u64, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}
struct Fixture {
    _directory: tempfile::TempDir,
    owner: Scrapes<ControlledClock>,
    writer: Writer,
    clock: ControlledClock,
    calls: Arc<AtomicU64>,
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
        let path = directory.path().join("archive.db");
        let store = Store::open(&path).unwrap();
        store.connection().execute("INSERT INTO collection_jobs VALUES ('discord-pacing','pacing',?,0)",[json!({"discord_ready_at":0,"last_scrape_request":0,"scrape_break_until":0,"scrape_confirmed_requests":99}).to_string()]).unwrap();
        drop(store);
        let calls = Arc::new(AtomicU64::new(0));
        async fn respond(State(calls): State<Arc<AtomicU64>>) -> Response<Body> {
            let count = calls.fetch_add(1, Ordering::SeqCst);
            let messages: Vec<_> = if count == 0 {
                (1..=100u64).rev().map(|n|json!({"id":(1450000000000000000u64+n).to_string(),"channel_id":"20","author":{"id":"30","username":"A"},"content":format!("message{n}"),"attachments":[]})).collect()
            } else {
                vec![]
            };
            Response::new(Body::from(json!(messages).to_string()))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(respond).with_state(calls.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let clock = ControlledClock(Arc::new(Time::default()));
        clock.0.blocked.store(true, Ordering::SeqCst);
        let writer = Writer::open(path.clone()).await.unwrap();
        let client = Discord::for_loopback(
            Gate::open(path.clone(), clock.clone()).await.unwrap(),
            &origin,
        )
        .unwrap();
        let profiles = Profiles::new(path.clone(), client.clone());
        let owner = Scrapes::open(path, writer.clone(), client, profiles)
            .await
            .unwrap();
        Self {
            _directory: directory,
            owner,
            writer,
            clock,
            calls,
            server,
        }
    }
    async fn start(&self) -> String {
        let id = self
            .owner
            .start(
                json!([{"id":"20","name":"fixture","guild_id":"10","guild_name":"Fixture"}]),
                0,
                false,
                "account".into(),
                "synthetic".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.clock.0.entered.notified())
            .await
            .unwrap();
        id
    }
    async fn wait(&self, id: &str, key: &str, value: Value) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = self.owner.snapshot(id).await.unwrap();
                if snapshot[key] == value {
                    return snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }
}
#[tokio::test]
async fn pause_during_shared_break_reports_paused_without_holding_gate_and_stop_joins_page() {
    let f = Fixture::new().await;
    let id = f.start().await;
    let snapshot = f.owner.snapshot(&id).await.unwrap();
    assert_eq!(snapshot["break_seconds"], 60);
    assert_eq!(snapshot["last_event"]["type"], "scrape_break");
    assert_eq!(snapshot["total_messages"], 100);
    f.owner.pause(&id).await.unwrap();
    let paused = f.wait(&id, "paused", json!(true)).await;
    assert_eq!(paused["break_seconds"], 60);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.owner.stop(&id).await.unwrap();
    let stopped = f.owner.snapshot(&id).await.unwrap();
    assert_eq!(stopped["last_event"]["type"], "cancelled");
    assert_eq!(stopped["total_messages"], 100);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn resumed_break_keeps_snapshot_deadline_then_delivers_end_before_terminal_event() {
    let f = Fixture::new().await;
    let id = f.start().await;
    f.owner.pause(&id).await.unwrap();
    f.wait(&id, "paused", json!(true)).await;
    let (_, mut events) = f.owner.subscribe(&id).await.unwrap();
    f.clock.0.blocked.store(false, Ordering::SeqCst);
    f.owner
        .resume(&id, "account", "synthetic".into())
        .await
        .unwrap();
    f.clock.0.release.notify_waiters();
    let done = f.wait(&id, "running", json!(false)).await;
    assert_eq!(done["break_seconds"], 0);
    assert_eq!(done["last_event"]["type"], "complete");
    let mut kinds = vec![];
    while let Ok(event) = events.try_recv() {
        kinds.push(event["type"].as_str().unwrap().to_owned());
    }
    let end = kinds.iter().position(|v| v == "scrape_break_end").unwrap();
    let complete = kinds.iter().position(|v| v == "complete").unwrap();
    assert!(end < complete);
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    f.writer.shutdown().await.unwrap();
}
