//! All six original permission/discovery regressions, plus actual HTTP ownership.
use searchcord_parity::Harness;
use serde_json::{Value, json};
const VIEW: i64 = 1 << 10;
const HISTORY: i64 = 1 << 16;
const READABLE: i64 = VIEW | HISTORY;
fn guild() -> Value {
    json!({"id":"10","owner_id":"99","roles":[{"id":"10","permissions":READABLE.to_string()},
    {"id":"40","permissions":"0"},{"id":"41","permissions":"0"}]})
}
fn member() -> Value {
    json!({"user":{"id":"30"},"roles":["40","41"]})
}
fn channel(id: &str, name: &str, kind: i64, overwrites: Vec<Value>) -> Value {
    json!({"id":id,"name":name,"type":kind,"permission_overwrites":overwrites})
}
fn overwrite(id: &str, deny: i64, allow: i64, kind: i64) -> Value {
    json!({"id":id,"type":kind,"deny":deny.to_string(),"allow":allow.to_string()})
}
fn permissions(
    h: &mut Harness,
    guild: Value,
    member: Value,
    channel: Value,
    channels: Value,
) -> i64 {
    h.command(json!({"op":"permissions","guild":guild,"member":member,"channel":channel,"channels":channels})).as_i64().unwrap()
}
fn call(h: &mut Harness, mut r: Value) -> Value {
    r["op"] = json!("job");
    h.command(r)
}
fn setup() -> Harness {
    let mut h = Harness::new().unwrap();
    h.init();
    call(&mut h, json!({"action":"configure"}));
    call(
        &mut h,
        json!({"action":"options","values":{"DISCORD_INTERVAL_SECONDS":0}}),
    );
    h
}
fn configure(h: &mut Harness, replies: Vec<Value>) {
    call(h, json!({"action":"configure","replies":replies}));
}
fn ok(body: Value) -> Value {
    json!({"status":200,"body":body})
}
fn scan(h: &mut Harness, threads: bool) -> Value {
    call(
        h,
        json!({"action":"http","method":"GET","path":format!("/api/guilds/10/channels?include_threads={threads}")}),
    )
}
fn metadata(channels: Vec<Value>) -> Vec<Value> {
    vec![
        ok(json!(channels)),
        ok(guild()),
        ok(json!({"id":"30"})),
        ok(member()),
    ]
}

