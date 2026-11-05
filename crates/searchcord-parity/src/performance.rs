//! Deterministic performance inputs, shared by baseline and later qualification.
use crate::Harness;
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub const BASE: i64 = 1_450_000_000_000_000_000;
pub const STEP: i64 = 1 << 22;
pub fn content(i: usize) -> String {
    format!(
        "{} {i} Héllo % _ ! {}",
        if i.is_multiple_of(997) {
            "rare needle"
        } else {
            "common"
        },
        "archived text ".repeat(8)
    )
}
pub fn fixture(rows: usize) -> Harness {
    // Fixture creation stays on the pinned reference even when a test selects
    // a native capability; the native operation/assertions live in its caller.
    let mut h = Harness::with_backend(crate::Backend::Python).expect("Python reference fixture");
    h.init();
    let mut db = h.database();
    let tx = db.transaction().unwrap();
    for i in 0..4 {
        tx.execute(
            "INSERT INTO guilds VALUES (?,?)",
            params![(i + 1).to_string(), format!("Server {i}")],
        )
        .unwrap();
    }
    for i in 0..16 {
        tx.execute(
            "INSERT INTO channels VALUES (?,?,?)",
            params![
                (i + 20).to_string(),
                (i % 4 + 1).to_string(),
                format!("Channel {i}")
            ],
        )
        .unwrap();
    }
    for i in 0..64 {
        tx.execute(
            "INSERT INTO authors(id,name) VALUES (?,?)",
            params![(i + 100).to_string(), format!("Author {i}")],
        )
        .unwrap();
    }
    {
        let mut insert = tx
            .prepare_cached(
                "INSERT INTO messages(id,channel_id,guild_id,author_id,content) VALUES (?,?,?,?,?)",
            )
            .unwrap();
        // Deterministic out-of-order inserts exercise public-ID ordering.
        for i in (0..rows).step_by(2).chain((1..rows).step_by(2)) {
            insert
                .execute(params![
                    BASE + i as i64 * STEP,
                    i as i64 % 16 + 20,
                    i as i64 % 4 + 1,
                    i as i64 % 64 + 100,
                    content(i)
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    drop(db);
    h
}
pub fn benchmark_fixture(rows: usize) -> Harness {
    let h = fixture(rows);
    let mut db = h.database();
    let tx = db.transaction().unwrap();
    for i in 0..64 {
        let uid = (i + 100).to_string();
        tx.execute("INSERT INTO profiles(user_id,username,global_name,avatar_hash,bot,fetched_at) VALUES (?,?,?,?,0,'2026-01-01T00:00:00+00:00')",params![uid,format!("author_{i}"),format!("Author {i}"),"abcdef0123456789"]).unwrap();
        tx.execute("INSERT INTO profile_details VALUES (?,?,'2026-01-01T00:00:00+00:00')",params![uid,json!({"user":{"id":uid,"username":format!("author_{i}"),"global_name":format!("Author {i}"),"avatar":"abcdef0123456789"},"user_profile":{"bio":format!("Saved fixture profile {i}")},"unknown":{"preserved":true}}).to_string()]).unwrap();
    }
    for i in (0..rows).step_by(257) {
        let message_id = BASE + i as i64 * STEP;
        let url = format!(
            "https://cdn.discordapp.com/attachments/20/{message_id}/fixture.png?ex=ffffffff"
        );
        let payload = json!({"id":message_id.to_string(),"channel_id":(i%16+20).to_string(),"author":{"id":(i%64+100).to_string()},"content":content(i),"attachments":[{"id":message_id.to_string(),"url":url,"filename":"fixture.png","content_type":"image/png","size":512}]});
        tx.execute(
            "UPDATE messages SET image_urls=? WHERE id=?",
            params![url, message_id],
        )
        .unwrap();
        tx.execute("INSERT INTO message_payloads(message_id,payload,first_seen_at,last_seen_at) VALUES (?,?,'2026-01-01T00:00:00+00:00','2026-01-01T00:00:00+00:00')",params![message_id,payload.to_string()]).unwrap();
    }
    tx.commit().unwrap();
    drop(db);
    h
}
#[derive(Clone)]
pub struct SearchWorkload {
    pub params: Vec<(&'static str, String)>,
    pub ids: Vec<String>,
}
pub fn search_workload(rows: usize) -> Vec<SearchWorkload> {
    [
        ("common", None),
        ("rare needle", None),
        ("H", None),
        ("Hé", None),
        ("% _ !", None),
        ("", Some(("author_id", "100"))),
        ("common", Some(("guild_id", "2"))),
        ("archived", Some(("channel_id", "35"))),
    ]
    .into_iter()
    .map(|(q, filter)| {
        let mut params = vec![("q", q.to_owned())];
        if let Some((name, value)) = filter {
            params.push((name, value.to_owned()));
        }
        let ids = (0..rows)
            .rev()
            .filter(|&i| {
                content(i)
                    .to_ascii_lowercase()
                    .contains(&q.to_ascii_lowercase())
                    && match filter {
                        Some(("author_id", _)) => i % 64 == 0,
                        Some(("guild_id", _)) => i % 4 == 1,
                        Some(("channel_id", _)) => i % 16 == 15,
                        _ => true,
                    }
            })
            .take(40)
            .map(|i| (BASE + i as i64 * STEP).to_string())
            .collect();
        SearchWorkload { params, ids }
    })
    .collect()
}
pub fn records_digest(db: &Connection) -> String {
    let mut hash = Sha256::new();
    let mut statement=db.prepare("SELECT id,channel_id,guild_id,author_id,content,timestamp,image_urls,channel_name,guild_name,author_name FROM message_records ORDER BY id").unwrap();
    let mut rows = statement.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let row: Vec<Value> = (0..10)
            .map(|i| match row.get_ref(i).unwrap() {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(n) => json!(n),
                ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                other => panic!("unexpected record field: {other:?}"),
            })
            .collect();
        let bytes = serde_json::to_vec(&row).unwrap();
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}
