//! Native collector HTTP contracts against pinned Python, using disposable data only.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, Response},
};
use searchcord_collector::{
    PageContext, Result, Store,
    discord::Discord,
    http::{App, router},
    pacing::{Clock, Gate},
    writer::Writer,
};
use searchcord_parity::{Backend, Harness, parse_events};
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
use tower::ServiceExt;

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
struct Fixture {
    replies: Mutex<VecDeque<Value>>,
    requests: Mutex<Vec<Value>>,
}
async fn upstream(State(f): State<Arc<Fixture>>, request: Request) -> Response<Body> {
    if !request.uri().path().starts_with("/api/v10/") {
        assert!(!request.headers().contains_key("authorization"));
        f.requests.lock().unwrap().push(
            json!({"method":request.method().as_str(),"path":request.uri().path(),"media":true}),
        );
        return Response::builder()
            .status(200)
            .header("content-type", "image/png")
            .body(Body::from("synthetic-media"))
            .unwrap();
    }
    f.requests.lock().unwrap().push(json!({"method":request.method().as_str(),"path":request.uri().path(),"query":request.uri().query(),"token":request.headers()["authorization"].to_str().unwrap()}));
    let reply = f
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected synthetic upstream request");
    Response::builder()
        .status(reply["status"].as_u64().unwrap() as u16)
        .body(Body::from(
            reply
                .get("body")
                .cloned()
                .unwrap_or(Value::Null)
                .to_string(),
        ))
        .unwrap()
}
async fn application(
    path: PathBuf,
    replies: Vec<Value>,
) -> (App<VirtualClock>, Arc<Fixture>, tokio::task::JoinHandle<()>) {
    let fixture = Arc::new(Fixture {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(vec![]),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mock = Router::new().fallback(upstream).with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    let clock = VirtualClock::default();
    let writer = Writer::open(path.clone()).await.unwrap();
    let gate = Gate::open(path.clone(), clock.clone()).await.unwrap();
    let client = Discord::for_loopback(gate, &origin).unwrap();
    let media = searchcord_collector::media_collection::MediaCollector::for_loopback(
        &path,
        &origin,
        Default::default(),
    )
    .unwrap();
    let mut app =
        App::open_with_media(path.clone(), client, Arc::new(clock), writer.clone(), media)
            .await
            .unwrap();
    let credentials = path.parent().unwrap().with_file_name("credentials");
    let paths = searchcord_collector::credentials::Paths::new(
        &path,
        &credentials.join("native.vault"),
        &credentials.join("native.key"),
    )
    .unwrap();
    let accounts = searchcord_collector::accounts::Accounts::with_paths(paths, writer);
    app.scrapes = app.scrapes.clone().with_accounts(accounts.clone());
    app.monitor = app.monitor.clone().with_accounts(accounts.clone());
    app.accounts = accounts;
    (app, fixture, server)
}
async fn request(app: &Router, method: &str, path: &str, body: Value) -> Value {
    let mut request = Request::builder().method(method).uri(path);
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        request = request.header("content-type", "application/json");
        body.to_string().into_bytes()
    };
    let response = app
        .clone()
        .oneshot(request.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers: serde_json::Map<String, Value> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().into(), json!(v.to_str().unwrap())))
        .collect();
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        to_bytes(response.into_body(), usize::MAX),
    )
    .await
    .unwrap()
    .unwrap();
    let body = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    json!({"status":status,"body":body,"headers":headers})
}
fn oracle(h: &mut Harness, method: &str, path: &str, body: Value) -> Value {
    h.command(json!({"op":"job","action":"http","method":method,"path":path,"body":body}))
}
fn normal(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove("elapsed_ms");
    }
    value
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
fn python() -> Harness {
    let mut h = Harness::with_backend(Backend::Python).unwrap();
    h.init();
    h.command(json!({"op":"job","action":"configure"}));
    h
}
fn message(id: i64, author: &str, content: &str) -> Value {
    json!({"id":id.to_string(),"channel_id":"20","author":{"id":author,"username":if author=="30"{"Alice"}else{"Bob"}},"content":content,"timestamp":"2026-01-01T00:00:00+00:00","attachments":[{"url":"https://cdn.discordapp.com/attachments/20/1/image.png","content_type":"image/png"}]})
}

