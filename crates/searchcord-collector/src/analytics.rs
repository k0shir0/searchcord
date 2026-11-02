//! Collection analytics and bounded ChatML exports. Call from a blocking worker.
use crate::{Result, discovery::http_error};
use chrono::{Datelike, Days, NaiveDate};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Params, params, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const DEFAULT_SYSTEM_TEMPLATE: &str =
    "You are {target}. You are texting {other}. Respond in your natural texting style.";
const PROMPT_ERROR: &str = "System prompt supports only {target} and {other} placeholders";

fn connect(path: &Path, timeout: u64) -> Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_secs(timeout))?;
    Ok(db)
}

fn object_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut object = serde_json::Map::new();
    for (i, name) in row.as_ref().column_names().iter().enumerate() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => json!(n),
            ValueRef::Real(n) => json!(n),
            ValueRef::Text(s) => json!(String::from_utf8_lossy(s)),
            ValueRef::Blob(_) => return Err(rusqlite::Error::InvalidQuery),
        };
        object.insert((*name).to_owned(), value);
    }
    Ok(Value::Object(object))
}

fn rows(db: &Connection, sql: &str, params: impl Params) -> Result<Vec<Value>> {
    Ok(db
        .prepare(sql)?
        .query_map(params, object_row)?
        .collect::<rusqlite::Result<_>>()?)
}

