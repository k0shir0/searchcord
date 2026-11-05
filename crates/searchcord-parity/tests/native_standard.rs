use searchcord_archive::{Archive, SearchOptions, search::Query};
use searchcord_parity::{Harness, performance};
use serde_json::json;
use std::{fs, time::Duration};

#[test]
fn native_standard_reads_match_python_with_filters_dates_and_full_cursor_scans() {
    let mut h = performance::fixture(600);
    let mut archive = Archive::new(h.archive(), true);
    archive.scan_window = 32;
    for q in ["", "common", "rare needle", "Hé", "%", "_", "!"] {
        for scan in [false, true] {
            let mut options = SearchOptions {
                q: q.into(),
                limit: 17,
                scan,
                ..Default::default()
            };
            let mut all = vec![];
            for _ in 0..80 {
                let page = archive.search_records(&options).unwrap();
                let expected=h.command(json!({"op":"archive_read","method":"search","window":32,"args":{"q":q,"scan":scan,"limit":17,"before":options.before}}));
                assert_eq!(json!(page.next_cursor), expected["next_cursor"]);
                assert_eq!(page.partial, expected["partial"]);
                assert_eq!(
                    page.records.len(),
                    expected["messages"].as_array().unwrap().len()
                );
                all.extend(page.records.iter().map(|r| r.id.clone()));
                if !page.has_more {
                    break;
                }
                options.before = Some(page.next_cursor.unwrap().parse().unwrap());
            }
            assert!(
                all.windows(2)
                    .all(|ids| ids[0].parse::<i64>().unwrap() > ids[1].parse::<i64>().unwrap())
            );
            let expected: i64 = h
                .database()
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE content LIKE ? ESCAPE '!'",
                    [format!("%{}%", searchcord_archive::search::like_literal(q))],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(all.len(), expected as usize);
        }
    }
    for date in ["2025-12-15", "20251215", "2025-W51-1", "2025W511"] {
        let options = SearchOptions {
            date_from: Some(date.into()),
            date_to: Some(date.into()),
            author_id: Some("000100".into()),
            ..Default::default()
        };
        let page = archive.search_records(&options).unwrap();
        assert_eq!(page.records.len(), 10);
        assert!(
            page.records
                .iter()
                .all(|r| r.author_id.as_deref() == Some("100"))
        );
    }
    for (field, value) in [
        ("author_id", "１２"),
        ("guild_id", "0"),
        ("channel_id", "x"),
    ] {
        let mut raw = json!({});
        raw[field] = json!(value);
        let options: SearchOptions = serde_json::from_value(raw).unwrap();
        assert_eq!(archive.search_records(&options).unwrap_err().status, 400);
    }
    for date in ["2026-02-30", "0000-01-01", "tomorrow", "2025-1-2"] {
        let options = SearchOptions {
            date_from: Some(date.into()),
            ..Default::default()
        };
        assert_eq!(
            archive.search_records(&options).unwrap_err().detail,
            "Enter a valid UTC date."
        );
    }
}

#[test]
fn native_connections_are_readonly_preserve_versions_and_pin_live_wal() {
    let h = performance::fixture(17);
    let original = fs::read(h.archive()).unwrap();
    {
        let session = Archive::new(h.archive(), true).connect().unwrap();
        assert!(
            session
                .database()
                .execute("DELETE FROM messages", [])
                .is_err()
        );
    }
    assert_eq!(fs::read(h.archive()).unwrap(), original);
    h.database().execute_batch("PRAGMA user_version=9").unwrap();
    assert_eq!(
        Archive::new(h.archive(), true)
            .search_records(&SearchOptions::default())
            .unwrap()
            .records
            .len(),
        17
    );
    assert_eq!(
        h.database()
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    let writer = h.database();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE messages SET content='live WAL' WHERE rowid=1;").unwrap();
    assert_eq!(
        Archive::new(h.archive(), true)
            .connect()
            .err()
            .unwrap()
            .status,
        503
    );
    let live = Archive::new(h.archive(), false);
    let session = live.connect().unwrap();
    assert_eq!(
        session
            .database()
            .query_row("SELECT content FROM messages WHERE rowid=1", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "live WAL"
    );
    writer
        .execute("UPDATE messages SET content='later WAL' WHERE rowid=1", [])
        .unwrap();
    assert_eq!(
        session
            .database()
            .query_row("SELECT content FROM messages WHERE rowid=1", [], |r| r
                .get::<_, String>(
                0
            ))
            .unwrap(),
        "live WAL"
    );
    drop(session);
    assert_eq!(
        live.raw_search(&Query {
            q: "later WAL".into(),
            ..Default::default()
        })
        .unwrap()
        .records
        .len(),
        1
    );
}

#[test]
fn native_missing_unsupported_and_deadline_failures_are_controlled() {
    let h = Harness::new().unwrap();
    let path = h.root().join("absent.db");
    let archive = Archive::new(&path, false);
    assert_eq!(
        archive.connect().err().unwrap().detail,
        "Archive not found. Set SEARCHCORD_DATA_DIR to an existing archive."
    );
    assert!(!path.exists());
    let h = performance::fixture(17);
    h.database().execute_batch("PRAGMA user_version=8").unwrap();
    assert_eq!(
        Archive::new(h.archive(), true)
            .connect()
            .err()
            .unwrap()
            .status,
        503
    );
    h.database()
        .execute_batch("PRAGMA user_version=10")
        .unwrap();
    let archive = Archive::new(h.archive(), true).with_query_timeout(Duration::from_millis(1));
    let session = archive.connect().unwrap();
    let error=session.database().query_row("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT SUM(x) FROM n",[],|r|r.get::<_,i64>(0)).unwrap_err();
    assert_eq!(searchcord_archive::ArchiveError::from(error).status, 408);
}
