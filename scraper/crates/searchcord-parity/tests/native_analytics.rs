use rusqlite::{Connection, params};
use searchcord_collector::{
    PageContext, Store,
    analytics::{self, ChatMLExportRequest},
    discovery::HttpError,
};
use searchcord_parity::{Backend, Harness};
use serde_json::{Value, json};

fn fixture() -> Harness {
    let mut h = Harness::with_backend(Backend::Python).unwrap();
    h.init();
    h
}

fn python(h: &mut Harness, method: &str, path: &str, body: Value) -> Value {
    h.command(json!({"op":"job","action":"http","method":method,"path":path,"body":body}))
}

fn get(h: &mut Harness, path: &str) -> Value {
    let response = python(h, "GET", path, Value::Null);
    assert_eq!(response["status"], 200, "{response}");
    response["body"].clone()
}

fn context(channel: &'static str, guild: Option<&'static str>) -> PageContext<'static> {
    PageContext {
        channel_id: channel,
        channel_name: "chat",
        guild_id: guild,
        guild_name: "Guild",
        ..Default::default()
    }
}

fn id(db: &Connection, timestamp: &str, suffix: i64) -> i64 {
    let seconds: i64 = db
        .query_row("SELECT unixepoch(?)", [timestamp], |r| r.get(0))
        .unwrap();
    ((seconds * 1000 - 1_420_070_400_000) << 22) + suffix
}

fn message(id: i64, author: &str, name: &str, content: Option<&str>) -> Value {
    // Stats intentionally derive UTC dates from IDs, not this historical override.
    json!({"id":id.to_string(),"author":{"id":author,"username":name},"content":content,
        "timestamp":"2020-01-01T00:00:00+00:00","attachments":[]})
}

fn error(error: Box<dyn std::error::Error + Send + Sync>, status: u16, detail: &str) {
    let error = error.downcast_ref::<HttpError>().unwrap();
    assert_eq!((error.status, error.detail.as_str()), (status, detail));
}

#[test]
fn chatml_matches_python_historical_participants_turns_and_utf8_lines() {
    let mut h = fixture();
    let mut store = Store::open(h.archive()).unwrap();
    let start = id(store.connection(), "2026-01-31T12:00:00Z", 0);
    let messages = vec![
        message(start + 1, "30", "old target", Some("orphan")),
        message(start + 2, "31", "Bób", Some(" \u{1c} hi \u{1f}")),
        message(start + 3, "32", "Third person", Some("there")),
        message(start + 4, "30", "New target", Some(" answer ")),
        message(start + 5, "31", "Bób", Some("\u{85} \t")),
        message(start + 6, "30", "New target", Some("again 😀")),
        message(start + 7, "31", "Bób", None),
        message(start + 8, "31", "Bób", Some("next")),
        message(start + 9, "30", "New target", Some("final\nline")),
        message(start + 10, "31", "Bób", Some("unanswered")),
    ];
    store
        .save_page(&messages, &context("20", Some("10")))
        .unwrap();
    store
        .connection()
        .execute("INSERT INTO name_values(value) VALUES ('old target')", [])
        .unwrap();
    store.connection().execute("INSERT INTO message_history(rowid,author_name_id) SELECT rowid,(SELECT id FROM name_values WHERE value='old target') FROM messages WHERE id=?", [start+1]).unwrap();
    let native = analytics::participants(h.archive(), "20").unwrap();
    assert_eq!(
        native,
        get(&mut h, "/api/export/participants?channel_id=20")
    );
    assert_eq!(native.as_array().unwrap().len(), 4); // one historical name per distinct pair
    for request in [
        json!({"channel_id":"20","target_id":"30","system_prompt":"Be {{literal}} {target}, reply to {other!s}"}),
        json!({"channel_id":"20","target_id":"30","other_name":"Custom","system_prompt":""}),
        json!({"channel_id":"20","target_id":"30","other_name":"","system_prompt":null}),
        json!({"channel_id":"20","target_id":"30","system_prompt":"{target:*>20.6s}|{other!r:^15}|{other!a}|{target[0]}"}),
        json!({"channel_id":"20","target_id":"30","other_name":"20","system_prompt":"{target:{other}}/{target:015}"}),
    ] {
        let request_struct: ChatMLExportRequest = serde_json::from_value(request.clone()).unwrap();
        let export = analytics::prepare_chatml(h.archive(), &request_struct).unwrap();
        assert_eq!(export.target_name, "old target");
        let mut lines = String::new();
        export
            .stream(|line| {
                lines.push_str(&line);
                Ok(())
            })
            .unwrap();
        let response = python(&mut h, "POST", "/api/export/chatml", request);
        assert_eq!(response["status"], 200);
        assert_eq!(lines, response["body"].as_str().unwrap());
        let lines: Vec<Value> = lines
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["messages"][2]["content"], "answer\nagain 😀");
        assert!(
            lines[0]["messages"][1]["content"]
                .as_str()
                .unwrap()
                .ends_with(": hi\nthere")
        );
    }
    for template in [
        "{unknown}",
        "{target",
        "target}",
        "{target!q}",
        "{target:+}",
        "{target:.}",
    ] {
        let request = json!({"channel_id":"20","target_id":"30","system_prompt":template});
        let native = analytics::prepare_chatml(
            h.archive(),
            &serde_json::from_value(request.clone()).unwrap(),
        )
        .unwrap_err();
        let response = python(&mut h, "POST", "/api/export/chatml", request);
        error(native, 400, response["body"]["detail"].as_str().unwrap());
    }
    let request = json!({"channel_id":"20","target_id":"99"});
    let native = analytics::prepare_chatml(
        h.archive(),
        &serde_json::from_value(request.clone()).unwrap(),
    )
    .unwrap_err();
    let response = python(&mut h, "POST", "/api/export/chatml", request);
    error(native, 404, response["body"]["detail"].as_str().unwrap());
}

