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
fn filters(query: &Query) -> impl Iterator<Item = (&'static str, &'static str, &String)> {
    [
        ("guild_id", "guild", &query.guild_id),
        ("channel_id", "channel", &query.channel_id),
        ("author_id", "author", &query.author_id),
    ]
    .into_iter()
    .filter_map(|(column, kind, value)| {
        value
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| (column, kind, s))
    })
}

fn filter_is_selective(db: &Connection, kind: &str, value: &str) -> rusqlite::Result<bool> {
    const SELECTIVE_FILTER_LIMIT: i64 = 4096;
    let count: Option<i64> = db
        .query_row(
            "SELECT count FROM stats_counts WHERE kind=? AND key=?",
            params![kind, value],
            |r| r.get(0),
        )
        .optional()?;
    Ok(count.is_some_and(|n| n <= SELECTIVE_FILTER_LIMIT))
}

fn standard_scope_is_selective(db: &Connection, query: &Query) -> rusqlite::Result<bool> {
    let mut filters = filters(query).peekable();
    if filters.peek().is_none()
        || !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='stats_counts')",
            [],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Ok(false);
    }
    for (_, kind, value) in filters {
        if filter_is_selective(db, kind, value)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Default)]
struct PackedScope {
    conditions: Vec<String>,
    parameters: Vec<SqlValue>,
    selective: bool,
    multi_source: bool,
    has_author: bool,
}

fn packed_scope(
    db: &Connection,
    decoder: &PackedDecoder,
    query: &Query,
    lower: Option<i64>,
) -> rusqlite::Result<Option<PackedScope>> {
    let mut scope = PackedScope::default();
    for (column, kind, value) in filters(query) {
        if column == "author_id" {
            let reference: Option<i64> = db
                .query_row("SELECT ref FROM ref_values WHERE value=?", [value], |r| {
                    r.get(0)
                })
                .optional()?;
            let Some(reference) = reference else {
                return Ok(None);
            };
            scope.conditions.push("m.author_id=?".to_owned());
            scope.parameters.push(SqlValue::Integer(reference));
            scope.has_author = true;
        } else {
            let references = ids(
                db,
                &format!("SELECT ref FROM message_sources WHERE {column}=?"),
                &[SqlValue::Text(value.clone())],
            )?;
            if references.is_empty() {
                return Ok(None);
            }
            scope.multi_source |= references.len() > 1;
            scope.conditions.push(format!(
                "m.source_id IN ({})",
                vec!["?"; references.len()].join(",")
            ));
            scope
                .parameters
                .extend(references.into_iter().map(SqlValue::Integer));
        }
        scope.selective |= filter_is_selective(db, kind, value)?;
    }
    for (value, low) in [(lower, true), (query.upper, false), (query.before, false)] {
        if let Some(value) = value {
            let boundary = decoder.row_before_id(db, value)?;
            scope
                .conditions
                .push(format!("m.rowid{}?", if low { ">=" } else { "<=" }));
            scope
                .parameters
                .push(SqlValue::Integer(if low { boundary + 1 } else { boundary }));
        }
    }
    Ok(Some(scope))
}

impl PackedScope {
    fn with_text(&mut self, q: &str) -> (&'static str, &'static str) {
        if !q.is_empty() {
            self.conditions
                .push("record_content(m.rowid) LIKE ? ESCAPE '!'".into());
            self.parameters
                .push(SqlValue::Text(format!("%{}%", like_literal(q))));
        }
        if let Some(matched) = (!self.selective).then(|| trigram_query(q)).flatten() {
            self.conditions.insert(0, "f.messages_fts MATCH ?".into());
            self.parameters.insert(0, SqlValue::Text(matched));
            (
                "messages_fts f JOIN message_index m ON m.rowid=f.rowid",
                "f.rowid",
            )
        } else {
            ("message_index m", "m.rowid")
        }
    }
}

fn where_sql(conditions: &[String]) -> String {
    if conditions.is_empty() {
        "1".into()
    } else {
        conditions.join(" AND ")
    }
}

