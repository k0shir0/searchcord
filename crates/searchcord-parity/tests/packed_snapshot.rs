//! Public-ID, codec and verified-publication contracts for the Python reference.
use flate2::read::ZlibDecoder;
use rusqlite::{Connection, types::ValueRef};
use searchcord_parity::Harness;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

const BASE: i64 = 1_450_000_000_000_000_000;
const FIELDS: [&str; 10] = [
    "id",
    "channel_id",
    "guild_id",
    "author_id",
    "content",
    "timestamp",
    "image_urls",
    "channel_name",
    "guild_name",
    "author_name",
];
fn fixture(n: usize) -> Harness {
    let mut h = Harness::new().unwrap();
    h.init();
    h.database().execute_batch("INSERT INTO guilds VALUES ('10','Current Guild');
        INSERT INTO channels VALUES ('20','10','Current Channel');
        INSERT INTO authors(id,name) VALUES ('30','Current Author'),('31','Current Author');
        INSERT INTO name_values(id,value) VALUES (1,'Historic Guild'),(2,'Historic Channel'),(3,'Historic Author');").unwrap();
    let texts = [
        "alpha common",
        "UPPER common",
        "é🙂 漢字",
        "literal % _ ! \\",
        "nul\0after",
        "x",
        "xy",
        "",
        "rare needle",
    ];
    let messages: Vec<_> = (0..n)
        .rev()
        .map(|i| {
            json!({"id":(BASE+i as i64).to_string(),
        "author":{"id":if i%2==0 {"30"} else {"31"},"username":"Historic Author"},
        "content":texts[i%texts.len()],"timestamp":"2025-12-25T12:34:56+00:00","attachments":[],
        "unknown":{"preserved":"in source"}})
        })
        .collect();
    h.command(json!({"op":"save","messages":messages,"channel_id":"20","channel_name":"Historic Channel","guild_id":"10","guild_name":"Historic Guild"}));
    h.database().execute_batch("INSERT INTO message_history(rowid,timestamp,timestamp_override,guild_name_id,channel_name_id,author_name_id)
        SELECT rowid,'2025-12-25T12:34:56+00:00',1,1,2,3 FROM messages;").unwrap();
    h.database()
        .execute(
            "INSERT INTO settings VALUES ('synthetic-secret','must-not-export')",
            [],
        )
        .unwrap();
    h
}
fn export(h: &mut Harness, dest: &str, block: usize) -> Value {
    h.command(json!({"op":"snapshot","destination":dest,"block_rows":block}))
}
fn search(h: &mut Harness, archive: &str, args: Value) -> Value {
    let reply =
        h.exchange(json!({"op":"archive_read","archive":archive,"method":"search","args":args}));
    assert_eq!(reply["ok"], true, "archive={archive} args={args}: {reply}");
    reply["result"].clone()
}
fn rows(db: &Connection) -> Vec<Value> {
    let mut statement = db
        .prepare(&format!(
            "SELECT {} FROM message_records ORDER BY id",
            FIELDS.join(",")
        ))
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(Value::Array(
                (0..FIELDS.len())
                    .map(|i| match row.get_ref(i).unwrap() {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(v) => json!(v),
                        ValueRef::Text(v) => json!(std::str::from_utf8(v).unwrap()),
                        other => panic!("unexpected field {other:?}"),
                    })
                    .collect(),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
fn digest(rows: &[Value]) -> String {
    let mut hash = Sha256::new();
    for row in rows {
        let bytes = serde_json::to_vec(row).unwrap();
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!("{:x}", hash.finalize())
}
fn normalized(mut result: Value) -> Value {
    result.as_object_mut().unwrap().remove("elapsed_ms");
    result
}
fn traverse(h: &mut Harness, archive: &str, q: &str, scan: bool) -> Vec<Value> {
    let mut result = vec![];
    let mut before = Value::Null;
    for _ in 0..100 {
        let boundary = before.as_str().map(|v| v.parse::<i64>().unwrap());
        let page = search(
            h,
            archive,
            json!({"q":q,"limit":7,"before":boundary,"scan":scan}),
        );
        result.extend(page["messages"].as_array().unwrap().iter().cloned());
        if page["has_more"] == false {
            return result;
        }
        let next = page["next_cursor"].clone();
        assert!(next.is_string());
        if let Some(old) = before.as_str() {
            assert!(next.as_str().unwrap().parse::<i64>().unwrap() < old.parse::<i64>().unwrap());
        }
        before = next;
    }
    panic!("unbounded traversal");
}

#[test]
fn snapshot_blocks_hashes_reports_and_settings_exclusion() {
    for n in [0, 1, 15, 16, 17, 31, 32, 33, 127, 128, 129, 257] {
        let mut h = fixture(n);
        let original = fs::read(h.archive()).unwrap();
        let stamp = fs::metadata(h.archive()).unwrap().modified().unwrap();
        let expected = rows(&h.database());
        let report = export(&mut h, "out/snapshot.db", 16);
        assert_eq!(report["messages"], n);
        assert_eq!(report["records_sha256"], digest(&expected));
        assert_eq!(report["fields_verified"], json!(FIELDS));
        assert_eq!(report["integrity"], "ok");
        assert_eq!(report["source_main_file_unchanged"], true);
        assert!(report["build_seconds"].as_f64().unwrap() >= 0.0);
        assert_eq!(report.as_object().unwrap().len(), 13);
        let path = h.root().join("archive/out/snapshot.db");
        assert_eq!(report["snapshot_bytes"], fs::metadata(&path).unwrap().len());
        let db = Connection::open(&path).unwrap();
        db.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES ('integrity-check')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO message_short_fts(message_short_fts) VALUES ('integrity-check')",
            [],
        )
        .unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            101
        );
        assert_eq!(
            db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name='settings'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let blocks: Vec<(i64, Vec<u8>)> = db
            .prepare("SELECT first_id,payload FROM message_blocks ORDER BY block")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(blocks.len(), n.div_ceil(16));
        for (i, (first, payload)) in blocks.iter().enumerate() {
            assert_eq!(*first, BASE + (i * 16) as i64);
            let mut bytes = vec![];
            ZlibDecoder::new(payload.as_slice())
                .read_to_end(&mut bytes)
                .unwrap();
            let body: Vec<Value> = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body.len(), 16.min(n - i * 16));
            for (j, row) in body.iter().enumerate() {
                assert_eq!(row[0], BASE + (i * 16 + j) as i64);
            }
        }
        let messages = traverse(&mut h, "out/snapshot.db", "", false);
        let ids: Vec<_> = messages
            .iter()
            .map(|m| m["id"].as_str().unwrap().parse::<i64>().unwrap())
            .collect();
        assert_eq!(
            ids,
            (0..n).rev().map(|i| BASE + i as i64).collect::<Vec<_>>()
        );
        for message in messages {
            assert!(!message.as_object().unwrap().contains_key("storage_rowid"));
            assert_eq!(message["channel_id"], "20");
            assert_eq!(message["guild_id"], "10");
            assert_eq!(message["channel_name"], "Historic Channel");
            assert_eq!(message["author_name"], "Historic Author");
            assert_eq!(message["timestamp"], "2025-12-25T12:34:56+00:00");
        }
        assert_eq!(fs::read(h.archive()).unwrap(), original);
        assert_eq!(
            fs::metadata(h.archive()).unwrap().modified().unwrap(),
            stamp
        );
    }
}

#[test]
fn standard_and_packed_queries_filters_cursors_and_unicode_match() {
    let mut h = fixture(257);
    export(&mut h, "packed.db", 16);
    for q in [
        "",
        "common",
        "ALPHA",
        "rare needle",
        "x",
        "xy",
        "é",
        "🙂",
        "漢字",
        "%",
        "_",
        "!",
        "\\",
        "nul\0after",
    ] {
        let standard = traverse(&mut h, "synthetic.db", q, false);
        let packed = traverse(&mut h, "packed.db", q, false);
        assert_eq!(packed, standard, "query {q:?}");
        let escaped = q.replace('!', "!!").replace('%', "!%").replace('_', "!_");
        let expected:Vec<String>=h.database().prepare("SELECT CAST(id AS TEXT) FROM messages WHERE content LIKE ? ESCAPE '!' ORDER BY id DESC")
            .unwrap().query_map([format!("%{escaped}%")],|r|r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(
            packed
                .iter()
                .map(|m| m["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            expected
        );
    }
    for args in [
        json!({"guild_id":"10","channel_id":"20","author_id":"30","limit":8}),
        json!({"guild_id":"99"}),
        json!({"author_id":"99"}),
        json!({"before":BASE+16,"limit":3}),
        json!({"before":BASE+17,"limit":3}),
        json!({"before":BASE,"limit":3}),
        json!({"date_from":"2025-12-15","date_to":"2026-12-31"}),
    ] {
        let a = normalized(search(&mut h, "synthetic.db", args.clone()));
        let b = normalized(search(&mut h, "packed.db", args));
        assert_eq!(a, b);
    }
    assert_eq!(
        traverse(&mut h, "packed.db", "rare needle", true),
        traverse(&mut h, "synthetic.db", "rare needle", true)
    );
    h.command(
        json!({"op":"archive_read","archive":"packed.db","method":"search","window":8,"args":{}}),
    );
    assert_eq!(
        traverse(&mut h, "packed.db", "rare needle", true),
        traverse(&mut h, "synthetic.db", "rare needle", true)
    );
}

#[test]
fn failed_builds_fresh_destinations_and_publication_races_are_safe() {
    let mut h = fixture(33);
    let source = fs::read(h.archive()).unwrap();
    let root = h.root().join("archive");
    fs::write(root.join("existing.db"), b"keep me").unwrap();
    for request in [
        json!({"destination":"existing.db"}),
        json!({"destination":"synthetic.db"}),
        json!({"destination":"invalid.db","block_rows":15}),
        json!({"destination":"invalid.db","block_rows":513}),
        json!({"source":"absent.db","destination":"invalid.db"}),
    ] {
        let mut request = request;
        request["op"] = json!("snapshot");
        assert_eq!(h.exchange(request)["ok"], false);
    }
    assert_eq!(fs::read(root.join("existing.db")).unwrap(), b"keep me");
    fs::write(root.join("occupied.db.building"), b"other build").unwrap();
    assert_eq!(
        h.exchange(json!({"op":"snapshot","destination":"occupied.db"}))["error"]["type"],
        "FileExistsError"
    );
    assert_eq!(
        fs::read(root.join("occupied.db.building")).unwrap(),
        b"other build"
    );
    let failed = h.exchange(
        json!({"op":"snapshot","destination":"fail.db","block_rows":16,"fail_compress_at":2}),
    );
    assert_eq!(failed["error"]["message"], "injected compression failure");
    assert!(!root.join("fail.db").exists());
    assert!(!root.join("fail.db.building").exists());
    assert!(!root.join("fail.db.building-journal").exists());
    assert_eq!(
        h.exchange(json!({"op":"snapshot","destination":"race.db","race":true}))["error"]["type"],
        "FileExistsError"
    );
    assert_eq!(
        fs::read(root.join("race.db")).unwrap(),
        b"concurrent exporter"
    );
    assert!(!root.join("race.db.building").exists());
    assert_eq!(fs::read(h.archive()).unwrap(), source);
    h.database().execute_batch("PRAGMA user_version=8").unwrap();
    assert_eq!(
        h.exchange(json!({"op":"snapshot","destination":"old.db"}))["error"]["type"],
        "ValueError"
    );
    assert!(!root.join("old.db.building").exists());
}

#[test]
fn export_pins_live_wal_and_default_block_policy() {
    let mut h = fixture(129);
    let writer = h.database();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE messages SET content='live wal needle' WHERE id=(SELECT MAX(id) FROM messages)").unwrap();
    assert!(
        fs::metadata(format!("{}-wal", h.archive().display()))
            .unwrap()
            .len()
            > 0
    );
    let report = h.command(json!({"op":"snapshot","destination":"wal.db"}));
    assert_eq!(report["messages"], 129);
    assert_eq!(report["records_sha256"], digest(&rows(&writer)));
    assert_eq!(
        search(&mut h, "wal.db", json!({"q":"live wal needle"}))["messages"][0]["id"],
        (BASE + 128).to_string()
    );
    let db = Connection::open(h.root().join("archive/wal.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT value FROM snapshot_info WHERE key='block_rows'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "128"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM message_blocks", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

fn media(directory: &Path, url: &str, bytes: &[u8], corrupt: bool) -> String {
    let key = format!("{:x}", Sha256::digest(url.as_bytes()));
    fs::create_dir_all(directory).unwrap();
    fs::write(
        directory.join(format!("{key}.bin")),
        if corrupt {
            b"damaged".as_slice()
        } else {
            bytes
        },
    )
    .unwrap();
    let manifest = json!({"status":"saved","source_url":url,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes)),"content_type":"application/pdf"});
    fs::write(directory.join(format!("{key}.json")), manifest.to_string()).unwrap();
    key
}

#[test]
fn referenced_media_export_verifies_hashes_and_excludes_unreferenced_files() {
    let mut h = fixture(0);
    let urls = [
        "https://cdn.discordapp.com/attachments/20/40/good.pdf",
        "https://cdn.discordapp.com/attachments/20/41/corrupt.pdf",
        "https://cdn.discordapp.com/attachments/20/42/missing.pdf",
    ];
    let attachments: Vec<_> = urls
        .iter()
        .map(|url| json!({"url":url,"filename":"saved.pdf","content_type":"application/pdf"}))
        .collect();
    h.command(json!({"op":"save","messages":[{"id":BASE.to_string(),"content":"media","author":{"id":"30","username":"fixture"},"attachments":attachments}],
        "channel_id":"20","channel_name":"fixture","guild_id":"10","guild_name":"Fixture"}));
    let directory = h.root().join("archive/media");
    let key = media(&directory, urls[0], b"good binary", false);
    let bad = media(&directory, urls[1], b"original", true);
    let unrelated = media(
        &directory,
        "https://cdn.discordapp.com/attachments/20/43/unrelated.pdf",
        b"unrelated",
        false,
    );
    export(&mut h, "out/snapshot.db", 16);
    let report = h.command(json!({"op":"export_media","destination":"out/snapshot.db"}));
    assert_eq!(report, json!({"copied":1,"unavailable":2,"bytes":11}));
    let target = h.root().join("archive/out/media");
    assert_eq!(
        fs::read(target.join(format!("{key}.bin"))).unwrap(),
        b"good binary"
    );
    assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
    assert!(!target.join(format!("{bad}.bin")).exists());
    assert!(!target.join(format!("{unrelated}.bin")).exists());
    assert!(directory.join(format!("{bad}.bin")).exists());
    let refusal = h.exchange(json!({"op":"export_media","destination":"out/snapshot.db"}));
    assert_eq!(refusal["error"]["type"], "FileExistsError");
    assert_eq!(
        fs::read(target.join(format!("{key}.bin"))).unwrap(),
        b"good binary"
    );
    let message = search(&mut h, "out/snapshot.db", json!({}))["messages"][0].clone();
    assert_eq!(message["attachment_files"].as_array().unwrap().len(), 3);
    assert_eq!(
        message["attachment_files"][0]["url"],
        format!("/api/media/{key}")
    );
    assert_eq!(message["attachment_files"][0]["filename"], "saved.pdf");
    h.start_viewer("out/snapshot.db", true, None);
    let response = h.get(&format!("/api/media/{key}"), &[]);
    assert_eq!(response.status, 200);
    assert_eq!(response.body, "good binary");
    assert_eq!(response.headers["x-content-type-options"], "nosniff");
    assert_eq!(response.headers["cache-control"], "no-store");
    assert_eq!(response.headers["content-type"], "application/octet-stream");
    assert!(response.headers["content-disposition"].contains("attachment"));
    fs::write(target.join(format!("{key}.bin")), b"bad! binary").unwrap();
    assert_eq!(h.get(&format!("/api/media/{key}"), &[]).status, 404);
    assert_eq!(h.get("/api/media/invalid", &[]).status, 404);
}

#[test]
fn profiles_observed_servers_and_unknown_saved_fields_survive_export() {
    let mut h = fixture(33);
    let payload = json!({"user":{"id":"30","username":"saved.name","global_name":"Saved Name","avatar":"a_ab12"},
        "user_profile":{"bio":"full bio","pronouns":"they/them","banner":"banner12"},
        "presence":{"status":"idle","activities":[{"type":4,"state":"saved status"}]},
        "connected_accounts":[{"type":"github","name":"saved"}],"unknown":{"nested":[1,2,3]}});
    h.database().execute("INSERT INTO profiles(user_id,username,fetched_at) VALUES ('30','saved.name','2026-01-01')",[]).unwrap();
    h.database()
        .execute(
            "INSERT INTO profile_details VALUES ('30',?,'2026-01-01')",
            [payload.to_string()],
        )
        .unwrap();
    export(&mut h, "profiles.db", 16);
    let mut expected = Value::Null;
    for archive in ["synthetic.db", "profiles.db"] {
        let profile = h.command(
            json!({"op":"archive_read","archive":archive,"method":"profile","args":{"uid":"30"}}),
        );
        assert_eq!(profile["details"], payload);
        assert_eq!(profile["message_count"], 17);
        assert_eq!(profile["online_status"], "idle");
        assert_eq!(profile["custom_status"], "saved status");
        assert_eq!(profile["profile_stale"], true);
        assert_eq!(profile["observation_kind"], "profile_fetch");
        assert_eq!(profile["display_name"], "Saved Name");
        let servers=h.command(json!({"op":"archive_read","archive":archive,"method":"profile_servers","args":{"uid":"30"}}));
        assert_eq!(
            servers["servers"],
            json!([{"id":"10","name":"Current Guild","messages":17,"latest_id":(BASE+32).to_string()}])
        );
        let mut profile = profile;
        profile
            .as_object_mut()
            .unwrap()
            .remove("profile_age_seconds");
        if expected.is_null() {
            expected = profile;
        } else {
            assert_eq!(profile, expected);
        }
    }
    let db = Connection::open(h.root().join("archive/profiles.db")).unwrap();
    db.execute_batch("DROP TABLE profile_details").unwrap();
    // Optional extended details on v9 remain optional to the exporter.
    h.database()
        .execute_batch("DROP TABLE profile_details; PRAGMA user_version=9")
        .unwrap();
    export(&mut h, "v9.db", 16);
    let basic = h.command(
        json!({"op":"archive_read","archive":"v9.db","method":"profile","args":{"uid":"30"}}),
    );
    assert_eq!(basic["extended"], false);
    assert_eq!(basic["username"], "saved.name");
}

#[test]
fn snapshot_cli_reports_are_exclusive_and_media_requires_fresh_directory() {
    let mut h = fixture(17);
    let result=h.command(json!({"op":"snapshot_cli","destination":"cli/out.db","report":"cli/report.json","with_media":true}));
    assert_eq!(result["code"], 0);
    let report_path = h.root().join("archive/cli/report.json");
    let bytes = fs::read(&report_path).unwrap();
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["messages"], 17);
    assert_eq!(
        report["media"],
        json!({"copied":0,"unavailable":0,"bytes":0})
    );
    assert_eq!(report["records_sha256"], digest(&rows(&h.database())));
    assert_eq!(
        serde_json::from_str::<Value>(result["stdout"].as_str().unwrap()).unwrap(),
        report
    );
    for (dest, report, media) in [
        ("other.db", "synthetic.db", false),
        ("other.db", "other.db", false),
        ("other.db", "cli/report.json", false),
        ("cli/other.db", "fresh-report.json", true),
    ] {
        let result = h.command(
            json!({"op":"snapshot_cli","destination":dest,"report":report,"with_media":media}),
        );
        assert_eq!(result["code"], 2);
        assert!(!h.root().join("archive").join(dest).exists());
    }
    assert_eq!(fs::read(report_path).unwrap(), bytes);
    assert!(!h.root().join("archive/fresh-report.json").exists());
}
