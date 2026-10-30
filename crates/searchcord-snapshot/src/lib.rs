//! Lossless v101 database generation; publication/media are separate operations.
mod publication;
use flate2::{Compression, write::ZlibEncoder};
pub use publication::{
    MediaReport, build_snapshot, export_media, export_media_with_hook, publish_with_hooks,
    resolve_path,
};
use rusqlite::{Connection, OptionalExtension, params};
use searchcord_archive::{
    codec::{self, FIELDS},
    search::short_token,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::Write,
    path::Path,
    time::Instant,
};

pub type Result<T> = std::result::Result<T, SnapshotError>;
#[derive(Debug)]
pub struct SnapshotError(pub String);
impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for SnapshotError {}
impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<rusqlite::Error> for SnapshotError {
    fn from(e: rusqlite::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for SnapshotError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}
fn invalid(message: &str) -> SnapshotError {
    SnapshotError(message.to_owned())
}
const METADATA: [&str; 6] = [
    "guilds",
    "channels",
    "authors",
    "profiles",
    "profile_details",
    "stats_counts",
];
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub messages: u64,
    pub source_bytes: u64,
    pub snapshot_bytes: u64,
    pub source_bytes_per_message: f64,
    pub snapshot_bytes_per_message: f64,
    pub saved_percent: f64,
    pub records_sha256: String,
    pub fields_verified: Vec<String>,
    pub integrity: String,
    pub source_pages: Option<BTreeMap<String, u64>>,
    pub snapshot_pages: Option<BTreeMap<String, u64>>,
    pub source_main_file_unchanged: bool,
    pub build_seconds: f64,
}
fn round2(value: f64) -> f64 {
    format!("{value:.2}").parse().unwrap_or(value)
}
pub fn page_usage(db: &Connection, schema: &str) -> Result<Option<BTreeMap<String, u64>>> {
    let statement = db.prepare("SELECT name,SUM(pgsize) FROM dbstat(?) GROUP BY name");
    let mut statement = match statement {
        Ok(s) => s,
        Err(error) if error.to_string().contains("no such table: dbstat") => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(
        statement
            .query_map([schema], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?,
    ))
}
fn json_field(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(match row.get_ref(index)? {
        rusqlite::types::ValueRef::Null => Value::Null,
        rusqlite::types::ValueRef::Integer(n) => Value::from(n),
        rusqlite::types::ValueRef::Text(_) => Value::String(row.get(index)?),
        _ => {
            return Err(rusqlite::Error::InvalidColumnType(
                index,
                index.to_string(),
                row.get_ref(index)?.data_type(),
            ));
        }
    })
}
pub fn digest_row(hash: &mut Sha256, row: &[Value]) -> Result<()> {
    let bytes = serde_json::to_vec(row)?;
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    Ok(())
}
pub fn records_digest(db: &Connection, order: &str) -> Result<String> {
    if !["id", "storage_rowid"].contains(&order) {
        return Err(invalid("Invalid verification order"));
    }
    let mut statement = db.prepare(&format!(
        "SELECT {} FROM message_records ORDER BY {order}",
        FIELDS.join(",")
    ))?;
    let mut rows = statement.query([])?;
    let mut hash = Sha256::new();
    while let Some(row) = rows.next()? {
        let fields = (0..10)
            .map(|i| json_field(row, i))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        digest_row(&mut hash, &fields)?;
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn short_terms(contents: &[Vec<Value>]) -> String {
    let mut terms = BTreeSet::new();
    for row in contents {
        let text = row[1]
            .as_str()
            .unwrap_or("")
            .split('\0')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let chars: Vec<_> = text.chars().collect();
        for c in &chars {
            terms.insert(short_token(&c.to_string()));
        }
        for pair in chars.windows(2) {
            terms.insert(short_token(&pair.iter().collect::<String>()));
        }
    }
    terms.into_iter().collect::<Vec<_>>().join(" ")
}

/// Write a verified database at an exclusive, fresh temporary path. Publication
/// callers must keep this path private until the returned report is verified.
pub fn build_database(source: &Path, destination: &Path, block_rows: usize) -> Result<Report> {
    build_database_with_hook(source, destination, block_rows, |_, _| Ok(()))
}
/// The hook observes completed packed blocks and may cancel before publication.
pub fn build_database_with_hook(
    source: &Path,
    destination: &Path,
    block_rows: usize,
    hook: impl FnMut(u64, u64) -> Result<()>,
) -> Result<Report> {
    if !source.is_file() {
        return Err(invalid("Source archive does not exist"));
    }
    if destination.exists() || destination.is_symlink() {
        return Err(invalid(
            "Refusing to replace an existing file or the source archive",
        ));
    }
    if !(16..=512).contains(&block_rows) {
        return Err(invalid("Block size must be between 16 and 512"));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    drop(
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)?,
    );
    let result = build_inner(source, destination, block_rows, hook);
    if result.is_err() {
        let _ = fs::remove_file(destination);
        let mut journal = destination.as_os_str().to_os_string();
        journal.push("-journal");
        let _ = fs::remove_file(journal);
    }
    result
}
fn build_inner(
    source: &Path,
    destination: &Path,
    block_rows: usize,
    mut hook: impl FnMut(u64, u64) -> Result<()>,
) -> Result<Report> {
    let started = Instant::now();
    let source = source.canonicalize()?;
    let stamp = fs::metadata(&source)?;
    let source_stamp = (stamp.len(), stamp.modified()?);
    let db = Connection::open(destination)?;
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=OFF;")?;
    db.execute_batch(include_str!("schema.sql"))?;
    let mut uri =
        url::Url::from_file_path(&source).map_err(|_| invalid("Source archive does not exist"))?;
    uri.query_pairs_mut().append_pair("mode", "ro");
    db.execute("ATTACH DATABASE ? AS source", [uri.as_str()])?;
    db.execute_batch("BEGIN")?;
    let version: i64 = db.query_row("PRAGMA source.user_version", [], |r| r.get(0))?;
    if ![9, 10].contains(&version) {
        return Err(invalid("Export needs a schema v9/v10 collection archive"));
    }
    let expected: u64 = db.query_row("SELECT COUNT(*) FROM source.messages", [], |r| r.get(0))?;
    let payloads = db
        .query_row(
            "SELECT 1 FROM source.sqlite_master WHERE name='message_payloads' AND type='table'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    if payloads {
        db.execute("INSERT INTO message_attachments SELECT message_id,json_extract(payload,'$.attachments') FROM source.message_payloads WHERE json_array_length(json_extract(payload,'$.attachments'))>0",[])?;
    }
    let source_bytes: u64 = db.query_row("PRAGMA source.page_count", [], |r| r.get::<_, u64>(0))?
        * db.query_row("PRAGMA source.page_size", [], |r| r.get::<_, u64>(0))?;
    let source_pages = page_usage(&db, "source")?;
    for table in METADATA {
        let sql: Option<String> = db
            .query_row(
                "SELECT sql FROM source.sqlite_master WHERE type='table' AND name=?",
                [table],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(sql) = sql {
            db.execute_batch(&sql)?;
            db.execute(
                &format!("INSERT INTO {table} SELECT * FROM source.{table}"),
                [],
            )?;
        } else if table == "profile_details" {
            db.execute_batch("CREATE TABLE profile_details(user_id TEXT PRIMARY KEY,payload TEXT NOT NULL,fetched_at TEXT NOT NULL)")?;
        } else {
            return Err(invalid("Missing required archive metadata table"));
        }
    }
    db.execute_batch("INSERT INTO ref_values(value) SELECT DISTINCT CAST(author_id AS TEXT) FROM source.messages WHERE author_id IS NOT NULL;
        INSERT INTO message_sources(channel_id,guild_id) SELECT DISTINCT CAST(channel_id AS TEXT),CAST(guild_id AS TEXT) FROM source.messages;
        INSERT INTO record_names(value) SELECT value FROM source.name_values WHERE value IS NOT NULL UNION SELECT name FROM source.guilds WHERE name IS NOT NULL
        UNION SELECT name FROM source.channels WHERE name IS NOT NULL UNION SELECT name FROM source.authors WHERE name IS NOT NULL;")?;
    let fields = FIELDS
        .iter()
        .map(|c| format!("r.{c}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut count = 0_u64;
    let mut hash = Sha256::new();
    {
        // Keep the source cursor in this scope so failures close all statements
        // before the caller removes its owned temporary file (also on Windows).
        let mut statement=db.prepare(&format!("SELECT {fields},s.ref,a.ref,nc.id,ng.id,na.id FROM source.message_records r
            LEFT JOIN message_sources s ON s.channel_id IS r.channel_id AND s.guild_id IS r.guild_id LEFT JOIN ref_values a ON a.value=r.author_id
            LEFT JOIN record_names nc ON nc.value=r.channel_name LEFT JOIN record_names ng ON ng.value=r.guild_name
            LEFT JOIN record_names na ON na.value=r.author_name ORDER BY r.id"))?;
        let mut rows = statement.query([])?;
        let mut body = Vec::with_capacity(block_rows);
        let mut first = None;
        loop {
            while body.len() < block_rows {
                let Some(row) = rows.next()? else { break };
                count += 1;
                let values = (0..10)
                    .map(|i| json_field(row, i))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                digest_row(&mut hash, &values)?;
                if first.is_none() {
                    first = values[0].as_i64();
                }
                db.execute(
                    "INSERT INTO message_index VALUES (?,?,?)",
                    params![
                        count,
                        row.get::<_, Option<i64>>(10)?,
                        row.get::<_, Option<i64>>(11)?
                    ],
                )?;
                db.execute(
                    "INSERT INTO messages_fts(rowid,content) VALUES (?,?)",
                    params![count, values[4].as_str()],
                )?;
                body.push(vec![
                    values[0].clone(),
                    values[4].clone(),
                    values[5].clone(),
                    values[6].clone(),
                    json_field(row, 12)?,
                    json_field(row, 13)?,
                    json_field(row, 14)?,
                ]);
            }
            if body.is_empty() {
                break;
            }
            let block = (count - 1) / block_rows as u64 + 1;
            db.execute(
                "INSERT INTO message_short_fts(rowid,tokens) VALUES (?,?)",
                params![block, short_terms(&body)],
            )?;
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(9));
            encoder.write_all(&serde_json::to_vec(&body)?)?;
            let payload = encoder.finish()?;
            db.execute(
                "INSERT INTO message_blocks VALUES (?,?,?)",
                params![
                    block,
                    first.ok_or_else(|| invalid("Invalid public message ID"))?,
                    payload
                ],
            )?;
            hook(count, expected)?;
            body.clear();
            first = None;
        }
    }
    if count != expected {
        return Err(invalid("Message count did not round-trip"));
    }
    db.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES ('optimize'); INSERT INTO message_short_fts(message_short_fts) VALUES ('optimize');
        CREATE INDEX idx_snapshot_source ON message_index(source_id); CREATE INDEX idx_snapshot_author ON message_index(author_id);
        CREATE INDEX idx_authors_name ON authors(name COLLATE NOCASE); CREATE INDEX idx_profiles_username ON profiles(username COLLATE NOCASE);")?;
    let records_sha256 = format!("{:x}", hash.finalize());
    for (key, value) in [
        ("format", "searchcord-search-snapshot".to_owned()),
        ("block_rows", block_rows.to_string()),
        ("messages", count.to_string()),
        ("records_sha256", records_sha256.clone()),
    ] {
        db.execute(
            "INSERT INTO snapshot_info VALUES (?,?)",
            params![key, value],
        )?;
    }
    db.execute_batch("COMMIT; DETACH DATABASE source; ANALYZE; VACUUM;")?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(invalid("Snapshot failed SQLite integrity check"));
    }
    db.execute_batch("INSERT INTO messages_fts(messages_fts) VALUES ('integrity-check'); INSERT INTO message_short_fts(message_short_fts) VALUES ('integrity-check');")?;
    codec::install_reader(&db)?;
    if records_digest(&db, "storage_rowid")? != records_sha256 {
        return Err(invalid("Message fields did not round-trip"));
    }
    if db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        return Err(invalid("Snapshot unexpectedly contains settings"));
    }
    db.execute_batch(&format!("PRAGMA user_version={}", codec::VERSION))?;
    let snapshot_pages = page_usage(&db, "main")?;
    drop(db);
    let snapshot_bytes = fs::metadata(destination)?.len();
    let stamp = fs::metadata(&source)?;
    Ok(Report {
        messages: count,
        source_bytes,
        snapshot_bytes,
        source_bytes_per_message: if count > 0 {
            round2(source_bytes as f64 / count as f64)
        } else {
            0.0
        },
        snapshot_bytes_per_message: if count > 0 {
            round2(snapshot_bytes as f64 / count as f64)
        } else {
            0.0
        },
        saved_percent: round2((1.0 - snapshot_bytes as f64 / source_bytes as f64) * 100.0),
        records_sha256,
        fields_verified: FIELDS.iter().map(|s| (*s).to_owned()).collect(),
        integrity,
        source_pages,
        snapshot_pages,
        source_main_file_unchanged: source_stamp == (stamp.len(), stamp.modified()?),
        build_seconds: round2(started.elapsed().as_secs_f64()),
    })
}