fn total(db: &Connection) -> Result<i64> {
    Ok(db
        .query_row(
            "SELECT count FROM stats_counts WHERE kind='total' AND key=''",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

// Python's str.strip additionally treats these four control characters as whitespace.
fn strip(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
}

fn literal(value: &str) -> String {
    value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_")
}

pub fn participants(path: impl AsRef<Path>, channel_id: &str) -> Result<Value> {
    let db = connect(path.as_ref(), 5)?;
    let mut people = rows(
        &db,
        "SELECT DISTINCT author_id, author_name FROM message_records WHERE channel_id=? ORDER BY author_name",
        [channel_id],
    )?;
    for person in &mut people {
        if person["author_name"].as_str().is_none_or(str::is_empty) {
            person["author_name"] = person["author_id"].clone();
        }
    }
    Ok(json!(people))
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChatMLExportRequest {
    pub channel_id: String,
    pub target_id: String,
    pub other_name: Option<String>,
    pub system_prompt: Option<String>,
}

/// Prepared metadata is available before the HTTP response starts streaming.
#[derive(Clone, Debug)]
pub struct ChatMLExport {
    path: PathBuf,
    pub channel_id: String,
    pub target_id: String,
    pub target_name: String,
    pub other_name: String,
    pub system_prompt: String,
}

fn prompt(template: &str, target: &str, other: &str) -> Result<String> {
    format_prompt(template, target, other, 0)
}

fn repr(value: &str, ascii: bool) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if ascii && !c.is_ascii()
                || c.is_control()
                || c.escape_debug().to_string().starts_with("\\u{") =>
            {
                let n = c as u32;
                if n <= 0xff {
                    result.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    result.push_str(&format!("\\u{n:04x}"));
                } else {
                    result.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}

fn format_string(value: &str, spec: &str) -> Result<String> {
    let invalid = || http_error(400, PROMPT_ERROR);
    let mut chars = spec.chars().peekable();
    let mut fill = ' ';
    let mut align = '<';
    let mut explicit_fill = false;
    let mut lookahead = chars.clone();
    let first = lookahead.next();
    let second = lookahead.next();
    if second.is_some_and(|c| matches!(c, '<' | '>' | '^')) {
        fill = chars.next().unwrap();
        align = chars.next().unwrap();
        explicit_fill = true;
    } else if first.is_some_and(|c| matches!(c, '<' | '>' | '^')) {
        align = chars.next().unwrap();
    }
    if chars.peek() == Some(&'0') {
        chars.next();
        if !explicit_fill {
            fill = '0';
        }
    }
    let width: String = std::iter::from_fn(|| chars.next_if(char::is_ascii_digit)).collect();
    let width = if width.is_empty() {
        0
    } else {
        width.parse::<usize>().map_err(|_| invalid())?
    };
    let precision = if chars.peek() == Some(&'.') {
        chars.next();
        let precision: String =
            std::iter::from_fn(|| chars.next_if(char::is_ascii_digit)).collect();
        Some(precision.parse::<usize>().map_err(|_| invalid())?)
    } else {
        None
    };
    if chars.peek() == Some(&'s') {
        chars.next();
    }
    if chars.next().is_some() {
        return Err(invalid());
    }
    let value: String = value
        .chars()
        .take(precision.unwrap_or(usize::MAX))
        .collect();
    let padding = width.saturating_sub(value.chars().count());
    let left = match align {
        '>' => padding,
        '^' => padding / 2,
        _ => 0,
    };
    let mut output = String::new();
    output.extend(std::iter::repeat_n(fill, left));
    output.push_str(&value);
    output.extend(std::iter::repeat_n(fill, padding - left));
    Ok(output)
}

fn format_prompt(template: &str, target: &str, other: &str, depth: usize) -> Result<String> {
    if depth > 2 {
        return Err(http_error(400, PROMPT_ERROR));
    }
    let mut output = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                output.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                output.push('}');
            }
            '{' => {
                let mut field = String::new();
                let mut nesting = 0usize;
                loop {
                    match chars.next() {
                        Some('}') if nesting == 0 => break,
                        Some('}') => {
                            nesting -= 1;
                            field.push('}');
                        }
                        Some('{') => {
                            nesting += 1;
                            field.push('{');
                        }
                        None => return Err(http_error(400, PROMPT_ERROR)),
                        Some(c) => field.push(c),
                    }
                }
                let (field, spec) = field.split_once(':').unwrap_or((&field, ""));
                let (name, conversion) = field
                    .split_once('!')
                    .map_or((field, None), |(name, conversion)| (name, Some(conversion)));
                let name_end = name.find('[').unwrap_or(name.len());
                let mut value = match &name[..name_end] {
                    "target" => target.to_owned(),
                    "other" => other.to_owned(),
                    _ => return Err(http_error(400, PROMPT_ERROR)),
                };
                let mut indices = &name[name_end..];
                while !indices.is_empty() {
                    let (index, rest) = indices
                        .strip_prefix('[')
                        .and_then(|s| s.split_once(']'))
                        .ok_or_else(|| http_error(400, PROMPT_ERROR))?;
                    let index = index
                        .parse::<usize>()
                        .map_err(|_| http_error(400, PROMPT_ERROR))?;
                    value = value
                        .chars()
                        .nth(index)
                        .ok_or("String index out of range")?
                        .to_string();
                    indices = rest;
                }
                value = match conversion {
                    None | Some("s") => value,
                    Some("r") => repr(&value, false),
                    Some("a") => repr(&value, true),
                    _ => return Err(http_error(400, PROMPT_ERROR)),
                };
                let spec = format_prompt(spec, target, other, depth + 1)?;
                output.push_str(&format_string(&value, &spec)?);
            }
            '}' => return Err(http_error(400, PROMPT_ERROR)),
            c => output.push(c),
        }
    }
    Ok(output)
}

pub fn prepare_chatml(
    path: impl AsRef<Path>,
    request: &ChatMLExportRequest,
) -> Result<ChatMLExport> {
    let path = path.as_ref();
    let db = connect(path, 1)?;
    let deadline = Instant::now() + Duration::from_secs(4);
    db.progress_handler(1000, Some(move || Instant::now() > deadline));
    let names = (|| -> Result<_> {
        let target: Option<Option<String>> = db.query_row(
            "SELECT author_name FROM message_records WHERE channel_id=? AND author_id=? ORDER BY id LIMIT 1",
            params![request.channel_id, request.target_id], |r| r.get(0)).optional()?;
        let target = target
            .ok_or_else(|| http_error(404, "Target user not found in this channel's messages"))?;
        let other: Option<Option<String>> = db.query_row(
            "SELECT author_name FROM message_records WHERE channel_id=? AND author_id<>? ORDER BY id LIMIT 1",
            params![request.channel_id, request.target_id], |r| r.get(0)).optional()?;
        Ok((target, other.flatten()))
    })();
    let (target_name, other_name) = names.map_err(|error| {
        if matches!(error.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::OperationInterrupted) {
            http_error(408, "Export lookup took too long; try a smaller conversation")
        } else { error }
    })?;
    let target_name = target_name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| request.target_id.clone());
    let other_name = request
        .other_name
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| other_name.filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "Them".into());
    let system_prompt = prompt(
        request
            .system_prompt
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_SYSTEM_TEMPLATE),
        &target_name,
        &other_name,
    )?;
    Ok(ChatMLExport {
        path: path.to_owned(),
        channel_id: request.channel_id.clone(),
        target_id: request.target_id.clone(),
        target_name,
        other_name,
        system_prompt,
    })
}

