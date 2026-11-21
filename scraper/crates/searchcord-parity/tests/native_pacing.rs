//! Native equivalents of all five reference pacing contracts, plus controls,
//! accepted-write cancellation, malformed cooldowns and actual HTTP transport.
use reqwest::Method;
use rusqlite::{Connection, params};
use searchcord_collector::{
    Result, Store,
    discord::{Discord, Request},
    pacing::{Checkpoint, Clock, Gate, Options, Response, SystemClock, WaitEvent},
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
struct Trace {
    draws: VecDeque<f64>,
    bounds: Vec<(f64, f64)>,
    sent: Vec<f64>,
    finished: Vec<f64>,
    replies: VecDeque<Result<Response>>,
}
#[derive(Clone)]
struct VirtualClock {
    start: tokio::time::Instant,
    trace: Arc<Mutex<Trace>>,
    response_seconds: f64,
}
impl VirtualClock {
    fn new(draws: Vec<f64>, response_seconds: f64) -> Self {
        Self {
            start: tokio::time::Instant::now(),
            trace: Arc::new(Mutex::new(Trace {
                draws: draws.into(),
                ..Trace::default()
            })),
            response_seconds,
        }
    }
    async fn send(self) -> Result<Response> {
        self.trace.lock().unwrap().sent.push(self.now());
        tokio::time::sleep(Duration::from_secs_f64(self.response_seconds)).await;
        let mut trace = self.trace.lock().unwrap();
        trace.finished.push(self.now());
        trace
            .replies
            .pop_front()
            .unwrap_or_else(|| Ok(response(200, json!([]), &[])))
    }
}
impl Clock for VirtualClock {
    fn now(&self) -> f64 {
        1000.0 + self.start.elapsed().as_secs_f64()
    }
    fn wall(&self) -> f64 {
        1_700_000_000.0 + self.now()
    }
    fn uniform(&self, low: f64, high: f64) -> Result<f64> {
        let mut trace = self.trace.lock().unwrap();
        trace.bounds.push((low, high));
        Ok(trace.draws.pop_front().expect("unexpected pacing draw"))
    }
    async fn sleep(&self, seconds: f64) {
        tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
    }
}
fn response(status: u16, body: Value, headers: &[(&str, &str)]) -> Response {
    Response {
        status,
        body: serde_json::to_vec(&body).unwrap(),
        headers: headers
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), v.parse().unwrap()))
            .collect(),
    }
}
fn archive() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic.db");
    drop(Store::open(&path).unwrap());
    (directory, path)
}
fn saved(path: &PathBuf) -> Checkpoint {
    let raw: String = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE id='discord-pacing' AND kind='pacing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("synthetic-token"));
    serde_json::from_str(&raw).unwrap()
}
fn seed(path: &PathBuf, saved: &Checkpoint) {
    Connection::open(path).unwrap().execute("INSERT INTO collection_jobs(id,kind,state_json,updated_at) VALUES ('discord-pacing','pacing',?,0)", params![serde_json::to_string(saved).unwrap()]).unwrap();
}
fn scrape() -> Options {
    Options {
        scrape: true,
        ..Options::default()
    }
}
async fn request(
    gate: &Gate<VirtualClock>,
    clock: &VirtualClock,
    options: Options,
) -> Result<Response> {
    let clock = clock.clone();
    gate.request(options, move || clock.clone().send()).await
}
fn near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
}

#[tokio::test(start_paused = true)]
async fn failed_network_request_persists_the_last_attempt_deadline() {
    let (_directory, path) = archive();
    let clock = VirtualClock::new(vec![0.75], 0.6);
    clock
        .trace
        .lock()
        .unwrap()
        .replies
        .push_back(Err("fixture ReadTimeout".into()));
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    assert!(
        request(&gate, &clock, scrape())
            .await
            .unwrap_err()
            .to_string()
            .contains("ReadTimeout")
    );
    let saved = saved(&path);
    near(saved.last_scrape_request, clock.wall());
    assert_eq!(saved.scrape_confirmed_requests, 0);
    let trace = clock.trace.lock().unwrap();
    near(trace.finished[0] - trace.sent[0], 0.6);
}