#[test]
fn chatml_blank_only_default_names_batch_boundaries_and_sink_cancellation() {
    let mut h = fixture();
    let mut store = Store::open(h.archive()).unwrap();
    let start = id(store.connection(), "2026-02-01T00:00:00Z", 0);
    let mut messages = vec![message(start, "31", "", Some("before"))];
    messages.extend((1..=1002).map(|n| message(start + n, "30", "", Some("target"))));
    messages.push(message(start + 1003, "31", "", Some("next")));
    messages.push(message(start + 1004, "30", "", Some("final")));
    store.save_page(&messages, &context("20", None)).unwrap();
    store
        .save_page(
            &[message(start + 2000, "30", "", Some(" \u{1c}\t"))],
            &context("21", None),
        )
        .unwrap();
    let request: ChatMLExportRequest =
        serde_json::from_value(json!({"channel_id":"20","target_id":"30"})).unwrap();
    let export = analytics::prepare_chatml(h.archive(), &request).unwrap();
    assert_eq!(export.target_name, "30");
    assert_eq!(export.other_name, "Them");
    let mut output = String::new();
    export
        .stream(|line| {
            output.push_str(&line);
            Ok(())
        })
        .unwrap();
    let response = python(
        &mut h,
        "POST",
        "/api/export/chatml",
        json!(request_to_json(&request)),
    );
    assert_eq!(output, response["body"].as_str().unwrap());
    let mut emitted = 0;
    let failure = export
        .stream(|_| {
            emitted += 1;
            Err("sink closed".into())
        })
        .unwrap_err();
    assert_eq!(failure.to_string(), "sink closed");
    assert_eq!(emitted, 1);
    let request: ChatMLExportRequest =
        serde_json::from_value(json!({"channel_id":"21","target_id":"30"})).unwrap();
    let export = analytics::prepare_chatml(h.archive(), &request).unwrap();
    assert_eq!(export.other_name, "Them");
    export
        .stream(|_| panic!("blank messages cannot produce a training pair"))
        .unwrap();
}

fn request_to_json(request: &ChatMLExportRequest) -> Value {
    json!({"channel_id":request.channel_id,"target_id":request.target_id,"other_name":request.other_name,"system_prompt":request.system_prompt})
}

