use rusqlite::Connection;
use searchcord_archive::{
    codec,
    search::{self, Query},
};
use searchcord_parity::performance;
use serde_json::json;

#[test]
fn native_packed_search_matches_reference_complete_traversal_including_empty_scan_pages() {
    let mut h = performance::fixture(4200);
    // Literal, Unicode and NUL cases are placed across several block boundaries.
    h.database().execute_batch("UPDATE messages SET content='🙂 Héllo 100%_ ! \\' WHERE rowid%233=0;
        UPDATE messages SET content='x' WHERE rowid%127=0; UPDATE messages SET content='xy' WHERE rowid%131=0;").unwrap();
    h.database()
        .execute(
            "UPDATE messages SET content=? WHERE rowid=19",
            ["nul\0after"],
        )
        .unwrap();
    h.command(json!({"op":"snapshot","destination":"packed.db","block_rows":16}));
    let db = Connection::open(h.root().join("archive/packed.db")).unwrap();
    let decoder = codec::install_reader(&db).unwrap();
    for q in [
        "",
        "common",
        "rare needle",
        "x",
        "xy",
        "Hé",
        "é",
        "🙂",
        "%",
        "_",
        "!",
        "\\",
        "nul\0after",
    ] {
        for scan in [false, true] {
            let mut query = Query {
                q: q.into(),
                limit: 17,
                scan,
                window: 64,
                ..Default::default()
            };
            let mut all = vec![];
            let mut empty = 0;
            for _ in 0..300 {
                let page = search::packed_search(&db, &decoder, &query).unwrap();
                let reference = h.command(
                    json!({"op":"archive_read","archive":"packed.db","method":"search","window":64,
                    "args":{"q":q,"limit":17,"scan":scan,"before":query.before}}),
                );
                assert_eq!(page.has_more, reference["has_more"].as_bool().unwrap());
                assert_eq!(page.partial, reference["partial"].as_bool().unwrap());
                assert_eq!(json!(page.next_cursor), reference["next_cursor"]);
                for (record, expected) in page
                    .records
                    .iter()
                    .zip(reference["messages"].as_array().unwrap())
                {
                    assert_eq!(record.id, expected["id"]);
                    assert_eq!(json!(record.content), expected["content"]);
                    assert_eq!(json!(record.timestamp), expected["timestamp"]);
                    assert_eq!(json!(record.channel_id), expected["channel_id"]);
                    assert_eq!(json!(record.author_id), expected["author_id"]);
                }
                assert_eq!(
                    page.records.len(),
                    reference["messages"].as_array().unwrap().len()
                );
                if page.partial && page.records.is_empty() {
                    empty += 1;
                }
                all.extend(page.records.iter().map(|r| r.id.clone()));
                if !page.has_more {
                    break;
                }
                let next = page.next_cursor.unwrap().parse::<i64>().unwrap();
                if let Some(old) = query.before {
                    assert!(next < old);
                }
                query.before = Some(next);
            }
            let escaped = search::like_literal(q);
            let expected:Vec<String>=h.database().prepare("SELECT CAST(id AS TEXT) FROM messages WHERE content LIKE ? ESCAPE '!' ORDER BY id DESC").unwrap()
                .query_map([format!("%{escaped}%")],|r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            assert_eq!(all, expected, "q={q:?} scan={scan}");
            if q == "rare needle" && scan {
                assert!(empty > 0);
            }
        }
    }
}

#[test]
fn native_packed_filters_and_public_id_bounds_match_independent_sql() {
    let mut h = performance::fixture(600);
    h.command(json!({"op":"snapshot","destination":"packed.db"}));
    let db = Connection::open(h.root().join("archive/packed.db")).unwrap();
    let decoder = codec::install_reader(&db).unwrap();
    for (guild, channel, author) in [
        (Some("1"), None, None),
        (None, Some("20"), None),
        (None, None, Some("100")),
        (Some("1"), Some("20"), Some("100")),
        (Some("999"), None, None),
    ] {
        let query = Query {
            guild_id: guild.map(str::to_owned),
            channel_id: channel.map(str::to_owned),
            author_id: author.map(str::to_owned),
            lower: Some(performance::BASE + 16 * performance::STEP),
            upper: Some(performance::BASE + 129 * performance::STEP),
            before: Some(performance::BASE + 128 * performance::STEP),
            limit: 100,
            ..Default::default()
        };
        let page = search::packed_search(&db, &decoder, &query).unwrap();
        let expected: Vec<_> = (16..128)
            .rev()
            .filter(|i| {
                guild.is_none_or(|g| g == format!("{}", i % 4 + 1))
                    && channel.is_none_or(|c| c == format!("{}", i % 16 + 20))
                    && author.is_none_or(|a| a == format!("{}", i % 64 + 100))
            })
            .take(100)
            .map(|i| (performance::BASE + i * performance::STEP).to_string())
            .collect();
        assert_eq!(
            page.records
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>(),
            expected
        );
    }
}
