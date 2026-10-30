//! Rust-owned display fixtures and assertions; requests cross a real HTTP socket.
use rusqlite::{Connection, params};
use searchcord_parity::Harness;
use serde_json::{Value, json};
use std::{collections::HashSet, fs};

// 2026-09-01T00:00:00Z, expressed as a public Discord snowflake.
const BASE: i64 = (1_788_220_800_000_i64 - 1_420_070_400_000) << 22;
const DAY: i64 = 86_400_000_i64 << 22;

fn id(index: i64) -> String {
    (BASE + index * DAY).to_string()
}

fn content(index: i64) -> String {
    match index {
        2 => "rare phrase <script>alert(1)</script>".into(),
        4 => "literal 100%_value and wow!".into(),
        6 => "literal 100xxvalue".into(),
        8 => "MiXeD Héllo unicode".into(),
        10 => "x".repeat(4000),
        i if i % 3 == 0 => "different text".into(),
        _ => "hello common".into(),
    }
}

fn saved_profile() -> Value {
    json!({"user":{"id":"50","username":"alex.saved","global_name":"Alex","avatar":"a_abc123"},
        "user_profile":{"bio":"Saved bio","pronouns":"they/them","banner":"banner123"},
        "connected_accounts":[{"type":"github","name":"alex"}]})
}

fn fixture() -> Harness {
    let mut h = Harness::new().unwrap();
    h.init();
    {
        let mut db = h.database();
        let tx = db.transaction().unwrap();
        tx.execute_batch(
            "INSERT INTO guilds VALUES ('10','Server Alpha'),('20','Server Beta');
            INSERT INTO channels VALUES ('30','10','📢┃general-chat'),('40','20','🌿 general');
            INSERT INTO authors(id,name) VALUES ('50','Alex'),('60','Alina');
            INSERT INTO profiles(user_id,username,avatar_hash,fetched_at) VALUES
                ('50','Alex','a_abc123','2026-09-01'),('60','Alina','../invalid','2026-09-01');",
        )
        .unwrap();
        tx.execute(
            "INSERT INTO profile_details VALUES (?,?,?)",
            params!["50", saved_profile().to_string(), "2026-09-01"],
        )
        .unwrap();
        // Later internal rowids deliberately contain older public message IDs.
        for i in (40..80).chain(0..40) {
            tx.execute("INSERT INTO messages(id,channel_id,guild_id,author_id,content,image_urls) VALUES (?,?,?,?,?,?)",
                params![BASE+i*DAY,if i%2==1 {30} else {40},if i%2==1 {10} else {20},
                    if i%2==1 {50} else {60},content(i),
                    if i==2 {"https://cdn.discordapp.com/attachments/test.png\njavascript:alert(1)"} else {""}]).unwrap();
        }
        tx.execute(
            "INSERT INTO name_values(id,value) VALUES (1,'Historical name')",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO message_history(rowid,author_name_id,timestamp_override,timestamp)
            SELECT rowid,1,1,? FROM messages WHERE id=?",
            params!["2026-09-03T00:00:00+00:00", BASE + 2 * DAY],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    h
}

fn viewer(window: Option<u64>) -> Harness {
    let mut h = fixture();
    h.start_viewer("synthetic.db", true, window);
    h
}

fn get(h: &mut Harness, path: &str, params: &[(&str, &str)]) -> Value {
    let params: Vec<_> = params.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    let response = h.get(path, &params);
    assert_eq!(response.status, 200, "{path}: {}", response.body);
    response.json()
}

fn search(h: &mut Harness, params: &[(&str, &str)]) -> Value {
    get(h, "/api/search", params)
}

fn ids(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            row["id"]
                .as_str()
                .expect("public IDs stay strings")
                .to_owned()
        })
        .collect()
}