impl ChatMLExport {
    /// Holds two turns plus a 1,000-message SQLite batch. A bounded sender in the
    /// callback provides backpressure; returning an error cancels the export.
    pub fn stream(&self, mut emit: impl FnMut(String) -> Result<()>) -> Result<()> {
        let db = connect(&self.path, 1)?;
        db.execute_batch("PRAGMA query_only=ON; BEGIN")?;
        let deadline = Arc::new(Mutex::new(Instant::now() + Duration::from_secs(4)));
        let progress = deadline.clone();
        db.progress_handler(
            1000,
            Some(move || Instant::now() > *progress.lock().unwrap()),
        );
        let mut statement = db
            .prepare("SELECT author_id,content FROM messages WHERE channel_id=? ORDER BY id ASC")?;
        let mut cursor = statement.query([&self.channel_id])?;
        let mut side = None;
        let mut pieces: Vec<String> = vec![];
        let mut previous: Option<String> = None;
        loop {
            *deadline.lock().unwrap() = Instant::now() + Duration::from_secs(4);
            let mut batch = Vec::with_capacity(1000);
            for _ in 0..1000 {
                let Some(row) = cursor.next()? else { break };
                batch.push((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ));
            }
            if batch.is_empty() {
                break;
            }
            for (author, content) in batch {
                let text = strip(content.as_deref().unwrap_or(""));
                if text.is_empty() {
                    continue;
                }
                let current =
                    author.map_or_else(|| "None".into(), |n| n.to_string()) == self.target_id;
                if side.is_some_and(|side| side != current) {
                    if side == Some(false) {
                        previous = Some(pieces.join("\n"));
                    } else if let Some(previous) = &previous {
                        emit(self.line(previous, &pieces))?;
                    }
                    pieces.clear();
                }
                side = Some(current);
                pieces.push(text.to_owned());
            }
        }
        if side == Some(true)
            && let Some(previous) = &previous
        {
            emit(self.line(previous, &pieces))?;
        }
        Ok(())
    }

    fn line(&self, previous: &str, pieces: &[String]) -> String {
        // Match json.dumps(ensure_ascii=False), including its NDJSON separators.
        format!(
            "{{\"messages\": [{{\"role\": \"system\", \"content\": {}}}, {{\"role\": \"user\", \"content\": {}}}, {{\"role\": \"assistant\", \"content\": {}}}]}}\n",
            json!(self.system_prompt),
            json!(format!("{}: {previous}", self.other_name)),
            json!(pieces.join("\n"))
        )
    }
}