#[test]
fn validation_initial_state_methods_docs_and_unchanged_assets_match_python() {
    let mut h = python();
    let rt = runtime();
    rt.block_on(async {
        let (app, fixture, server) = application(h.archive(), vec![]).await;
        let http = router(app.clone());
        let mut cases = vec![
            ("GET", "/api/settings", Value::Null),
            ("GET", "/api/scrape/active", Value::Null),
            ("GET", "/api/live/status", Value::Null),
            ("GET", "/api/invites/status", Value::Null),
            ("GET", "/api/profile-backfill", Value::Null),
            (
                "GET",
                "/api/search?limit=0&page=0&scan=bad&before=0",
                Value::Null,
            ),
            ("GET", "/api/stats?days=-1", Value::Null),
            (
                "GET",
                "/api/stats/contributors?offset=-1&limit=101",
                Value::Null,
            ),
            ("GET", "/api/stats/servers?limit=no", Value::Null),
            (
                "GET",
                "/api/profiles/30/servers?limit=0&offset=-1",
                Value::Null,
            ),
            ("GET", "/api/messages/1/archive?limit=101", Value::Null),
            ("GET", "/api/messages/0/archive", Value::Null),
            ("GET", "/api/messages/no/archive", Value::Null),
            ("GET", "/api/scrape/progress/absent", Value::Null),
            ("GET", "/api/dm-clear/progress/absent", Value::Null),
            ("GET", "/api/stats/", Value::Null),
            ("HEAD", "/api/stats", Value::Null),
            ("POST", "/api/stats", Value::Null),
            ("OPTIONS", "/api/stats", Value::Null),
            ("GET", "/api/export/participants", Value::Null),
            ("POST", "/api/dm-clear/start", json!({"channel_id":"abc"})),
            ("POST", "/api/dm-clear/start", json!({"channel_id":123})),
            ("POST", "/api/scrape/start", json!({"channels":[]})),
            (
                "POST",
                "/api/scrape/start",
                json!({"channels":null,"limit":-1,"harvest_profiles":"bad"}),
            ),
            (
                "POST",
                "/api/settings",
                json!({"token":123,"token_label":[],"active_token_id":false}),
            ),
            ("POST", "/api/invites/start", json!({"invites":""})),
            (
                "POST",
                "/api/invites/start",
                json!({"invites":"https://example.com/invalid"}),
            ),
            ("POST", "/api/export/chatml", json!({})),
            ("POST", "/api/live/stop", json!({"channel_ids":null})),
        ];
        cases.extend(
            [
                "/docs",
                "/redoc",
                "/openapi.json",
                "/docs/oauth2-redirect",
                "/",
                "/style.css",
                "/app.js",
                "/collection.js",
                "/profile-view.js",
                "/search/index.html",
            ]
            .into_iter()
            .map(|path| ("GET", path, Value::Null)),
        );
        for (method, path, body) in cases {
            let expected = oracle(&mut h, method, path, body.clone());
            let actual = request(&http, method, path, body).await;
            assert_eq!(
                (actual["status"].clone(), normal(actual["body"].clone())),
                (expected["status"].clone(), normal(expected["body"].clone())),
                "{method} {path}"
            );
            assert_eq!(
                actual["headers"]["content-type"], expected["headers"]["content-type"],
                "{method} {path}"
            );
        }
        let health = request(&http, "GET", "/api/health", Value::Null).await;
        assert_eq!(health["body"], "searchcord-ready");
        assert_eq!(
            health["headers"]["x-searchcord-instance"],
            app.instance_id()
        );
        assert!(fixture.requests.lock().unwrap().is_empty());
        app.shutdown().await.unwrap();
        app.writer.shutdown().await.unwrap();
        server.abort();
    });
}

