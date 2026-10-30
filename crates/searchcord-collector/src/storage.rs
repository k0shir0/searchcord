use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const SCHEMA: &str = include_str!("schema.sql");
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
fn ident(v: &Value) -> Result<i64> {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_i64())
        .ok_or_else(|| "Invalid message identifier".into())
}
fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
fn nonempty(v: &Value) -> Option<&str> {
    v.as_str().filter(|s| !s.is_empty())
}
pub(crate) fn image_urls(attachments: &Value) -> Option<String> {
    let extensions = [
        "png", "jpg", "jpeg", "jfif", "gif", "webp", "avif", "apng", "bmp", "ico", "svg", "heic",
        "heif", "tif", "tiff", "dng", "exr",
    ];
    let urls: Vec<_> = attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let url = nonempty(&a["url"])?;
            let mime = a["content_type"].as_str().unwrap_or("").to_lowercase();
            let extension = url::Url::parse(url)
                .ok()
                .map(|u| {
                    percent_encoding::percent_decode_str(u.path())
                        .decode_utf8_lossy()
                        .into_owned()
                })
                .unwrap_or_else(|| {
                    percent_encoding::percent_decode_str(url.split(['?', '#']).next().unwrap_or(""))
                        .decode_utf8_lossy()
                        .into_owned()
                });
            let image = mime.starts_with("image/")
                || mime.is_empty()
                    && extension
                        .rsplit('.')
                        .next()
                        .is_some_and(|e| extensions.contains(&e.to_lowercase().as_str()));
            image.then_some(url)
        })
        .collect();
    if urls.is_empty() {
        None
    } else {
        Some(urls.join("\n"))
    }
}
#[derive(Default)]
pub struct PageContext<'a> {
    pub channel_id: &'a str,
    pub channel_name: &'a str,
    pub guild_id: Option<&'a str>,
    pub guild_name: &'a str,
    pub history_page: bool,
    pub pending: Option<(Option<&'a str>, Option<&'a str>)>,
    pub clear_pending: bool,
    pub history_complete: bool,
}
pub struct Store {
    pub path: PathBuf,
    db: Connection,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = crate::credentials::resolve(path.as_ref())?;
        crate::migrations::initialize(&path)?;
        let db = Connection::open(&path)?;
        db.busy_timeout(Duration::from_secs(30))?;
        Ok(Self { path, db })
    }
    pub fn connection(&self) -> &Connection {
        &self.db
    }
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.db
    }
    pub fn save_page(&mut self, messages: &[Value], context: &PageContext) -> Result<usize> {
        self.save_page_with_hook(messages, context, |_, _| Ok(()))
    }
    pub fn save_page_with_hook(
        &mut self,
        messages: &[Value],
        context: &PageContext,
        hook: impl FnOnce(&Connection, usize) -> Result<()>,
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }
        let tx = self.db.transaction()?;
        if let Some(guild) = context.guild_id {
            tx.execute("INSERT INTO guilds VALUES (?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name WHERE name IS NOT excluded.name",params![guild,context.guild_name])?;
        }
        tx.execute("INSERT INTO channels VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET guild_id=excluded.guild_id,name=excluded.name WHERE guild_id IS NOT excluded.guild_id OR name IS NOT excluded.name",params![context.channel_id,context.guild_id,context.channel_name])?;
        let mut authors: HashMap<String, (i64, String)> = HashMap::new();
        let mut newest = i64::MIN;
        let mut oldest = i64::MAX;
        for m in messages {
            let id = ident(&m["id"])?;
            newest = newest.max(id);
            oldest = oldest.min(id);
            let author = text(&m["author"]["id"]);
            if authors.get(&author).is_none_or(|old| id > old.0) {
                authors.insert(
                    author.clone(),
                    (
                        id,
                        nonempty(&m["author"]["global_name"])
                            .or_else(|| nonempty(&m["author"]["username"]))
                            .unwrap_or("")
                            .to_owned(),
                    ),
                );
            }
            if m.get("webhook_id").is_none_or(|v| !truthy(v)) {
                tx.execute(
                    "INSERT OR IGNORE INTO channel_authors VALUES (?,?)",
                    params![context.channel_id, author],
                )?;
            }
        }
        for (author, (id, name)) in authors {
            tx.execute("INSERT INTO authors VALUES (?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,last_message_id=excluded.last_message_id WHERE CAST(excluded.last_message_id AS INTEGER)>CAST(authors.last_message_id AS INTEGER)",params![author,name,id.to_string()])?;
        }
        let inserted = save_payloads(&tx, messages, context, "collection")?;
        tx.execute("INSERT INTO scrape_cursors(channel_id,guild_id,newest_message_id,oldest_message_id) VALUES (?,?,?,?) ON CONFLICT(channel_id) DO UPDATE SET guild_id=excluded.guild_id,newest_message_id=CASE WHEN CAST(excluded.newest_message_id AS INTEGER)>CAST(scrape_cursors.newest_message_id AS INTEGER) THEN excluded.newest_message_id ELSE scrape_cursors.newest_message_id END,oldest_message_id=CASE WHEN excluded.oldest_message_id IS NULL THEN scrape_cursors.oldest_message_id WHEN scrape_cursors.oldest_message_id IS NULL OR CAST(excluded.oldest_message_id AS INTEGER)<CAST(scrape_cursors.oldest_message_id AS INTEGER) THEN excluded.oldest_message_id ELSE scrape_cursors.oldest_message_id END",params![context.channel_id,context.guild_id,newest.to_string(),context.history_page.then(||oldest.to_string())])?;
        if let Some((after, before)) = context.pending {
            tx.execute("UPDATE scrape_cursors SET pending_after_message_id=?,pending_before_message_id=? WHERE channel_id=?",params![after,before,context.channel_id])?;
        } else if context.clear_pending {
            tx.execute("UPDATE scrape_cursors SET pending_after_message_id=NULL,pending_before_message_id=NULL WHERE channel_id=?",[context.channel_id])?;
        }
        if context.history_complete {
            tx.execute(
                "UPDATE scrape_cursors SET history_complete=1 WHERE channel_id=?",
                [context.channel_id],
            )?;
        }
        hook(&tx, inserted)?;
        tx.commit()?;
        Ok(inserted)
    }
    pub fn mark_deleted(&mut self, ids: &[i64], source: &str) -> Result<usize> {
        let tx = self.db.transaction()?;
        let count = mark_deleted(&tx, ids, source)?;
        tx.commit()?;
        Ok(count)
    }
    pub fn read_message(&self, id: i64, after: i64, limit: usize) -> Result<Option<Value>> {
        read_message(&self.db, id, after, limit)
    }
}
struct Old {
    _rowid: i64,
    channel: Option<i64>,
    guild: Option<i64>,
    author: Option<i64>,
    content: Option<String>,
    images: Option<String>,
    payload: Option<String>,
    deleted: Option<String>,
}
fn old(db: &Connection, id: i64) -> rusqlite::Result<Option<Old>> {
    db.query_row("SELECT m.rowid,m.channel_id,m.guild_id,m.author_id,m.content,m.image_urls,p.payload,p.deleted_at FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.id=?",[id],|r|Ok(Old{_rowid:r.get(0)?,channel:r.get(1)?,guild:r.get(2)?,author:r.get(3)?,content:r.get(4)?,images:r.get(5)?,payload:r.get(6)?,deleted:r.get(7)?})).optional()
}
fn legacy(id: i64, old: &Old) -> Value {
    json!({"id":id.to_string(),"channel_id":old.channel.map(|n|n.to_string()).unwrap_or("None".into()),"guild_id":old.guild.map(|n|n.to_string()),"author":{"id":old.author.map(|n|n.to_string()).unwrap_or("None".into())},"content":old.content,"archived_image_urls":old.images.as_deref().map(|s|s.split('\n').collect::<Vec<_>>()).unwrap_or_default()})
}
fn revision(
    db: &Connection,
    id: i64,
    now: &str,
    event: &str,
    source: &str,
    payload: Option<&str>,
) -> Result<()> {
    db.execute("INSERT INTO message_revisions(message_id,observed_at,event,source,payload) VALUES (?,?,?,?,?)",params![id,now,event,source,payload])?;
    Ok(())
}
pub fn save_payloads(
    db: &Connection,
    messages: &[Value],
    context: &PageContext,
    source: &str,
) -> Result<usize> {
    let mut incoming: Vec<(i64, &Value)> = Vec::new();
    let mut positions = HashMap::new();
    for m in messages {
        let id = ident(&m["id"])?;
        if let Some(&index) = positions.get(&id) {
            incoming[index] = (id, m);
        } else {
            positions.insert(id, incoming.len());
            incoming.push((id, m));
        }
    }
    let now = now();
    let mut inserts = 0;
    for (id, m) in incoming {
        let author = ident(&m["author"]["id"])?;
        let channel = context.channel_id.parse::<i64>()?;
        if m.get("channel_id")
            .filter(|v| !v.is_null())
            .is_some_and(|v| text(v) != context.channel_id)
        {
            return Err("Received message belongs to a different channel".into());
        }
        let previous = old(db, id)?;
        if previous
            .as_ref()
            .is_some_and(|o| o.channel != Some(channel) || o.author != Some(author))
        {
            return Err("Received message has different immutable archive identifiers".into());
        }
        let payload = canonical_json(m)?;
        if let Some(old) = &previous
            && old.payload.is_none()
        {
            let mut legacy = legacy(id, old);
            let attachment: Option<String> = db
                .query_row(
                    "SELECT attachments FROM legacy_message_attachments WHERE message_id=?",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(attachment) = attachment {
                legacy["archived_attachments"] = json!(attachment);
            }
            revision(
                db,
                id,
                &now,
                "message",
                "legacy_archive",
                Some(&canonical_json(&legacy)?),
            )?;
        }
        if previous
            .as_ref()
            .is_none_or(|o| o.payload.as_deref() != Some(payload.as_str()) || o.deleted.is_some())
        {
            revision(db, id, &now, "message", source, Some(&payload))?;
        }
        if previous.is_none() {
            inserts += 1;
        }
        db.execute("INSERT INTO messages(id,channel_id,guild_id,author_id,content,image_urls) VALUES (?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET content=excluded.content,image_urls=excluded.image_urls WHERE content IS NOT excluded.content OR image_urls IS NOT excluded.image_urls",params![id,channel,context.guild_id.map(str::parse::<i64>).transpose()?,author,nonempty(&m["content"]).unwrap_or(""),image_urls(&m["attachments"])])?;
        db.execute("INSERT INTO message_payloads(message_id,payload,first_seen_at,last_seen_at) VALUES (?,?,?,?) ON CONFLICT(message_id) DO UPDATE SET payload=excluded.payload,last_seen_at=excluded.last_seen_at,deleted_at=NULL,complete=1",params![id,payload,now,now])?;
    }
    Ok(inserts)
}
pub fn mark_deleted(db: &Connection, ids: &[i64], source: &str) -> Result<usize> {
    let now = now();
    let mut seen = HashSet::new();
    let mut count = 0;
    for &id in ids {
        if !seen.insert(id) {
            continue;
        }
        let Some(old) = old(db, id)? else { continue };
        if old.deleted.is_some() {
            continue;
        }
        if old.payload.is_none() {
            let payload = canonical_json(&legacy(id, &old))?;
            db.execute("INSERT INTO message_payloads(message_id,payload,first_seen_at,last_seen_at,deleted_at,complete) VALUES (?,?,?,?,?,0)",params![id,payload,now,now,now])?;
            revision(db, id, &now, "message", "legacy_archive", Some(&payload))?;
        } else {
            db.execute(
                "UPDATE message_payloads SET deleted_at=? WHERE message_id=?",
                params![now, id],
            )?;
        }
        revision(db, id, &now, "deleted", source, None)?;
        count += 1;
    }
    Ok(count)
}
pub fn read_message(db: &Connection, id: i64, after: i64, limit: usize) -> Result<Option<Value>> {
    let limit = limit.clamp(1, 100);
    let row=db.query_row("SELECT p.payload,p.first_seen_at,p.last_seen_at,p.deleted_at,p.complete FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.id=?",[id],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<bool>>(4)?))).optional()?;
    let Some((payload, first, last, deleted, complete)) = row else {
        return Ok(None);
    };
    let attachment: Option<String> = db
        .query_row(
            "SELECT attachments FROM legacy_message_attachments WHERE message_id=?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let mut statement=db.prepare("SELECT revision_id,observed_at,event,source,payload FROM message_revisions WHERE message_id=? AND revision_id>? ORDER BY revision_id LIMIT ?")?;
    let rows = statement.query_map(params![id, after, limit as i64 + 1], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut revisions = Vec::new();
    for row in rows {
        let (id, observed, event, source, payload) = row?;
        revisions.push(json!({"revision_id":id,"observed_at":observed,"event":event,"source":source,"payload":payload.map(|s|serde_json::from_str::<Value>(&s)).transpose()?}));
    }
    let more = revisions.len() > limit;
    let next = if more {
        revisions[limit - 1]["revision_id"].clone()
    } else {
        Value::Null
    };
    revisions.truncate(limit);
    Ok(Some(
        json!({"id":id.to_string(),"payload":payload.as_deref().map(serde_json::from_str::<Value>).transpose()?,"complete":payload.is_some() && complete.unwrap_or(false),"first_seen_at":first,"last_seen_at":last,"deleted_at":deleted,"legacy_attachments":attachment,"revisions":revisions,"has_more":more,"next_revision":next}),
    ))
}

/// Python's canonical JSON uses float repr thresholds and two-digit exponents.
/// Keeping its spelling prevents a cross-language observation becoming an edit.
pub fn canonical_json(value: &Value) -> Result<String> {
    use serde::Serialize;
    struct PythonFormatter;
    fn number(value: f64) -> String {
        let raw = serde_json::to_string(&value).expect("finite JSON float");
        let (sign, raw) = raw
            .strip_prefix('-')
            .map_or(("", raw.as_str()), |r| ("-", r));
        let (base, power) = raw
            .split_once('e')
            .map_or((raw, 0), |(b, e)| (b, e.parse::<i32>().unwrap()));
        let point = base.find('.').unwrap_or(base.len()) as i32;
        let all = base.replace('.', "");
        let leading = all.bytes().take_while(|b| *b == b'0').count();
        let digits = all[leading..].trim_end_matches('0');
        if digits.is_empty() {
            return format!("{sign}0.0");
        }
        let exponent = point - 1 - leading as i32 + power;
        let value = if !(-4..16).contains(&exponent) {
            let mut mantissa = digits[..1].to_owned();
            if digits.len() > 1 {
                mantissa.push('.');
                mantissa.push_str(&digits[1..]);
            }
            format!(
                "{mantissa}e{}{:02}",
                if exponent >= 0 { '+' } else { '-' },
                exponent.unsigned_abs()
            )
        } else if exponent < 0 {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        } else {
            let split = exponent as usize + 1;
            if split >= digits.len() {
                format!("{digits}{}.0", "0".repeat(split - digits.len()))
            } else {
                format!("{}.{}", &digits[..split], &digits[split..])
            }
        };
        format!("{sign}{value}")
    }
    impl serde_json::ser::Formatter for PythonFormatter {
        fn write_f64<W: std::io::Write + ?Sized>(
            &mut self,
            writer: &mut W,
            value: f64,
        ) -> std::io::Result<()> {
            writer.write_all(number(value).as_bytes())
        }
        fn write_f32<W: std::io::Write + ?Sized>(
            &mut self,
            writer: &mut W,
            value: f32,
        ) -> std::io::Result<()> {
            writer.write_all(number(value as f64).as_bytes())
        }
    }
    let mut bytes = Vec::new();
    value.serialize(&mut serde_json::Serializer::with_formatter(
        &mut bytes,
        PythonFormatter,
    ))?;
    Ok(String::from_utf8(bytes)?)
}
