use searchcord_parity::{Backend, Harness, parse_events};
use serde_json::{Value, json};

#[test]
fn disposable_archives_control_reference_and_verify_fts5_trigram() {
    let mut a = Harness::new().unwrap();
    let mut b = Harness::new().unwrap();
    assert_ne!(a.root(), b.root());
    a.init();
    b.init();
    let db = a.database();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        10
    );
    db.execute_batch("CREATE VIRTUAL TABLE trigram_check USING fts5(content,tokenize='trigram'); INSERT INTO trigram_check VALUES ('needle');").unwrap();
    assert_eq!(
        db.query_row(
            "SELECT content FROM trigram_check WHERE trigram_check MATCH 'eed'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "needle"
    );
    assert_eq!(
        b.database()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='trigram_check'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert!(!a.vault().exists());
}

#[test]
fn real_http_event_and_command_traces_are_collected() {
    let mut h = Harness::new().unwrap();
    h.init();
    h.start_viewer("synthetic.db", true, None);
    let response = h.get("/api/summary", &[]);
    assert_eq!(response.status, 200);
    assert_eq!(response.json(), json!({"messages":0,"servers":0}));
    h.command(json!({"op":"sse_fixture","body":": heartbeat\r\nid: 7\r\nevent: snapshot\r\ndata: {\"count\":0}\r\n\r\nevent: complete\ndata: first\ndata: second\n\n"}));
    let events = h.events("/__parity/events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, "snapshot");
    assert_eq!(events[0].id.as_deref(), Some("7"));
    assert_eq!(events[0].data, "{\"count\":0}");
    assert_eq!(events[1].event, "complete");
    assert_eq!(events[1].data, "first\nsecond");
    assert!(parse_events("data: unfinished\n").is_empty());
    let stored: Vec<Value> =
        serde_json::from_slice(&std::fs::read(h.write_trace().unwrap()).unwrap()).unwrap();
    for kind in ["command", "reply", "request", "response", "event"] {
        assert!(stored.iter().any(|t| t["kind"] == kind));
    }
}

#[test]
fn driver_blocks_live_network_and_path_escape() {
    let mut h = Harness::new().unwrap();
    for dns in [false, true] {
        let reply = h.exchange(json!({"op":"network_probe","dns":dns}));
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["error"]["type"], "RuntimeError");
        assert!(
            reply["error"]["message"]
                .as_str()
                .unwrap()
                .contains("disabled by parity driver")
        );
    }
    let reply = h.exchange(json!({"op":"load_credentials","archive":"../../escape.db"}));
    assert_eq!(reply["error"]["type"], "ValueError");
}

#[test]
fn backend_selection_distinguishes_reference_commands_from_native_services() {
    assert_eq!(Backend::parse("python").unwrap(), Backend::Python);
    assert!(Backend::parse("other").is_err());
    let error = Harness::with_backend(Backend::Rust)
        .err()
        .expect("reference-only command adapter rejects native selection");
    assert!(error.to_string().contains("Python fixtures"));
}

#[test]
fn dropping_harness_joins_reference_and_removes_temporary_archives() {
    let path;
    {
        let mut h = Harness::new().unwrap();
        h.init();
        h.start_viewer("synthetic.db", true, None);
        path = h.root().to_owned();
        assert!(path.exists());
    }
    assert!(!path.exists());
}