#[tokio::test(start_paused = true)]
async fn random_intervals_retries_and_100_success_break_are_shared() {
    let (_directory, path) = archive();
    let mut draws = Vec::new();
    for i in 0..103 {
        draws.push(if i % 2 == 0 { 1.5 } else { 0.5 });
        if i == 101 {
            draws.push(60.0);
        }
    }
    let clock = VirtualClock::new(draws, 0.0);
    clock.trace.lock().unwrap().replies.extend([
        Ok(response(429, json!({"retry_after":0}), &[])),
        Ok(response(503, json!(null), &[])),
    ]);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let other_job = gate.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    for i in 0..102 {
        let events = events.clone();
        let options = Options {
            on_wait: Some(Arc::new(move |event| events.lock().unwrap().push(event))),
            ..scrape()
        };
        let reply = request(if i % 2 == 0 { &gate } else { &other_job }, &clock, options)
            .await
            .unwrap();
        assert_eq!(reply.status, if i == 0 { 503 } else { 200 });
    }
    let trace = clock.trace.lock().unwrap();
    assert_eq!(trace.bounds.len(), 104);
    assert_eq!(
        trace.bounds.iter().filter(|b| **b == (0.5, 1.5)).count(),
        103
    );
    assert_eq!(trace.bounds[102], (60.0, 90.0));
    assert_eq!(trace.sent.len(), 103);
    let gaps: Vec<_> = trace.sent[..102].windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.iter().all(|g| (0.499..=1.501).contains(g)));
    assert!(gaps.iter().any(|g| *g < 0.501));
    assert!(gaps.iter().any(|g| *g > 1.499));
    near(trace.sent[102] - trace.sent[101], 60.0);
    assert!(trace.draws.is_empty());
    assert_eq!(saved(&path).scrape_confirmed_requests, 1);
    assert!(
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, WaitEvent::ScrapeBreak(_)))
    );
}

#[tokio::test(start_paused = true)]
async fn actual_random_interval_bounds_and_slow_response_are_shared() {
    let (_directory, path) = archive();
    let clock = VirtualClock::new(vec![0.75; 4], 0.6);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let gate = gate.clone();
        let clock = clock.clone();
        requests.spawn(async move { request(&gate, &clock, scrape()).await.unwrap() });
    }
    while let Some(reply) = requests.join_next().await {
        assert_eq!(reply.unwrap().status, 200);
    }
    let trace = clock.trace.lock().unwrap();
    assert_eq!(trace.bounds, vec![(0.5, 1.5); 4]);
    for i in 0..3 {
        near(trace.sent[i + 1] - trace.finished[i], 0.75);
    }
    assert_eq!(saved(&path).scrape_confirmed_requests, 4);
}

#[tokio::test(start_paused = true)]
async fn counter_cooldowns_and_break_deadline_survive_restart_and_job_changes() {
    let (_directory, path) = archive();
    seed(
        &path,
        &Checkpoint {
            scrape_confirmed_requests: 99,
            ..Checkpoint::default()
        },
    );
    let clock = VirtualClock::new(vec![1.5, 90.0, 0.5], 0.0);
    clock.trace.lock().unwrap().replies.push_back(Ok(response(
        200,
        json!([]),
        &[
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset-After", "30"),
        ],
    )));
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    request(&gate, &clock, scrape()).await.unwrap();
    let checkpoint = saved(&path);
    assert_eq!(checkpoint.scrape_confirmed_requests, 0);
    near(checkpoint.scrape_break_until - clock.wall(), 90.0);
    near(checkpoint.discord_ready_at - clock.wall(), 30.0);
    drop(gate);
    tokio::time::advance(Duration::from_secs(10)).await;
    let restarted = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let checkpoint = restarted.checkpoint().await;
    near(checkpoint.scrape_break_until - clock.wall(), 80.0);
    near(checkpoint.discord_ready_at - clock.wall(), 20.0);
    // Non-scrape work shares the cooldown, but need not wait the scrape break.
    request(&restarted, &clock, Options::default())
        .await
        .unwrap();
    request(&restarted.clone(), &clock, scrape()).await.unwrap();
    let trace = clock.trace.lock().unwrap();
    near(trace.sent[1] - trace.sent[0], 30.0);
    near(trace.sent[2] - trace.sent[0], 90.0);
    assert_eq!(saved(&path).scrape_confirmed_requests, 1);
}

