//! Existing v101 zlib/JSON blocks and connection-local SQLite view functions.
use flate2::read::ZlibDecoder;
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, types::Value as SqlValue};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    io::{self, Read},
    mem::size_of,
    sync::{Arc, Mutex},
};

pub const VERSION: i64 = 101;
pub const BLOCK_ROWS: usize = 128;
pub const FIELDS: [&str; 10] = [
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
const MAX_BLOCK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
#[derive(Deserialize)]
struct PackedRow(
    i64,            // Message ID
    Option<String>, // Content
    Option<String>, // Timestamp
    Option<String>, // Image URLs
    Option<i64>,    // Channel name reference
    Option<i64>,    // Guild name reference
    Option<i64>,    // Author name reference
);
type Block = Arc<Vec<PackedRow>>;
fn invalid(message: &str) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    )))
}
#[derive(Default)]
struct Cache {
    blocks: VecDeque<(i64, Block, usize)>,
    names: VecDeque<(i64, String)>,
    bytes: usize,
}
#[derive(Clone)]
pub struct PackedDecoder {
    pub block_rows: usize,
    pub messages: usize,
    cache: Arc<Mutex<Cache>>,
}
#[derive(Debug)]
pub struct CacheUsage {
    pub blocks: usize,
    pub names: usize,
    pub bytes: usize,
}
impl PackedDecoder {
    pub fn usage(&self) -> rusqlite::Result<CacheUsage> {
        let c = self
            .cache
            .lock()
            .map_err(|_| invalid("Snapshot cache is unavailable"))?;
        Ok(CacheUsage {
            blocks: c.blocks.len(),
            names: c.names.len(),
            bytes: c.bytes,
        })
    }
    fn block(&self, db: &Connection, number: i64) -> rusqlite::Result<Block> {
        if number < 1 {
            return Err(invalid("Invalid message block number"));
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| invalid("Snapshot cache is unavailable"))?;
        if let Some(position) = cache.blocks.iter().position(|b| b.0 == number) {
            let item = cache
                .blocks
                .remove(position)
                .ok_or_else(|| invalid("Snapshot cache changed"))?;
            let block = item.1.clone();
            cache.blocks.push_front(item);
            return Ok(block);
        }
        let (first, payload): (i64, Vec<u8>) = db
            .query_row(
                "SELECT first_id,payload FROM message_blocks WHERE block=?",
                [number],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| invalid("Missing message block"))?;
        let mut bytes = Vec::new();
        ZlibDecoder::new(payload.as_slice())
            .take(MAX_BLOCK_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("Invalid compressed message block"))?;
        if bytes.len() as u64 > MAX_BLOCK_BYTES {
            return Err(invalid("Message block exceeds decoding limit"));
        }
        let body: Vec<PackedRow> =
            serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid message block JSON"))?;
        let start = (number as usize - 1)
            .checked_mul(self.block_rows)
            .ok_or_else(|| invalid("Invalid message block range"))?;
        let expected = self.messages.saturating_sub(start).min(self.block_rows);
        if expected == 0 || body.len() != expected {
            return Err(invalid("Invalid message block length"));
        }
        let mut previous = 0;
        let mut footprint = body.capacity() * size_of::<PackedRow>();
        for row in &body {
            let id = row.0;
            if id <= previous {
                return Err(invalid("Invalid public message ID order"));
            }
            if previous == 0 && id != first {
                return Err(invalid("Message block boundary does not match"));
            }
            previous = id;
            if [row.4, row.5, row.6].into_iter().flatten().any(|n| n <= 0) {
                return Err(invalid("Invalid message record field"));
            }
            footprint += [&row.1, &row.2, &row.3]
                .into_iter()
                .flatten()
                .map(String::capacity)
                .sum::<usize>();
        }
        let body = Arc::new(body);
        cache.bytes += footprint;
        cache.blocks.push_front((number, body.clone(), footprint));
        while cache.blocks.len() > 16 || cache.bytes > MAX_CACHE_BYTES {
            if let Some(item) = cache.blocks.pop_back() {
                cache.bytes = cache.bytes.saturating_sub(item.2);
            }
        }
        Ok(body)
    }
    fn field(&self, db: &Connection, rowid: i64, index: usize) -> rusqlite::Result<SqlValue> {
        if rowid < 1 || rowid as u64 > self.messages as u64 {
            return Err(invalid("Invalid internal message reference"));
        }
        let block = self.block(db, (rowid - 1) / self.block_rows as i64 + 1)?;
        let row = block
            .get((rowid - 1) as usize % self.block_rows)
            .ok_or_else(|| invalid("Missing message row"))?;
        let key = match index {
            0 => return Ok(SqlValue::Integer(row.0)),
            1 => return Ok(row.1.clone().map_or(SqlValue::Null, SqlValue::Text)),
            2 => return Ok(row.2.clone().map_or(SqlValue::Null, SqlValue::Text)),
            3 => return Ok(row.3.clone().map_or(SqlValue::Null, SqlValue::Text)),
            4 => row.4,
            5 => row.5,
            6 => row.6,
            _ => unreachable!("registered record field"),
        };
        if let Some(key) = key {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| invalid("Snapshot cache is unavailable"))?;
            if let Some(position) = cache.names.iter().position(|n| n.0 == key) {
                let item = cache
                    .names
                    .remove(position)
                    .ok_or_else(|| invalid("Snapshot cache changed"))?;
                let value = item.1.clone();
                cache.names.push_front(item);
                return Ok(SqlValue::Text(value));
            }
            let name: String = db
                .query_row("SELECT value FROM record_names WHERE id=?", [key], |r| {
                    r.get(0)
                })
                .optional()?
                .ok_or_else(|| invalid("Missing record name"))?;
            cache.names.push_front((key, name.clone()));
            if cache.names.len() > 256 {
                cache.names.pop_back();
            }
            return Ok(SqlValue::Text(name));
        }
        Ok(SqlValue::Null)
    }
    pub fn row_before_id(&self, db: &Connection, value: i64) -> rusqlite::Result<i64> {
        let number: Option<i64> = db
            .query_row(
                "SELECT block FROM message_blocks WHERE first_id<? ORDER BY first_id DESC LIMIT 1",
                [value],
                |r| r.get(0),
            )
            .optional()?;
        let Some(number) = number else { return Ok(0) };
        let block = self.block(db, number)?;
        let position = block.partition_point(|r| r.0 < value);
        Ok((number - 1) * self.block_rows as i64 + position as i64)
    }
}

