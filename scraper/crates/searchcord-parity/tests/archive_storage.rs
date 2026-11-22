//! Rust-owned equivalents of all seven original archive-storage regressions.
use flate2::read::GzDecoder;
use rusqlite::{Connection, params, types::FromSql};
use searchcord_parity::Harness;
use serde_json::{Value, json};
use std::{fs, io::Read, path::Path};

const MESSAGE_ID: &str = "1450000000000000001";

fn message(content: &str) -> Value {
    json!({"id":MESSAGE_ID,"channel_id":"20","timestamp":"2026-01-01T00:00:00+00:00",
        "edited_timestamp":null,"content":content,"author":{"id":"30","username":"fixture"},
        "attachments":[{"id":"40","filename":"file.zip",
            "url":"https://cdn.discordapp.com/attachments/20/30/file.zip",
            "content_type":"application/zip","size":500}],
        "embeds":[{"description":"kept"}],"poll":{"question":{"text":"kept"}}})
}

fn save_request(messages: Vec<Value>) -> Value {
    json!({"op":"save","messages":messages,"channel_id":"20","channel_name":"Channel",
        "guild_id":"10","guild_name":"Guild"})
}

fn save(h: &mut Harness, message: &Value) -> Value {
    h.command(save_request(vec![message.clone()]))
}

fn read(h: &mut Harness) -> Value {
    h.command(json!({"op":"read_message","id":MESSAGE_ID}))
}

fn scalar<T: FromSql>(db: &Connection, sql: &str) -> T {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn assert_no_plaintext(path: &Path, token: &str) {
    assert!(
        !fs::read(path)
            .unwrap()
            .windows(token.len())
            .any(|w| w == token.as_bytes()),
        "synthetic credential was stored as plaintext in {}",
        path.display()
    );
}

#[test]
fn native_credentials_migrate_out_of_sqlite_and_backup() {
    let mut h = Harness::new().unwrap();
    h.init();
    let token = "synthetic-collector-credential-do-not-ship";
    h.database()
        .execute("INSERT INTO settings VALUES ('token',?)", [token])
        .unwrap();
    assert_eq!(h.init()["credentials_migrated"], true);
    let state = h.command(json!({"op":"load_credentials"}));
    assert_eq!(
        state["tokens"],
        json!([{"id":"legacy","label":"Saved token","token":token}])
    );
    assert_eq!(state["active_token_id"], "legacy");
    assert_no_plaintext(&h.vault(), token);
    {
        let db = h.database();
        let values = db
            .prepare("SELECT value FROM settings")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(values.iter().all(|value| !value.contains(token)));
        // A recovery copy must scrub its own pages while leaving its source alone.
        db.execute("INSERT INTO settings VALUES ('token',?)", [token])
            .unwrap();
    }
    let backup = h.command(json!({"op":"backup"}));
    let path = Path::new(backup.as_str().unwrap());
    assert!(
        path.canonicalize()
            .unwrap()
            .starts_with(h.root().canonicalize().unwrap())
    );
    assert_no_plaintext(path, token);
    let db = Connection::open(path).unwrap();
    assert_eq!(scalar::<i64>(&db, "SELECT COUNT(*) FROM settings"), 0);
    assert_eq!(scalar::<String>(&db, "PRAGMA integrity_check"), "ok");
    assert_eq!(
        scalar::<String>(
            &h.database(),
            "SELECT value FROM settings WHERE key='token'"
        ),
        token
    );
    h.command(json!({"op":"clear_credentials"}));
    assert_eq!(
        h.command(json!({"op":"load_credentials"})),
        json!({"tokens":[],"active_token_id":null})
    );
    assert!(!h.vault().exists());
}

#[test]
fn portable_vault_fails_closed_when_key_is_missing() {
    let mut h = Harness::new().unwrap();
    let tokens = json!([{"id":"one","label":"Fixture","token":"synthetic-portable-token"}]);
    let request =
        json!({"op":"save_credentials","tokens":tokens,"active_token_id":"one","portable":true});
    h.command(request.clone());
    assert_no_plaintext(&h.vault(), "synthetic-portable-token");
    assert!(
        fs::read(h.vault())
            .unwrap()
            .starts_with(b"SEARCHCORD-FERNET-1\n")
    );
    assert_eq!(
        h.command(json!({"op":"load_credentials","portable":true}))["tokens"],
        tokens
    );
    fs::remove_file(h.vault().with_extension("key")).unwrap();
    let before = fs::read(h.vault()).unwrap();
    for request in [request, json!({"op":"load_credentials","portable":true})] {
        let response = h.exchange(request);
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["type"], "CredentialError");
        assert_eq!(fs::read(h.vault()).unwrap(), before);
    }
    h.command(json!({"op":"clear_credentials"}));
    assert!(!h.vault().exists());
}