pub fn stats(path: impl AsRef<Path>, days: i64) -> Result<Value> {
    if !(0..=3650).contains(&days) {
        return Err(http_error(422, "days must be between 0 and 3650"));
    }
    let path = path.as_ref();
    let db = connect(path, 5)?;
    let total_messages = total(&db)?;
    let totals = rows(
        &db,
        "SELECT kind,COUNT(*) AS count FROM stats_counts WHERE kind IN ('guild','channel','author') AND key<>'' AND count>0 GROUP BY kind",
        [],
    )?;
    let count = |kind: &str| {
        totals
            .iter()
            .find(|r| r["kind"] == kind)
            .map_or(0, |r| r["count"].as_i64().unwrap_or(0))
    };
    let top_users = rows(
        &db,
        "SELECT ranked.author_id,authors.name AS author_name,ranked.count FROM (SELECT key AS author_id,count FROM stats_counts WHERE kind='author' AND count>0 ORDER BY count DESC,key LIMIT 50) AS ranked LEFT JOIN authors ON authors.id=ranked.author_id",
        [],
    )?;
    let by_server = rows(
        &db,
        "SELECT guilds.name AS guild_name,ranked.guild_id,ranked.count FROM (SELECT NULLIF(key,'') AS guild_id,count FROM stats_counts WHERE kind='guild' AND count>0 ORDER BY count DESC,key LIMIT 20) AS ranked LEFT JOIN guilds ON guilds.id=ranked.guild_id",
        [],
    )?;
    let daily = rows(
        &db,
        "SELECT key AS date,count FROM stats_counts WHERE kind='day' AND count>0 ORDER BY key",
        [],
    )?;
    let mut weekdays = [0i64; 7];
    let mut months = BTreeMap::<String, i64>::new();
    for row in &daily {
        let date = row["date"].as_str().ok_or("Invalid cached date")?;
        let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")?;
        let count = row["count"].as_i64().unwrap_or(0);
        weekdays[parsed.weekday().num_days_from_monday() as usize] += count;
        *months.entry(date[..7].to_owned()).or_default() += count;
    }
    let range_end = daily.last().and_then(|r| r["date"].as_str());
    let range_start = if days > 0 {
        range_end
            .map(|end| -> Result<_> {
                Ok(NaiveDate::parse_from_str(end, "%Y-%m-%d")?
                    .checked_sub_days(Days::new((days - 1) as u64))
                    .ok_or("Date range overflow")?
                    .to_string())
            })
            .transpose()?
    } else {
        daily
            .first()
            .and_then(|r| r["date"].as_str())
            .map(str::to_owned)
    };
    let by_day: Vec<_> = daily
        .iter()
        .filter(|r| {
            range_start
                .as_deref()
                .is_some_and(|start| r["date"].as_str().unwrap_or("") >= start)
        })
        .collect();
    let by_channel = rows(
        &db,
        "SELECT ranked.key AS channel_id,channels.name AS channel_name,guilds.name AS guild_name,ranked.count FROM (SELECT key,count FROM stats_counts WHERE kind='channel' AND count>0 ORDER BY count DESC,key LIMIT 20) ranked LEFT JOIN channels ON channels.id=ranked.key LEFT JOIN guilds ON guilds.id=channels.guild_id",
        [],
    )?;
    let by_hour = rows(
        &db,
        "SELECT CAST(NULLIF(key,'') AS INTEGER) AS hour,count FROM stats_counts WHERE kind='hour' AND count>0 ORDER BY hour ASC",
        [],
    )?;
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let db_size: u64 = [path, Path::new(&wal)]
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    Ok(
        json!({"total_messages":total_messages,"total_servers":count("guild"),"total_channels":count("channel"),"total_users":count("author"),
        "db_size_bytes":db_size,"top_users":top_users,"messages_by_server":by_server,"messages_by_day":by_day,
        "messages_by_hour":by_hour,"messages_by_channel":by_channel,
        "messages_by_weekday":weekdays.iter().enumerate().map(|(day,count)|json!({"day":day,"count":count})).collect::<Vec<_>>(),
        "messages_by_month":months.iter().map(|(month,count)|json!({"month":month,"count":count})).collect::<Vec<_>>(),
        "range_start":range_start,"range_end":range_end}),
    )
}