pub fn install_reader(db: &Connection) -> rusqlite::Result<PackedDecoder> {
    let info = |key: &str| -> rusqlite::Result<usize> {
        let value: String =
            db.query_row("SELECT value FROM snapshot_info WHERE key=?", [key], |r| {
                r.get(0)
            })?;
        value
            .parse()
            .map_err(|_| invalid("Invalid snapshot metadata"))
    };
    let block_rows = info("block_rows")?;
    if !(16..=512).contains(&block_rows) {
        return Err(invalid("Invalid snapshot block size"));
    }
    let decoder = PackedDecoder {
        block_rows,
        messages: info("messages")?,
        cache: Arc::new(Mutex::new(Cache::default())),
    };
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC;
    for (index, name) in [
        "id",
        "content",
        "timestamp",
        "image_urls",
        "channel_name",
        "guild_name",
        "author_name",
    ]
    .iter()
    .enumerate()
    {
        let decoder = decoder.clone();
        db.create_scalar_function(format!("record_{name}").as_str(), 1, flags, move |ctx| {
            // SAFETY: this handle is borrowed only during the invoking SQLite
            // callback, never stored/sent, and only issues reads on that thread.
            let connection = unsafe { ctx.get_connection()? };
            decoder.field(&connection, ctx.get(0)?, index)
        })?;
    }
    let before = decoder.clone();
    db.create_scalar_function("row_before_id", 1, flags, move |ctx| {
        // SAFETY: same callback-local, read-only borrowing as above.
        let connection = unsafe { ctx.get_connection()? };
        before.row_before_id(&connection, ctx.get(0)?)
    })?;
    Ok(decoder)
}