#[tokio::test(start_paused = true)]
async fn nonzero_counter_survives_restart() {
    let (_directory, path) = archive();
    let clock = VirtualClock::new(vec![0.75; 3], 0.0);
    clock.trace.lock().unwrap().replies.extend([
        Ok(response(200, json!([]), &[])),
        Ok(response(503, json!([]), &[])),
    ]);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    assert_eq!(request(&gate, &clock, scrape()).await.unwrap().status, 200);
    assert_eq!(request(&gate, &clock, scrape()).await.unwrap().status, 503);
    drop(gate);
    let restarted = Gate::open(path.clone(), clock.clone()).await.unwrap();
    assert_eq!(restarted.checkpoint().await.scrape_confirmed_requests, 1);
    assert_eq!(saved(&path).scrape_confirmed_requests, 1);
    request(&restarted, &clock, scrape()).await.unwrap();
    assert_eq!(saved(&path).scrape_confirmed_requests, 2);
}

#[tokio::test(start_paused = true)]
async fn paused_jobs_release_the_gate_and_stop_interrupts_waiters_and_active_attempts() {
    let (_directory, path) = archive();
    let clock = VirtualClock::new(vec![0.5], 0.0);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let paused = scrape();
    paused.control.pause();
    let resume = paused.control.clone();
    let task = {
        let gate = gate.clone();
        let clock = clock.clone();
        tokio::spawn(async move { request(&gate, &clock, paused).await })
    };
    tokio::task::yield_now().await;
    assert!(clock.trace.lock().unwrap().sent.is_empty());
    request(&gate, &clock, Options::default()).await.unwrap();
    resume.resume();
    assert_eq!(task.await.unwrap().unwrap().status, 200);
    assert_eq!(clock.trace.lock().unwrap().sent.len(), 2);

    let entered = Arc::new(Notify::new());
    let active = Options::default();
    let stop_active = active.control.clone();
    let task = {
        let gate = gate.clone();
        let entered = entered.clone();
        tokio::spawn(async move {
            gate.request(active, move || {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<Result<Response>>().await
                }
            })
            .await
        })
    };
    entered.notified().await;
    let waiting = Options::default();
    let stop_waiting = waiting.control.clone();
    let waiter = {
        let gate = gate.clone();
        let clock = clock.clone();
        tokio::spawn(async move { request(&gate, &clock, waiting).await })
    };
    tokio::task::yield_now().await;
    stop_waiting.stop();
    assert!(
        waiter
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
    stop_active.stop();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
    gate.barrier().await.unwrap();
    assert_eq!(saved(&path).scrape_confirmed_requests, 1);
}

