//! Durable job contracts: Rust controls requests, checkpoints, barriers and assertions.
use searchcord_parity::Harness;
use serde_json::{Value, json};

fn channel(id: &str) -> Value {
    json!({"id":id,"name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
fn message(n: u64) -> Value {
    json!({"id":(1_450_000_000_000_000_000+n).to_string(),"author":{"id":"30","username":"fixture"},
        "content":"synthetic","attachments":[]})
}
fn job(h: &mut Harness, mut command: Value) -> Value {
    command["op"] = json!("job");
    h.command(command)
}
fn setup() -> Harness {
    let mut h = Harness::new().unwrap();
    h.init();
    job(&mut h, json!({"action":"configure"}));
    h
}
fn configure(h: &mut Harness, messages: Vec<Value>, hold: bool) {
    job(
        h,
        json!({"action":"configure","hold":hold,"replies":[{"status":200,"body":messages}]}),
    );
}
fn start(h: &mut Harness, limit: u64, profiles: bool, channels: Vec<Value>) -> String {
    let reply = job(
        h,
        json!({"action":"http","path":"/api/scrape/start","body":{
        "channels":channels,"limit":limit,"harvest_profiles":profiles}}),
    );
    assert_eq!(reply["status"], 200, "{reply}");
    reply["body"]["job_id"].as_str().unwrap().to_owned()
}
fn barrier(h: &mut Harness, name: &str) {
    job(h, json!({"action":"wait_barrier","name":name}));
}
fn release(h: &mut Harness, name: &str) {
    job(h, json!({"action":"release","name":name}));
}
fn control(h: &mut Harness, method: &str, id: &str) -> Value {
    job(h, json!({"action":"control","method":method,"id":id}))
}
fn suspend(h: &mut Harness) {
    job(h, json!({"action":"control","method":"suspend"}));
}
fn wait(h: &mut Harness, id: &str, fields: Value) -> Value {
    job(h, json!({"action":"wait_snapshot","id":id,"fields":fields}))
}
fn inspect(h: &mut Harness) -> Value {
    job(h, json!({"action":"inspect"}))
}
fn saved(h: &Harness, id: &str) -> Value {
    let raw: String = h
        .database()
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE id=?",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
fn count(h: &Harness, table: &str) -> i64 {
    assert!(["messages", "scrape_cursors", "collection_jobs"].contains(&table));
    h.database()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn spawn_control(h: &mut Harness, method: &str, id: Option<&str>) {
    let mut call = json!({"action":"control","method":method});
    if let Some(id) = id {
        call["id"] = json!(id);
    }
    job(h, json!({"action":"spawn","name":method,"call":call}));
}
fn http(h: &mut Harness, method: &str, path: &str) -> Value {
    job(h, json!({"action":"http","method":method,"path":path}))
}

#[test]
fn checkpoint_failure_rolls_back_page_cursor_flags_and_projection() {
    let mut h = setup();
    job(&mut h, json!({"action":"commit_barrier","failures":1}));
    configure(&mut h, vec![message(41), message(40)], false);
    let id = start(&mut h, 3, false, vec![channel("20")]);
    barrier(&mut h, "commit");
    assert_eq!(
        job(&mut h, json!({"action":"snapshot","id":id}))["total_messages"],
        0
    );
    assert_eq!(count(&h, "messages"), 0);
    assert_eq!(count(&h, "scrape_cursors"), 0);
    assert_eq!(saved(&h, &id)["total_messages"], 0);
    spawn_control(&mut h, "pause", Some(&id));
    release(&mut h, "commit");
    job(&mut h, json!({"action":"task","name":"pause","join":true}));
    let after = wait(&mut h, &id, json!({"paused":true}));
    assert_eq!(after["total_messages"], 2);
    assert_eq!(after["rows"][0]["messages"], 2);
    let cursor: (String, String, i64) = h
        .database()
        .query_row(
            "SELECT newest_message_id,oldest_message_id,history_complete FROM scrape_cursors",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        cursor,
        (
            message(41)["id"].as_str().unwrap().into(),
            message(40)["id"].as_str().unwrap().into(),
            1
        )
    );
    assert_eq!(inspect(&mut h)["attempts"], 2);
    assert_eq!(inspect(&mut h)["requests"].as_array().unwrap().len(), 1);
    assert_eq!(saved(&h, &id)["rows"], after["rows"]);
}

#[test]
fn stop_joins_inflight_commit_before_archive_clear() {
    let mut h = setup();
    job(&mut h, json!({"action":"commit_barrier"}));
    configure(&mut h, vec![message(41), message(40)], false);
    let id = start(&mut h, 0, false, vec![channel("20")]);
    barrier(&mut h, "commit");
    spawn_control(&mut h, "stop", Some(&id));
    assert_eq!(
        job(&mut h, json!({"action":"task","name":"stop"}))["done"],
        false
    );
    assert_eq!(
        http(&mut h, "DELETE", "/api/messages?confirm=true")["status"],
        409
    );
    assert_eq!(
        http(&mut h, "DELETE", "/api/settings/tokens?confirm=true")["status"],
        409
    );
    release(&mut h, "commit");
    job(&mut h, json!({"action":"task","name":"stop","join":true}));
    assert_eq!(
        wait(&mut h, &id, json!({"running":false}))["total_messages"],
        2
    );
    assert_eq!(count(&h, "messages"), 2);
    assert_eq!(inspect(&mut h)["has_work"], false);
    assert_eq!(
        http(&mut h, "DELETE", "/api/messages?confirm=true")["status"],
        200
    );
    assert_eq!(count(&h, "messages"), 0);
    let scrape_jobs: i64 = h
        .database()
        .query_row(
            "SELECT COUNT(*) FROM collection_jobs WHERE kind='scrape'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(scrape_jobs, 0);
}

#[test]
fn shutdown_joins_commit_and_recovers_committed_rows_budget_and_queue() {
    let mut h = setup();
    job(&mut h, json!({"action":"commit_barrier"}));
    configure(&mut h, (101..=200).rev().map(message).collect(), false);
    let id = start(&mut h, 101, false, vec![channel("20"), channel("21")]);
    barrier(&mut h, "commit");
    spawn_control(&mut h, "suspend", None);
    assert_eq!(
        job(&mut h, json!({"action":"task","name":"suspend"}))["done"],
        false
    );
    release(&mut h, "commit");
    job(
        &mut h,
        json!({"action":"task","name":"suspend","join":true}),
    );
    job(&mut h, json!({"action":"recover"}));
    let snapshot = wait(&mut h, &id, json!({"paused":true}));
    assert_eq!(snapshot["total_messages"], 100);
    assert_eq!(snapshot["rows"][0]["messages"], 100);
    configure(&mut h, vec![message(100)], false);
    control(&mut h, "resume", &id);
    let done = wait(&mut h, &id, json!({"running":false}));
    assert_eq!(done["total_messages"], 101);
    assert_eq!(done["completed"], 2);
    let trace = inspect(&mut h);
    assert_eq!(
        trace["requests"][1]["params"],
        json!({"limit":"1","before":message(101)["id"]})
    );
    assert_eq!(
        trace["requests"][2]["path"],
        "/api/v10/channels/21/messages"
    );
    assert_eq!(count(&h, "messages"), 101);
}

#[test]
fn restart_restores_pending_profiles_before_remaining_page_budget() {
    let mut h = setup();
    configure(&mut h, (101..=200).rev().map(message).collect(), true);
    let id = start(&mut h, 101, true, vec![channel("20")]);
    barrier(&mut h, "fetch");
    control(&mut h, "pause", &id);
    release(&mut h, "fetch");
    wait(&mut h, &id, json!({"paused":true}));
    suspend(&mut h);
    job(&mut h, json!({"action":"recover"}));
    assert_eq!(
        saved(&h, &id)["channel_state"]["pending_profiles"],
        json!(["30"])
    );
    assert_eq!(
        http(&mut h, "DELETE", "/api/messages?confirm=true")["status"],
        409
    );
    let conflict = job(
        &mut h,
        json!({"action":"http","path":"/api/scrape/start","body":{"channels":[channel("20")]}}),
    );
    assert_eq!(conflict["status"], 409);
    job(
        &mut h,
        json!({"action":"configure","replies":[
        {"status":200,"body":{"user":{"id":"30"}}},{"status":200,"body":[message(100)]}]}),
    );
    control(&mut h, "resume", &id);
    wait(&mut h, &id, json!({"running":false}));
    let trace = inspect(&mut h);
    let paths: Vec<_> = trace["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/api/v10/channels/20/messages",
            "/api/v10/users/30/profile",
            "/api/v10/channels/20/messages"
        ]
    );
    assert_eq!(
        trace["requests"][2]["params"],
        json!({"limit":"1","before":message(101)["id"]})
    );
}

#[test]
fn old_checkpoint_representation_requires_original_account_and_keeps_budget() {
    let mut h = setup();
    h.command(json!({"op":"save","messages":[message(41),message(40)],"channel_id":"20","channel_name":"fixture","guild_id":"10","guild_name":"Fixture"}));
    let old = json!({"running":true,"cancelled":false,"pause_requested":false,"paused":false,
        "channels":[channel("20")],"limit":3,"harvest_profiles":false,"index":1,"total_messages":2,"completed":0,
        "rows":[{"type":"progress","channel":"fixture","messages":2,"total_messages":2}],
        "last_event":{"type":"progress","channel":"fixture","messages":2,"total_messages":2},
        "next_index":0,"failed_channels":0,"credential_id":"account","profile_auth_failed":null,
        "channel_state":{"stage":"history","messages":2,"profiles":0,"pending_profiles":[],"anchor":null,
            "before":message(40)["id"],"newest":null,"oldest":null,"history_complete":false,"resuming_pending":false}});
    h.database()
        .execute(
            "INSERT INTO collection_jobs VALUES ('old-scrape','scrape',?,0)",
            [old.to_string()],
        )
        .unwrap();
    job(&mut h, json!({"action":"recover"}));
    wait(&mut h, "old-scrape", json!({"paused":true}));
    job(&mut h, json!({"action":"credentials","id":"different"}));
    assert_eq!(
        http(&mut h, "POST", "/api/scrape/old-scrape/resume")["status"],
        409
    );
    assert_eq!(inspect(&mut h)["requests"], json!([]));
    job(&mut h, json!({"action":"credentials","id":"account"}));
    configure(&mut h, vec![message(39)], false);
    control(&mut h, "resume", "old-scrape");
    assert_eq!(
        wait(&mut h, "old-scrape", json!({"running":false}))["total_messages"],
        3
    );
    assert_eq!(
        inspect(&mut h)["requests"][0]["params"],
        json!({"limit":"1","before":message(40)["id"]})
    );
}

#[test]
fn account_race_between_resume_and_worker_remains_paused() {
    let mut h = setup();
    configure(&mut h, vec![], true);
    let id = start(&mut h, 0, false, vec![channel("20")]);
    barrier(&mut h, "fetch");
    suspend(&mut h);
    job(&mut h, json!({"action":"recover"}));
    job(
        &mut h,
        json!({"action":"credentials","sequence":[["account","synthetic"],["different","synthetic-other"]]}),
    );
    control(&mut h, "resume", &id);
    wait(&mut h, &id, json!({"paused":true}));
    assert_eq!(inspect(&mut h)["requests"].as_array().unwrap().len(), 1);
    let state = saved(&h, &id);
    assert_eq!(state["running"], true);
    assert_eq!(state["paused"], true);
    assert_eq!(state["credential_id"], "account");
}

#[test]
fn simultaneous_start_reserves_channels_before_checkpoint_write() {
    let mut h = setup();
    configure(&mut h, vec![], true);
    let reply = job(
        &mut h,
        json!({"action":"concurrent_http","requests":[
        {"path":"/api/scrape/start","body":{"channels":[channel("20")]}},
        {"path":"/api/scrape/start","body":{"channels":[channel("20")]}}]}),
    );
    let mut statuses: Vec<_> = reply
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["status"].as_u64().unwrap())
        .collect();
    statuses.sort();
    assert_eq!(statuses, vec![200, 409]);
    assert_eq!(count(&h, "collection_jobs"), 1);
}

#[test]
fn shutdown_before_worker_first_turn_keeps_recoverable_checkpoint() {
    let mut h = setup();
    let id = job(
        &mut h,
        json!({"action":"start_suspend","channels":[channel("20")]}),
    );
    job(&mut h, json!({"action":"recover"}));
    let snapshot = wait(&mut h, id.as_str().unwrap(), json!({"paused":true}));
    assert_eq!(snapshot["total_messages"], 0);
    assert_eq!(inspect(&mut h)["requests"], json!([]));
}

#[test]
fn resume_cannot_launch_worker_after_shutdown_barrier() {
    let mut h = setup();
    configure(&mut h, vec![], true);
    let id = start(&mut h, 0, false, vec![channel("20")]);
    barrier(&mut h, "fetch");
    control(&mut h, "pause", &id);
    job(&mut h, json!({"action":"credentials_barrier"}));
    job(
        &mut h,
        json!({"action":"spawn","name":"resume","call":{"action":"http","path":format!("/api/scrape/{id}/resume")}}),
    );
    barrier(&mut h, "credentials");
    spawn_control(&mut h, "suspend", None);
    release(&mut h, "credentials");
    assert_eq!(
        job(&mut h, json!({"action":"task","name":"resume","join":true}))["status"],
        409
    );
    job(
        &mut h,
        json!({"action":"task","name":"suspend","join":true}),
    );
    assert_eq!(
        job(&mut h, json!({"action":"snapshot","id":id}))["paused"],
        true
    );
    assert_eq!(inspect(&mut h)["requests"].as_array().unwrap().len(), 1);
}