/// Traverse even empty partial pages; a non-advancing cursor fails immediately.
fn traverse(h: &mut Harness, query: Option<&str>, limit: &str, scan: bool) -> (Vec<String>, bool) {
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut empty_partial = false;
    for _ in 0..100 {
        let mut params = vec![
            ("limit", limit),
            ("scan", if scan { "true" } else { "false" }),
        ];
        if let Some(q) = query {
            params.push(("q", q));
        }
        if let Some(c) = &cursor {
            params.push(("before", c));
        }
        let result = search(h, &params);
        let page = ids(&result["messages"]);
        assert!(page.len() <= limit.parse::<usize>().unwrap());
        empty_partial |= page.is_empty() && result["partial"] == true;
        assert!(result["scan"].is_boolean());
        assert!(result["elapsed_ms"].is_number());
        all.extend(page);
        if result["has_more"] == false {
            assert_eq!(result["next_cursor"], Value::Null);
            return (all, empty_partial);
        }
        let next = result["next_cursor"]
            .as_str()
            .expect("continuation must have a public cursor")
            .to_owned();
        assert!(cursors.insert(next.clone()), "repeated cursor: {next}");
        if let Some(previous) = &cursor {
            assert!(next.parse::<i64>().unwrap() < previous.parse::<i64>().unwrap());
        }
        cursor = Some(next);
    }
    panic!("cursor traversal did not terminate");
}

#[test]
fn summary_and_readonly_surface() {
    let mut h = viewer(None);
    let before = fs::read(h.archive()).unwrap();
    for path in ["/privacy.html", "/bauhaus.css"] {
        assert_eq!(h.get(path, &[]).status, 200);
    }
    assert_eq!(
        get(&mut h, "/api/summary", &[]),
        json!({"messages":80,"servers":2})
    );
    for path in [
        "/api/settings",
        "/api/token",
        "/api/scrape",
        "/api/export",
        "/app.js",
        "/../app.py",
    ] {
        assert_eq!(h.get(path, &[]).status, 404, "{path}");
    }
    assert_eq!(h.request("POST", "/api/search", &[]).status, 405);
    let response = h.get("/api/search", &[]);
    assert_eq!(response.headers["cache-control"], "no-store");
    assert_eq!(response.headers["x-content-type-options"], "nosniff");
    let result = h.exchange(json!({"op":"readonly_execute","sql":"DELETE FROM messages"}));
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["type"], "OperationalError");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("readonly")
    );
    assert_eq!(fs::read(h.archive()).unwrap(), before);
    assert!(!h.vault().exists());
    for suffix in ["-wal", "-shm"] {
        assert!(
            !h.archive()
                .with_file_name(format!("synthetic.db{suffix}"))
                .exists()
        );
    }
}

#[test]
fn cursor_pages_cover_archive_without_duplicates() {
    let mut h = viewer(None);
    let (actual, _) = traverse(&mut h, None, "7", false);
    assert_eq!(actual.len(), 80);
    assert_eq!(actual.iter().collect::<HashSet<_>>().len(), 80);
    assert_eq!(actual, (0..80).rev().map(id).collect::<Vec<_>>());
    assert_eq!(search(&mut h, &[("before", &id(0))])["messages"], json!([]));
}

#[test]
fn adaptive_query_matches_reference_all_pages() {
    let mut h = viewer(Some(8));
    for query in [
        "hello",
        "rare phrase",
        "100%_value",
        "wow!",
        "MiXeD",
        "Héllo",
        "not present",
        "he",
        "h",
        "%",
        "%%%",
        "_",
        "!",
    ] {
        // SQLite defines LIKE semantics; a second independent fixture expectation
        // prevents an accidental empty database from making this comparison pass.
        let literal = format!(
            "%{}%",
            query
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_")
        );
        let expected = h
            .database()
            .prepare("SELECT id FROM messages WHERE content LIKE ? ESCAPE '!' ORDER BY id DESC")
            .unwrap()
            .query_map([literal], |r| Ok(r.get::<_, i64>(0)?.to_string()))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        let known = (0..80)
            .rev()
            .filter(|&i| match query {
                "hello" | "he" => content(i) == "hello common",
                "h" => content(i) == "hello common" || i == 2 || i == 8,
                "rare phrase" => i == 2,
                "100%_value" | "wow!" | "%" | "_" | "!" => i == 4,
                "MiXeD" | "Héllo" => i == 8,
                "not present" | "%%%" => false,
                _ => unreachable!(),
            })
            .map(id)
            .collect::<Vec<_>>();
        assert_eq!(
            expected, known,
            "independent fixture expectation for {query}"
        );
        for scan in [false, true] {
            let (actual, empty_partial) = traverse(&mut h, Some(query), "5", scan);
            assert_eq!(actual, expected, "query={query}, scan={scan}");
            if scan && query == "rare phrase" {
                assert!(empty_partial, "rare match must cross empty scan windows");
            }
        }
    }
}

