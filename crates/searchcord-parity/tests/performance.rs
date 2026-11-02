use searchcord_parity::performance;
#[test]
fn performance_fixture_is_reproducible_and_keeps_out_of_order_public_ids() {
    let a = performance::benchmark_fixture(1000);
    let b = performance::benchmark_fixture(1000);
    assert_eq!(
        performance::records_digest(&a.database()),
        performance::records_digest(&b.database())
    );
    let db = a.database();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1000
    );
    let first: i64 = db
        .query_row("SELECT id FROM messages ORDER BY rowid LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(first, performance::BASE);
    let backfill: i64 = db
        .query_row(
            "SELECT id FROM messages ORDER BY rowid LIMIT 1 OFFSET 500",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(backfill, performance::BASE + performance::STEP);
    let rare: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE content LIKE '%rare needle%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rare, 2);
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM profile_details", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        64
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM messages WHERE image_urls IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        4
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM message_payloads WHERE json_array_length(json_extract(payload,'$.attachments'))=1",[],|r|r.get::<_,i64>(0)).unwrap(),4);
}
