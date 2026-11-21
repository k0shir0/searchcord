use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::Response,
};
use searchcord_collector::{
    Result,
    discord::Discord,
    pacing::{Gate, SystemClock},
    profiles::Profiles,
    scrapes::{Scrapes, validate_channels},
    writer::Writer,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, mpsc};
fn channel() -> Value {
    json!({"id":"20","name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
fn message(n: u64) -> Value {
    json!({"id":(1450000000000000000u64+n).to_string(),"channel_id":"20","author":{"id":"30","username":"Alice"},"content":format!("message {n}"),"timestamp":"2026-01-01T12:00:00+00:00","attachments":[]})
}
struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    owner: Scrapes<SystemClock>,
    writer: Writer,
    server: tokio::task::JoinHandle<()>,
    requests: mpsc::UnboundedReceiver<String>,
    release: Arc<Notify>,
    hold: Arc<Mutex<bool>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<Value>, hold: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        let writer = Writer::open(path.clone()).await.unwrap();
        let (sent, requests) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let hold = Arc::new(Mutex::new(hold));
        type Data = (
            Arc<Mutex<VecDeque<Value>>>,
            mpsc::UnboundedSender<String>,
            Arc<Notify>,
            Arc<Mutex<bool>>,
        );
        async fn handler(
            State((replies, sent, release, hold)): State<Data>,
            request: Request,
        ) -> Response<Body> {
            assert_eq!(request.headers()["authorization"], "fixture");
            let _ = sent.send(request.uri().to_string());
            let wait = *hold.lock().unwrap();
            if wait {
                release.notified().await;
            }
            let body = replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json!([]));
            Response::new(Body::from(body.to_string()))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(handler).with_state((
            Arc::new(Mutex::new(replies.into())),
            sent,
            release.clone(),
            hold.clone(),
        ));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Discord::for_loopback(
            Gate::open(path.clone(), SystemClock::default())
                .await
                .unwrap(),
            &origin,
        )
        .unwrap();
        let profiles = Profiles::new(path.clone(), client.clone());
        let owner = Scrapes::open(path.clone(), writer.clone(), client, profiles)
            .await
            .unwrap();
        Self {
            _dir: dir,
            path,
            owner,
            writer,
            server,
            requests,
            release,
            hold,
        }
    }
    async fn start(&self, limit: u64) -> String {
        self.owner
            .start(
                json!([channel()]),
                limit,
                false,
                "account".into(),
                "fixture".into(),
            )
            .await
            .unwrap()
    }
    async fn finished(&self, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let state = self.owner.snapshot(id).await.unwrap();
                if state["running"] == false {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    fn count(&self) -> i64 {
        rusqlite::Connection::open(&self.path)
            .unwrap()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap()
    }
}
#[tokio::test]
async fn bounded_writer_joins_accepted_work_and_acknowledges_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("archive.db");
    let writer = Writer::open(path.clone()).await.unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let copy = writer.clone();
    let task = tokio::spawn(async move {
        copy.write(move |store| {
            let _ = started.send(());
            wait.recv()?;
            store
                .connection()
                .execute("INSERT INTO authors VALUES ('30','joined','0')", [])?;
            Ok(Value::Null)
        })
        .await
    });
    ready.await.unwrap();
    task.abort();
    let copy = writer.clone();
    let barrier = tokio::spawn(async move { copy.barrier().await });
    tokio::task::yield_now().await;
    assert!(!barrier.is_finished());
    release.send(()).unwrap();
    barrier.await.unwrap().unwrap();
    let error = writer
        .write(|store| -> Result<Value> {
            let tx = store.connection_mut().transaction()?;
            tx.execute("INSERT INTO authors VALUES ('31','rollback','0')", [])?;
            Err("injected rollback".into())
        })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "injected rollback");
    writer.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    let authors: i64 = db
        .query_row("SELECT count(*) FROM authors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(authors, 1);
    assert!(writer.write(|_| Ok(Value::Null)).await.is_err());
}
#[tokio::test]
async fn page_progress_checkpoint_and_cursor_commit_together_and_limits_use_insertions() {
    let mut f = Fixture::new(vec![json!([message(3), message(2), message(1)])], false).await;
    let id = f.start(3).await;
    let result = f.finished(&id).await;
    assert_eq!(result["total_messages"], 3);
    assert_eq!(result["completed"], 1);
    assert_eq!(result["rows"][0]["messages"], 3);
    assert_eq!(result["last_event"]["type"], "complete");
    assert_eq!(f.count(), 3);
    assert_eq!(
        f.requests.recv().await.unwrap(),
        "/api/v10/channels/20/messages?limit=3"
    );
    let db = rusqlite::Connection::open(&f.path).unwrap();
    let saved: String = db
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE id=?",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!saved.contains("fixture-token"));
    let state: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(state["total_messages"], 3);
    assert_eq!(state["next_index"], 1);
    let cursor: String = db
        .query_row(
            "SELECT oldest_message_id FROM scrape_cursors WHERE channel_id='20'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cursor, message(1)["id"].as_str().unwrap());
    drop(db);
    let (snapshot, _) = f.owner.subscribe(&id).await.unwrap();
    assert_eq!(snapshot, result);
    f.owner.suspend().await.unwrap();
    f.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn pause_in_flight_saves_page_reserves_channel_and_requires_original_account() {
    let mut f = Fixture::new(vec![json!([message(2), message(1)])], true).await;
    let id = f.start(2).await;
    f.requests.recv().await.unwrap();
    f.owner.pause(&id).await.unwrap();
    assert_eq!(
        f.owner.snapshot(&id).await.unwrap()["pause_requested"],
        true
    );
    assert!(
        f.owner
            .start(
                json!([channel()]),
                1,
                false,
                "account".into(),
                "fixture".into()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("still being scraped")
    );
    assert!(
        f.owner
            .resume(&id, "different", "fixture".into())
            .await
            .unwrap_err()
            .to_string()
            .contains("Select the account")
    );
    *f.hold.lock().unwrap() = false;
    f.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let s = f.owner.snapshot(&id).await.unwrap();
            if s["paused"] == true {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.count(), 2);
    assert_eq!(f.owner.snapshot(&id).await.unwrap()["total_messages"], 2);
    f.owner
        .resume(&id, "account", "fixture".into())
        .await
        .unwrap();
    assert_eq!(f.finished(&id).await["last_event"]["type"], "complete");
    f.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn stop_cancels_fetch_and_shutdown_recovers_paused_without_requests() {
    let mut f = Fixture::new(vec![json!([message(1)])], true).await;
    let id = f.start(0).await;
    f.requests.recv().await.unwrap();
    f.owner.stop(&id).await.unwrap();
    assert_eq!(f.count(), 0);
    let snapshot = f.owner.snapshot(&id).await.unwrap();
    assert_eq!(snapshot["cancelled"], true);
    assert_eq!(snapshot["running"], false);
    let mut other = channel();
    other["id"] = json!("21");
    let id = f
        .owner
        .start(json!([other]), 0, false, "account".into(), "fixture".into())
        .await
        .unwrap();
    f.requests.recv().await.unwrap();
    f.owner.suspend().await.unwrap();
    let state = f.owner.snapshot(&id).await.unwrap();
    assert_eq!(state["running"], true);
    assert_eq!(state["paused"], true);
    f.writer.shutdown().await.unwrap();
    let client = Discord::new(
        Gate::open(f.path.clone(), SystemClock::default())
            .await
            .unwrap(),
    )
    .unwrap();
    let profiles = Profiles::new(f.path.clone(), client.clone());
    let writer = Writer::open(f.path.clone()).await.unwrap();
    let recovered = Scrapes::open(f.path.clone(), writer.clone(), client, profiles)
        .await
        .unwrap();
    let snapshot = recovered.snapshot(&id).await.unwrap();
    assert_eq!(snapshot["recovered"], true);
    assert_eq!(snapshot["paused"], true);
    assert_eq!(recovered.active().await.len(), 1);
    recovered.stop(&id).await.unwrap();
    writer.shutdown().await.unwrap();
}
#[test]
fn source_validation_matches_python_and_keeps_copied_metadata() {
    assert!(
        validate_channels(&json!([]))
            .unwrap_err()
            .to_string()
            .contains("between 1 and 2000")
    );
    assert!(validate_channels(&json!([null])).is_err());
    for field in ["id", "guild_id"] {
        let mut c = channel();
        c[field] = json!("1".repeat(4301));
        assert!(validate_channels(&json!([c])).is_err());
    }
    let c = channel();
    assert_eq!(
        validate_channels(&json!([c.clone(), c.clone()])).unwrap(),
        vec![c]
    );
}