#[test]
fn filters_and_inclusive_dates() {
    let mut h = viewer(None);
    let result = search(
        &mut h,
        &[
            ("guild_id", "20"),
            ("channel_id", "40"),
            ("author_id", "60"),
            ("date_from", "2026-09-03"),
            ("date_to", "2026-09-03"),
        ],
    );
    assert_eq!(result["messages"].as_array().unwrap().len(), 1);
    let row = &result["messages"][0];
    assert_eq!(row["id"], id(2));
    assert_eq!(row["guild_id"], "20");
    assert_eq!(row["channel_id"], "40");
    assert_eq!(row["author_id"], "60");
    assert_eq!(row["author_name"], "Historical name");
    assert_eq!(row["timestamp"], "2026-09-03T00:00:00+00:00");
    assert_eq!(row["content"], content(2));
    assert_eq!(
        row["attachments"],
        json!(["https://cdn.discordapp.com/attachments/test.png"])
    );
    assert_eq!(row["avatar_url"], Value::Null);
    assert_eq!(
        search(&mut h, &[("author_id", "50")])["messages"][0]["avatar_url"],
        "https://cdn.discordapp.com/avatars/50/a_abc123.gif?size=64"
    );
    assert_eq!(
        search(&mut h, &[("guild_id", "10"), ("channel_id", "40")])["messages"],
        json!([])
    );
    assert_eq!(
        ids(&search(
            &mut h,
            &[("date_from", "2026-09-02"), ("date_to", "2026-09-03")]
        )["messages"]),
        vec![id(2), id(1)]
    );
}

#[test]
fn validation() {
    let mut h = viewer(None);
    for (params, detail) in [
        (
            vec![("guild_id", "name")],
            "Choose a filter suggestion or enter a valid Discord ID.",
        ),
        (
            vec![("author_id", "999999999999999999999999999999")],
            "Choose a filter suggestion or enter a valid Discord ID.",
        ),
        (vec![("date_from", "2026-99-01")], "Enter a valid UTC date."),
        (
            vec![("date_from", "2026-09-04"), ("date_to", "2026-09-01")],
            "From date must be on or before the to date.",
        ),
    ] {
        let params = params
            .into_iter()
            .map(|(k, v)| (k, v.to_owned()))
            .collect::<Vec<_>>();
        let response = h.get("/api/search", &params);
        assert_eq!(response.status, 400);
        assert_eq!(response.json(), json!({"detail":detail}));
    }
    for (key, value) in [
        ("limit", "101".into()),
        ("before", "-1".into()),
        ("q", "a".repeat(201)),
    ] {
        let response = h.get("/api/search", &[(key, value)]);
        assert_eq!(response.status, 422);
        assert_eq!(response.json()["detail"][0]["loc"], json!(["query", key]));
    }
}

#[test]
fn suggestions_scoping_and_minimum() {
    let mut h = viewer(None);
    assert_eq!(
        get(&mut h, "/api/suggestions/author", &[("q", "A")]),
        json!([])
    );
    assert_eq!(
        get(&mut h, "/api/suggestions/author", &[("q", "Al")]),
        json!([{"id":"50","name":"Alex","username":"Alex"},{"id":"60","name":"Alina","username":"Alina"}])
    );
    assert_eq!(
        get(&mut h, "/api/suggestions/channel", &[("guild_id", "10")]),
        json!([{"id":"30","name":"📢┃general-chat"}])
    );
    assert_eq!(
        get(&mut h, "/api/suggestions/author", &[("q", "Al%")]),
        json!([])
    );
    assert_eq!(h.get("/api/suggestions/settings", &[]).status, 422);
    assert_eq!(
        ids(&get(&mut h, "/api/suggestions/author", &[("q", "50")])),
        vec!["50"]
    );
}

