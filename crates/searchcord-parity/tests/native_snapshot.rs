use searchcord_archive::{Archive, SearchOptions};
use searchcord_parity::performance;
use searchcord_snapshot::build_database;
use serde_json::{Value, json};
use std::fs;

#[test]
fn short_term_tokens_keep_ascii_fold_unicode_nul_and_sorted_block_identity() {
    let body = vec![
        vec![json!(1), json!("AaA\0x")],
        vec![json!(2), json!("é😀x")],
    ];
    assert_eq!(
        searchcord_snapshot::short_terms(&body),
        "p000061000061 p0000e901f600 p01f600000078 u000061 u000078 u0000e9 u01f600"
    );
    assert_eq!(
        searchcord_snapshot::short_terms(&[vec![json!(3), Value::Null]]),
        ""
    );
}

#[test]
fn native_exports_are_python_readable_with_exact_records_hashes_metadata_and_blocks() {
    for count in [0, 1, 16, 17, 128, 129, 257] {
        let mut h = performance::fixture(count);
        h.database()
            .execute(
                "INSERT INTO settings VALUES ('synthetic-secret','must-not-export')",
                [],
            )
            .unwrap();
        let original = fs::read(h.archive()).unwrap();
        let digest = performance::records_digest(&h.database());
        let dest = h.root().join("archive/native.db");
        let report = build_database(&h.archive(), &dest, 16).unwrap();
        assert_eq!(report.messages, count as u64);
        assert_eq!(report.records_sha256, digest);
        assert_eq!(report.integrity, "ok");
        assert!(report.source_main_file_unchanged);
        let db = rusqlite::Connection::open(&dest).unwrap();
        searchcord_archive::codec::install_reader(&db).unwrap();
        assert_eq!(performance::records_digest(&db), digest);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='settings'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM message_blocks", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            count.div_ceil(16) as i64
        );
        drop(db);
        let python=h.command(json!({"op":"archive_read","archive":"native.db","method":"search","args":{"limit":100}}));
        let rust = Archive::new(&dest, true)
            .search(&SearchOptions {
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rust["messages"], python["messages"]);
        assert_eq!(rust["has_more"], python["has_more"]);
        assert_eq!(rust["next_cursor"], python["next_cursor"]);
        assert_eq!(fs::read(h.archive()).unwrap(), original);
    }
}

#[test]
fn native_export_reads_live_wal_and_preserves_unknown_profiles_and_attachments() {
    let mut h = performance::fixture(129);
    let profile = json!({"user":{"id":"100","username":"saved"},"unknown":{"full":[1,2,3]}});
    h.database()
        .execute(
            "INSERT INTO profile_details VALUES ('100',?,'2026-01-01')",
            [profile.to_string()],
        )
        .unwrap();
    let message = json!({"id":performance::BASE.to_string(),"author":{"id":"100","username":"saved"},"content":"NUL\0Unicode 🙂 % _ !",
        "attachments":[{"filename":"file.pdf","url":"https://cdn.discordapp.com/attachments/20/40/file.pdf","content_type":"application/pdf"}]});
    h.command(json!({"op":"save","messages":[message],"channel_id":"20","channel_name":"general","guild_id":"1","guild_name":"Fixture"}));
    let writer = h.database();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE messages SET content='WAL updated' WHERE rowid=2").unwrap();
    let dest = h.root().join("archive/native-wal.db");
    let report = build_database(&h.archive(), &dest, 128).unwrap();
    assert_eq!(report.records_sha256, performance::records_digest(&writer));
    let actual=h.command(json!({"op":"archive_read","archive":"native-wal.db","method":"profile","args":{"uid":"100"}}));
    assert_eq!(actual["details"], profile);
    let message = h.command(
        json!({"op":"archive_read","archive":"native-wal.db","method":"search","args":{"q":"NUL"}}),
    );
    assert_eq!(
        message["messages"][0]["attachment_files"][0]["filename"],
        "file.pdf"
    );
    assert_eq!(h.command(json!({"op":"archive_read","archive":"native-wal.db","method":"search","args":{"q":"WAL updated"}}))["messages"].as_array().unwrap().len(),1);
}