#[tokio::test(start_paused = true)]
async fn stop_interrupts_cooldown_and_mandatory_break_without_upstream_requests() {
    for mandatory_break in [false, true] {
        let (_directory, path) = archive();
        let clock = VirtualClock::new(vec![0.75], 0.0);
        seed(
            &path,
            &Checkpoint {
                discord_ready_at: if mandatory_break {
                    0.0
                } else {
                    clock.wall() + 60.0
                },
                scrape_break_until: if mandatory_break {
                    clock.wall() + 90.0
                } else {
                    0.0
                },
                ..Checkpoint::default()
            },
        );
        let gate = Gate::open(path, clock.clone()).await.unwrap();
        let options = scrape();
        let control = options.control.clone();
        let waited = Arc::new(Notify::new());
        let notify = waited.clone();
        let options = Options {
            on_wait: Some(Arc::new(move |_| notify.notify_one())),
            ..options
        };
        let trace = clock.trace.clone();
        let task = tokio::spawn(async move { request(&gate, &clock, options).await });
        waited.notified().await;
        control.stop();
        assert!(
            task.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("stopped")
        );
        assert!(trace.lock().unwrap().sent.is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn malformed_checkpoints_and_entropy_failures_do_not_bypass_persisted_breaks() {
    let (directory, path) = archive();
    let missing = directory.path().join("must-not-create.db");
    assert!(
        Gate::open(missing.clone(), VirtualClock::new(vec![], 0.0))
            .await
            .is_err()
    );
    assert!(!missing.exists());
    seed(
        &path,
        &Checkpoint {
            scrape_confirmed_requests: 100,
            ..Checkpoint::default()
        },
    );
    assert!(
        Gate::open(path.clone(), VirtualClock::new(vec![], 0.0))
            .await
            .is_err()
    );
    Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM collection_jobs", [])
        .unwrap();
    seed(
        &path,
        &Checkpoint {
            scrape_confirmed_requests: 99,
            ..Checkpoint::default()
        },
    );
    let clock = VirtualClock::new(vec![0.5, 100.0], 0.0);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    assert!(
        request(&gate, &clock, scrape())
            .await
            .unwrap_err()
            .to_string()
            .contains("random draw")
    );
    let checkpoint = saved(&path);
    assert_eq!(checkpoint.scrape_confirmed_requests, 0);
    near(checkpoint.scrape_break_until - clock.wall(), 90.0);
    near(checkpoint.last_scrape_request, clock.wall());
}

#[test]
fn pacing_checkpoints_interchange_with_the_real_python_recovery() {
    use searchcord_parity::{Backend, Harness};
    let mut h = Harness::with_backend(Backend::Python).unwrap();
    h.init();
    h.command(json!({"op":"job","action":"configure"}));
    h.command(json!({"op":"job","action":"block_fetch"}));
    let started = h.command(json!({"op":"job","action":"http","path":"/api/scrape/start","body":{"channels":[{"id":"20","name":"fixture","guild_id":"10","guild_name":"Fixture"}]}}));
    let id = &started["body"]["job_id"];
    h.command(json!({"op":"job","action":"wait_snapshot","id":id,"fields":{"index":1}}));
    h.command(json!({"op":"job","action":"virtual_clock","draws":[0.75],"bounds":[0.5,1.5],"response_seconds":0}));
    h.command(json!({"op":"job","action":"set_counter","value":41}));
    let reply = h.command(json!({"op":"job","action":"pacing_requests","ids":[id]}));
    assert_eq!(reply["results"], json!([{"status":200}]));
    assert_eq!(saved(&h.archive()).scrape_confirmed_requests, 42);
    h.command(json!({"op":"job","action":"control","method":"suspend"}));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let gate = Gate::open(h.archive(), SystemClock::default())
            .await
            .unwrap();
        assert_eq!(gate.checkpoint().await.scrape_confirmed_requests, 42);
        gate.request(scrape(), || async {
            Ok(response(
                200,
                json!([]),
                &[
                    ("X-RateLimit-Remaining", "0"),
                    ("X-RateLimit-Reset-After", "30"),
                ],
            ))
        })
        .await
        .unwrap();
        gate.barrier().await.unwrap();
    });
    assert_eq!(saved(&h.archive()).scrape_confirmed_requests, 43);
    h.command(json!({"op":"job","action":"recover_pacing"}));
    let inspected = h.command(json!({"op":"job","action":"pacing_inspect"}));
    assert_eq!(inspected["counter"], 43);
    assert!(inspected["cooldown_seconds"].as_f64().unwrap() > 20.0);
}

#[tokio::test(start_paused = true)]
async fn failed_checkpoint_writes_fail_the_request_and_shutdown_barrier() {
    let (_directory, path) = archive();
    let clock = VirtualClock::new(vec![0.75], 0.0);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE collection_jobs")
        .unwrap();
    assert!(request(&gate, &clock, scrape()).await.is_err());
    assert_eq!(clock.trace.lock().unwrap().sent.len(), 1);
    assert!(
        gate.barrier()
            .await
            .unwrap_err()
            .to_string()
            .contains("checkpoint failed")
    );
}

#[tokio::test]
async fn dropping_a_caller_cannot_abandon_an_accepted_checkpoint_write() {
    let (_directory, path) = archive();
    let clock = VirtualClock::new(vec![0.75], 0.0);
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let entered = Arc::new(Notify::new());
    let task = {
        let gate = gate.clone();
        let entered = entered.clone();
        tokio::spawn(async move {
            gate.request(scrape(), move || {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    Ok(response(200, json!([]), &[]))
                }
            })
            .await
        })
    };
    entered.notified().await;
    task.abort();
    let _ = task.await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), gate.barrier())
            .await
            .is_err()
    );
    blocker.execute_batch("COMMIT").unwrap();
    gate.barrier().await.unwrap();
    assert_eq!(saved(&path).scrape_confirmed_requests, 1);
}