#[test]
fn emoji_channel_suggestions_keep_stable_ids() {
    let mut h = viewer(None);
    for query in ["general", "📢┃general", "Ｇｅｎｅｒａｌ"] {
        let rows = get(
            &mut h,
            "/api/suggestions/channel",
            &[("q", query), ("guild_id", "10")],
        );
        assert_eq!(rows, json!([{"id":"30","name":"📢┃general-chat"}]));
    }
    assert_eq!(
        get(&mut h, "/api/suggestions/channel", &[("q", "30")])[0]["id"],
        "30"
    );
    let rows = search(&mut h, &[("channel_id", "30")]);
    assert_eq!(rows["messages"].as_array().unwrap().len(), 40);
    assert!(
        rows["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["channel_id"] == "30")
    );
}

#[test]
fn saved_profile_is_readable_without_collection_routes() {
    let mut h = viewer(None);
    let profile = get(&mut h, "/api/profiles/50", &[]);
    assert_eq!(profile["id"], "50");
    assert_eq!(profile["extended"], true);
    assert_eq!(profile["username"], "alex.saved");
    assert_eq!(profile["bio"], "Saved bio");
    assert_eq!(profile["pronouns"], "they/them");
    assert_eq!(profile["message_count"], 40);
    assert_eq!(
        profile["avatar_url"],
        "https://cdn.discordapp.com/avatars/50/a_abc123.gif?size=256"
    );
    assert_eq!(
        profile["connections"],
        json!([{"type":"github","name":"alex"}])
    );
    assert_eq!(profile["details"], saved_profile());
    assert_eq!(profile["observation_kind"], "profile_fetch");
    assert_eq!(
        get(&mut h, "/api/profiles/50/servers", &[]),
        json!({"servers":[{
        "id":"10","name":"Server Alpha","messages":40,"latest_id":id(79)}],
        "has_more":false,"offset":0,"observation_kind":"archived_messages","current_membership":null})
    );
    assert_eq!(get(&mut h, "/api/profiles/60", &[])["extended"], false);
    assert_eq!(h.get("/api/profiles/99", &[]).status, 404);
    assert_eq!(h.get("/api/profiles/nope", &[]).status, 400);
    for path in ["/api/profiles/50/fetch", "/api/profile-backfill"] {
        assert!([404, 405].contains(&h.request("POST", path, &[]).status));
    }
    for path in ["/profile-view.js", "/profile-view.css"] {
        assert_eq!(h.get(path, &[]).status, 200);
    }
    assert!(!h.vault().exists());
}

#[test]
fn missing_old_and_wal_archives() {
    let mut missing = Harness::new().unwrap();
    missing.start_viewer("synthetic.db", true, None);
    let response = missing.get("/api/summary", &[]);
    assert_eq!(response.status, 503);
    assert!(
        response.json()["detail"]
            .as_str()
            .unwrap()
            .contains("Archive not found")
    );
    assert!(!missing.archive().exists());

    let mut old = Harness::new().unwrap();
    fs::create_dir_all(old.archive().parent().unwrap()).unwrap();
    Connection::open(old.archive()).unwrap().close().unwrap();
    old.start_viewer("synthetic.db", true, None);
    let response = old.get("/api/summary", &[]);
    assert_eq!(response.status, 503);
    assert!(
        response.json()["detail"]
            .as_str()
            .unwrap()
            .contains("schema v9/v10")
    );
    fs::write(old.archive().with_file_name("synthetic.db-wal"), b"pending").unwrap();
    let response = old.get("/api/summary", &[]);
    assert_eq!(response.status, 503);
    assert!(response.json()["detail"].as_str().unwrap().contains("WAL"));

    let mut v9 = fixture();
    v9.database()
        .execute_batch("PRAGMA user_version=9")
        .unwrap();
    let before = fs::read(v9.archive()).unwrap();
    v9.start_viewer("synthetic.db", true, None);
    assert_eq!(
        get(&mut v9, "/api/summary", &[]),
        json!({"messages":80,"servers":2})
    );
    assert_eq!(fs::read(v9.archive()).unwrap(), before);
    assert_eq!(
        v9.database()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
}
