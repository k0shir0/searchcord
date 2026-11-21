//! Recognized historical collection schemas only. Every conversion is verified.
use crate::{
    credentials::{self, CredentialError, Paths, Vault},
    recovery,
    storage::{self, Result, SCHEMA},
};
use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, functions::FunctionFlags, params,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Debug, Serialize)]
pub struct InitReport {
    pub migrated: bool,
    pub backup: Option<PathBuf>,
    pub credentials_migrated: bool,
}
fn columns(db: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    db.prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect()
}
fn exists(db: &Connection, table: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT 1 FROM sqlite_master WHERE name=? AND type='table'",
        [table],
        |_| Ok(true),
    )
    .optional()
    .map(|r| r.unwrap_or(false))
}
fn timestamp(id: &str) -> String {
    format!(
        "CASE WHEN (({id} >> 22)+1420070400000)%1000=0 THEN strftime('%Y-%m-%dT%H:%M:%S',(({id} >> 22)+1420070400000)/1000.0,'unixepoch')||'+00:00' ELSE strftime('%Y-%m-%dT%H:%M:%f',(({id} >> 22)+1420070400000)/1000.0,'unixepoch')||'000+00:00' END"
    )
}
fn object_sql(db: &Connection, include_view: bool) -> Result<()> {
    let schema = SCHEMA.replace("\r\n", "\n");
    for section in schema.split("\n\n") {
        let is_view = section.trim_start().starts_with("CREATE VIEW");
        if is_view != include_view {
            continue;
        }
        let sql = section
            .replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ")
            .replace(
                "CREATE VIRTUAL TABLE ",
                "CREATE VIRTUAL TABLE IF NOT EXISTS ",
            )
            .replace("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ")
            .replace("CREATE TRIGGER ", "CREATE TRIGGER IF NOT EXISTS ")
            .replace("CREATE VIEW ", "CREATE VIEW IF NOT EXISTS ");
        db.execute_batch(&sql)?;
    }
    Ok(())
}
pub fn migrate_credentials(db: &mut Connection, vault: &Vault) -> Result<bool> {
    if !exists(db, "settings")? {
        return Ok(false);
    }
    let settings:HashMap<String,Option<String>>=db.prepare("SELECT key,value FROM settings WHERE key IN ('token','saved_tokens','active_token_id')")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let error =
        || CredentialError("Legacy credential settings are invalid; archive was kept".into());
    let mut tokens: Vec<Value> = match settings.get("saved_tokens").and_then(Option::as_deref) {
        Some(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .ok_or_else(error)?,
        None => vec![],
    };
    let legacy = settings
        .get("token")
        .and_then(Option::as_deref)
        .filter(|s| !s.is_empty());
    if legacy.is_none()
        && !tokens
            .iter()
            .any(|v| v.is_object() && v.get("token").is_some())
    {
        return Ok(false);
    }
    if let Some(token) = legacy
        && tokens.is_empty()
    {
        tokens.push(json!({"id":"legacy","label":"Saved token","token":token}));
    }
    let active = settings
        .get("active_token_id")
        .and_then(Option::as_deref)
        .filter(|s| !s.is_empty())
        .map(|s| json!(s))
        .unwrap_or_else(|| {
            tokens
                .first()
                .map(|t| t["id"].clone())
                .unwrap_or(Value::Null)
        });
    credentials::validate(&json!({"tokens":tokens,"active_token_id":active}))?;
    let state = vault.load()?;
    let mut merged = state["tokens"].as_array().unwrap().clone();
    for token in tokens {
        if let Some(existing) = merged.iter_mut().find(|t| t["id"] == token["id"]) {
            if existing["token"] != token["token"] {
                return Err(CredentialError(
                    "Legacy credential IDs conflict with the vault; settings were kept".into(),
                )
                .into());
            }
            *existing = token;
        } else {
            merged.push(token);
        }
    }
    let active = if state["active_token_id"].is_null() {
        active
    } else {
        state["active_token_id"].clone()
    };
    vault.save(&json!(merged), &active)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM settings WHERE key='token'", [])?;
    tx.execute(
        "INSERT OR REPLACE INTO settings VALUES ('saved_tokens',?)",
        [serde_json::to_string(&credentials::metadata(&merged))?],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO settings VALUES ('active_token_id',?)",
        [active.as_str()],
    )?;
    tx.commit()?;
    Ok(true)
}
pub fn initialize(path: &Path) -> Result<InitReport> {
    let vault = Vault::platform(Paths::for_database(path)?);
    initialize_with_vault(path, &vault)
}
pub fn initialize_with_vault(path: &Path, vault: &Vault) -> Result<InitReport> {
    initialize_with_hook(path, vault, |_| Ok(()))
}
pub fn initialize_with_hook(
    path: &Path,
    vault: &Vault,
    before_commit: impl FnOnce(&Connection) -> Result<()>,
) -> Result<InitReport> {
    let path = credentials::resolve(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut db = Connection::open(&path)?;
    db.busy_timeout(Duration::from_secs(30))?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > 10 {
        return Err("This archive uses a newer storage format; update Searchcord first".into());
    }
    let credentials_migrated = migrate_credentials(&mut db, vault)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    let message_columns = columns(&db, "messages")?;
    let migrate = !message_columns.is_empty() && !message_columns.contains("rowid");
    let history_columns = columns(&db, "message_history")?;
    let migrate_history = !migrate && history_columns.contains("attachments");
    let has_fts = exists(&db, "messages_fts")?;
    let has_stats = exists(&db, "stats_counts")?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut backup = if migrate || migrate_history {
        Some(recovery::backup(&path)?)
    } else {
        None
    };
    tx.execute_batch("DROP VIEW IF EXISTS message_records;")?;
    object_sql(&tx, false)?;
    if !columns(&tx, "authors")?.contains("last_message_id") {
        tx.execute_batch(
            "ALTER TABLE authors ADD COLUMN last_message_id TEXT NOT NULL DEFAULT '0'",
        )?;
    }
    for name in ["pending_after_message_id", "pending_before_message_id"] {
        if !columns(&tx, "scrape_cursors")?.contains(name) {
            tx.execute_batch(&format!(
                "ALTER TABLE scrape_cursors ADD COLUMN {name} TEXT"
            ))?;
        }
    }
    if migrate {
        tx.execute_batch("CREATE TABLE channel_authors_compact(channel_id TEXT NOT NULL,author_id TEXT NOT NULL,PRIMARY KEY(channel_id,author_id)) WITHOUT ROWID; INSERT INTO channel_authors_compact SELECT * FROM channel_authors; DROP TABLE channel_authors; ALTER TABLE channel_authors_compact RENAME TO channel_authors;")?;
        migrate_messages(&tx, &message_columns)?;
    } else if migrate_history {
        migrate_old_history(&tx)?;
    }
    object_sql(&tx, false)?;
    if migrate || !has_stats {
        tx.execute("DELETE FROM stats_counts", [])?;
        tx.execute(
            "INSERT INTO stats_counts SELECT 'total','',COUNT(*) FROM messages HAVING COUNT(*)>0",
            [],
        )?;
        for (kind, expression) in [
            ("guild", "COALESCE(CAST(guild_id AS TEXT),'')"),
            ("channel", "COALESCE(CAST(channel_id AS TEXT),'')"),
            ("author", "COALESCE(CAST(author_id AS TEXT),'')"),
            ("day", "date(((id >> 22)+1420070400000)/1000.0,'unixepoch')"),
            (
                "hour",
                "strftime('%H',((id >> 22)+1420070400000)/1000.0,'unixepoch')",
            ),
        ] {
            tx.execute(&format!("INSERT INTO stats_counts SELECT ?,{expression},COUNT(*) FROM messages GROUP BY {expression}"),[kind])?;
        }
    }
    if migrate || !has_fts {
        tx.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES ('rebuild'); INSERT INTO messages_fts(messages_fts) VALUES ('optimize');")?;
    }
    object_sql(&tx, true)?;
    tx.execute_batch("PRAGMA user_version=10")?;
    before_commit(&tx)?;
    tx.commit()?;
    if migrate || migrate_history {
        db.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        if db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? != "ok" {
            return Err("Migrated database failed integrity check; backup retained".into());
        }
        db.execute(
            "INSERT INTO messages_fts(messages_fts,rank) VALUES ('integrity-check',1)",
            [],
        )?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        if let Some(raw) = &backup {
            match recovery::pack_backup(raw) {
                Ok(packed) => backup = Some(packed),
                Err(_) => {
                    eprintln!("Could not compress recovery backup; uncompressed copy retained")
                }
            }
        }
    }
    Ok(InitReport {
        migrated: migrate || migrate_history,
        backup,
        credentials_migrated,
    })
}
fn migrate_messages(db: &Connection, columns: &BTreeSet<String>) -> Result<()> {
    let base: BTreeSet<_> = ["id", "channel_id", "guild_id", "author_id", "content"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let historical: BTreeSet<_> = [
        "timestamp",
        "attachments",
        "guild_name",
        "channel_name",
        "author_name",
        "guild_name_id",
        "channel_name_id",
        "author_name_id",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if !base.is_subset(columns)
        || columns
            .iter()
            .any(|c| !base.contains(c) && !historical.contains(c) && c != "image_urls")
    {
        return Err("Unrecognized message columns; refusing a potentially lossy migration".into());
    }
    db.execute_batch("DROP VIEW IF EXISTS message_records; DROP TRIGGER IF EXISTS messages_stats_insert; DROP TRIGGER IF EXISTS messages_stats_delete; DROP TRIGGER IF EXISTS messages_fts_insert; DROP TRIGGER IF EXISTS messages_fts_delete; DROP TRIGGER IF EXISTS messages_fts_update; DROP TABLE IF EXISTS messages_fts;")?;
    if columns.contains("channel_name") {
        legacy_metadata(db)?;
    }
    db.execute_batch("CREATE TABLE messages_compact(rowid INTEGER PRIMARY KEY,id INTEGER NOT NULL UNIQUE,channel_id INTEGER,guild_id INTEGER,author_id INTEGER,content TEXT,image_urls TEXT)")?;
    let count: i64 = db.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    db.create_scalar_function(
        "legacy_images",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let raw = context.get::<Option<String>>(0)?;
            let attachments = raw
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            let attachments = json!(
                attachments
                    .iter()
                    .filter(|v| v.is_string())
                    .map(|url| json!({"url":url}))
                    .collect::<Vec<_>>()
            );
            Ok(storage::image_urls(&attachments))
        },
    )?;
    let images = if columns.contains("image_urls") {
        "image_urls"
    } else {
        "legacy_images(attachments)"
    };
    db.execute(&format!("INSERT INTO messages_compact(id,channel_id,guild_id,author_id,content,image_urls) SELECT CAST(id AS INTEGER),channel_id,guild_id,author_id,content,{images} FROM messages ORDER BY CAST(id AS INTEGER)"),[])?;
    if db.query_row("SELECT COUNT(*) FROM messages_compact", [], |r| {
        r.get::<_, i64>(0)
    })? != count
    {
        return Err("Message migration row count mismatch".into());
    }
    if columns.contains("attachments") {
        db.execute("INSERT OR IGNORE INTO legacy_message_attachments SELECT CAST(id AS INTEGER),attachments FROM messages WHERE attachments IS NOT NULL",[])?;
    }
    let mut comparisons = vec![
        "CAST(old.id AS TEXT) IS CAST(new.id AS TEXT)",
        "CAST(old.channel_id AS TEXT) IS CAST(new.channel_id AS TEXT)",
        "CAST(old.guild_id AS TEXT) IS CAST(new.guild_id AS TEXT)",
        "CAST(old.author_id AS TEXT) IS CAST(new.author_id AS TEXT)",
        "old.content IS new.content",
    ];
    if columns.contains("image_urls") {
        comparisons.push("old.image_urls IS new.image_urls");
    }
    if db.query_row(&format!("SELECT 1 FROM messages old JOIN messages_compact new ON new.id=CAST(old.id AS INTEGER) WHERE NOT ({}) LIMIT 1",comparisons.join(" AND ")),[],|_|Ok(true)).optional()?.unwrap_or(false){return Err("Message values did not round-trip; migration rolled back".into());}
    if columns
        .iter()
        .any(|c| historical.contains(c) && c != "attachments")
    {
        for name in ["guild_name", "channel_name", "author_name"] {
            if columns.contains(name) {
                db.execute(&format!("INSERT OR IGNORE INTO name_values(value) SELECT DISTINCT {name} FROM messages WHERE {name} IS NOT NULL"),[])?;
            }
        }
        let reconstructed = timestamp("CAST(old.id AS INTEGER)");
        let (timestamp, override_) = if columns.contains("timestamp") {
            (
                format!(
                    "CASE WHEN old.timestamp IS {reconstructed} THEN NULL ELSE old.timestamp END"
                ),
                format!("old.timestamp IS NOT {reconstructed}"),
            )
        } else {
            ("NULL".into(), "0".into())
        };
        let expressions: Vec<_> = ["guild_name", "channel_name", "author_name"]
            .iter()
            .map(|name| {
                if columns.contains(*name) {
                    format!("(SELECT id FROM name_values WHERE value=old.{name})")
                } else if columns.contains(&format!("{name}_id")) {
                    format!("old.{name}_id")
                } else {
                    "NULL".into()
                }
            })
            .collect();
        db.execute(&format!("INSERT INTO message_history(rowid,timestamp,timestamp_override,guild_name_id,channel_name_id,author_name_id) SELECT new.rowid,{timestamp},{override_},{} FROM messages old JOIN messages_compact new ON new.id=CAST(old.id AS INTEGER)",expressions.join(",")),[])?;
        let checks: Vec<_> = columns
            .intersection(&historical)
            .filter(|c| c.as_str() != "attachments")
            .map(|name| {
                let restored = if ["guild_name", "channel_name", "author_name"]
                    .contains(&name.as_str())
                {
                    format!("(SELECT value FROM name_values WHERE id=h.{name}_id)")
                } else if name == "timestamp" {
                    format!(
                        "CASE WHEN h.timestamp_override THEN h.timestamp ELSE {reconstructed} END"
                    )
                } else {
                    format!("h.{name}")
                };
                format!("old.{name} IS {restored}")
            })
            .collect();
        if db.query_row(&format!("SELECT 1 FROM messages old JOIN messages_compact new ON new.id=CAST(old.id AS INTEGER) LEFT JOIN message_history h ON h.rowid=new.rowid WHERE h.rowid IS NULL OR NOT ({}) LIMIT 1",checks.join(" AND ")),[],|_|Ok(true)).optional()?.unwrap_or(false){return Err("Historical values did not round-trip; migration rolled back".into());}
    }
    db.execute_batch("DROP TABLE messages; ALTER TABLE messages_compact RENAME TO messages;")?;
    Ok(())
}
fn sql_text(row: &rusqlite::Row, index: usize) -> rusqlite::Result<Option<String>> {
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(index)? {
        ValueRef::Null => None,
        ValueRef::Integer(n) => Some(n.to_string()),
        ValueRef::Real(n) => Some(n.to_string()),
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(_) => return Err(rusqlite::Error::InvalidQuery),
    })
}
fn legacy_metadata(db: &Connection) -> Result<()> {
    for (table, id, name, extra) in [
        ("guilds", "guild_id", "guild_name", ""),
        ("channels", "channel_id", "channel_name", ", guild_id"),
        ("authors", "author_id", "author_name", ", id"),
    ] {
        let sql = format!(
            "SELECT {id},{name}{extra} FROM (SELECT {id},{name}{extra},ROW_NUMBER() OVER (PARTITION BY {id} ORDER BY CAST(id AS INTEGER) DESC) rn FROM messages WHERE {id} IS NOT NULL) WHERE rn=1"
        );
        let mut statement = db.prepare(&sql)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let id = sql_text(row, 0)?;
            let name: Option<String> = row.get(1)?;
            match table {
                "guilds" => {
                    db.execute(
                        "INSERT OR IGNORE INTO guilds(id,name) VALUES (?,?)",
                        params![id, name.unwrap_or_default()],
                    )?;
                }
                "channels" => {
                    db.execute(
                        "INSERT OR IGNORE INTO channels(id,name,guild_id) VALUES (?,?,?)",
                        params![id, name.unwrap_or_default(), sql_text(row, 2)?],
                    )?;
                }
                _ => {
                    db.execute(
                        "INSERT OR IGNORE INTO authors VALUES (?,?,?)",
                        params![
                            id,
                            name.unwrap_or_default(),
                            sql_text(row, 2)?.unwrap_or_default()
                        ],
                    )?;
                }
            }
        }
    }
    db.execute_batch("INSERT OR IGNORE INTO channel_authors SELECT DISTINCT channel_id,author_id FROM messages WHERE channel_id IS NOT NULL AND author_id IS NOT NULL; INSERT OR IGNORE INTO scrape_cursors(channel_id,guild_id,newest_message_id,oldest_message_id) SELECT channel_id,MAX(guild_id),CAST(MAX(CAST(id AS INTEGER)) AS TEXT),CAST(MIN(CAST(id AS INTEGER)) AS TEXT) FROM messages WHERE channel_id IS NOT NULL GROUP BY channel_id;")?;
    Ok(())
}
fn migrate_old_history(db: &Connection) -> Result<()> {
    db.execute_batch("DROP VIEW IF EXISTS message_records; DROP TRIGGER IF EXISTS messages_history_delete; ALTER TABLE message_history RENAME TO message_history_full; INSERT OR IGNORE INTO legacy_message_attachments SELECT m.id,h.attachments FROM message_history_full h JOIN messages m ON m.rowid=h.rowid WHERE h.attachments IS NOT NULL; CREATE TABLE message_history(rowid INTEGER PRIMARY KEY,timestamp TEXT,timestamp_override INTEGER NOT NULL DEFAULT 0,guild_name_id INTEGER,channel_name_id INTEGER,author_name_id INTEGER);")?;
    let restored = timestamp("m.id");
    db.execute(&format!("INSERT INTO message_history SELECT h.rowid,CASE WHEN h.timestamp IS {restored} THEN NULL ELSE h.timestamp END,h.timestamp IS NOT {restored},h.guild_name_id,h.channel_name_id,h.author_name_id FROM message_history_full h JOIN messages m ON m.rowid=h.rowid"),[])?;
    let count = |table: &str| {
        db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })
    };
    if count("message_history_full")? != count("message_history")? {
        return Err("Historical row count mismatch; migration rolled back".into());
    }
    if db.query_row(&format!("SELECT 1 FROM message_history_full h JOIN message_history compact ON compact.rowid=h.rowid JOIN messages m ON m.rowid=h.rowid WHERE h.timestamp IS NOT CASE WHEN compact.timestamp_override THEN compact.timestamp ELSE {restored} END OR h.guild_name_id IS NOT compact.guild_name_id OR h.channel_name_id IS NOT compact.channel_name_id OR h.author_name_id IS NOT compact.author_name_id LIMIT 1"),[],|_|Ok(true)).optional()?.unwrap_or(false){return Err("Historical values did not round-trip; migration rolled back".into());}
    db.execute_batch("DROP TABLE message_history_full")?;
    Ok(())
}