#[test]
fn search_statistics_profiles_archives_and_streaming_export_match_python() {
    let mut h = python();
    {
        let mut store = Store::open(h.archive()).unwrap();
        store
            .save_page(
                &[
                    message(1450000000000000001, "30", "hello"),
                    message(1450000000000000002, "31", "answer"),
                    message(1450000000000000003, "30", "next"),
                ],
                &PageContext {
                    channel_id: "20",
                    channel_name: "chat",
                    guild_id: Some("10"),
                    guild_name: "Guild",
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let rt = runtime();
    rt.block_on(async {
        let (app, _fixture, server) = application(h.archive(), vec![]).await;
        let http = router(app.clone());
        for path in [
            "/api/search",
            "/api/search?cursor=true&limit=2",
            "/api/search?channel_id=20&limit=1&page=2",
            "/api/search?q=hello",
            "/api/search?date_from=2020-01-01&date_to=2030-01-01",
            "/api/stats?days=0",
            "/api/stats/contributors",
            "/api/stats/servers",
            "/api/search/filters?include_users=false",
            "/api/search/authors?q=Al",
            "/api/profiles/30",
            "/api/profiles/30/servers",
            "/api/profiles/no",
            "/api/export/participants?channel_id=20",
            "/api/messages/1450000000000000001/archive",
        ] {
            let expected = oracle(&mut h, "GET", path, Value::Null);
            let actual = request(&http, "GET", path, Value::Null).await;
            assert_eq!(
                (actual["status"].clone(), normal(actual["body"].clone())),
                (expected["status"].clone(), normal(expected["body"].clone())),
                "{path}"
            );
        }
        let body = json!({"channel_id":"20","target_id":"30"});
        let expected = oracle(&mut h, "POST", "/api/export/chatml", body.clone());
        let actual = request(&http, "POST", "/api/export/chatml", body).await;
        assert_eq!(actual["body"], expected["body"]);
        assert_eq!(
            actual["headers"]["content-disposition"],
            expected["headers"]["content-disposition"]
        );
        app.shutdown().await.unwrap();
        app.writer.shutdown().await.unwrap();
        server.abort();
    });
}

#[test]
fn discovery_invites_scrape_and_dm_http_streams_use_original_credentials() {
    let h = python();
    let rt = runtime();
    rt.block_on(async{
        let(app,fixture,server)=application(h.archive(),vec![json!({"status":200,"body":{"id":"30","global_name":"Alice"}}),json!({"status":200,"body":[{"id":"10","name":"Guild","icon":null}]}),json!({"status":200,"body":[{"id":"20","type":1,"recipients":[{"username":"Bob"}]}]}),json!({"status":403,"body":{"code":340015}}),json!({"status":200,"body":[]}),json!({"status":200,"body":{"id":"30"}}),json!({"status":200,"body":[message(1450000000000000011,"31","theirs"),message(1450000000000000010,"30","mine")]}),json!({"status":204})]).await;
        app.accounts.update(json!({"token":"synthetic-account","token_label":"Fixture"})).await.unwrap();let http=router(app.clone());
        assert_eq!(request(&http,"GET","/api/token/validate",Value::Null).await["body"],json!({"valid":true,"id":"30","username":"Alice"}));
        assert_eq!(request(&http,"GET","/api/guilds",Value::Null).await["body"],json!([{"id":"10","name":"Guild","icon":null}]));
        assert_eq!(request(&http,"GET","/api/dms",Value::Null).await["body"],json!([{"id":"20","type":1,"name":"Bob"}]));
        let started=request(&http,"POST","/api/invites/start",json!({"invites":"restricted later"})).await;assert_eq!(started["status"],200);app.invites.barrier().await.unwrap();assert_eq!(request(&http,"GET","/api/invites/status",Value::Null).await["body"]["job"]["status"],"blocked");
        let scrape=request(&http,"POST","/api/scrape/start",json!({"channels":[{"id":"20","name":"Fixture"}]})).await;let id=scrape["body"]["job_id"].as_str().unwrap();
        let progress=request(&http,"GET",&format!("/api/scrape/progress/{id}"),Value::Null).await;let frames=parse_events(progress["body"].as_str().unwrap());assert_eq!(serde_json::from_str::<Value>(&frames[0].data).unwrap()["type"],"snapshot");assert_eq!(serde_json::from_str::<Value>(&frames.last().unwrap().data).unwrap()["type"],"complete");
        let clear=request(&http,"POST","/api/dm-clear/start",json!({"channel_id":"20"})).await;let id=clear["body"]["job_id"].as_str().unwrap();app.dm.barrier(id).await.unwrap();let progress=request(&http,"GET",&format!("/api/dm-clear/progress/{id}"),Value::Null).await;let frames=parse_events(progress["body"].as_str().unwrap());assert_eq!(serde_json::from_str::<Value>(&frames.last().unwrap().data).unwrap(),json!({"type":"complete","deleted":1,"failed":0,"already_missing":0,"scanned":2}));
        assert!(fixture.requests.lock().unwrap().iter().all(|r|r["token"]=="synthetic-account"));app.shutdown().await.unwrap();app.writer.shutdown().await.unwrap();server.abort();
    });
}

#[test]
fn admin_confirmations_clear_preserves_tokens_and_token_clear_reopens_collection() {
    let h = python();
    let rt = runtime();
    rt.block_on(async {
        let (app, _fixture, server) =
            application(h.archive(), vec![json!({"status":200,"body":[]})]).await;
        app.writer
            .write(|store| {
                store.save_page(
                    &[message(1450000000000000001, "30", "synthetic")],
                    &PageContext {
                        channel_id: "20",
                        channel_name: "Fixture",
                        ..Default::default()
                    },
                )?;
                Ok(Value::Null)
            })
            .await
            .unwrap();
        app.accounts
            .update(json!({"token":"synthetic-account"}))
            .await
            .unwrap();
        let http = router(app.clone());
        for path in ["/api/messages", "/api/settings/tokens"] {
            assert_eq!(
                request(&http, "DELETE", path, Value::Null).await["status"],
                400
            );
        }
        assert_eq!(
            request(&http, "DELETE", "/api/messages?confirm=true", Value::Null).await["body"],
            json!({"ok":true})
        );
        assert_eq!(
            request(&http, "GET", "/api/search", Value::Null).await["body"]["total"],
            0
        );
        assert_eq!(
            request(&http, "GET", "/api/settings", Value::Null).await["body"]["token_set"],
            true
        );
        assert_eq!(
            request(
                &http,
                "DELETE",
                "/api/settings/tokens?confirm=true",
                Value::Null
            )
            .await["body"],
            json!({"ok":true,"tokens":[],"active_token_id":null,"token_set":false})
        );
        app.accounts
            .update(json!({"token":"replacement-account"}))
            .await
            .unwrap();
        let start = request(
            &http,
            "POST",
            "/api/scrape/start",
            json!({"channels":[{"id":"20","name":"Fixture"}]}),
        )
        .await;
        assert_eq!(start["status"], 200);
        let id = start["body"]["job_id"].as_str().unwrap();
        app.scrapes.stop(id).await.unwrap();
        app.shutdown().await.unwrap();
        app.writer.shutdown().await.unwrap();
        server.abort();
    });
}

#[test]
fn cors_gzip_static_ranges_cached_media_and_missing_credentials_are_safe() {
    let h = python();
    let rt = runtime();
    rt.block_on(async {
        let (app, fixture, server) = application(h.archive(), vec![]).await;
        let http = router(app.clone());
        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/settings")
            .header("origin", "http://127.0.0.1:8000")
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .unwrap();
        let response = http.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["access-control-allow-origin"],
            "http://127.0.0.1:8000"
        );
        let req = Request::builder()
            .uri("/")
            .header("accept-encoding", "gzip")
            .body(Body::empty())
            .unwrap();
        let response = http.clone().oneshot(req).await.unwrap();
        assert_eq!(response.headers()["content-encoding"], "gzip");
        let req = Request::builder()
            .uri("/style.css")
            .header("range", "bytes=0-9")
            .body(Body::empty())
            .unwrap();
        let response = http.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(to_bytes(response.into_body(), 100).await.unwrap().len(), 10);
        assert_eq!(
            request(&http, "GET", "/api/token/validate", Value::Null).await["body"],
            json!({"valid":false})
        );
        let started = request(
            &http,
            "POST",
            "/api/dm-clear/start",
            json!({"channel_id":"20"}),
        )
        .await;
        assert_eq!(started["status"], 200);
        let id = started["body"]["job_id"].as_str().unwrap();
        app.dm.barrier(id).await.unwrap();
        assert_eq!(
            app.dm.state(id).await.unwrap()["terminal"]["message"],
            "401: No token configured"
        );
        assert!(fixture.requests.lock().unwrap().is_empty());
        app.shutdown().await.unwrap();
        app.writer.shutdown().await.unwrap();
        server.abort();
    });
}

#[test]
fn active_http_event_stream_ends_before_graceful_server_shutdown() {
    let h = python();
    let rt = runtime();
    rt.block_on(async {
        let (app, _fixture, upstream) = application(h.archive(), vec![]).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (signal, stop) = tokio::sync::oneshot::channel();
        let shutdown = app.clone();
        let http = router(app.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, http)
                .with_graceful_shutdown(async move {
                    let _ = stop.await;
                    shutdown.shutdown().await.unwrap();
                })
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut response = tokio::time::timeout(
            Duration::from_secs(2),
            client.get(format!("{origin}/api/live/events")).send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "text/event-stream; charset=utf-8"
        );
        signal.send(()).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), response.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        app.writer.shutdown().await.unwrap();
        upstream.abort();
    });
}

#[test]
fn message_refresh_requires_exact_missing_code_and_confirmed_channel_access() {
    let h = python();
    let rt = runtime();
    rt.block_on(async {
        let id = 1450000000000000001i64;
        let mut edited = message(id, "30", "edited");
        edited["attachments"] = json!([]);
        let (app, fixture, server) = application(
            h.archive(),
            vec![
                json!({"status":200,"body":edited}),
                json!({"status":404,"body":{"code":10003}}),
                json!({"status":404,"body":{"code":10008}}),
                json!({"status":403}),
                json!({"status":404,"body":{"code":10008}}),
                json!({"status":200,"body":{"id":"20"}}),
            ],
        )
        .await;
        app.accounts
            .update(json!({"token":"synthetic-account"}))
            .await
            .unwrap();
        app.writer
            .write(move |store| {
                store.save_page(
                    &[message(id, "30", "before")],
                    &PageContext {
                        channel_id: "20",
                        channel_name: "chat",
                        ..Default::default()
                    },
                )?;
                Ok(Value::Null)
            })
            .await
            .unwrap();
        let http = router(app.clone());
        let path = format!("/api/messages/{id}/refresh");
        let refreshed = request(&http, "POST", &path, Value::Null).await;
        assert_eq!(refreshed["status"], 200);
        assert_eq!(refreshed["body"]["payload"]["content"], "edited");
        assert!(refreshed["body"]["deleted_at"].is_null());
        assert_eq!(
            request(&http, "POST", &path, Value::Null).await["status"],
            404
        );
        assert_eq!(
            request(&http, "POST", &path, Value::Null).await["status"],
            409
        );
        assert!(
            app.writer
                .write(move |store| Ok(
                    store.read_message(id, 0, 100)?.unwrap()["deleted_at"].clone()
                ))
                .await
                .unwrap()
                .is_null()
        );
        let missing = request(&http, "POST", &path, Value::Null).await;
        assert_eq!(missing["status"], 200);
        assert!(missing["body"]["deleted_at"].is_string());
        assert_eq!(
            missing["body"]["revisions"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["source"],
            "upstream_not_found"
        );
        assert_eq!(fixture.requests.lock().unwrap().len(), 6);
        app.shutdown().await.unwrap();
        app.writer.shutdown().await.unwrap();
        server.abort();
    });
}

#[test]
fn archived_images_redirect_to_verified_local_media_and_clear_only_owned_files() {
    use sha2::{Digest, Sha256};
    let h = python();
    let rt = runtime();
    rt.block_on(async {
        let(app,fixture,server)=application(h.archive(),vec![]).await;let id=1450000000000000001i64;
        app.writer.write(move|store|{store.save_page(&[message(id,"30","image")],&PageContext{channel_id:"20",channel_name:"chat",..Default::default()})?;Ok(Value::Null)}).await.unwrap();
        let url="https://cdn.discordapp.com/attachments/20/1/image.png";let key=searchcord_archive::media::media_key(url).unwrap();let directory=searchcord_archive::media::directory(&h.archive());std::fs::create_dir_all(&directory).unwrap();let bytes=b"synthetic-media";std::fs::write(directory.join(format!("{key}.bin")),bytes).unwrap();std::fs::write(directory.join(format!("{key}.json")),json!({"status":"saved","bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes)),"content_type":"image/png"}).to_string()).unwrap();std::fs::write(directory.join("keep.txt"),b"unrelated").unwrap();let http=router(app.clone());
        let image=request(&http,"GET",&format!("/api/images/{id}/0"),Value::Null).await;assert_eq!(image["status"],302);assert_eq!(image["headers"]["location"],format!("/api/media/{key}"));
        let media=request(&http,"GET",&format!("/api/media/{key}"),Value::Null).await;assert_eq!(media["body"],"synthetic-media");assert_eq!(media["headers"]["content-security-policy"],"default-src 'none'; sandbox");assert_eq!(media["headers"]["cache-control"],"private, no-store");
        std::fs::write(directory.join(format!("{key}.bin")),b"corrupt").unwrap();assert_eq!(request(&http,"GET",&format!("/api/media/{key}"),Value::Null).await["status"],404);
        assert_eq!(request(&http,"DELETE","/api/messages?confirm=true",Value::Null).await["status"],200);assert!(!directory.join(format!("{key}.bin")).exists());assert!(!directory.join(format!("{key}.json")).exists());assert!(directory.join("keep.txt").exists());assert!(fixture.requests.lock().unwrap().is_empty());
        app.shutdown().await.unwrap();app.writer.shutdown().await.unwrap();server.abort();
    });
}

#[test]
fn profile_manual_fetch_and_backfill_routes_share_presentation_and_exact_idle_stop() {
    let mut h = python();
    let rt = runtime();
    rt.block_on(async {
        let(app,fixture,server)=application(h.archive(),vec![json!({"status":200,"body":{"user":{"id":"30","username":"Alice","global_name":"Alice"},"user_profile":{"bio":"Fixture bio"},"mutual_guilds":[]}})]).await;
        app.writer.write(|store|{store.save_page(&[message(1450000000000000001,"30","profile")],&PageContext{channel_id:"20",channel_name:"chat",..Default::default()})?;Ok(Value::Null)}).await.unwrap();app.accounts.update(json!({"token":"synthetic-account"})).await.unwrap();let http=router(app.clone());
        let expected=oracle(&mut h,"POST","/api/profile-backfill/stop",Value::Null);let actual=request(&http,"POST","/api/profile-backfill/stop",Value::Null).await;assert_eq!(actual["body"],expected["body"]);
        let fetched=request(&http,"POST","/api/profiles/30/fetch",Value::Null).await;assert_eq!(fetched["status"],200);assert_eq!(fetched["body"],oracle(&mut h,"GET","/api/profiles/30",Value::Null)["body"]);
        let started=request(&http,"POST","/api/profile-backfill",Value::Null).await;assert_eq!(started["body"]["running"],true);let stopped=request(&http,"POST","/api/profile-backfill/stop",Value::Null).await;assert_eq!(stopped["body"]["running"],false);assert_eq!(stopped["body"]["cancelled"],true);
        assert_eq!(fixture.requests.lock().unwrap().len(),1);app.shutdown().await.unwrap();app.writer.shutdown().await.unwrap();server.abort();
    });
}