#[test]
fn vault_cannot_be_captured_with_archive_directory() {
    let mut h = Harness::new().unwrap();
    let result = h.exchange(json!({"op":"load_credentials","vault_in_archive":"secret.vault"}));
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["type"], "CredentialError");
    assert!(!h.archive().with_file_name("secret.vault").exists());
}

#[test]
fn exact_payload_edits_deletions_fts_and_counts() {
    let mut h = Harness::new().unwrap();
    h.init();
    let original = message("first searchable");
    assert_eq!(save(&mut h, &original), 1);
    let first = read(&mut h);
    assert_eq!(first["payload"], original);
    assert_eq!(first["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(save(&mut h, &original), 0);
    assert_eq!(read(&mut h)["revisions"].as_array().unwrap().len(), 1);
    let rowid = scalar::<i64>(&h.database(), "SELECT rowid FROM messages");
    let mut edited = message("edited searchable");
    edited["edited_timestamp"] = json!("2026-01-02T00:00:00+00:00");
    assert_eq!(save(&mut h, &edited), 0);
    assert_eq!(
        h.command(json!({"op":"delete","ids":[MESSAGE_ID],"source":"confirmed_delete"})),
        1
    );
    assert_eq!(h.command(json!({"op":"delete","ids":[MESSAGE_ID]})), 0);
    let deleted = read(&mut h);
    assert!(deleted["deleted_at"].is_string());
    assert_eq!(deleted["payload"], edited);
    assert_eq!(
        deleted["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["event"].clone())
            .collect::<Vec<_>>(),
        vec![json!("message"), json!("message"), json!("deleted")]
    );
    assert_eq!(deleted["revisions"][0]["payload"], original);
    assert_eq!(deleted["revisions"][1]["payload"], edited);
    assert_eq!(deleted["revisions"][2]["payload"], Value::Null);
    assert_eq!(deleted["revisions"][2]["source"], "confirmed_delete");
    assert_eq!(save(&mut h, &edited), 0);
    let restored = read(&mut h);
    assert_eq!(restored["deleted_at"], Value::Null);
    assert_eq!(restored["first_seen_at"], first["first_seen_at"]);
    assert_eq!(restored["complete"], true);
    assert_eq!(restored["payload"], edited);
    assert_eq!(restored["revisions"].as_array().unwrap().len(), 4);
    assert_eq!(restored["revisions"][3]["payload"], edited);
    {
        let db = h.database();
        assert_eq!(scalar::<i64>(&db, "SELECT rowid FROM messages"), rowid);
        assert_eq!(
            scalar::<i64>(&db, "SELECT count FROM stats_counts WHERE kind='total'"),
            1
        );
        assert_eq!(
            scalar::<i64>(
                &db,
                "SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%edited%'"
            ),
            1
        );
        assert_eq!(
            scalar::<i64>(
                &db,
                "SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%first%'"
            ),
            0
        );
        db.execute(
            "INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)",
            [],
        )
        .unwrap();
        assert_eq!(scalar::<String>(&db, "PRAGMA integrity_check"), "ok");
    }
    let page = h.command(json!({"op":"read_message","id":MESSAGE_ID,"limit":2}));
    assert_eq!(page["has_more"], true);
    assert_eq!(page["revisions"].as_array().unwrap().len(), 2);
    let next = h.command(json!({"op":"read_message","id":MESSAGE_ID,"limit":2,"after_revision":page["next_revision"]}));
    assert_eq!(next["has_more"], false);
    let mut revisions = page["revisions"].as_array().unwrap().clone();
    revisions.extend(next["revisions"].as_array().unwrap().iter().cloned());
    assert_eq!(revisions, *restored["revisions"].as_array().unwrap());
    let db = h.database();
    db.execute("DELETE FROM messages", []).unwrap();
    for table in ["message_revisions", "message_payloads", "messages_fts"] {
        assert_eq!(
            scalar::<i64>(&db, &format!("SELECT COUNT(*) FROM {table}")),
            0
        );
    }
    assert_eq!(
        scalar::<i64>(&db, "SELECT count FROM stats_counts WHERE kind='total'"),
        0
    );
}

#[test]
fn legacy_attachment_migration_preserves_all_original_values() {
    let mut h = Harness::new().unwrap();
    fs::create_dir_all(h.archive().parent().unwrap()).unwrap();
    let attachments =
        r#"["https://cdn.discordapp.com/file.png", "https://cdn.discordapp.com/file.zip"]"#;
    {
        let db = h.database();
        db.execute_batch("CREATE TABLE messages(id TEXT PRIMARY KEY,channel_id TEXT,guild_id TEXT,
            author_id TEXT,content TEXT,timestamp TEXT,attachments TEXT,channel_name TEXT,guild_name TEXT,author_name TEXT)").unwrap();
        db.execute(
            "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![
                MESSAGE_ID,
                "20",
                "10",
                "30",
                "legacy exact",
                "2026-01-01T00:00:00+00:00",
                attachments,
                "Channel",
                "Guild",
                "Author"
            ],
        )
        .unwrap();
    }
    let result = h.init();
    assert_eq!(result["migrated"], true);
    {
        let db = h.database();
        assert_eq!(scalar::<i64>(&db, "PRAGMA user_version"), 10);
        let values = db
            .query_row(
                "SELECT content,timestamp,channel_name,guild_name,author_name FROM message_records",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            values,
            (
                "legacy exact".into(),
                "2026-01-01T00:00:00+00:00".into(),
                "Channel".into(),
                "Guild".into(),
                "Author".into()
            )
        );
        assert_eq!(
            scalar::<String>(&db, "SELECT attachments FROM legacy_message_attachments"),
            attachments
        );
        assert_eq!(scalar::<String>(&db, "PRAGMA integrity_check"), "ok");
    }
    let backup = Path::new(result["backup"].as_str().unwrap());
    assert!(
        backup
            .canonicalize()
            .unwrap()
            .starts_with(h.root().canonicalize().unwrap())
    );
    let mut recovered = Vec::new();
    GzDecoder::new(fs::File::open(backup).unwrap())
        .read_to_end(&mut recovered)
        .unwrap();
    assert!(recovered.starts_with(b"SQLite format 3\0"));
    let recovered_path = h.root().join("recovered.db");
    fs::write(&recovered_path, recovered).unwrap();
    let recovered_db = Connection::open(recovered_path).unwrap();
    assert_eq!(
        scalar::<String>(&recovered_db, "SELECT attachments FROM messages"),
        attachments
    );
    assert_eq!(
        scalar::<String>(&recovered_db, "PRAGMA integrity_check"),
        "ok"
    );
    let before = read(&mut h);
    assert_eq!(before["complete"], false);
    assert_eq!(before["payload"], Value::Null);
    let updated = message("updated");
    save(&mut h, &updated);
    let after = read(&mut h);
    assert_eq!(after["revisions"][0]["source"], "legacy_archive");
    assert_eq!(after["revisions"][0]["payload"]["content"], "legacy exact");
    assert_eq!(
        after["revisions"][0]["payload"]["archived_attachments"],
        attachments
    );
    assert_eq!(after["payload"], updated);
    assert_eq!(h.init()["migrated"], false);
}

#[test]
fn old_history_attachments_survive_timestamp_compression() {
    let mut h = Harness::new().unwrap();
    h.init();
    let attachments = r#"["https://cdn.discordapp.com/file.zip"]"#;
    let rowid;
    {
        let db = h.database();
        db.execute_batch("ALTER TABLE message_history ADD COLUMN attachments TEXT")
            .unwrap();
        db.execute(
            "INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,?,?,?,?)",
            params![MESSAGE_ID.parse::<i64>().unwrap(), 20, 10, 30, "unchanged"],
        )
        .unwrap();
        rowid = scalar::<i64>(&db, "SELECT rowid FROM messages");
        db.execute("INSERT INTO message_history(rowid,timestamp,timestamp_override,attachments) VALUES (?,?,1,?)",
            params![rowid,"2020-01-01T00:00:00+00:00",attachments]).unwrap();
        db.execute_batch("PRAGMA user_version=8").unwrap();
    }
    assert_eq!(h.init()["migrated"], true);
    let db = h.database();
    assert_eq!(
        scalar::<String>(&db, "SELECT timestamp FROM message_records"),
        "2020-01-01T00:00:00+00:00"
    );
    assert_eq!(
        scalar::<String>(&db, "SELECT attachments FROM legacy_message_attachments"),
        attachments
    );
    assert_eq!(scalar::<i64>(&db, "SELECT rowid FROM messages"), rowid);
    assert_eq!(
        scalar::<String>(&db, "SELECT content FROM messages"),
        "unchanged"
    );
}

#[test]
fn invalid_message_rolls_back_payload_and_search_together() {
    let mut h = Harness::new().unwrap();
    h.init();
    let original = message("first");
    save(&mut h, &original);
    let before = read(&mut h);
    let mut edited = message("should never commit");
    edited["channel_id"] = json!("wrong-channel");
    let result = h.exchange(save_request(vec![edited]));
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["type"], "ValueError");
    // Force a successful write through the same transaction wrapper, then roll it back.
    let mut rollback = save_request(vec![message("also never committed")]);
    rollback["rollback"] = json!(true);
    assert_eq!(h.command(rollback), 0);
    assert_eq!(read(&mut h), before);
    let db = h.database();
    assert_eq!(
        scalar::<String>(&db, "SELECT content FROM messages"),
        "first"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT COUNT(*) FROM message_revisions"),
        1
    );
    assert_eq!(
        scalar::<i64>(
            &db,
            "SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%never%'"
        ),
        0
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT count FROM stats_counts WHERE kind='total'"),
        1
    );
    db.execute(
        "INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)",
        [],
    )
    .unwrap();
}