fn validate_page(offset: i64, limit: i64, q: &str) -> Result<()> {
    if offset < 0 || !(1..=100).contains(&limit) || q.chars().count() > 100 {
        return Err(http_error(422, "Invalid leaderboard page"));
    }
    Ok(())
}

pub fn contributors(path: impl AsRef<Path>, offset: i64, limit: i64, q: &str) -> Result<Value> {
    validate_page(offset, limit, q)?;
    let query = literal(strip(q));
    let db = connect(path.as_ref(), 5)?;
    let mut users = rows(
        &db,
        "SELECT counts.key AS author_id,authors.name AS author_name,counts.count FROM stats_counts counts LEFT JOIN authors ON authors.id=counts.key WHERE counts.kind='author' AND counts.count>0 AND (?='' OR authors.name LIKE ? ESCAPE '!' OR counts.key=?) ORDER BY counts.count DESC,counts.key LIMIT ? OFFSET ?",
        params![query, format!("%{query}%"), strip(q), limit + 1, offset],
    )?;
    let has_more = users.len() > limit as usize;
    users.truncate(limit as usize);
    Ok(json!({"users":users,"has_more":has_more,"offset":offset,"limit":limit}))
}

pub fn ranked_servers(path: impl AsRef<Path>, offset: i64, limit: i64, q: &str) -> Result<Value> {
    validate_page(offset, limit, q)?;
    let query = literal(strip(q));
    let db = connect(path.as_ref(), 5)?;
    let total_messages = total(&db)?;
    let mut servers = rows(
        &db,
        "SELECT counts.key AS guild_id,guilds.name AS guild_name,counts.count FROM stats_counts counts LEFT JOIN guilds ON guilds.id=counts.key WHERE counts.kind='guild' AND counts.key<>'' AND counts.count>0 AND (?='' OR guilds.name LIKE ? ESCAPE '!' OR counts.key=?) ORDER BY counts.count DESC,counts.key LIMIT ? OFFSET ?",
        params![query, format!("%{query}%"), strip(q), limit + 1, offset],
    )?;
    let has_more = servers.len() > limit as usize;
    servers.truncate(limit as usize);
    Ok(
        json!({"servers":servers,"has_more":has_more,"offset":offset,"limit":limit,"total_messages":total_messages}),
    )
}

pub fn search_filters(path: impl AsRef<Path>, include_users: bool) -> Result<Value> {
    let db = connect(path.as_ref(), 5)?;
    let guilds = rows(
        &db,
        "SELECT id AS guild_id,name AS guild_name FROM guilds ORDER BY name",
        [],
    )?;
    let channels = rows(
        &db,
        "SELECT id AS channel_id,name AS channel_name,guild_id FROM channels ORDER BY guild_id,name",
        [],
    )?;
    let users = if include_users {
        rows(
            &db,
            "SELECT id AS author_id,name AS author_name FROM authors ORDER BY name",
            [],
        )?
    } else {
        vec![]
    };
    Ok(json!({"guilds":guilds,"channels":channels,"users":users,"total_messages":total(&db)?}))
}

pub fn search_authors(path: impl AsRef<Path>, q: &str) -> Result<Value> {
    let q = strip(q);
    if q.chars().count() < 2 {
        return Ok(json!([]));
    }
    let pattern = format!("{}%", literal(&q.chars().take(100).collect::<String>()));
    let db = connect(path.as_ref(), 5)?;
    Ok(json!(rows(
        &db,
        "SELECT id AS author_id,name AS author_name FROM authors WHERE name LIKE ? ESCAPE '!' ORDER BY name COLLATE NOCASE,id LIMIT 20",
        [pattern]
    )?))
}

