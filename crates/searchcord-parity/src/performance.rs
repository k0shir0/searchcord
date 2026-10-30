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
    let mut h = Harness::new().expect("Python reference fixture");
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