#[test]
fn role_allows_win_over_combined_role_denies_then_member_wins() {
    let mut h = Harness::new().unwrap();
    let mut c = channel(
        "20",
        "general",
        0,
        vec![
            overwrite("10", READABLE, 0, 0),
            overwrite("40", READABLE, 0, 0),
            overwrite("41", 0, READABLE, 0),
        ],
    );
    assert_eq!(
        permissions(&mut h, guild(), member(), c.clone(), json!({"20":c})) & READABLE,
        READABLE
    );
    c["permission_overwrites"]
        .as_array_mut()
        .unwrap()
        .push(overwrite("30", HISTORY, 0, 1));
    assert_eq!(
        permissions(&mut h, guild(), member(), c.clone(), json!({"20":c})) & READABLE,
        VIEW
    );
}
#[test]
fn owner_and_administrator_bypass_overwrites() {
    let mut h = Harness::new().unwrap();
    let c = channel("20", "general", 0, vec![overwrite("10", READABLE, 0, 0)]);
    assert_eq!(
        permissions(
            &mut h,
            guild(),
            json!({"user":{"id":"99"},"roles":[]}),
            c.clone(),
            json!({"20":c})
        ) & READABLE,
        READABLE
    );
    let mut g = guild();
    g["roles"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"42","permissions":"8"}));
    assert_eq!(
        permissions(
            &mut h,
            g,
            json!({"user":{"id":"30"},"roles":["42"]}),
            c.clone(),
            json!({"20":c})
        ) & READABLE,
        READABLE
    );
}
#[test]
fn thread_uses_parent_not_its_own_overwrites() {
    let mut h = Harness::new().unwrap();
    let parent = channel("20", "general", 0, vec![overwrite("30", HISTORY, 0, 1)]);
    let mut thread = channel("21", "thread", 11, vec![overwrite("30", 0, READABLE, 1)]);
    thread["parent_id"] = json!("20");
    assert_eq!(
        permissions(&mut h, guild(), member(), thread, json!({"20":parent})) & READABLE,
        VIEW
    );
}
#[test]
fn filters_private_and_view_only_but_keeps_empty_readable_and_bots() {
    let mut h = setup();
    let mut private = channel("25", "private-thread", 12, vec![]);
    private["parent_id"] = json!("20");
    let mut replies = metadata(vec![
        channel("20", "general", 0, vec![]),
        channel("21", "view-only", 0, vec![overwrite("10", HISTORY, 0, 0)]),
        channel("22", "hidden", 0, vec![overwrite("10", VIEW, 0, 0)]),
        channel("23", "bot-commands", 0, vec![]),
        channel("24", "stale-access", 0, vec![]),
        private,
    ]);
    replies.extend([
        ok(json!([])),
        ok(json!([])),
        json!({"status":403,"body":{}}),
        json!({"status":403,"body":{}}),
    ]);
    configure(&mut h, replies);
    let response = scan(&mut h, false);
    assert_eq!(response["status"], 200);
    assert_eq!(
        response["body"],
        json!([{"id":"20","name":"general","type":0,"category":"","category_id":null,"nsfw":false},
        {"id":"23","name":"bot-commands","type":0,"category":"","category_id":null,"nsfw":false}])
    );
    let trace = call(&mut h, json!({"action":"inspect"}));
    let probes: Vec<_> = trace["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"].as_str().unwrap().ends_with("/messages"))
        .collect();
    assert_eq!(probes.len(), 4);
    assert_eq!(
        probes
            .iter()
            .map(|r| r["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "/api/v10/channels/20/messages",
            "/api/v10/channels/23/messages",
            "/api/v10/channels/24/messages",
            "/api/v10/channels/25/messages"
        ]
    );
    assert!(probes.iter().all(|r| r["params"] == json!({"limit":"1"})));
}
#[test]
fn discovers_and_paginates_threads_without_duplicates() {
    let mut h = setup();
    let parent = channel("20", "general", 0, vec![]);
    let mut active = channel("21", "active", 11, vec![]);
    active["parent_id"] = json!("20");
    let mut archived = channel("22", "archive", 11, vec![]);
    archived["parent_id"] = json!("20");
    archived["thread_metadata"] = json!({"archive_timestamp":"2026-01-01T00:00:00Z"});
    let mut last = channel("23", "older", 11, vec![]);
    last["parent_id"] = json!("20");
    let mut replies = metadata(vec![parent]);
    replies.extend([
        ok(json!({"threads":[active.clone()]})),
        ok(json!({"threads":[active,archived.clone()],"has_more":true})),
        ok(json!({"threads":[last],"has_more":false})),
        ok(json!({"threads":[archived],"has_more":false})),
    ]);
    replies.extend((0..4).map(|_| ok(json!([]))));
    configure(&mut h, replies);
    let response = scan(&mut h, true);
    assert_eq!(response["status"], 200);
    assert_eq!(
        response["body"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["20", "21", "22", "23"]
    );
    let trace = call(&mut h, json!({"action":"inspect"}));
    let cursors: Vec<_> = trace["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"].as_str().unwrap().ends_with("/archived/public"))
        .map(|r| r["params"].clone())
        .collect();
    assert_eq!(
        cursors,
        json!([{"limit":"100"},{"limit":"100","before":"2026-01-01T00:00:00Z"}])
            .as_array()
            .unwrap()
            .clone()
    );
}
#[test]
fn transient_probe_does_not_misreport_a_partial_scan() {
    let mut h = setup();
    let mut replies = metadata(vec![channel("20", "general", 0, vec![])]);
    replies.push(json!({"status":503,"body":{}}));
    configure(&mut h, replies);
    let response = scan(&mut h, false);
    assert_eq!(response["status"], 503);
    assert_eq!(
        response["body"]["detail"],
        "Could not verify channel access (HTTP 503). Try again."
    );
}
#[test]
fn readable_channels_contracts() {
    let mut h = setup();
    configure(&mut h, vec![ok(json!([]))]);
    assert_eq!(scan(&mut h, true)["body"], json!([]));
    assert_eq!(
        call(&mut h, json!({"action":"inspect"}))["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let response = call(
        &mut h,
        json!({"action":"http","method":"POST","path":"/api/guilds/10/channels"}),
    );
    assert_eq!(response["status"], 405);
}