#[test]
fn cached_statistics_match_python_dates_null_guilds_ties_zero_counts_and_db_size() {
    let mut h = fixture();
    let mut store = Store::open(h.archive()).unwrap();
    let mut messages = vec![];
    for (n, date, author) in [
        (1, "2026-01-01T00:00:00Z", "30"),
        (2, "2026-01-31T23:59:59Z", "30"),
        (3, "2026-02-01T00:00:00Z", "31"),
        (4, "2026-02-02T12:00:00Z", "31"),
    ] {
        messages.push(message(
            id(store.connection(), date, n),
            author,
            author,
            Some("counted"),
        ));
    }
    store
        .save_page(&messages, &context("20", Some("10")))
        .unwrap();
    let direct = message(
        id(store.connection(), "2026-02-02T12:00:00Z", 10),
        "32",
        "DM",
        Some("direct"),
    );
    store.save_page(&[direct], &context("21", None)).unwrap();
    store.connection().execute_batch("INSERT INTO stats_counts VALUES ('author','empty',0),('channel','',0),('guild','deleted',-2),('hour','',1); INSERT INTO authors(id,name) VALUES ('unused','Unused'); INSERT INTO guilds VALUES ('unused','Unused');").unwrap();
    for days in [0, 1, 2, 7, 90, 3650] {
        let native = analytics::stats(h.archive(), days).unwrap();
        let expected = get(&mut h, &format!("/api/stats?days={days}"));
        assert_eq!(native, expected, "days={days}");
        assert_eq!(native["total_messages"], 5);
        assert_eq!(native["total_servers"], 1);
        assert_eq!(native["total_users"], 3);
        assert_eq!(native["messages_by_server"][1]["guild_id"], Value::Null);
        assert_eq!(native["messages_by_hour"][0]["hour"], Value::Null);
        assert_eq!(native["top_users"][0]["author_id"], "30");
        assert_eq!(native["top_users"][1]["author_id"], "31");
        assert_eq!(native["range_end"], "2026-02-02");
    }
    for days in [-1, 3651] {
        assert_eq!(
            analytics::stats(h.archive(), days)
                .unwrap_err()
                .downcast_ref::<HttpError>()
                .unwrap()
                .status,
            422
        );
    }
    // Verify this uses cached counts rather than a scan of the message table.
    store
        .connection()
        .execute("UPDATE stats_counts SET count=123 WHERE kind='total'", [])
        .unwrap();
    assert_eq!(
        analytics::stats(h.archive(), 0).unwrap()["total_messages"],
        123
    );
}

