use rusqlite::Connection;
use searchcord_collector::{PageContext, Store};
use searchcord_parity::{Backend, Harness};
use serde_json::{Value, json};
const ID: i64 = 1450000000000000001;
fn context() -> PageContext<'static> {
    PageContext {
        channel_id: "20",
        channel_name: "Channel",
        guild_id: Some("10"),
        guild_name: "Guild",
        history_page: true,
        ..Default::default()
    }
}
fn message(id: i64, content: &str) -> Value {
    json!({"id":id.to_string(),"channel_id":"20","timestamp":"2026-01-01T00:00:00+00:00","author":{"id":"30","username":"fixture"},"content":content,"attachments":[{"url":"https://cdn.discordapp.com/attachments/20/40/file.zip","filename":"file.zip","content_type":"application/zip"},{"url":"https://cdn.discordapp.com/attachments/20/41/file%2EPNG","filename":"image"}],"unknown":{"nested":[1,false,null,"Héllo"],"floats":[1e-7,1e-6,1e-4,1e15,1e16,1e20,-0.0,123.5]},"poll":{"question":"kept"}})
}
fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}
#[test]
fn native_payload_observations_revisions_tombstones_fts_and_python_interchange() {
    let mut h = Harness::with_backend(Backend::Python).unwrap();
    let mut store = Store::open(h.archive()).unwrap();
    let original = message(ID, "first searchable");
    assert_eq!(
        store
            .save_page(std::slice::from_ref(&original), &context())
            .unwrap(),
        1
    );
    let first = store.read_message(ID, 0, 100).unwrap().unwrap();
    assert_eq!(first["payload"], original);
    assert_eq!(first["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(
        store
            .save_page(std::slice::from_ref(&original), &context())
            .unwrap(),
        0
    );
    assert_eq!(
        store.read_message(ID, 0, 100).unwrap().unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let rowid = count(store.connection(), "SELECT rowid FROM messages");
    let mut edit = message(ID, "edited searchable");
    edit["edited_timestamp"] = json!("2026-01-02T00:00:00+00:00");
    store
        .save_page(std::slice::from_ref(&edit), &context())
        .unwrap();
    assert_eq!(
        store
            .mark_deleted(&[ID, ID, ID + 1], "confirmed_delete")
            .unwrap(),
        1
    );
    assert_eq!(store.mark_deleted(&[ID], "again").unwrap(), 0);
    let deleted = store.read_message(ID, 0, 100).unwrap().unwrap();
    assert!(deleted["deleted_at"].is_string());
    assert_eq!(deleted["payload"], edit);
    assert_eq!(deleted["revisions"][2]["event"], "deleted");
    assert!(deleted["revisions"][2]["payload"].is_null());
    store
        .save_page(std::slice::from_ref(&edit), &context())
        .unwrap();
    let restored = store.read_message(ID, 0, 100).unwrap().unwrap();
    assert!(restored["deleted_at"].is_null());
    assert_eq!(restored["first_seen_at"], first["first_seen_at"]);
    assert_eq!(restored["revisions"].as_array().unwrap().len(), 4);
    assert_eq!(
        count(store.connection(), "SELECT rowid FROM messages"),
        rowid
    );
    let python = h.command(json!({"op":"read_message","id":ID.to_string()}));
    assert_eq!(restored, python);
    assert_eq!(
        count(
            store.connection(),
            "SELECT count FROM stats_counts WHERE kind='total' AND key=''"
        ),
        1
    );
    assert_eq!(
        count(
            store.connection(),
            "SELECT COUNT(*) FROM messages_fts WHERE content LIKE '%edited%'"
        ),
        1
    );
    store
        .connection()
        .execute(
            "INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)",
            [],
        )
        .unwrap();
    let page = store.read_message(ID, 0, 2).unwrap().unwrap();
    assert_eq!(page["has_more"], true);
    let next = page["next_revision"].as_i64().unwrap();
    let tail = store.read_message(ID, next, 2).unwrap().unwrap();
    assert_eq!(tail["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(tail["has_more"], false);
    h.command(json!({"op":"save","messages":[edit.clone()],"channel_id":"20","channel_name":"Channel","guild_id":"10","guild_name":"Guild"}));
    assert_eq!(
        store.read_message(ID, 0, 100).unwrap().unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    h.command(json!({"op":"save","messages":[message(ID,"python edit")],"channel_id":"20","channel_name":"Channel","guild_id":"10","guild_name":"Guild"}));
    assert_eq!(
        store.read_message(ID, 0, 100).unwrap().unwrap()["payload"]["content"],
        "python edit"
    );
    let value = searchcord_archive::Archive::new(h.archive(), false)
        .search(&Default::default())
        .unwrap();
    assert_eq!(value["messages"][0]["content"], "python edit");
    store
        .connection()
        .execute("DELETE FROM messages", [])
        .unwrap();
    for table in [
        "message_payloads",
        "message_revisions",
        "message_history",
        "legacy_message_attachments",
    ] {
        assert_eq!(
            count(store.connection(), &format!("SELECT COUNT(*) FROM {table}")),
            0
        );
    }
    assert_eq!(
        count(
            store.connection(),
            "SELECT count FROM stats_counts WHERE kind='total' AND key=''"
        ),
        0
    );
}
#[test]
fn native_page_metadata_dedup_backfill_legacy_and_atomic_rollback() {
    let h = Harness::with_backend(Backend::Python).unwrap();
    let mut store = Store::open(h.archive()).unwrap();
    let mut new = message(ID + 10, "new");
    new["author"]["global_name"] = json!("Newest name");
    assert_eq!(
        store
            .save_page(
                &[
                    new.clone(),
                    message(ID, "old"),
                    message(ID, "duplicate last")
                ],
                &context()
            )
            .unwrap(),
        2
    );
    assert_eq!(
        store.read_message(ID, 0, 100).unwrap().unwrap()["payload"]["content"],
        "duplicate last"
    );
    assert_eq!(
        count(store.connection(), "SELECT COUNT(*) FROM message_revisions"),
        2
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT name FROM authors", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "Newest name"
    );
    let cursor: (String, String) = store
        .connection()
        .query_row(
            "SELECT newest_message_id,oldest_message_id FROM scrape_cursors",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cursor, ((ID + 10).to_string(), ID.to_string()));
    let mut bad = message(ID + 20, "bad");
    bad["channel_id"] = json!("999");
    assert!(
        store
            .save_page(&[message(ID + 19, "valid"), bad], &context())
            .is_err()
    );
    assert_eq!(
        count(store.connection(), "SELECT COUNT(*) FROM messages"),
        2
    );
    assert_eq!(
        count(store.connection(), "SELECT COUNT(*) FROM message_revisions"),
        2
    );
    let mut author = message(ID, "bad author");
    author["author"]["id"] = json!("99");
    assert!(
        store
            .save_page(&[author], &context())
            .unwrap_err()
            .to_string()
            .contains("immutable")
    );
    let failure =
        store.save_page_with_hook(&[message(ID + 21, "rollback")], &context(), |db, count| {
            assert_eq!(count, 1);
            assert_eq!(self::count(db, "SELECT COUNT(*) FROM messages"), 3);
            Err("injected checkpoint failure".into())
        });
    assert!(failure.is_err());
    assert_eq!(
        count(store.connection(), "SELECT COUNT(*) FROM messages"),
        2
    );
    store.connection().execute("INSERT INTO messages(id,channel_id,guild_id,author_id,content,image_urls) VALUES (?,20,10,30,'legacy','https://old.png')",[ID+22]).unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO legacy_message_attachments VALUES (?,'[\"original value\"]')",
            [ID + 22],
        )
        .unwrap();
    store
        .save_page(&[message(ID + 22, "complete")], &context())
        .unwrap();
    let legacy = store.read_message(ID + 22, 0, 100).unwrap().unwrap();
    assert_eq!(legacy["revisions"][0]["source"], "legacy_archive");
    assert_eq!(legacy["revisions"][0]["payload"]["content"], "legacy");
    assert_eq!(
        legacy["revisions"][0]["payload"]["archived_attachments"],
        "[\"original value\"]"
    );
    let mut webhook = message(ID + 25, "webhook");
    webhook["author"]["id"] = json!("999");
    webhook["webhook_id"] = json!("800");
    store.save_page(&[webhook], &context()).unwrap();
    assert_eq!(
        count(
            store.connection(),
            "SELECT COUNT(*) FROM channel_authors WHERE author_id='999'"
        ),
        0
    );
    let mut real = message(ID + 26, "real author");
    real["author"]["id"] = json!("888");
    real["webhook_id"] = json!(0);
    store.save_page(&[real], &context()).unwrap();
    assert_eq!(
        count(
            store.connection(),
            "SELECT COUNT(*) FROM channel_authors WHERE author_id='888'"
        ),
        1
    );
    store
        .connection()
        .execute(
            "INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)",
            [],
        )
        .unwrap();
    assert_eq!(count(store.connection(), "PRAGMA user_version"), 10);
}
