use rusqlite::Connection;
use searchcord_archive::codec;
use searchcord_parity::performance;
use serde_json::json;

#[test]
fn native_decoder_reads_python_blocks_exactly_with_bounded_caches() {
    let mut h = performance::fixture(1100);
    let expected = performance::records_digest(&h.database());
    h.command(json!({"op":"snapshot","destination":"packed.db","block_rows":16}));
    let db = Connection::open(h.root().join("archive/packed.db")).unwrap();
    let decoder = codec::install_reader(&db).unwrap();
    assert_eq!(performance::records_digest(&db), expected);
    let usage = decoder.usage().unwrap();
    assert!(usage.blocks <= 16);
    assert!(usage.names <= 256);
    assert!(usage.bytes <= 64 * 1024 * 1024);
    for index in [0, 1, 15, 16, 17, 127, 128, 129, 1099, 1100] {
        let id = performance::BASE + index * performance::STEP;
        assert_eq!(decoder.row_before_id(&db, id).unwrap(), index);
        assert_eq!(
            decoder.row_before_id(&db, id + 1).unwrap(),
            (index + 1).min(1100)
        );
    }
    assert!(
        db.query_row("SELECT record_id(0)", [], |r| r.get::<_, i64>(0))
            .is_err()
    );
    assert!(
        db.query_row("SELECT record_id(1101)", [], |r| r.get::<_, i64>(0))
            .is_err()
    );
}

#[test]
fn malformed_packed_data_returns_errors_instead_of_panics() {
    for fault in [
        "UPDATE snapshot_info SET value='0' WHERE key='block_rows'",
        "DELETE FROM message_blocks WHERE block=1",
        "UPDATE message_blocks SET payload=x'001122' WHERE block=1",
        "DELETE FROM record_names",
        "UPDATE message_blocks SET first_id=1 WHERE block=1",
    ] {
        let mut h = performance::fixture(17);
        h.command(json!({"op":"snapshot","destination":"packed.db","block_rows":16}));
        let db = Connection::open(h.root().join("archive/packed.db")).unwrap();
        db.execute_batch(fault).unwrap();
        match codec::install_reader(&db) {
            Err(_) => {}
            Ok(_) => assert!(
                db.query_row(
                    "SELECT channel_name FROM message_records WHERE storage_rowid=1",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .is_err(),
                "{fault}"
            ),
        }
    }
}