fn iso_date(value: &str) -> Option<NaiveDate> {
    if !value.is_ascii() {
        return None;
    }
    let year = value.get(..4)?.parse::<i32>().ok()?;
    if !(1..=9999).contains(&year) || !value.as_bytes()[..4].iter().all(u8::is_ascii_digit) {
        return None;
    }
    if value.get(4..5) == Some("W") || value.get(4..6) == Some("-W") {
        let separated = value.as_bytes().get(4) == Some(&b'-');
        let position = if separated { 6 } else { 5 };
        let digits = value.get(position..position + 2)?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let week = digits.parse::<u32>().ok()?;
        let day = if value.len() == position + 2 {
            1
        } else {
            let position = position + 2;
            if separated && value.as_bytes().get(position) != Some(&b'-') {
                return None;
            }
            let day_position = position + usize::from(separated);
            if value.len() != day_position + 1 {
                return None;
            }
            let digit = value.get(day_position..)?;
            if !digit.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digit.parse::<u32>().ok()?
        };
        let weekday = [
            chrono::Weekday::Mon,
            chrono::Weekday::Tue,
            chrono::Weekday::Wed,
            chrono::Weekday::Thu,
            chrono::Weekday::Fri,
            chrono::Weekday::Sat,
            chrono::Weekday::Sun,
        ]
        .get(day.checked_sub(1)? as usize)?;
        NaiveDate::from_isoywd_opt(year, week, *weekday)
    } else if value.len() == 10
        && value.as_bytes().get(4) == Some(&b'-')
        && value.as_bytes().get(7) == Some(&b'-')
    {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
    } else if value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit()) {
        NaiveDate::parse_from_str(value, "%Y%m%d").ok()
    } else {
        None
    }
}

fn iso_clock(value: &str) -> Option<[u32; 4]> {
    if !value.is_ascii() {
        return None;
    }
    let bytes = value.as_bytes();
    let mut position = 0;
    let mut components = [0; 4];
    let separated = bytes.get(2) == Some(&b':');
    for (index, component) in components[..3].iter_mut().enumerate() {
        if index > 0 && separated {
            if bytes.get(position) != Some(&b':') {
                return None;
            }
            position += 1;
        }
        let digits = value.get(position..position + 2)?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *component = digits.parse().ok()?;
        position += 2;
        if position == bytes.len() || matches!(bytes.get(position), Some(b'.' | b',')) {
            break;
        }
    }
    if position < bytes.len() {
        if !matches!(bytes.get(position), Some(b'.' | b',')) {
            return None;
        }
        let digits = &value[position + 1..];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits = &digits[..digits.len().min(6)];
        components[3] = digits.parse::<u32>().ok()? * 10u32.pow((6 - digits.len()) as u32);
    }
    Some(components)
}