fn packed_ids(
    db: &Connection,
    decoder: &PackedDecoder,
    query: &Query,
    lower: Option<i64>,
    take: usize,
) -> rusqlite::Result<Vec<i64>> {
    let Some(mut scope) = packed_scope(db, decoder, query, lower)? else {
        return Ok(vec![]);
    };
    let q = strip(&query.q);
    let short = q.split('\0').next().unwrap_or("").to_ascii_lowercase();
    if (1..=2).contains(&short.chars().count()) && !scope.selective {
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
        let mut block_query = db.prepare(
            "SELECT rowid FROM message_short_fts WHERE message_short_fts MATCH ? AND rowid BETWEEN ? AND ? ORDER BY rowid DESC",
        )?;
        let mut blocks = block_query.query(params![
            short_token(&short),
            (first - 1).div_euclid(block_rows) + 1,
            (last - 1).div_euclid(block_rows) + 1,
        ])?;
        scope.conditions.push("m.rowid BETWEEN ? AND ?".into());
        scope
            .conditions
            .push("record_content(m.rowid) LIKE ? ESCAPE '!'".into());
        let mut matches = db.prepare(&format!(
            "SELECT m.rowid FROM message_index m WHERE {} ORDER BY m.rowid DESC LIMIT ?",
            where_sql(&scope.conditions),
        ))?;
        let mut result = Vec::with_capacity(take);
        let literal = SqlValue::Text(format!("%{}%", like_literal(q)));
        while let Some(row) = blocks.next()? {
            let block: i64 = row.get(0)?;
            let bounds = [
                SqlValue::Integer((block - 1) * block_rows + 1),
                SqlValue::Integer(block * block_rows),
            ];
            let limit = SqlValue::Integer((take - result.len()) as i64);
            let params = scope
                .parameters
                .iter()
                .chain(&bounds)
                .chain([&literal, &limit]);
            let mut rows = matches.query(params_from_iter(params))?;
            while let Some(row) = rows.next()? {
                result.push(row.get(0)?);
            }
            if result.len() == take {
                break;
            }
        }
        return Ok(result);
    }
    let (relation, order) = scope.with_text(q);
    let hint = if q.is_empty() && scope.multi_source && !scope.selective && !scope.has_author {
        " NOT INDEXED"
    } else {
        ""
    };
    scope.parameters.push(SqlValue::Integer(take as i64));
    ids(
        db,
        &format!(
            "SELECT m.rowid FROM {relation}{hint} WHERE {} ORDER BY {order} DESC LIMIT ?",
            where_sql(&scope.conditions)
        ),
        &scope.parameters,
    )
}