#[test]
fn ranked_pages_and_filters_match_python_literal_wildcards_prefixes_and_limits() {
    let mut h = fixture();
    let db = h.database();
    for (id, name, count) in [
        ("2", "Alpha", 7),
        ("10", "alpha", 7),
        ("30", "100%_!", 5),
        ("31", "100XYZ", 4),
        ("32", "Héllo", 3),
        ("33", "ALpine", 2),
        ("99", "No messages", 0),
    ] {
        db.execute(
            "INSERT INTO authors(id,name) VALUES (?,?)",
            params![id, name],
        )
        .unwrap();
        db.execute(
            "INSERT INTO guilds(id,name) VALUES (?,?)",
            params![id, name],
        )
        .unwrap();
        db.execute(
            "INSERT INTO stats_counts VALUES ('author',?,?),('guild',?,?)",
            params![id, count, id, count],
        )
        .unwrap();
    }
    db.execute_batch("INSERT INTO stats_counts VALUES ('guild','',9),('total','',28),('author','missing',1),('guild','missing',1); INSERT INTO channels VALUES ('20','2','Zoo'),('21',NULL,'Direct'),('22','10','Alpha');").unwrap();
    drop(db);
    for q in [
        "",
        "alpha",
        " 10 ",
        "%_!",
        "!",
        "hé",
        "hÉ",
        "missing",
        "\u{1c}Alpha\u{1f}",
        "No messages",
    ] {
        for (offset, limit) in [(0, 1), (1, 2), (2, 100), (99, 3)] {
            let encoded = url_query(q);
            assert_eq!(
                analytics::contributors(h.archive(), offset, limit, q).unwrap(),
                get(
                    &mut h,
                    &format!("/api/stats/contributors?offset={offset}&limit={limit}&q={encoded}")
                ),
                "q={q},offset={offset}"
            );
            assert_eq!(
                analytics::ranked_servers(h.archive(), offset, limit, q).unwrap(),
                get(
                    &mut h,
                    &format!("/api/stats/servers?offset={offset}&limit={limit}&q={encoded}")
                ),
                "q={q},offset={offset}"
            );
        }
    }
    for users in [true, false] {
        assert_eq!(
            analytics::search_filters(h.archive(), users).unwrap(),
            get(
                &mut h,
                &format!("/api/search/filters?include_users={users}")
            )
        );
    }
    for q in ["", "a", "Al", "10", "100%_!", " Hé", "hÉ", "\u{1c}Al\u{1f}"] {
        assert_eq!(
            analytics::search_authors(h.archive(), q).unwrap(),
            get(&mut h, &format!("/api/search/authors?q={}", url_query(q)))
        );
    }
    for (offset, limit, q) in [
        (-1, 1, String::new()),
        (0, 0, String::new()),
        (0, 101, String::new()),
        (0, 1, "😀".repeat(101)),
    ] {
        assert_eq!(
            analytics::contributors(h.archive(), offset, limit, &q)
                .unwrap_err()
                .downcast_ref::<HttpError>()
                .unwrap()
                .status,
            422
        );
        assert_eq!(
            analytics::ranked_servers(h.archive(), offset, limit, &q)
                .unwrap_err()
                .downcast_ref::<HttpError>()
                .unwrap()
                .status,
            422
        );
    }
}

fn url_query(q: &str) -> String {
    // Percent-encode every byte so the fixture exercises literal query contents.
    q.as_bytes().iter().map(|b| format!("%{b:02X}")).collect()
}

#[test]
fn empty_database_statistics_and_relative_paths_keep_empty_response_fields() {
    let mut h = fixture();
    let native = analytics::stats(h.archive(), 90).unwrap();
    assert_eq!(native, get(&mut h, "/api/stats"));
    assert_eq!(native["range_start"], Value::Null);
    assert_eq!(native["range_end"], Value::Null);
    assert_eq!(native["messages_by_month"], json!([]));
    assert_eq!(native["messages_by_weekday"].as_array().unwrap().len(), 7);
    assert_eq!(
        analytics::participants(h.archive(), "missing").unwrap(),
        json!([])
    );
    // Resolve an explicit relative path without changing process-global cwd.
    let cwd = std::env::current_dir().unwrap();
    let directory = tempfile::Builder::new()
        .prefix("analytics-relative-")
        .tempdir_in(&cwd)
        .unwrap();
    let path = directory.path().join("synthetic.db");
    let store = Store::open(&path).unwrap();
    let relative = path.strip_prefix(&cwd).unwrap();
    assert_eq!(
        analytics::search_filters(relative, true).unwrap()["total_messages"],
        0
    );
    assert_eq!(
        analytics::stats(relative, 0).unwrap()["range_start"],
        Value::Null
    );
    drop(store);
}

