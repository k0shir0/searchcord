//! Remaining collector contracts. No live Discord, credentials or archives.
use searchcord_parity::{Harness, parse_events};
use serde_json::{Value, json};
use std::fs;
const BASE: i64 = 1_450_000_000_000_000_000;
fn call(h: &mut Harness, mut r: Value) -> Value {
    r["op"] = json!("job");
    h.command(r)
}
fn request(h: &mut Harness, method: &str, path: &str, body: Value) -> Value {
    call(
        h,
        json!({"action":"http","method":method,"path":path,"body":body}),
    )
}
fn get(h: &mut Harness, path: &str) -> Value {
    request(h, "GET", path, Value::Null)
}
fn post(h: &mut Harness, path: &str, body: Value) -> Value {
    request(h, "POST", path, body)
}
fn configure(h: &mut Harness, replies: Value, hold: bool) {
    call(
        h,
        json!({"action":"configure","replies":replies,"hold":hold}),
    );
}
fn channel() -> Value {
    json!({"id":"20","name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
fn message(n: i64, author: &str, content: &str) -> Value {
    json!({"id":(BASE+n).to_string(),"channel_id":"20","author":{"id":author,"username":if author=="30"{"Alice"}else{"Bob"}},
        "content":content,"timestamp":"2026-01-01T12:00:00+00:00","attachments":[]})
}
fn fixture(messages: Vec<Value>) -> Harness {
    let mut h = Harness::new().unwrap();
    h.init();
    h.database().execute_batch("INSERT INTO guilds VALUES ('10','Fixture'); INSERT INTO channels VALUES ('20','10','fixture');
        INSERT INTO authors(id,name) VALUES ('30','Alice'),('31','Bob');").unwrap();
    h.command(json!({"op":"save","messages":messages,"channel_id":"20","channel_name":"fixture","guild_id":"10","guild_name":"Fixture"}));
    call(&mut h, json!({"action":"configure"}));
    call(
        &mut h,
        json!({"action":"options","values":{"DISCORD_INTERVAL_SECONDS":0}}),
    );
    h
}
fn count(h: &Harness, table: &str) -> i64 {
    h.database()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn wait_http(h: &mut Harness, path: &str, keys: Value, value: Value) -> Value {
    call(
        h,
        json!({"action":"wait_http","path":path,"keys":keys,"value":value}),
    )
}

#[test]
fn settings_sealed_vault_metadata_validation_switching_and_removal() {
    let mut h = fixture(vec![message(1, "30", "keep archive")]);
    assert_eq!(
        get(&mut h, "/api/settings")["body"],
        json!({"tokens":[],"active_token_id":null,"token_set":false})
    );
    for (body, status, detail) in [
        (json!({}), 400, "Provide a token or a saved token ID"),
        (
            json!({"token":" "}),
            400,
            "Token must contain 1 to 4096 characters",
        ),
        (
            json!({"token":"x","active_token_id":"x"}),
            400,
            "Provide a token or a saved token ID",
        ),
        (
            json!({"token":"x","token_label":"a".repeat(41)}),
            400,
            "Token name must be at most 40 characters",
        ),
        (
            json!({"active_token_id":"absent"}),
            404,
            "Saved token not found",
        ),
    ] {
        let reply = post(&mut h, "/api/settings", body);
        assert_eq!(reply["status"], status);
        assert_eq!(reply["body"]["detail"], detail);
    }
    let first = post(
        &mut h,
        "/api/settings",
        json!({"token":" synthetic-first ","token_label":" First "}),
    );
    assert_eq!(first["status"], 200);
    let id = first["body"]["active_token_id"].clone();
    let repeated = post(
        &mut h,
        "/api/settings",
        json!({"token":"synthetic-first","token_label":"Renamed"}),
    );
    assert_eq!(repeated["body"]["active_token_id"], id);
    assert_eq!(
        repeated["body"]["tokens"],
        json!([{"id":id,"label":"Renamed"}])
    );
    post(&mut h, "/api/settings", json!({"token":"synthetic-second"}));
    assert_eq!(
        post(&mut h, "/api/settings", json!({"active_token_id":id}))["status"],
        200
    );
    let settings = get(&mut h, "/api/settings")["body"].clone();
    assert_eq!(settings["tokens"].as_array().unwrap().len(), 2);
    assert_eq!(settings["active_token_id"], id);
    assert_eq!(settings["token_set"], true);
    assert!(!settings.to_string().contains("synthetic-"));
    let vault = fs::read(h.vault()).unwrap();
    assert!(!vault.windows(15).any(|v| v == b"synthetic-first"));
    let raw: String = h
        .database()
        .query_row(
            "SELECT value FROM settings WHERE key='saved_tokens'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!raw.contains("synthetic-"));
    assert_eq!(
        request(&mut h, "DELETE", "/api/settings/tokens", Value::Null)["status"],
        400
    );
    assert_eq!(
        request(
            &mut h,
            "DELETE",
            "/api/settings/tokens?confirm=true",
            Value::Null
        )["body"],
        json!({"ok":true,"tokens":[],"active_token_id":null,"token_set":false})
    );
    assert!(!h.vault().exists());
    assert_eq!(count(&h, "messages"), 1);
}

#[test]
fn guild_dm_and_token_discovery_preserves_public_ids_and_errors() {
    let mut h = fixture(vec![]);
    configure(
        &mut h,
        json!([{"status":200,"body":{"id":"30","username":"fallback","global_name":"Display"}},
        {"status":200,"body":[{"id":"10","name":"Fixture","icon":"abc"},{"id":"11","name":"Other","icon":null}]},
        {"status":200,"body":[{"id":"20","type":1,"recipients":[{"id":"31","username":"Bob","global_name":"Bobby"}]},
            {"id":"21","type":3,"name":"Group","recipients":[{"id":"32","username":"Cat"}]}]},
        {"status":403,"body":{}},{"status":401,"body":{}}]),
        false,
    );
    assert_eq!(
        get(&mut h, "/api/token/validate")["body"],
        json!({"valid":true,"username":"Display","id":"30"})
    );
    let guilds = get(&mut h, "/api/guilds");
    assert_eq!(guilds["status"], 200);
    assert_eq!(guilds["body"][0]["id"], "10");
    assert_eq!(
        guilds["body"][0]["icon"],
        "https://cdn.discordapp.com/icons/10/abc.png?size=64"
    );
    let dms = get(&mut h, "/api/dms");
    assert_eq!(dms["status"], 200);
    assert_eq!(dms["body"][0]["id"], "20");
    assert_eq!(dms["body"][1]["id"], "21");
    assert_eq!(
        get(&mut h, "/api/guilds")["body"]["detail"],
        "Failed to fetch guilds"
    );
    assert_eq!(
        get(&mut h, "/api/token/validate")["body"],
        json!({"valid":false})
    );
}

#[test]
fn manual_profiles_keep_full_payload_and_failed_refresh_keeps_saved_data() {
    let mut h = fixture(vec![message(1, "30", "saved")]);
    let payload = json!({"user":{"id":"30","username":"saved.alice","global_name":"Saved Alice"},
        "user_profile":{"bio":"full","pronouns":"she/her"},"unknown":{"kept":true}});
    configure(
        &mut h,
        json!([{"status":200,"body":payload},{"status":503,"body":{}},
        {"status":200,"body":{"user":{"id":"31"}}}]),
        false,
    );
    let reply = post(&mut h, "/api/profiles/30/fetch", Value::Null);
    assert_eq!(reply["status"], 200);
    assert_eq!(reply["body"]["details"], payload);
    assert_eq!(reply["body"]["message_count"], 1);
    assert_eq!(
        post(&mut h, "/api/profiles/30/refresh", Value::Null)["status"],
        503
    );
    assert_eq!(
        post(&mut h, "/api/profiles/30/refresh", Value::Null)["status"],
        502
    );
    assert_eq!(get(&mut h, "/api/profiles/30")["body"]["details"], payload);
    assert_eq!(
        get(&mut h, "/api/profiles/30/servers")["body"]["servers"][0]["id"],
        "10"
    );
    assert_eq!(
        post(&mut h, "/api/profiles/99/fetch", Value::Null)["status"],
        404
    );
    assert_eq!(get(&mut h, "/api/profiles/invalid")["status"], 400);
}

#[test]
fn profile_backfill_is_exclusive_stoppable_and_fills_missing_profiles() {
    let mut h = fixture(vec![message(1, "30", "saved"), message(2, "31", "saved")]);
    configure(
        &mut h,
        json!([{"status":200,"body":{"user":{"id":"30","username":"Alice"}}},
        {"status":200,"body":{"user":{"id":"31","username":"Bob"}}}]),
        true,
    );
    assert_eq!(
        post(&mut h, "/api/profile-backfill", Value::Null)["status"],
        200
    );
    call(&mut h, json!({"action":"wait_barrier","name":"fetch"}));
    assert_eq!(
        post(&mut h, "/api/profile-backfill", Value::Null)["status"],
        409
    );
    call(&mut h, json!({"action":"release","name":"fetch"}));
    let done = wait_http(
        &mut h,
        "/api/profile-backfill",
        json!(["running"]),
        json!(false),
    );
    assert_eq!(done["body"]["saved"], 2);
    assert_eq!(done["body"]["failed"], 0);
    assert_eq!(count(&h, "profile_details"), 2);
    assert_eq!(
        post(&mut h, "/api/profile-backfill/stop", Value::Null)["body"]["running"],
        false
    );
    let before = call(&mut h, json!({"action":"inspect"}))["requests"]
        .as_array()
        .unwrap()
        .len();
    post(&mut h, "/api/profile-backfill", Value::Null);
    wait_http(
        &mut h,
        "/api/profile-backfill",
        json!(["running"]),
        json!(false),
    );
    assert_eq!(
        call(&mut h, json!({"action":"inspect"}))["requests"]
            .as_array()
            .unwrap()
            .len(),
        before
    );
    configure(
        &mut h,
        json!([{"status":200,"body":{"user":{"id":"30","username":"refreshed"}}},
        {"status":200,"body":{"user":{"id":"31","username":"refreshed"}}}]),
        false,
    );
    h.database()
        .execute(
            "UPDATE profile_details SET fetched_at='2000-01-01T00:00:00+00:00'",
            [],
        )
        .unwrap();
    post(
        &mut h,
        "/api/profile-backfill?refresh_stale=true",
        Value::Null,
    );
    assert_eq!(
        wait_http(
            &mut h,
            "/api/profile-backfill",
            json!(["running"]),
            json!(false)
        )["body"]["saved"],
        2
    );
}

#[test]
fn invite_queues_parse_deduplicate_block_recover_and_require_original_account() {
    let mut h = fixture(vec![]);
    assert_eq!(
        get(&mut h, "/api/invites/status")["body"],
        json!({"job":null})
    );
    for invites in [
        "https://example.com/nope".to_owned(),
        (0..201)
            .map(|i| format!("code{i}"))
            .collect::<Vec<_>>()
            .join(" "),
    ] {
        assert_eq!(
            post(&mut h, "/api/invites/start", json!({"invites":invites}))["status"],
            422
        );
    }
    configure(
        &mut h,
        json!([{"status":200,"body":{"guild":{"id":"10","name":"Fixture"}}}]),
        true,
    );
    let started = post(
        &mut h,
        "/api/invites/start",
        json!({"invites":"https://discord.gg/Abc Abc;discord.com/invite/Def"}),
    );
    assert_eq!(started["status"], 200);
    let id = started["body"]["job"]["id"].as_str().unwrap().to_owned();
    assert_eq!(started["body"]["job"]["items"].as_array().unwrap().len(), 2);
    call(&mut h, json!({"action":"wait_barrier","name":"fetch"}));
    assert_eq!(
        post(&mut h, "/api/invites/start", json!({"invites":"x"}))["status"],
        409
    );
    let status = get(&mut h, "/api/invites/status");
    assert!(status["body"]["job"]["wait_seconds"].as_i64().unwrap() >= 29);
    call(&mut h, json!({"action":"suspend_all"}));
    call(&mut h, json!({"action":"recover_all"}));
    let recovered = get(&mut h, "/api/invites/status");
    assert_eq!(recovered["body"]["job"]["paused"], true);
    assert_eq!(recovered["body"]["job"]["items"][0]["status"], "uncertain");
    assert_eq!(recovered["body"]["job"]["items"][1]["status"], "pending");
    call(&mut h, json!({"action":"credentials","id":"different"}));
    assert_eq!(
        post(&mut h, &format!("/api/invites/{id}/resume"), Value::Null)["status"],
        409
    );
    call(&mut h, json!({"action":"credentials","id":"account"}));
    assert_eq!(
        post(&mut h, &format!("/api/invites/{id}/resume"), Value::Null)["status"],
        200
    );
    assert_eq!(
        post(&mut h, &format!("/api/invites/{id}/stop"), Value::Null)["body"]["job"]["running"],
        false
    );
    assert_eq!(
        post(&mut h, "/api/invites/absent/stop", Value::Null)["status"],
        404
    );
}

#[test]
fn invite_account_errors_block_instead_of_retrying() {
    let mut h = fixture(vec![]);
    configure(
        &mut h,
        json!([{"status":403,"body":{"code":340015}}]),
        false,
    );
    post(&mut h, "/api/invites/start", json!({"invites":"code"}));
    let status = wait_http(
        &mut h,
        "/api/invites/status",
        json!(["job", "running"]),
        json!(false),
    );
    assert_eq!(status["body"]["job"]["status"], "blocked");
    assert_eq!(status["body"]["job"]["items"][0]["discord_code"], 340015);
    assert_eq!(
        call(&mut h, json!({"action":"inspect"}))["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn chatml_groups_turns_ignores_blank_messages_and_substitutes_names() {
    let mut h = fixture(vec![
        message(1, "30", "orphan target"),
        message(2, "31", " hi "),
        message(3, "31", "there"),
        message(4, "30", " answer "),
        message(5, "30", "again"),
        message(6, "31", "   "),
        message(7, "31", "next"),
        message(8, "30", "final"),
    ]);
    let participants = get(&mut h, "/api/export/participants?channel_id=20");
    assert_eq!(
        participants["body"],
        json!([{"author_id":"30","author_name":"Alice"},{"author_id":"31","author_name":"Bob"}])
    );
    let reply = post(
        &mut h,
        "/api/export/chatml",
        json!({"channel_id":"20","target_id":"30","system_prompt":"Be {target}, reply to {other}"}),
    );
    assert_eq!(reply["status"], 200);
    assert!(
        reply["headers"]["content-type"]
            .as_str()
            .unwrap()
            .starts_with("application/x-ndjson")
    );
    assert!(
        reply["headers"]["content-disposition"]
            .as_str()
            .unwrap()
            .contains("chatml_20.jsonl")
    );
    let lines: Vec<Value> = reply["body"]
        .as_str()
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(lines,json!([
        {"messages":[{"role":"system","content":"Be Alice, reply to Bob"},{"role":"user","content":"Bob: hi\nthere"},{"role":"assistant","content":"answer\nagain"}]},
        {"messages":[{"role":"system","content":"Be Alice, reply to Bob"},{"role":"user","content":"Bob: next"},{"role":"assistant","content":"final"}]}]).as_array().unwrap().clone());
    assert_eq!(
        post(
            &mut h,
            "/api/export/chatml",
            json!({"channel_id":"20","target_id":"30","system_prompt":"{unknown}"})
        )["status"],
        400
    );
    assert_eq!(
        post(
            &mut h,
            "/api/export/chatml",
            json!({"channel_id":"20","target_id":"99"})
        )["status"],
        404
    );
}

#[test]
fn counted_search_statistics_rankings_and_filters_keep_response_shapes() {
    let mut h = fixture(vec![
        message(1, "30", "literal 100%_"),
        message(2, "31", "common"),
        message(3, "30", "common"),
        message(4, "31", "common"),
    ]);
    let page = get(&mut h, "/api/search?limit=2&page=2");
    assert_eq!(page["body"]["total"], 4);
    assert_eq!(page["body"]["pages"], 2);
    assert_eq!(page["body"]["messages"][0]["id"], (BASE + 2).to_string());
    assert_eq!(get(&mut h, "/api/search?q=100%25_")["body"]["total"], 1);
    let cursor = get(&mut h, "/api/search?cursor=true&limit=2");
    assert_eq!(cursor["body"]["total"], Value::Null);
    assert_eq!(cursor["body"]["next_cursor"], (BASE + 3).to_string());
    let stats = get(&mut h, "/api/stats?days=0");
    for (key, value) in [
        ("total_messages", 4),
        ("total_users", 2),
        ("total_servers", 1),
        ("total_channels", 1),
    ] {
        assert_eq!(stats["body"][key], value);
    }
    assert_eq!(stats["body"]["top_users"][0]["author_id"], "30");
    assert_eq!(stats["body"]["top_users"][1]["author_id"], "31");
    assert_eq!(
        stats["body"]["messages_by_weekday"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert_eq!(
        stats["body"]["messages_by_month"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["count"].as_i64().unwrap())
            .sum::<i64>(),
        4
    );
    let contributors = get(&mut h, "/api/stats/contributors?limit=1");
    assert_eq!(
        contributors["body"],
        json!({"users":[{"author_id":"30","author_name":"Alice","count":2}],"has_more":true,"offset":0,"limit":1})
    );
    assert_eq!(
        get(&mut h, "/api/stats/contributors?q=Bob")["body"]["users"][0]["author_id"],
        "31"
    );
    assert_eq!(
        get(&mut h, "/api/stats/servers")["body"]["servers"],
        json!([{"guild_id":"10","guild_name":"Fixture","count":4}])
    );
    let filters = get(&mut h, "/api/search/filters?include_users=false");
    assert_eq!(filters["body"]["users"], json!([]));
    assert_eq!(filters["body"]["total_messages"], 4);
    assert_eq!(
        get(&mut h, "/api/search/authors?q=Al")["body"],
        json!([{"author_id":"30","author_name":"Alice"}])
    );
}

#[test]
fn message_refresh_distinguishes_generic_absence_confirmed_deletion_and_restoration() {
    let original = message(1, "30", "original");
    let mut h = fixture(vec![original.clone()]);
    let path = format!("/api/messages/{}/refresh", BASE + 1);
    configure(
        &mut h,
        json!([{"status":404,"body":{}},{"status":404,"body":{"code":10008}},
        {"status":403,"body":{}},{"status":404,"body":{"code":10008}},{"status":200,"body":{"id":"20"}},
        {"status":200,"body":message(1,"30","restored")}]),
        false,
    );
    assert_eq!(post(&mut h, &path, Value::Null)["status"], 404);
    assert_eq!(count(&h, "message_revisions"), 1);
    assert_eq!(post(&mut h, &path, Value::Null)["status"], 409);
    assert_eq!(count(&h, "message_revisions"), 1);
    let deleted = post(&mut h, &path, Value::Null);
    assert_eq!(deleted["status"], 200);
    assert!(deleted["body"]["deleted_at"].is_string());
    let restored = post(&mut h, &path, Value::Null);
    assert_eq!(restored["body"]["deleted_at"], Value::Null);
    assert_eq!(restored["body"]["payload"]["content"], "restored");
    assert_eq!(
        get(
            &mut h,
            &format!("/api/messages/{}/archive?limit=1", BASE + 1)
        )["body"]["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(get(&mut h, "/api/messages/99/archive")["status"], 404);
    assert_eq!(
        post(&mut h, "/api/messages/0/refresh", Value::Null)["status"],
        400
    );
}

#[test]
fn image_refresh_uses_matching_attachment_path_and_preserves_redirects() {
    let mut msg = message(1, "30", "image");
    msg["attachments"] = json!([{"id":"40","filename":"image.png","content_type":"image/png","url":"https://cdn.discordapp.com/attachments/20/40/image.png?ex=0"}]);
    let mut h = fixture(vec![msg.clone()]);
    call(&mut h, json!({"action":"patch_media_fetch"}));
    msg["attachments"][0]["url"] =
        json!("https://cdn.discordapp.com/attachments/20/40/image.png?ex=ffffffff");
    configure(&mut h, json!([{"status":200,"body":msg}]), false);
    let response = get(&mut h, &format!("/api/images/{}/0", BASE + 1));
    assert_eq!(response["status"], 302);
    assert_eq!(
        response["headers"]["location"],
        "https://cdn.discordapp.com/attachments/20/40/image.png?ex=ffffffff"
    );
    assert_eq!(
        get(&mut h, &format!("/api/images/{}/1", BASE + 1))["status"],
        404
    );
    assert_eq!(get(&mut h, "/api/images/99/0")["status"], 404);
}

#[test]
fn archive_clear_keeps_credentials_and_reports_partial_media_cleanup() {
    let mut h = fixture(vec![message(1, "30", "archive")]);
    post(&mut h, "/api/settings", json!({"token":"synthetic-keep"}));
    let vault = fs::read(h.vault()).unwrap();
    assert_eq!(
        request(&mut h, "DELETE", "/api/messages", Value::Null)["status"],
        400
    );
    call(&mut h, json!({"action":"clear_media_fault"}));
    let reply = request(&mut h, "DELETE", "/api/messages?confirm=true", Value::Null);
    assert_eq!(reply["status"], 503);
    assert_eq!(
        reply["body"]["detail"],
        "Archive rows cleared, but cached media could not be removed; retry clear to finish"
    );
    for table in [
        "messages",
        "message_payloads",
        "message_revisions",
        "profiles",
        "authors",
        "channels",
        "guilds",
    ] {
        assert_eq!(count(&h, table), 0);
    }
    assert_eq!(fs::read(h.vault()).unwrap(), vault);
    assert_eq!(get(&mut h, "/api/settings")["body"]["token_set"], true);
}

#[test]
fn dm_cleanup_only_deletes_current_account_and_counts_confirmed_missing_separately() {
    let mut h = fixture(vec![
        message(1, "30", "mine"),
        message(2, "30", "missing"),
        message(3, "31", "other"),
    ]);
    configure(
        &mut h,
        json!([{"status":200,"body":{"id":"30"}},
        {"status":200,"body":[message(3,"31","other"),message(2,"30","missing"),message(1,"30","mine")]},
        {"status":404,"body":{"code":10008}},{"status":204}]),
        false,
    );
    let reply = post(&mut h, "/api/dm-clear/start", json!({"channel_id":"20"}));
    assert_eq!(reply["status"], 200);
    let id = reply["body"]["job_id"].as_str().unwrap();
    let events = call(&mut h, json!({"action":"drain_dm","id":id,"retain":true}));
    assert_eq!(
        events.as_array().unwrap().last().unwrap(),
        &json!({"type":"complete","deleted":1,"already_missing":1,"failed":0,"scanned":3})
    );
    let trace = call(&mut h, json!({"action":"inspect"}));
    assert_eq!(trace["requests"].as_array().unwrap().len(), 4);
    assert_eq!(
        trace["requests"][2]["path"],
        format!("/api/v10/channels/20/messages/{}", BASE + 2)
    );
    let tombstones: i64 = h
        .database()
        .query_row(
            "SELECT COUNT(*) FROM message_payloads WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tombstones, 2);
    assert_eq!(
        post(&mut h, &format!("/api/dm-clear/{id}/stop"), Value::Null)["status"],
        200
    );
    let sse = get(&mut h, &format!("/api/dm-clear/progress/{id}"));
    let frames = parse_events(sse["body"].as_str().unwrap());
    assert_eq!(
        serde_json::from_str::<Value>(&frames.last().unwrap().data).unwrap(),
        events.as_array().unwrap().last().unwrap().clone()
    );
    assert_eq!(get(&mut h, "/api/dm-clear/progress/absent")["status"], 404);
}

#[test]
fn live_monitor_commits_observations_recovers_paused_and_enforces_account() {
    let mut h = fixture(vec![]);
    call(
        &mut h,
        json!({"action":"options","values":{"LIVE_POLL_SECONDS":0.01}}),
    );
    configure(
        &mut h,
        json!([{"status":200,"body":[]},{"status":200,"body":[message(1,"30","live")]}]),
        false,
    );
    assert_eq!(
        post(&mut h, "/api/live/start", json!({"channels":[channel()]}))["body"],
        json!({"active":["20"]})
    );
    call(
        &mut h,
        json!({"action":"wait_database","sql":"SELECT COUNT(*) FROM messages","value":1}),
    );
    let raw: String = h
        .database()
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE kind='monitor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let state: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(state["last_id"], (BASE + 1).to_string());
    call(&mut h, json!({"action":"suspend_all"}));
    call(&mut h, json!({"action":"recover_all"}));
    assert_eq!(
        get(&mut h, "/api/live/status")["body"]["channels"][0]["paused"],
        true
    );
    h.start_collector();
    let initial = h.event_prefix("/api/live/events", 1);
    let event: Value = serde_json::from_str(&initial[0].data).unwrap();
    assert_eq!(
        event,
        json!({"type":"monitor_paused","channel_id":"20","channel":"fixture","guild":"Fixture","guild_id":"10"})
    );
    call(&mut h, json!({"action":"credentials","id":"different"}));
    assert_eq!(
        post(&mut h, "/api/live/start", json!({"channels":[channel()]}))["status"],
        409
    );
    assert_eq!(
        post(&mut h, "/api/live/stop", json!({}))["body"],
        json!({"stopped":["20"]})
    );
    assert_eq!(
        get(&mut h, "/api/live/status")["body"],
        json!({"channels":[]})
    );
    let monitors: i64 = h
        .database()
        .query_row(
            "SELECT COUNT(*) FROM collection_jobs WHERE kind='monitor'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(monitors, 0);
}

#[test]
fn invite_successes_obey_spacing_and_rate_limit_retries() {
    let mut h = fixture(vec![]);
    call(&mut h, json!({"action":"invite_clock"}));
    configure(
        &mut h,
        json!([{"status":200,"body":{"guild":{"id":"10","name":"First"}}},{"status":200,"body":{}},
        {"status":429,"body":{"retry_after":45}},{"status":200,"body":{"guild":{"id":"11","name":"Second"}}},
        {"status":204}]),
        false,
    );
    post(
        &mut h,
        "/api/invites/start",
        json!({"invites":"First Second"}),
    );
    let done = wait_http(
        &mut h,
        "/api/invites/status",
        json!(["job", "running"]),
        json!(false),
    );
    assert_eq!(done["body"]["job"]["status"], "complete");
    assert_eq!(done["body"]["job"]["items"][0]["status"], "joined");
    assert_eq!(done["body"]["job"]["items"][1]["status"], "joined");
    assert_eq!(done["body"]["job"]["items"][1]["attempts"], 1);
    let waits = call(&mut h, json!({"action":"invite_waits"}));
    assert_eq!(waits, json!([30.0, 45.0]));
    let trace = call(&mut h, json!({"action":"inspect"}));
    assert_eq!(trace["requests"].as_array().unwrap().len(), 5);
    let first = trace["requests"][1]["invite_time"].as_f64().unwrap();
    let second = trace["requests"][4]["invite_time"].as_f64().unwrap();
    assert!(second - first >= 75.0);
}

#[test]
fn stalled_scrape_subscriptions_are_bounded_and_snapshot_copies_do_not_mutate_jobs() {
    let mut h = fixture(vec![]);
    configure(&mut h, json!([]), true);
    let started = post(&mut h, "/api/scrape/start", json!({"channels":[channel()]}));
    let id = started["body"]["job_id"].clone();
    call(&mut h, json!({"action":"wait_barrier","name":"fetch"}));
    let mut snapshot = call(&mut h, json!({"action":"subscribe","id":id}));
    snapshot["channels"][0]["id"] = json!("mutated");
    let notices: Vec<_> = (0..100)
        .map(|i| json!({"type":"retry","sequence":i}))
        .collect();
    call(&mut h, json!({"action":"report","id":id,"events":notices}));
    let queue = call(&mut h, json!({"action":"subscription_read","id":id}));
    assert_eq!(queue["capacity"], 20);
    assert_eq!(queue["events"].as_array().unwrap().len(), 20);
    assert_eq!(queue["events"][0]["sequence"], 80);
    assert_eq!(queue["events"][19]["sequence"], 99);
    let current = call(&mut h, json!({"action":"snapshot","id":id}));
    assert_eq!(current["channels"][0]["id"], "20");
    assert_eq!(current["total_messages"], 0);
    let active = get(&mut h, "/api/scrape/active");
    assert_eq!(active["body"][0]["job_id"], id);
    assert_eq!(
        post(
            &mut h,
            &format!("/api/scrape/{}/pause", id.as_str().unwrap()),
            Value::Null
        )["status"],
        200
    );
    call(&mut h, json!({"action":"control","method":"stop","id":id}));
    let queue = call(&mut h, json!({"action":"subscription_read","id":id}));
    assert_eq!(
        queue["events"].as_array().unwrap().last().unwrap()["type"],
        "cancelled"
    );
}

// Every explicit collector route has a named behavioral owner, including
// framework routes and routers included by FastAPI. Keep the ledger in sync.
const ROUTE_OWNERS: &[(&str, &str, &str)] = &[
    (
        "GET,HEAD",
        "/openapi.json",
        "framework_validation_documentation_health_and_startup_shutdown_are_owned",
    ),
    (
        "GET,HEAD",
        "/docs",
        "framework_validation_documentation_health_and_startup_shutdown_are_owned",
    ),
    (
        "GET,HEAD",
        "/docs/oauth2-redirect",
        "framework_validation_documentation_health_and_startup_shutdown_are_owned",
    ),
    (
        "GET,HEAD",
        "/redoc",
        "framework_validation_documentation_health_and_startup_shutdown_are_owned",
    ),
    (
        "GET",
        "/api/health",
        "framework_validation_documentation_health_and_startup_shutdown_are_owned",
    ),
    (
        "GET",
        "/api/token/validate",
        "guild_dm_and_token_discovery_preserves_public_ids_and_errors",
    ),
    (
        "GET",
        "/api/guilds",
        "guild_dm_and_token_discovery_preserves_public_ids_and_errors",
    ),
    (
        "GET",
        "/api/guilds/{guild_id}/channels",
        "collection_access::readable_channels_contracts",
    ),
    (
        "GET",
        "/api/dms",
        "guild_dm_and_token_discovery_preserves_public_ids_and_errors",
    ),
    (
        "POST",
        "/api/scrape/start",
        "durable_scrape::simultaneous_start_reserves_channels_before_checkpoint_write",
    ),
    (
        "GET",
        "/api/scrape/active",
        "stalled_scrape_subscriptions_are_bounded_and_snapshot_copies_do_not_mutate_jobs",
    ),
    (
        "POST",
        "/api/scrape/{job_id}/pause",
        "stalled_scrape_subscriptions_are_bounded_and_snapshot_copies_do_not_mutate_jobs",
    ),
    (
        "POST",
        "/api/scrape/{job_id}/resume",
        "durable_scrape::resume_cannot_launch_worker_after_shutdown_barrier",
    ),
    (
        "POST",
        "/api/scrape/{job_id}/stop",
        "durable_scrape::stop_joins_inflight_commit_before_archive_clear",
    ),
    (
        "GET",
        "/api/scrape/progress/{job_id}",
        "scrape_sse_reconnect_starts_with_snapshot_and_repeats_terminal_event",
    ),
    (
        "POST",
        "/api/dm-clear/start",
        "dm_cleanup_only_deletes_current_account_and_counts_confirmed_missing_separately",
    ),
    (
        "POST",
        "/api/dm-clear/{job_id}/stop",
        "dm_cleanup_only_deletes_current_account_and_counts_confirmed_missing_separately",
    ),
    (
        "GET",
        "/api/dm-clear/progress/{job_id}",
        "dm_cleanup_only_deletes_current_account_and_counts_confirmed_missing_separately",
    ),
    (
        "POST",
        "/api/live/start",
        "live_monitor_commits_observations_recovers_paused_and_enforces_account",
    ),
    (
        "POST",
        "/api/live/stop",
        "live_monitor_commits_observations_recovers_paused_and_enforces_account",
    ),
    (
        "GET",
        "/api/live/status",
        "live_monitor_commits_observations_recovers_paused_and_enforces_account",
    ),
    (
        "GET",
        "/api/live/events",
        "live_monitor_commits_observations_recovers_paused_and_enforces_account",
    ),
    (
        "GET",
        "/api/export/participants",
        "chatml_groups_turns_ignores_blank_messages_and_substitutes_names",
    ),
    (
        "POST",
        "/api/export/chatml",
        "chatml_groups_turns_ignores_blank_messages_and_substitutes_names",
    ),
    (
        "GET",
        "/api/search",
        "counted_search_statistics_rankings_and_filters_keep_response_shapes",
    ),
    (
        "GET",
        "/api/images/{message_id}/{image_index}",
        "image_refresh_uses_matching_attachment_path_and_preserves_redirects",
    ),
    (
        "GET",
        "/api/stats",
        "counted_search_statistics_rankings_and_filters_keep_response_shapes",
    ),
    (
        "GET",
        "/api/stats/contributors",
        "counted_search_statistics_rankings_and_filters_keep_response_shapes",
    ),
    (
        "GET",
        "/api/stats/servers",
        "counted_search_statistics_rankings_and_filters_keep_response_shapes",
    ),
    (
        "GET",
        "/api/search/filters",
        "counted_search_statistics_rankings_and_filters_keep_response_shapes",
    ),
    (
        "GET",
        "/api/search/authors",
        "counted_search_statistics_rankings_and_filters_keep_response_shapes",
    ),
    (
        "GET",
        "/api/settings",
        "settings_sealed_vault_metadata_validation_switching_and_removal",
    ),
    (
        "POST",
        "/api/settings",
        "settings_sealed_vault_metadata_validation_switching_and_removal",
    ),
    (
        "DELETE",
        "/api/settings/tokens",
        "settings_sealed_vault_metadata_validation_switching_and_removal",
    ),
    (
        "DELETE",
        "/api/messages",
        "archive_clear_keeps_credentials_and_reports_partial_media_cleanup",
    ),
    (
        "GET",
        "/api/messages/{message_id}/archive",
        "message_refresh_distinguishes_generic_absence_confirmed_deletion_and_restoration",
    ),
    (
        "POST",
        "/api/messages/{message_id}/refresh",
        "message_refresh_distinguishes_generic_absence_confirmed_deletion_and_restoration",
    ),
    (
        "GET",
        "/api/profiles/{uid}",
        "manual_profiles_keep_full_payload_and_failed_refresh_keeps_saved_data",
    ),
    (
        "GET",
        "/api/profiles/{uid}/servers",
        "manual_profiles_keep_full_payload_and_failed_refresh_keeps_saved_data",
    ),
    (
        "POST",
        "/api/profiles/{uid}/fetch",
        "manual_profiles_keep_full_payload_and_failed_refresh_keeps_saved_data",
    ),
    (
        "POST",
        "/api/profiles/{uid}/refresh",
        "manual_profiles_keep_full_payload_and_failed_refresh_keeps_saved_data",
    ),
    (
        "GET",
        "/api/profile-backfill",
        "profile_backfill_is_exclusive_stoppable_and_fills_missing_profiles",
    ),
    (
        "POST",
        "/api/profile-backfill",
        "profile_backfill_is_exclusive_stoppable_and_fills_missing_profiles",
    ),
    (
        "POST",
        "/api/profile-backfill/stop",
        "stopping_profile_backfill_during_fetch_preserves_saved_profiles",
    ),
    (
        "GET",
        "/api/media/{key}",
        "packed_snapshot::referenced_media_export_verifies_hashes_and_excludes_unreferenced_files",
    ),
    (
        "POST",
        "/api/invites/start",
        "invite_queues_parse_deduplicate_block_recover_and_require_original_account",
    ),
    (
        "GET",
        "/api/invites/status",
        "invite_successes_obey_spacing_and_rate_limit_retries",
    ),
    (
        "POST",
        "/api/invites/{job_id}/stop",
        "invite_queues_parse_deduplicate_block_recover_and_require_original_account",
    ),
    (
        "POST",
        "/api/invites/{job_id}/resume",
        "invite_queues_parse_deduplicate_block_recover_and_require_original_account",
    ),
];

#[test]
fn every_collector_route_has_a_behavioral_owner_in_the_ledger() {
    use std::{collections::BTreeSet, path::Path};
    let mut h = fixture(vec![]);
    let inventory = call(&mut h, json!({"action":"route_inventory"}));
    let mut actual = BTreeSet::new();
    for route in inventory.as_array().unwrap() {
        for method in route["methods"].as_array().unwrap() {
            assert!(
                actual.insert((
                    route["path"].as_str().unwrap().to_owned(),
                    method.as_str().unwrap().to_owned()
                )),
                "duplicate route: {route}"
            );
        }
    }
    let mut expected = BTreeSet::new();
    let ledger =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../PARITY_LEDGER.md"))
            .unwrap();
    for (methods, path, owner) in ROUTE_OWNERS {
        for method in methods.split(',') {
            expected.insert((path.to_string(), method.to_owned()));
        }
        assert!(
            ledger.contains(&format!("| `{methods}` | `{path}` | `{owner}` |")),
            "missing ledger route: {methods} {path}"
        );
    }
    assert_eq!(actual, expected);
}

#[test]
fn live_gap_pages_drain_to_anchor_and_generic_404_cannot_create_tombstones() {
    let mut h = fixture(vec![message(1, "30", "anchor")]);
    let state = json!({"channel_name":"fixture","guild_id":"10","guild_name":"Fixture","credential_id":"account",
        "running":true,"paused":true,"last_id":(BASE+1).to_string(),"pending_before":null,"pending_anchor":null,"pending_newest":null});
    h.database()
        .execute(
            "INSERT INTO collection_jobs VALUES ('20','monitor',?,0)",
            [state.to_string()],
        )
        .unwrap();
    call(&mut h, json!({"action":"recover_all"}));
    call(
        &mut h,
        json!({"action":"options","values":{"LIVE_POLL_SECONDS":0.01}}),
    );
    let first: Vec<_> = (52..=101)
        .rev()
        .map(|n| message(n, "30", "newest"))
        .collect();
    let second: Vec<_> = (2..=51).rev().map(|n| message(n, "30", "gap")).collect();
    configure(
        &mut h,
        json!([{"status":200,"body":first},{"status":200,"body":second},
        {"status":200,"body":[message(1,"30","anchor")]},{"status":200,"body":[]},{"status":404,"body":{}}]),
        false,
    );
    post(&mut h, "/api/live/start", json!({"channels":[channel()]}));
    call(
        &mut h,
        json!({"action":"wait_database","sql":"SELECT json_extract(state_json,'$.last_id') FROM collection_jobs WHERE kind='monitor'","value":(BASE+101).to_string()}),
    );
    assert_eq!(count(&h, "messages"), 101);
    let trace = call(&mut h, json!({"action":"wait_requests","count":5}));
    assert_eq!(trace[0]["params"], json!({"limit":"50"}));
    assert_eq!(
        trace[1]["params"],
        json!({"limit":"50","before":(BASE+52).to_string()})
    );
    assert_eq!(
        trace[2]["params"],
        json!({"limit":"50","before":(BASE+2).to_string()})
    );
    let missing: i64 = h
        .database()
        .query_row(
            "SELECT COUNT(*) FROM message_payloads WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(missing, 0);
    post(&mut h, "/api/live/stop", json!({}));
}

#[test]
fn stopping_profile_backfill_during_fetch_preserves_saved_profiles() {
    let mut h = fixture(vec![message(1, "30", "saved"), message(2, "31", "missing")]);
    let payload = json!({"user":{"id":"30","username":"keep"},"unknown":"saved"});
    h.database()
        .execute(
            "INSERT INTO profile_details VALUES ('30',?,'2026-01-01')",
            [payload.to_string()],
        )
        .unwrap();
    configure(
        &mut h,
        json!([{"status":200,"body":{"user":{"id":"31","username":"pending"}}}]),
        true,
    );
    post(&mut h, "/api/profile-backfill", Value::Null);
    call(&mut h, json!({"action":"wait_barrier","name":"fetch"}));
    let stopped = post(&mut h, "/api/profile-backfill/stop", Value::Null);
    assert_eq!(stopped["body"]["running"], false);
    assert_eq!(stopped["body"]["saved"], 0);
    assert_eq!(count(&h, "profile_details"), 1);
    let raw: String = h
        .database()
        .query_row(
            "SELECT payload FROM profile_details WHERE user_id='30'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), payload);
}

#[test]
fn scrape_sse_reconnect_starts_with_snapshot_and_repeats_terminal_event() {
    let mut h = fixture(vec![]);
    configure(&mut h, json!([{"status":200,"body":[]}]), false);
    let start = post(&mut h, "/api/scrape/start", json!({"channels":[channel()]}));
    let id = start["body"]["job_id"].as_str().unwrap();
    call(
        &mut h,
        json!({"action":"wait_snapshot","id":id,"fields":{"running":false}}),
    );
    for _ in 0..2 {
        let reply = get(&mut h, &format!("/api/scrape/progress/{id}"));
        assert_eq!(reply["status"], 200);
        assert!(
            reply["headers"]["content-type"]
                .as_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        let events = parse_events(reply["body"].as_str().unwrap());
        assert_eq!(events.len(), 2);
        let first: Value = serde_json::from_str(&events[0].data).unwrap();
        let last: Value = serde_json::from_str(&events[1].data).unwrap();
        assert_eq!(first["type"], "snapshot");
        assert_eq!(first["running"], false);
        assert_eq!(last["type"], "complete");
        assert_eq!(last["total_messages"], 0);
    }
}

#[test]
fn framework_validation_documentation_health_and_startup_shutdown_are_owned() {
    let mut h = fixture(vec![]);
    for (path, status) in [
        ("/api/search?limit=0", 422),
        ("/api/search?page=0", 422),
        ("/api/search?before=0", 422),
        ("/api/stats?days=-1", 422),
        ("/api/stats/contributors?offset=-1", 422),
        ("/api/stats/servers?limit=101", 422),
        ("/api/profiles/30/servers?limit=0", 422),
        ("/api/messages/1/archive?limit=101", 422),
        ("/api/messages/0/archive", 400),
        ("/api/scrape/progress/absent", 404),
    ] {
        assert_eq!(get(&mut h, path)["status"], status, "{path}");
    }
    assert_eq!(
        post(&mut h, "/api/dm-clear/start", json!({"channel_id":"abc"}))["status"],
        422
    );
    assert_eq!(
        post(&mut h, "/api/scrape/start", json!({"channels":[]}))["status"],
        422
    );
    for path in [
        "/docs",
        "/redoc",
        "/openapi.json",
        "/docs/oauth2-redirect",
        "/",
    ] {
        assert_eq!(get(&mut h, path)["status"], 200, "{path}");
    }
    assert_eq!(post(&mut h, "/api/stats", Value::Null)["status"], 405);
    assert_eq!(get(&mut h, "/api/stats/")["status"], 404);
    assert_eq!(get(&mut h, "/api/health")["status"], 200);
    let lifecycle = call(&mut h, json!({"action":"lifespan"}));
    assert_eq!(
        lifecycle,
        json!({"during":{"ready":true,"client":true},"after":{"ready":false,"client":false,"has_work":false}})
    );
}