pub(crate) fn count(
    db: &Connection,
    decoder: Option<&PackedDecoder>,
    query: &Query,
) -> rusqlite::Result<i64> {
    let q = strip(&query.q);
    let (relation, conditions, parameters) = if let Some(decoder) = decoder {
        let Some(mut scope) = packed_scope(db, decoder, query, query.lower)? else {
            return Ok(0);
        };
        let (mut relation, _) = scope.with_text(q);
        let short = q.split('\0').next().unwrap_or("").to_ascii_lowercase();
        if (1..=2).contains(&short.chars().count()) && !scope.selective {
            relation = "message_short_fts b CROSS JOIN message_index m ON m.rowid BETWEEN (b.rowid-1)*?+1 AND b.rowid*?";
            scope.conditions.push("b.message_short_fts MATCH ?".into());
            let rows = decoder.block_rows as i64;
            scope
                .parameters
                .splice(0..0, [SqlValue::Integer(rows), SqlValue::Integer(rows)]);
            scope.parameters.push(SqlValue::Text(short_token(&short)));
        }
        (relation, scope.conditions, scope.parameters)
    } else {
        let (mut conditions, mut parameters) = standard_bounds(query);
        add_standard_filters(query, &mut conditions, &mut parameters);
        // A small indexed scope is cheaper than an archive-wide FTS candidate list.
        if let Some(matched) = trigram_query(q)
            && !standard_scope_is_selective(db, query)?
        {
            conditions.push(
                "rowid IN (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?)".into(),
            );
            parameters.push(SqlValue::Text(matched));
        }
        if !q.is_empty() {
            conditions.push("content LIKE ? ESCAPE '!'".into());
            parameters.push(SqlValue::Text(format!("%{}%", like_literal(q))));
        }
        ("messages", conditions, parameters)
    };
    if conditions.is_empty() {
        return db.query_row(&format!("SELECT COUNT(*) FROM {relation}"), [], |r| {
            r.get(0)
        });
    }
    db.query_row(
        &format!(
            "SELECT COUNT(*) FROM {relation} WHERE {}",
            where_sql(&conditions)
        ),
        params_from_iter(parameters),
        |r| r.get(0),
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
        parameters.push(SqlValue::Integer(query.window as i64 - 1));
        let tail = ids(
            db,
            &format!(
                "SELECT rowid FROM message_index WHERE {} ORDER BY rowid DESC LIMIT 2 OFFSET ?",
                if conditions.is_empty() {
                    "1".into()
                } else {
                    conditions.join(" AND ")
                }
            ),
            &parameters,
        )?;
        let floor: Option<i64> = tail
            .first()
            .map(|rowid| db.query_row("SELECT record_id(?)", [rowid], |r| r.get(0)))
            .transpose()?;
        let result = packed_ids(db, decoder, query, floor.or(query.lower), take)?;
        if let Some(floor) = floor.filter(|_| tail.len() == 2 && result.len() < take) {
            scan_cursor = Some(floor.to_string());
        }
        result
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

fn standard_bounds(query: &Query) -> (Vec<String>, Vec<SqlValue>) {
    let mut bounds = vec![];
    let mut bound_params = vec![];
    for (clause, value) in [
        ("id>=?", query.lower),
        ("id<?", query.upper),
        ("id<?", query.before),
    ] {
        if let Some(value) = value {
            bounds.push(clause.to_owned());
            bound_params.push(SqlValue::Integer(value));
        }
    }
    (bounds, bound_params)
}

fn add_standard_filters(
    query: &Query,
    conditions: &mut Vec<String>,
    parameters: &mut Vec<SqlValue>,
) {
    for (column, _, value) in filters(query) {
        conditions.push(format!("{column}=?"));
        parameters.push(SqlValue::Text(value.clone()));
    }
}

pub fn standard_search(db: &Connection, query: &Query) -> rusqlite::Result<Page> {
    if !(1..=100).contains(&query.limit) || query.window == 0 || query.window > 1_000_000 {
        return Err(rusqlite::Error::InvalidParameterName(
            "Invalid search limit/window".into(),
        ));
    }
    let (bounds, bound_params) = standard_bounds(query);
    let mut conditions = vec![];
    let mut parameters = vec![];
    add_standard_filters(query, &mut conditions, &mut parameters);
    conditions.extend(bounds.clone());
    parameters.extend(bound_params.clone());
    let condition = where_sql(&conditions);
    let bound_condition = where_sql(&bounds);
    let q = strip(&query.q);
    let literal = format!("%{}%", like_literal(q));
    let take = query.limit + 1;
    let mut scan_cursor = None;
    let candidates = if query.scan {
        let mut params = bound_params;
        params.push(SqlValue::Integer(query.window as i64 - 1));
        let tail = ids(
            db,
            &format!(
                "SELECT id FROM messages WHERE {bound_condition} ORDER BY id DESC LIMIT 2 OFFSET ?"
            ),
            &params,
        )?;
        let mut params = parameters;
        let range = if let Some(&floor) = tail.first() {
            params.push(SqlValue::Integer(floor));
            "AND id>=?"
        } else {
            ""
        };
        let text = if q.is_empty() {
            ""
        } else {
            params.push(SqlValue::Text(literal));
            "AND content LIKE ? ESCAPE '!'"
        };
        params.push(SqlValue::Integer(take as i64));
        let result = ids(
            db,
            &format!(
                "SELECT id FROM messages WHERE {condition} {range} {text} ORDER BY id DESC LIMIT ?"
            ),
            &params,
        )?;
        if let [floor, _] = tail.as_slice()
            && result.len() < take
        {
            scan_cursor = Some(floor.to_string());
        }
        result
    } else if !q.is_empty() {
        let mut params = parameters.clone();
        params.push(SqlValue::Integer(query.window as i64 - 1));
        let boundary: Option<i64> = db
            .query_row(
                &format!(
                    "SELECT id FROM messages WHERE {condition} ORDER BY id DESC LIMIT 1 OFFSET ?"
                ),
                params_from_iter(&params),
                |r| r.get(0),
            )
            .optional()?;
        let floor = boundary.unwrap_or(0);
        let mut params = parameters.clone();
        params.extend([
            SqlValue::Integer(floor),
            SqlValue::Text(literal.clone()),
            SqlValue::Integer(take as i64),
        ]);
        let mut result = ids(
            db,
            &format!(
                "SELECT id FROM messages WHERE {condition} AND id>=? AND content LIKE ? ESCAPE '!' ORDER BY id DESC LIMIT ?"
            ),
            &params,
        )?;
        if result.len() < take && boundary.is_some() {
            let anchor = q.split(['%', '_']).fold("", |longest, item| {
                if item.chars().count() > longest.chars().count() {
                    item
                } else {
                    longest
                }
            });
            let indexed = anchor.chars().count() >= 3;
            let mut params = parameters;
            params.push(SqlValue::Integer(floor));
            let candidate = if indexed {
                params.push(SqlValue::Text(format!("%{anchor}%")));
                "AND rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?)"
            } else {
                ""
            };
            params.extend([
                SqlValue::Text(literal),
                SqlValue::Integer((take - result.len()) as i64),
            ]);
            result.extend(ids(db,&format!("SELECT id FROM messages WHERE {condition} AND id<? {candidate} AND content LIKE ? ESCAPE '!' ORDER BY id DESC LIMIT ?"),&params)?);
        }
        result
    } else {
        let mut params = parameters;
        params.push(SqlValue::Integer(take as i64));
        ids(
            db,
            &format!("SELECT id FROM messages WHERE {condition} ORDER BY id DESC LIMIT ?"),
            &params,
        )?
    };
    let has_more = candidates.len() > query.limit || scan_cursor.is_some();
    let records = records(db, &candidates[..query.limit.min(candidates.len())], "id")?;
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
