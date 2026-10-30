//! Typed candidate selection. SQLite remains authoritative for literal LIKE.
use crate::codec::PackedDecoder;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    pub q: String,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub author_id: Option<String>,
    pub lower: Option<i64>,
    pub upper: Option<i64>,
    pub before: Option<i64>,
    pub limit: usize,
    pub scan: bool,
    pub window: usize,
}
impl Default for Query {
    fn default() -> Self {
        Self {
            q: String::new(),
            guild_id: None,
            channel_id: None,
            author_id: None,
            lower: None,
            upper: None,
            before: None,
            limit: 40,
            scan: false,
            window: 16384,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
    pub author_id: Option<String>,
    pub content: Option<String>,
    pub timestamp: Option<String>,
    pub image_urls: Option<String>,
    pub channel_name: Option<String>,
    pub guild_name: Option<String>,
    pub author_name: Option<String>,
}
impl Record {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get::<_, i64>("id")?.to_string(),
            channel_id: row.get("channel_id")?,
            guild_id: row.get("guild_id")?,
            author_id: row.get("author_id")?,
            content: row.get("content")?,
            timestamp: row.get("timestamp")?,
            image_urls: row.get("image_urls")?,
            channel_name: row.get("channel_name")?,
            guild_name: row.get("guild_name")?,
            author_name: row.get("author_name")?,
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub records: Vec<Record>,
    pub has_more: bool,
    pub next_cursor: Option<String>,
    pub scan: bool,
    pub partial: bool,
}
pub fn strip(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
}
pub fn like_literal(value: &str) -> String {
    value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}
pub fn short_token(value: &str) -> String {
    format!(
        "{}{}",
        if value.chars().count() == 1 { "u" } else { "p" },
        value
            .chars()
            .map(|c| format!("{:06x}", c as u32))
            .collect::<String>()
    )
}
pub fn trigram_query(q: &str) -> Option<String> {
    let value: Vec<char> = q.split('\0').next().unwrap_or("").chars().collect();
    if value.len() < 3 {
        return None;
    }
    let step = ((value.len() - 2) / 8).max(1);
    let mut seen = HashSet::new();
    let mut terms = vec![];
    for i in (0..value.len() - 2).step_by(step) {
        let gram: String = value[i..i + 3].iter().collect();
        if seen.insert(gram.clone()) {
            terms.push(format!("\"{}\"", gram.replace('"', "\"\"")));
        }
    }
    Some(terms.join(" AND "))
}
fn ids(db: &Connection, sql: &str, params: &[SqlValue]) -> rusqlite::Result<Vec<i64>> {
    db.prepare(sql)?
        .query_map(params_from_iter(params), |r| r.get(0))?
        .collect()
}
pub(crate) fn records(
    db: &Connection,
    keys: &[i64],
    column: &str,
) -> rusqlite::Result<Vec<Record>> {
    if keys.is_empty() {
        return Ok(vec![]);
    }
    let sql = format!(
        "SELECT * FROM message_records WHERE {column} IN ({}) ORDER BY {column} DESC",
        vec!["?"; keys.len()].join(",")
    );
    db.prepare(&sql)?
        .query_map(params_from_iter(keys), Record::from_row)?
        .collect()
}
fn packed_ids(
    db: &Connection,
    decoder: &PackedDecoder,
    query: &Query,
    lower: Option<i64>,
    take: usize,
) -> rusqlite::Result<Vec<i64>> {
    let mut conditions = vec![];
    let mut parameters = vec![];
    let (mut selective, mut multi_source, mut has_author) = (false, false, false);
    for (column, kind, value) in [
        ("guild_id", "guild", &query.guild_id),
        ("channel_id", "channel", &query.channel_id),
        ("author_id", "author", &query.author_id),
    ] {
        let Some(value) = value.as_ref().filter(|v| !v.is_empty()) else {
            continue;
        };
        if column == "author_id" {
            let reference: Option<i64> = db
                .query_row("SELECT ref FROM ref_values WHERE value=?", [value], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(reference) = reference else {
                return Ok(vec![]);
            };
            conditions.push("m.author_id=?".to_owned());
            parameters.push(SqlValue::Integer(reference));
            has_author = true;
        } else {
            let references = ids(
                db,
                &format!("SELECT ref FROM message_sources WHERE {column}=?"),
                &[SqlValue::Text(value.clone())],
            )?;
            if references.is_empty() {
                return Ok(vec![]);
            }
            multi_source |= references.len() > 1;
            conditions.push(format!(
                "m.source_id IN ({})",
                vec!["?"; references.len()].join(",")
            ));
            parameters.extend(references.into_iter().map(SqlValue::Integer));
        }
        let count: Option<i64> = db
            .query_row(
                "SELECT count FROM stats_counts WHERE kind=? AND key=?",
                params![kind, value],
                |r| r.get(0),
            )
            .optional()?;
        selective |= count.is_some_and(|n| n <= 4096);
    }
    for (value, low) in [(lower, true), (query.upper, false), (query.before, false)] {
        if let Some(value) = value {
            let boundary = decoder.row_before_id(db, value)?;
            conditions.push(format!("m.rowid{}?", if low { ">=" } else { "<=" }));
            parameters.push(SqlValue::Integer(if low { boundary + 1 } else { boundary }));
        }
    }
    let q = strip(&query.q);
    let short = q.split('\0').next().unwrap_or("").to_ascii_lowercase();
    let literal = format!("%{}%", like_literal(q));
    if !q.is_empty() && (1..=2).contains(&short.chars().count()) && !selective {
        let first = if let Some(lower) = lower {
            decoder.row_before_id(db, lower)? + 1
        } else {
            1
        };
        let last = match (query.upper, query.before) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let last = if let Some(id) = last {
            decoder.row_before_id(db, id)?
        } else {
            decoder.messages as i64
        };
        let block_rows = decoder.block_rows as i64;
        let blocks = ids(
            db,
            "SELECT rowid FROM message_short_fts WHERE message_short_fts MATCH ? AND rowid BETWEEN ? AND ? ORDER BY rowid DESC",
            &[
                SqlValue::Text(short_token(&short)),
                SqlValue::Integer((first - 1).div_euclid(block_rows) + 1),
                SqlValue::Integer((last - 1).div_euclid(block_rows) + 1),
            ],
        )?;
        let mut result = vec![];
        let condition = conditions
            .iter()
            .cloned()
            .chain([
                "m.rowid BETWEEN ? AND ?".into(),
                "record_content(m.rowid) LIKE ? ESCAPE '!'".into(),
            ])
            .collect::<Vec<_>>()
            .join(" AND ");
        for block in blocks {
            let mut params = parameters.clone();
            params.extend([
                SqlValue::Integer((block - 1) * block_rows + 1),
                SqlValue::Integer(block * block_rows),
                SqlValue::Text(literal.clone()),
                SqlValue::Integer((take - result.len()) as i64),
            ]);
            result.extend(ids(db,&format!("SELECT m.rowid FROM message_index m WHERE {condition} ORDER BY m.rowid DESC LIMIT ?"),&params)?);
            if result.len() == take {
                break;
            }
        }
        return Ok(result);
    }
    let matched = if !q.is_empty() && !selective {
        trigram_query(q)
    } else {
        None
    };
    let (mut relation, order) = if let Some(matched) = matched {
        conditions.insert(0, "f.messages_fts MATCH ?".into());
        parameters.insert(0, SqlValue::Text(matched));
        (
            "messages_fts f JOIN message_index m ON m.rowid=f.rowid".to_owned(),
            "f.rowid",
        )
    } else {
        ("message_index m".to_owned(), "m.rowid")
    };
    if q.is_empty() && multi_source && !selective && !has_author {
        relation.push_str(" NOT INDEXED");
    }
    if !q.is_empty() {
        conditions.push("record_content(m.rowid) LIKE ? ESCAPE '!'".into());
        parameters.push(SqlValue::Text(literal));
    }
    parameters.push(SqlValue::Integer(take as i64));
    ids(
        db,
        &format!(
            "SELECT m.rowid FROM {relation} WHERE {} ORDER BY {order} DESC LIMIT ?",
            if conditions.is_empty() {
                "1".into()
            } else {
                conditions.join(" AND ")
            }
        ),
        &parameters,
    )
}
pub fn packed_search(
    db: &Connection,
    decoder: &PackedDecoder,
    query: &Query,
) -> rusqlite::Result<Page> {
    if !(1..=100).contains(&query.limit) || query.window == 0 || query.window > 1_000_000 {
        return Err(rusqlite::Error::InvalidParameterName(
            "Invalid search limit/window".into(),
        ));
    }
    let take = query.limit + 1;
    let mut scan_cursor = None;
    let candidates = if query.scan {
        let mut conditions = vec![];
        let mut parameters = vec![];
        for (value, low) in [
            (query.lower, true),
            (query.upper, false),
            (query.before, false),
        ] {
            if let Some(value) = value {
                let boundary = decoder.row_before_id(db, value)?;
                conditions.push(format!("rowid{}?", if low { ">=" } else { "<=" }));
                parameters.push(SqlValue::Integer(if low { boundary + 1 } else { boundary }));
            }
        }
        parameters.push(SqlValue::Integer(query.window as i64 + 1));
        let window = ids(
            db,
            &format!(
                "SELECT rowid FROM message_index WHERE {} ORDER BY rowid DESC LIMIT ?",
                if conditions.is_empty() {
                    "1".into()
                } else {
                    conditions.join(" AND ")
                }
            ),
            &parameters,
        )?;
        if window.is_empty() {
            vec![]
        } else {
            let floor: i64 = db.query_row(
                "SELECT record_id(?)",
                [window[query.window.min(window.len()) - 1]],
                |r| r.get(0),
            )?;
            let ids = packed_ids(db, decoder, query, Some(floor), take)?;
            if window.len() > query.window && ids.len() < take {
                scan_cursor = Some(floor.to_string());
            }
            ids
        }
    } else {
        packed_ids(db, decoder, query, query.lower, take)?
    };
    let has_more = candidates.len() > query.limit || scan_cursor.is_some();
    let records = records(
        db,
        &candidates[..query.limit.min(candidates.len())],
        "storage_rowid",
    )?;
    let partial = scan_cursor.is_some();
    let next_cursor = scan_cursor.or_else(|| {
        if has_more {
            records.last().map(|r| r.id.clone())
        } else {
            None
        }
    });
    Ok(Page {
        records,
        has_more,
        next_cursor,
        scan: query.scan,
        partial,
    })
}