#[tokio::test(start_paused = true)]
async fn rate_limit_retry_cap_opt_out_and_malformed_delay_parsing_match_python() {
    use searchcord_collector::pacing::retry_after;
    for (body, headers, expected) in [
        (json!({"retry_after":2.5}), vec![("Retry-After", "30")], 2.5),
        (json!({"retry_after":null}), vec![("Retry-After", "3")], 3.0),
        (
            json!({"retry_after":"invalid"}),
            vec![("Retry-After", "3")],
            1.0,
        ),
        (json!({"retry_after":-4}), vec![], 0.0),
        (json!({"retry_after":"NaN"}), vec![], 1.0),
        (json!({"retry_after":true}), vec![], 1.0),
        (json!([]), vec![("Retry-After", "Infinity")], 1.0),
    ] {
        near(retry_after(&response(429, body, &headers)), expected);
    }
    for retry in [true, false] {
        let (_directory, path) = archive();
        let clock = VirtualClock::new(vec![], 0.0);
        clock
            .trace
            .lock()
            .unwrap()
            .replies
            .extend((0..6).map(|_| Ok(response(429, json!({"retry_after":0}), &[]))));
        let gate = Gate::open(path, clock.clone()).await.unwrap();
        let reply = request(
            &gate,
            &clock,
            Options {
                retry_rate_limits: retry,
                ..Options::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(reply.status, 429);
        assert_eq!(
            clock.trace.lock().unwrap().sent.len(),
            if retry { 6 } else { 1 }
        );
    }
}

#[tokio::test]
async fn real_http_transport_preserves_headers_query_json_body_and_refuses_redirects() {
    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::{Request as HttpRequest, State},
        http::{Response as HttpResponse, header},
    };
    let (_directory, path) = archive();
    let records = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_records = records.clone();
    async fn handler(
        State(records): State<Arc<Mutex<Vec<Value>>>>,
        request: HttpRequest,
    ) -> HttpResponse<Body> {
        let (parts, body) = request.into_parts();
        let body = to_bytes(body, 4096).await.unwrap();
        let mut records = records.lock().unwrap();
        records.push(json!({"method":parts.method.as_str(),"path":parts.uri.path(),"query":parts.uri.query(),"token":parts.headers["authorization"].to_str().unwrap(),"agent":parts.headers["user-agent"].to_str().unwrap(),"content_type":parts.headers["content-type"].to_str().unwrap(),"body":String::from_utf8(body.to_vec()).unwrap()}));
        match records.len() {
            1 => HttpResponse::builder()
                .status(429)
                .body(Body::from(r#"{"retry_after":0}"#))
                .unwrap(),
            2 => HttpResponse::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("[]"))
                .unwrap(),
            _ => HttpResponse::builder()
                .status(302)
                .header(header::LOCATION, "/must-not-follow")
                .body(Body::from("redirect"))
                .unwrap(),
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().fallback(handler).with_state(server_records),
        )
        .await
        .unwrap();
    });
    let gate = Gate::open(path.clone(), SystemClock::default())
        .await
        .unwrap();
    assert!(Discord::for_loopback(gate.clone(), "http://discord.com").is_err());
    let client = Discord::for_loopback(gate, &origin).unwrap();
    let mut request = Request::new(Method::GET, "/channels/20/messages", "synthetic-token");
    request.params = vec![
        ("limit".into(), "2".into()),
        ("before".into(), "123".into()),
    ];
    assert_eq!(
        client
            .request(request, Options::default())
            .await
            .unwrap()
            .json()
            .unwrap(),
        json!([])
    );
    let mut request = Request::new(Method::POST, "/invites/fixture", "synthetic-token");
    request.json = Some(json!({"fixture":true}));
    let response = client.request(request, Options::default()).await.unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(response.body, b"redirect");
    let records = records.lock().unwrap().clone();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0], records[1]);
    assert_eq!(records[0]["path"], "/api/v10/channels/20/messages");
    assert_eq!(records[0]["query"], "limit=2&before=123");
    assert_eq!(records[0]["token"], "synthetic-token");
    assert_eq!(
        records[0]["agent"],
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"
    );
    assert_eq!(records[0]["content_type"], "application/json");
    assert_eq!(records[2]["method"], "POST");
    assert_eq!(
        serde_json::from_str::<Value>(records[2]["body"].as_str().unwrap()).unwrap(),
        json!({"fixture":true})
    );
    assert_eq!(saved(&path).scrape_confirmed_requests, 0);
    server.abort();
    let _ = server.await;
}