#[test]
fn counted_search_and_image_lookup_match_collector_literal_and_datetime_contracts() {
    let mut h = fixture();
    let mut store = Store::open(h.archive()).unwrap();
    let start = id(store.connection(), "2026-02-01T12:30:40Z", 0);
    let mut first = message(start, "30", "Alice", Some("literal 100%_! \\ common"));
    first["attachments"] = json!([{"url":"https://cdn.discordapp.com/attachments/20/40/a.png"},{"url":"https://cdn.discordapp.com/attachments/20/41/b.jpg"}]);
    store
        .save_page(
            &[
                first,
                message(start + 1, "31", "Bob", Some("common a_b")),
                message(start + 2, "30", "Alice", Some("common")),
            ],
            &context("20", Some("10")),
        )
        .unwrap();
    store.connection().execute_batch("INSERT INTO profiles(user_id,username,avatar_hash,fetched_at) VALUES ('30','Alice','a_abcd','2026-02-01T12:00:00Z')").unwrap();
    for (q, guild, channel, author, date_from, date_to, page, limit) in [
        ("", None, None, None, None, None, 1, 2),
        ("", Some("10"), None, None, None, None, 2, 2),
        ("", None, Some("20"), Some("30"), None, None, 1, 50),
        ("common", None, None, None, None, None, 2, 1),
        ("%_!", None, None, None, None, None, 1, 50),
        ("\\", None, None, None, None, None, 1, 50),
        ("a_b", None, None, None, None, None, 1, 50),
        (" ", None, None, None, None, None, 1, 50),
        (
            "",
            None,
            None,
            None,
            Some("2026-02-01"),
            Some("2026-02-01"),
            1,
            50,
        ),
        (
            "",
            None,
            None,
            None,
            Some("2026-02-01T14:30:40+02:00"),
            Some("2026-02-01T12:30:40Z"),
            1,
            50,
        ),
        (
            "",
            None,
            None,
            None,
            Some("2026-02-01T12:30:40.000Z"),
            Some("2026-02-01T12:30:40.000Z"),
            1,
            50,
        ),
        ("", None, None, None, Some("2026-02-02"), None, 1, 50),
    ] {
        let options = searchcord_archive::SearchOptions {
            q: q.into(),
            guild_id: guild.map(str::to_owned),
            channel_id: channel.map(str::to_owned),
            author_id: author.map(str::to_owned),
            date_from: date_from.map(str::to_owned),
            date_to: date_to.map(str::to_owned),
            limit,
            ..Default::default()
        };
        let mut query = format!("/api/search?q={}&page={page}&limit={limit}", url_query(q));
        for (name, value) in [
            ("guild_id", guild),
            ("channel_id", channel),
            ("author_id", author),
            ("date_from", date_from),
            ("date_to", date_to),
        ] {
            if let Some(value) = value {
                query.push_str(&format!("&{name}={}", url_query(value)));
            }
        }
        assert_eq!(
            analytics::counted_search(h.archive(), &options, page).unwrap(),
            get(&mut h, &query),
            "{query}"
        );
    }
    let image = analytics::image_lookup(h.archive(), start, 1).unwrap();
    assert_eq!(image.channel_id, Some(20));
    assert_eq!(
        image.url,
        "https://cdn.discordapp.com/attachments/20/41/b.jpg"
    );
    for value in [
        "20260201",
        "2026-W05-7",
        "2026W057",
        "2026-W05",
        "2026W05",
        "20260201T123040Z",
        "2026-02-01x12:30:40",
        "2026-02-01😀12:30:40",
        "2026-02-01T12:30:40,000Z",
        "2026-02-01T12.5",
        "2026-02-01T12:30.5",
        "2026-02-01T14:30:40+02",
        "2026-02-01T14:30:40+0200",
        "2026-02-01T12:30:40+00:00:00.5",
        "2026-02-01T14:09:40+00:99",
    ] {
        let options = searchcord_archive::SearchOptions {
            date_from: Some(value.into()),
            date_to: Some(value.into()),
            limit: 50,
            ..Default::default()
        };
        let query = format!(
            "/api/search?date_from={}&date_to={}",
            url_query(value),
            url_query(value)
        );
        assert_eq!(
            analytics::counted_search(h.archive(), &options, 1).unwrap(),
            get(&mut h, &query),
            "{value}"
        );
    }
    for (message, index) in [(start, -1), (start, 2), (start + 1, 0), (0, 0)] {
        error(
            analytics::image_lookup(h.archive(), message, index).unwrap_err(),
            404,
            "Image not found",
        );
    }
    for (date_from, date_to, detail) in [
        ("invalid", "", "Invalid date filter"),
        (
            "2026-02-02",
            "2026-02-01",
            "From date must be on or before the to date",
        ),
    ] {
        let options = searchcord_archive::SearchOptions {
            date_from: Some(date_from.into()),
            date_to: Some(date_to.into()),
            ..Default::default()
        };
        error(
            analytics::counted_search(h.archive(), &options, 1).unwrap_err(),
            400,
            detail,
        );
    }
}
