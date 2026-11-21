use rusqlite::{Connection, params};
use searchcord_collector::{
    PageContext, Store,
    credentials::{Paths, Vault},
    migrations::{self, initialize_with_vault},
    recovery,
};
use searchcord_parity::{Backend, Harness};
use serde_json::json;
use std::{fs, io::Read, path::Path};
const ID: i64 = 1450000000000000001;
const ATTACHMENTS: &str =
    r#"["https://cdn.discordapp.com/file.png", "https://cdn.discordapp.com/file.zip"]"#;
fn fixture() -> (Harness, Vault) {
    let h = Harness::with_backend(Backend::Python).unwrap();
    fs::create_dir_all(h.archive().parent().unwrap()).unwrap();
    let v = Vault::platform(
        Paths::new(&h.archive(), &h.vault(), &h.vault().with_extension("key")).unwrap(),
    );
    (h, v)
}
fn legacy(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("CREATE TABLE messages(id TEXT PRIMARY KEY,channel_id TEXT,guild_id TEXT,author_id TEXT,content TEXT,timestamp TEXT,attachments TEXT,channel_name TEXT,guild_name TEXT,author_name TEXT)").unwrap();
    db.execute(
        "INSERT INTO messages VALUES (?,?,?,?,?,?,?,?,?,?)",
        params![
            ID.to_string(),
            "20",
            "10",
            "30",
            "legacy exact",
            "2026-01-01T00:00:00+00:00",
            ATTACHMENTS,
            "Channel",
            "Guild",
            "Author"
        ],
    )
    .unwrap();
}
fn scalar(db: &Connection, sql: &str) -> String {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}
fn decode(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(path).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}
#[test]
fn native_legacy_upgrade_preserves_fields_credentials_and_sanitized_recovery() {
    let (mut h, v) = fixture();
    legacy(&h.archive());
    let token = "synthetic-legacy-credential-never-ship";
    {
        let db = h.database();
        db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT)")
            .unwrap();
        db.execute("INSERT INTO settings VALUES ('token',?)", [token])
            .unwrap();
    }
    let report = initialize_with_vault(&h.archive(), &v).unwrap();
    assert!(report.migrated);
    assert!(report.credentials_migrated);
    assert_eq!(
        v.load().unwrap()["tokens"],
        json!([{"id":"legacy","label":"Saved token","token":token}])
    );
    assert_eq!(
        h.command(json!({"op":"load_credentials"}))["tokens"],
        v.load().unwrap()["tokens"]
    );
    let db = h.database();
    let fields:(String,String,String,String,String,String)=db.query_row("SELECT content,timestamp,channel_name,guild_name,author_name,image_urls FROM message_records",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    assert_eq!(
        fields,
        (
            "legacy exact".into(),
            "2026-01-01T00:00:00+00:00".into(),
            "Channel".into(),
            "Guild".into(),
            "Author".into(),
            "https://cdn.discordapp.com/file.png".into()
        )
    );
    assert_eq!(
        scalar(&db, "SELECT attachments FROM legacy_message_attachments"),
        ATTACHMENTS
    );
    assert_eq!(scalar(&db, "PRAGMA integrity_check"), "ok");
    db.execute(
        "INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)",
        [],
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count FROM stats_counts WHERE kind='total' AND key=''",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    let backup = report.backup.unwrap();
    assert_eq!(backup.extension().unwrap(), "gz");
    let bytes = decode(&backup);
    assert!(!bytes.windows(token.len()).any(|b| b == token.as_bytes()));
    let restored = h.root().join("restored.db");
    fs::write(&restored, &bytes).unwrap();
    let old = Connection::open(restored).unwrap();
    assert_eq!(
        scalar(&old, "SELECT attachments FROM messages"),
        ATTACHMENTS
    );
    assert_eq!(scalar(&old, "PRAGMA integrity_check"), "ok");
    assert_eq!(
        old.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    let before = h.command(json!({"op":"read_message","id":ID.to_string()}));
    assert_eq!(before["complete"], false);
    assert!(before["payload"].is_null());
    drop(db);
    assert!(!initialize_with_vault(&h.archive(), &v).unwrap().migrated);
    assert_eq!(h.init()["migrated"], false);
    let mut store = Store::open(h.archive()).unwrap();
    let message = json!({"id":ID.to_string(),"channel_id":"20","author":{"id":"30","username":"fixture"},"content":"new payload","unknown":{"kept":true}});
    store
        .save_page(
            std::slice::from_ref(&message),
            &PageContext {
                channel_id: "20",
                channel_name: "Channel",
                guild_id: Some("10"),
                guild_name: "Guild",
                ..Default::default()
            },
        )
        .unwrap();
    let value = store.read_message(ID, 0, 100).unwrap().unwrap();
    assert_eq!(
        value["revisions"][0]["payload"]["archived_attachments"],
        ATTACHMENTS
    );
    assert_eq!(value["payload"], message);
}
#[test]
fn native_history_upgrade_keeps_rowids_unknown_json_and_exact_timestamp_overrides() {
    let (mut h, v) = fixture();
    {
        let store = Store::open(h.archive()).unwrap();
        store
            .connection()
            .execute_batch(
                "ALTER TABLE message_history ADD COLUMN attachments TEXT; PRAGMA user_version=8;",
            )
            .unwrap();
        store.connection().execute("INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,20,10,30,'unchanged')",[ID]).unwrap();
        store.connection().execute("INSERT INTO message_history(rowid,timestamp,timestamp_override,attachments) VALUES (1,'2020-01-01T00:00:00+00:00',1,?)",[ATTACHMENTS]).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO profile_details VALUES ('30',?,'2026-01-01')",
                [json!({"unknown":{"nested":[1,2]},"user":{"id":"30"}}).to_string()],
            )
            .unwrap();
    }
    h.database()
        .execute(
            "INSERT INTO collection_jobs VALUES ('scrape','scrape',?,1.25)",
            [
                json!({"status":"paused","account_id":"synthetic","unknown":{"pending":[1,2]}})
                    .to_string(),
            ],
        )
        .unwrap();
    let report = initialize_with_vault(&h.archive(), &v).unwrap();
    assert!(report.migrated);
    assert!(scalar(&h.database(), "SELECT state_json FROM collection_jobs").contains("pending"));
    let db = h.database();
    assert_eq!(
        scalar(&db, "SELECT timestamp FROM message_records"),
        "2020-01-01T00:00:00+00:00"
    );
    assert_eq!(scalar(&db, "SELECT content FROM messages"), "unchanged");
    assert_eq!(
        scalar(&db, "SELECT attachments FROM legacy_message_attachments"),
        ATTACHMENTS
    );
    assert_eq!(
        db.query_row("SELECT rowid FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(scalar(&db, "SELECT payload FROM profile_details").contains("nested"));
    drop(db);
    assert_eq!(h.init()["migrated"], false);
}
#[test]
fn native_startup_rejects_newer_unknown_and_lossy_formats_and_rolls_back_interruptions() {
    let (h, v) = fixture();
    {
        let db = h.database();
        db.execute_batch("PRAGMA user_version=101").unwrap();
    }
    let before = fs::read(h.archive()).unwrap();
    assert!(
        initialize_with_vault(&h.archive(), &v)
            .unwrap_err()
            .to_string()
            .contains("newer")
    );
    assert_eq!(fs::read(h.archive()).unwrap(), before);
    for mode in ["unknown", "lossy", "checkpoint"] {
        let (h, v) = fixture();
        legacy(&h.archive());
        {
            let db = h.database();
            if mode == "unknown" {
                db.execute_batch("ALTER TABLE messages ADD COLUMN unknown_original_value TEXT")
                    .unwrap();
            }
            if mode == "lossy" {
                db.execute("UPDATE messages SET id=?", [format!("0{ID}")])
                    .unwrap();
            }
        }
        let result = if mode == "checkpoint" {
            migrations::initialize_with_hook(&h.archive(), &v, |_| {
                Err("injected startup interruption".into())
            })
        } else {
            initialize_with_vault(&h.archive(), &v)
        };
        assert!(result.is_err(), "{mode}");
        let db = h.database();
        assert_eq!(scalar(&db, "SELECT attachments FROM messages"), ATTACHMENTS);
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='messages_compact'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(scalar(&db, "PRAGMA integrity_check"), "ok");
        let backups: Vec<_> = fs::read_dir(h.archive().parent().unwrap())
            .unwrap()
            .filter_map(|e| {
                let p = e.unwrap().path();
                (p.extension().is_some_and(|e| e == "bak")).then_some(p)
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            scalar(
                &Connection::open(&backups[0]).unwrap(),
                "PRAGMA integrity_check"
            ),
            "ok"
        );
    }
}
#[test]
fn native_recovery_pins_wal_scrubs_pages_and_preserves_raw_copy_on_corrupt_compression() {
    let (h, _) = fixture();
    let store = Store::open(h.archive()).unwrap();
    let db = store.connection();
    let token = "synthetic-backup-only-plaintext";
    db.execute("INSERT INTO settings VALUES ('token',?)", [token])
        .unwrap();
    db.execute(
        "INSERT INTO messages(id,content) VALUES (?,'committed WAL')",
        [ID],
    )
    .unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let backup = recovery::backup(&h.archive()).unwrap();
    db.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        scalar(
            &Connection::open(&backup).unwrap(),
            "SELECT content FROM messages"
        ),
        "committed WAL"
    );
    let bytes = fs::read(&backup).unwrap();
    assert!(!bytes.windows(token.len()).any(|s| s == token.as_bytes()));
    assert_eq!(
        scalar(db, "SELECT value FROM settings WHERE key='token'"),
        token
    );
    assert!(
        recovery::pack_backup_with_hook(&backup, |path| {
            fs::write(path, b"damaged gzip")?;
            Ok(())
        })
        .is_err()
    );
    assert_eq!(fs::read(&backup).unwrap(), bytes);
    let mut name = backup.as_os_str().to_os_string();
    name.push(".gz");
    let packed = std::path::PathBuf::from(name);
    assert!(!packed.exists());
    fs::write(&packed, b"another exporter").unwrap();
    assert!(recovery::pack_backup(&backup).is_err());
    assert_eq!(fs::read(&packed).unwrap(), b"another exporter");
    fs::remove_file(&packed).unwrap();
    let packed = recovery::pack_backup(&backup).unwrap();
    assert!(!backup.exists());
    assert_eq!(decode(&packed), bytes);
}
#[test]
fn native_startup_fills_old_metadata_columns_counts_and_reconstructs_timestamp_shapes() {
    let (mut h, v) = fixture();
    legacy(&h.archive());
    {
        let db = h.database();
        db.execute_batch("CREATE TABLE authors(id TEXT PRIMARY KEY,name TEXT NOT NULL); CREATE TABLE scrape_cursors(channel_id TEXT PRIMARY KEY,guild_id TEXT,newest_message_id TEXT NOT NULL,oldest_message_id TEXT,history_complete INTEGER NOT NULL DEFAULT 0);").unwrap();
        db.execute("UPDATE messages SET timestamp=strftime('%Y-%m-%dT%H:%M:%S',((CAST(id AS INTEGER) >> 22)+1420070400000)/1000.0,'unixepoch')||'+00:00'",[]).unwrap();
    }
    initialize_with_vault(&h.archive(), &v).unwrap();
    let db = h.database();
    assert_eq!(
        db.query_row("SELECT timestamp_override FROM message_history", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    ); // this ID includes milliseconds, so whole seconds are an explicit override
    assert!(
        scalar(&db, "SELECT last_message_id FROM authors")
            .parse::<i64>()
            .is_ok()
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('scrape_cursors') WHERE name LIKE 'pending_%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    drop(db);
    assert_eq!(h.init()["migrated"], false);
}
#[test]
fn native_credential_migration_merges_metadata_and_refuses_conflicts_without_recovery() {
    let (h, v) = fixture();
    legacy(&h.archive());
    let existing = json!([{"id":"existing","label":"Existing","token":"synthetic-existing"}]);
    v.save(&existing, &json!("existing")).unwrap();
    {
        let db = h.database();
        db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT)")
            .unwrap();
        db.execute(
            "INSERT INTO settings VALUES ('saved_tokens',?)",
            [json!([{"id":"legacy","label":"Legacy","token":"synthetic-legacy"}]).to_string()],
        )
        .unwrap();
    }
    let report = initialize_with_vault(&h.archive(), &v).unwrap();
    assert!(report.credentials_migrated);
    let state = v.load().unwrap();
    assert_eq!(state["active_token_id"], "existing");
    assert_eq!(state["tokens"].as_array().unwrap().len(), 2);
    assert!(
        !scalar(
            &h.database(),
            "SELECT value FROM settings WHERE key='saved_tokens'"
        )
        .contains("synthetic")
    );
    let (h, v) = fixture();
    legacy(&h.archive());
    v.save(&existing, &json!("existing")).unwrap();
    {
        let db = h.database();
        db.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT)")
            .unwrap();
        db.execute(
            "INSERT INTO settings VALUES ('saved_tokens',?)",
            [
                json!([{"id":"existing","label":"Conflict","token":"different-synthetic"}])
                    .to_string(),
            ],
        )
        .unwrap();
    }
    let before = fs::read(&v.paths.vault).unwrap();
    assert!(
        initialize_with_vault(&h.archive(), &v)
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
    assert_eq!(fs::read(&v.paths.vault).unwrap(), before);
    assert_eq!(
        h.database()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        fs::read_dir(h.archive().parent().unwrap())
            .unwrap()
            .all(|p| !p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("pre-compact"))
    );
}

#[test]
fn native_name_id_compact_upgrade_reconstructs_exact_utc_timestamp_shapes() {
    let (mut h, v) = fixture();
    let whole = (1_767_225_600_000_i64 - 1_420_070_400_000) << 22;
    let fractional = (1_767_225_600_123_i64 - 1_420_070_400_000) << 22;
    {
        let store = Store::open(h.archive()).unwrap();
        let db = store.connection();
        db.execute_batch("DROP VIEW message_records; DROP TABLE messages_fts; DROP TABLE messages; CREATE TABLE messages(id TEXT PRIMARY KEY,channel_id INTEGER,guild_id INTEGER,author_id INTEGER,content TEXT,image_urls TEXT,timestamp TEXT,guild_name_id INTEGER,channel_name_id INTEGER,author_name_id INTEGER); INSERT INTO name_values VALUES (1,'Historical Guild'),(2,'Historical Channel'),(3,'Historical Author'); PRAGMA user_version=7;").unwrap();
        for (id, timestamp, content) in [
            (whole, "2026-01-01T00:00:00+00:00", None),
            (fractional, "2026-01-01T00:00:00.123000+00:00", Some("")),
        ] {
            db.execute(
                "INSERT INTO messages VALUES (?,20,10,30,?,NULL,?,1,2,3)",
                params![id.to_string(), content, timestamp],
            )
            .unwrap();
        }
    }
    assert!(initialize_with_vault(&h.archive(), &v).unwrap().migrated);
    let db = h.database();
    let rows:Vec<_>=db.prepare("SELECT id,content,timestamp,guild_name,channel_name,author_name FROM message_records ORDER BY id").unwrap().query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(
        rows,
        vec![
            (
                whole,
                None,
                "2026-01-01T00:00:00+00:00".into(),
                "Historical Guild".into(),
                "Historical Channel".into(),
                "Historical Author".into()
            ),
            (
                fractional,
                Some("".into()),
                "2026-01-01T00:00:00.123000+00:00".into(),
                "Historical Guild".into(),
                "Historical Channel".into(),
                "Historical Author".into()
            )
        ]
    );
    assert_eq!(
        db.query_row(
            "SELECT SUM(timestamp_override) FROM message_history",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM message_history WHERE timestamp IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    assert_eq!(h.init()["migrated"], false);
}