fn iso_moment(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let value = value.replace('Z', "+00:00");
    let chars: Vec<_> = value.chars().collect();
    let length = chars.len();
    if length < 7 {
        return None;
    }
    let separator = if length == 7 {
        7
    } else if chars[4] == '-' {
        if chars[5] == 'W' {
            if length < 8 {
                return None;
            }
            if length > 8 && chars[8] == '-' {
                if length == 9 {
                    return None;
                }
                if length > 10 && chars[10].is_ascii_digit() {
                    8
                } else {
                    10
                }
            } else {
                8
            }
        } else {
            10
        }
    } else if chars[4] == 'W' {
        let mut end = 7;
        while end < length && chars[end].is_ascii_digit() {
            end += 1;
        }
        if end < 9 {
            end
        } else if end % 2 == 0 {
            7
        } else {
            8
        }
    } else {
        8
    };
    let date = iso_date(&chars.get(..separator)?.iter().collect::<String>())?;
    if length == separator {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
    }
    let time: String = chars.get(separator + 1..)?.iter().collect();
    let timezone = time.find(['+', '-']);
    let clock = iso_clock(timezone.map_or(time.as_str(), |position| &time[..position]))?;
    if clock[0] > 23 || clock[1] > 59 || clock[2] > 59 {
        return None;
    }
    let mut moment = date
        .and_hms_micro_opt(clock[0], clock[1], clock[2], clock[3])?
        .and_utc();
    if let Some(position) = timezone {
        let offset = iso_clock(&time[position + 1..])?;
        // CPython treats a zero whole-second UTC offset as UTC, even when an
        // otherwise accepted offset contains fractional seconds.
        let whole = (i64::from(offset[0]) * 60 + i64::from(offset[1])) * 60 + i64::from(offset[2]);
        let offset = if whole == 0 {
            0
        } else {
            whole * 1_000_000 + i64::from(offset[3])
        };
        if offset >= 86_400_000_000 {
            return None;
        }
        let sign = if time.as_bytes()[position] == b'-' {
            -1
        } else {
            1
        };
        moment = moment.checked_sub_signed(chrono::Duration::microseconds(sign * offset))?;
    }
    Some(moment)
}

fn counted_date_bound(value: &str, upper: bool) -> Result<i64> {
    let invalid = || http_error(400, "Invalid date filter");
    let mut moment = iso_moment(value).ok_or_else(invalid)?;
    if upper {
        let advance = if value.chars().count() == 10 {
            chrono::Duration::days(1)
        } else if !value.contains('.') {
            chrono::Duration::seconds(1)
        } else {
            chrono::Duration::milliseconds(1)
        };
        moment = moment.checked_add_signed(advance).ok_or_else(invalid)?;
    }
    let millis = ((moment.timestamp() as f64 + moment.timestamp_subsec_nanos() as f64 / 1e9)
        * 1000.0) as i128;
    Ok(i64::try_from((millis - 1_420_070_400_000) << 22)?)
}

