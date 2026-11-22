use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::Response,
};
use searchcord_collector::{
    Store,
    discord::Discord,
    discovery::{Discovery, HttpError, READABLE, permissions_for},
    pacing::{Gate, SystemClock},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

struct Mock {
    service: Discovery<SystemClock>,
    replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    trace: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Mock {
    async fn new(replies: Vec<(u16, Value)>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive.db");
        drop(Store::open(&path).unwrap());
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let trace = Arc::new(Mutex::new(Vec::new()));
        type StateData = (Arc<Mutex<VecDeque<(u16, Value)>>>, Arc<Mutex<Vec<String>>>);
        async fn respond(
            State((replies, trace)): State<StateData>,
            request: Request,
        ) -> Response<Body> {
            assert_eq!(request.headers()["authorization"], "fixture");
            trace.lock().unwrap().push(request.uri().to_string());
            let (status, body) = replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            Response::builder()
                .status(status)
                .body(Body::from(body.to_string()))
                .unwrap()
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(respond)
            .with_state((replies.clone(), trace.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let gate = Gate::open(path, SystemClock::default()).await.unwrap();
        Self {
            service: Discovery::new(Discord::for_loopback(gate, &origin).unwrap()),
            replies,
            trace,
            server,
            _dir: dir,
        }
    }
}
fn guild() -> Value {
    json!({"id":"10","owner_id":"99","roles":[{"id":"10","permissions":READABLE.to_string()},{"id":"40","permissions":"0"},{"id":"41","permissions":"0"}]})
}
fn member() -> Value {
    json!({"user":{"id":"30"},"roles":["40","41"]})
}
fn channel(id: &str) -> Value {
    json!({"id":id,"name":id,"type":0,"permission_overwrites":[]})
}
fn overwrite(id: &str, deny: u64, allow: u64, kind: u8) -> Value {
    json!({"id":id,"type":kind,"deny":deny.to_string(),"allow":allow.to_string()})
}
fn metadata(channels: Value) -> Vec<(u16, Value)> {
    vec![
        (200, channels),
        (200, guild()),
        (200, json!({"id":"30"})),
        (200, member()),
    ]
}
fn thread(id: &str) -> Value {
    json!({"id":id,"name":id,"type":11,"parent_id":"20","thread_metadata":{"archive_timestamp":format!("date-{id}")}})
}
#[test]
fn permission_precedence_owner_admin_parent_and_arbitrary_width() {
    let mut c = channel("20");
    c["permission_overwrites"] = json!([
        overwrite("10", READABLE, 0, 0),
        overwrite("40", READABLE, 0, 0),
        overwrite("41", 0, READABLE, 0)
    ]);
    let map = HashMap::from([("20".into(), c.clone())]);
    assert_eq!(
        permissions_for(&guild(), &member(), &c, &map).unwrap() & READABLE,
        READABLE
    );
    c["permission_overwrites"]
        .as_array_mut()
        .unwrap()
        .push(overwrite("30", 1 << 16, 0, 1));
    let map = HashMap::from([("20".into(), c.clone())]);
    assert_eq!(
        permissions_for(&guild(), &member(), &thread("21"), &map).unwrap() & READABLE,
        1 << 10
    );
    let mut owner = member();
    owner["user"]["id"] = json!("99");
    assert_eq!(
        permissions_for(&guild(), &owner, &c, &map).unwrap(),
        u64::MAX
    );
    let mut g = guild();
    g["roles"][0]["permissions"] = json!("18446744073709551624");
    assert_eq!(permissions_for(&g, &member(), &c, &map).unwrap(), u64::MAX);
    g["roles"][0]["permissions"] = json!("not-permissions");
    assert!(permissions_for(&g, &member(), &c, &map).is_err());
}
#[tokio::test]
async fn native_discovery_keeps_empty_readable_bot_channels_and_excludes_denied() {
    let mut denied = channel("21");
    denied["permission_overwrites"] = json!([overwrite("10", 1 << 16, 0, 0)]);
    let mut bot = channel("22");
    bot["name"] = json!("bot-commands");
    let mut replies = metadata(json!([channel("20"), denied, bot, channel("23")]));
    replies.extend([(200, json!([])), (200, json!([])), (403, json!({}))]);
    let m = Mock::new(replies).await;
    let result = m.service.channels("10", "fixture", false).await.unwrap();
    assert_eq!(
        result,
        json!([{"id":"20","name":"20","type":0,"category":"","category_id":null,"nsfw":false},{"id":"22","name":"bot-commands","type":0,"category":"","category_id":null,"nsfw":false}])
    );
    assert!(m.replies.lock().unwrap().is_empty());
    assert_eq!(
        &m.trace.lock().unwrap()[4..],
        &[
            "/api/v10/channels/20/messages?limit=1",
            "/api/v10/channels/22/messages?limit=1",
            "/api/v10/channels/23/messages?limit=1"
        ]
    );
}
#[tokio::test]
async fn native_threads_paginate_deduplicate_and_use_private_id_cursor() {
    let mut g = guild();
    g["roles"][0]["permissions"] = json!((READABLE | (1 << 34)).to_string());
    let mut replies = metadata(json!([channel("20")]));
    replies[1] = (200, g);
    replies.extend([
        (200, json!({"threads":[thread("21")]})),
        (
            200,
            json!({"threads":[thread("21"),thread("22")],"has_more":true}),
        ),
        (200, json!({"threads":[thread("23")],"has_more":false})),
        (200, json!({"threads":[thread("22")],"has_more":true})),
        (200, json!({"threads":[],"has_more":false})),
        (403, json!({})),
    ]);
    replies.extend((0..4).map(|_| (200, json!([]))));
    let m = Mock::new(replies).await;
    let r = m.service.channels("10", "fixture", true).await.unwrap();
    assert_eq!(
        r.as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["20", "21", "22", "23"]
    );
    let trace = m.trace.lock().unwrap();
    assert!(
        trace
            .iter()
            .any(|s| s.ends_with("/archived/public?limit=100&before=date-22"))
    );
    assert!(
        trace
            .iter()
            .any(|s| s.ends_with("/users/@me/threads/archived/private?limit=100&before=22"))
    );
    assert!(m.replies.lock().unwrap().is_empty());
}
#[tokio::test]
async fn native_discovery_propagates_transient_and_verification_errors_and_refuses_bad_pages() {
    for (status, payload, expected, detail) in [
        (
            503,
            json!({}),
            503,
            "Could not verify channel access (HTTP 503). Try again.",
        ),
        (
            403,
            json!({"code":40002}),
            403,
            "Complete account verification in Discord before checking channels.",
        ),
        (
            200,
            json!({}),
            502,
            "Discord returned an invalid channel access probe. Try again.",
        ),
    ] {
        let mut replies = metadata(json!([channel("20")]));
        replies.push((status, payload));
        let m = Mock::new(replies).await;
        let err = m
            .service
            .channels("10", "fixture", false)
            .await
            .unwrap_err();
        let err = err.downcast_ref::<HttpError>().unwrap();
        assert_eq!(err.status, expected);
        assert_eq!(err.detail, detail);
    }
    let m = Mock::new(vec![(200, json!([]))]).await;
    assert_eq!(
        m.service.channels("10", "fixture", true).await.unwrap(),
        json!([])
    );
    assert_eq!(m.trace.lock().unwrap().len(), 1);
    let mut replies = metadata(json!([channel("20")]));
    replies.extend([
        (200, json!({"threads":[]})),
        (200, json!({"threads":[],"has_more":true})),
    ]);
    let m = Mock::new(replies).await;
    assert!(
        m.service
            .channels("10", "fixture", true)
            .await
            .unwrap_err()
            .to_string()
            .contains("incomplete archived threads")
    );
}
#[tokio::test]
async fn account_guild_and_dm_shapes_keep_public_ids_and_labels() {
    let m=Mock::new(vec![(200,json!({"id":"30","username":"user","global_name":"Label"})),(401,json!({})),(200,json!([{"id":"10","name":"guild","icon":"hash"},{"id":"11","name":"other","icon":null}])),(200,json!([{"id":"20","type":1,"recipients":[{"username":"a","global_name":"A"}]},{"id":"21","type":3,"recipients":[{"username":"b"},{"username":"c"}]},{"id":"22","type":3,"recipients":[]},{"id":"23","type":2}]))]).await;
    assert_eq!(
        m.service.validate("fixture").await.unwrap(),
        json!({"valid":true,"username":"Label","id":"30"})
    );
    assert_eq!(
        m.service.validate("fixture").await.unwrap(),
        json!({"valid":false})
    );
    assert_eq!(
        m.service.guilds("fixture").await.unwrap(),
        json!([{"id":"10","name":"guild","icon":"https://cdn.discordapp.com/icons/10/hash.png?size=64"},{"id":"11","name":"other","icon":null}])
    );
    assert_eq!(
        m.service.dms("fixture").await.unwrap(),
        json!([{"id":"20","name":"A","type":1},{"id":"21","name":"b, c","type":3},{"id":"22","name":"Group DM","type":3}])
    );
}