/// Collector's counted search intentionally keeps its original LIKE semantics
/// and response shape; cursor search belongs to the archive reader.
pub fn counted_search(
    path: impl AsRef<Path>,
    options: &searchcord_archive::SearchOptions,
    page: i64,
) -> Result<Value> {
    if page < 1 || !(1..=100).contains(&options.limit) || options.q.chars().count() > 200 {
        return Err(http_error(422, "Invalid search page"));
    }
    let mut conditions = vec![];
    let mut values = vec![];
    if !options.q.is_empty() {
        let literal = options
            .q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        conditions.push(
            if options.q.chars().count() >= 3 && !literal.contains('\\') {
                "rowid IN (SELECT rowid FROM messages_fts WHERE content LIKE ?)"
            } else {
                "content LIKE ? ESCAPE '\\'"
            },
        );
        values.push(SqlValue::Text(format!("%{literal}%")));
    }
    let filters = [
        ("guild", "guild_id = ?", &options.guild_id),
        ("channel", "channel_id = ?", &options.channel_id),
        ("author", "author_id = ?", &options.author_id),
    ];
    let mut active = vec![];
    for (kind, condition, value) in filters {
        if let Some(value) = value.as_deref().filter(|s| !s.is_empty()) {
            conditions.push(condition);
            values.push(SqlValue::Text(value.to_owned()));
            active.push((kind, value));
        }
    }
    let lower = options
        .date_from
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| counted_date_bound(s, false))
        .transpose()?;
    let upper = options
        .date_to
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| counted_date_bound(s, true))
        .transpose()?;
    if matches!((lower,upper),(Some(lower),Some(upper)) if lower>=upper) {
        return Err(http_error(
            400,
            "From date must be on or before the to date",
        ));
    }
    if let Some(lower) = lower {
        conditions.push("id >= ?");
        values.push(SqlValue::Integer(lower));
    }
    if let Some(upper) = upper {
        conditions.push("id < ?");
        values.push(SqlValue::Integer(upper));
    }
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };
    let cached = if options.q.is_empty() && lower.is_none() && upper.is_none() {
        match active.as_slice() {
            [] => Some(("total", "")),
            [only] => Some(*only),
            _ => None,
        }
    } else {
        None
    };
    let offset = (page - 1)
        .checked_mul(options.limit as i64)
        .ok_or("Search page overflow")?;
    let result = (|| -> Result<_> {
        let db = connect(path.as_ref(), 5)?;
        let deadline = Instant::now() + Duration::from_secs(4);
        db.progress_handler(1000, Some(move || Instant::now() > deadline));
        let total: i64 = if let Some((kind, key)) = cached {
            db.query_row(
                "SELECT count FROM stats_counts WHERE kind=? AND key=? AND count>0",
                params![kind, key],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0)
        } else {
            db.query_row(
                &format!("SELECT COUNT(*) FROM messages {where_clause}"),
                params_from_iter(&values),
                |r| r.get(0),
            )?
        };
        values.extend([
            SqlValue::Integer(options.limit as i64),
            SqlValue::Integer(offset),
        ]);
        let records = db.prepare(&format!("SELECT * FROM message_records WHERE id IN (SELECT id FROM messages {where_clause} ORDER BY id DESC LIMIT ? OFFSET ?) ORDER BY id DESC"))?
            .query_map(params_from_iter(&values),searchcord_archive::search::Record::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut messages = Vec::with_capacity(records.len());
        for record in records {
            let avatar: Option<Option<String>> = db
                .query_row(
                    "SELECT avatar_hash FROM profiles WHERE user_id=?",
                    [&record.author_id],
                    |r| r.get(0),
                )
                .optional()?;
            let avatar = record.author_id.as_deref().and_then(|id| {
                searchcord_archive::image_url(id, avatar.flatten().as_deref(), "avatars", 64)
            });
            let attachments: Vec<_> =
                searchcord_archive::media::split_lines(record.image_urls.as_deref().unwrap_or(""))
                    .iter()
                    .enumerate()
                    .map(|(index, _)| format!("/api/images/{}/{index}", record.id))
                    .collect();
            let mut message = serde_json::to_value(record)?;
            message.as_object_mut().unwrap().remove("image_urls");
            message["avatar_url"] = json!(avatar);
            message["attachments"] = json!(attachments);
            messages.push(message);
        }
        Ok(
            json!({"total":total,"page":page,"limit":options.limit,"pages":1.max((total+options.limit as i64-1)/options.limit as i64),"messages":messages}),
        )
    })();
    result.map_err(|error| {
        if matches!(error.downcast_ref::<rusqlite::Error>(),Some(rusqlite::Error::SqliteFailure(e,_)) if e.code==rusqlite::ErrorCode::OperationInterrupted) {
            http_error(408,"Counted search exceeded its deadline; retry with cursor=true")
        } else { error }
    })
}

#[derive(Clone, Debug)]
pub struct ImageLookup {
    pub channel_id: Option<i64>,
    pub url: String,
}

pub fn image_lookup(
    path: impl AsRef<Path>,
    message_id: i64,
    image_index: i64,
) -> Result<ImageLookup> {
    let db = connect(path.as_ref(), 5)?;
    let row: Option<(Option<i64>, Option<String>)> = db
        .query_row(
            "SELECT channel_id,image_urls FROM messages WHERE id=?",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (channel_id, urls) = row.ok_or_else(|| http_error(404, "Image not found"))?;
    let urls = urls.ok_or_else(|| http_error(404, "Image not found"))?;
    let url = usize::try_from(image_index)
        .ok()
        .and_then(|index| {
            searchcord_archive::media::split_lines(&urls)
                .get(index)
                .map(|s| (*s).to_owned())
        })
        .ok_or_else(|| http_error(404, "Image not found"))?;
    Ok(ImageLookup { channel_id, url })
}
