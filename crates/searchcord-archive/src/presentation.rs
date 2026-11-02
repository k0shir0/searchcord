use crate::{Archive, ArchiveError, SearchOptions, media, search};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, functions::FunctionFlags, params, params_from_iter};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::OnceLock, time::Instant};
use unicode_normalization::UnicodeNormalization;

pub fn search_label(value: &str) -> String {
    static FOLD: OnceLock<BTreeMap<u32, String>> = OnceLock::new();
    static CATEGORIES: OnceLock<Vec<[u32; 2]>> = OnceLock::new();
    let fold = FOLD.get_or_init(|| {
        serde_json::from_str::<Vec<(u32, String)>>(include_str!("casefold-15.json"))
            .expect("bundled Unicode data")
            .into_iter()
            .collect()
    });
    let categories = CATEGORIES.get_or_init(|| {
        serde_json::from_str(include_str!("label-categories-15.json"))
            .expect("bundled Unicode categories")
    });
    let mut folded = String::new();
    for char in value.nfkc() {
        if let Some(s) = fold.get(&(char as u32)) {
            folded.push_str(s);
        } else {
            folded.push(char);
        }
    }
    folded
        .chars()
        .map(|c| {
            let cp = c as u32;
            let i = categories.partition_point(|r| r[1] < cp);
            if categories.get(i).is_some_and(|r| r[0] <= cp) {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn unicode_digits(value: &str) -> bool {
    static DIGITS: OnceLock<Vec<[u32; 2]>> = OnceLock::new();
    let ranges = DIGITS.get_or_init(|| {
        serde_json::from_str(include_str!("digits-15.json")).expect("bundled Unicode digits")
    });
    !value.is_empty()
        && value.chars().all(|c| {
            let cp = c as u32;
            let i = ranges.partition_point(|r| r[1] < cp);
            ranges.get(i).is_some_and(|r| r[0] <= cp)
        })
}
pub(crate) fn splitlines(value: &str) -> Vec<&str> {
    if value.is_empty() {
        return vec![];
    }
    let mut lines = vec![];
    let mut start = 0;
    let mut iter = value.char_indices().peekable();
    while let Some((index, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            lines.push(&value[start..index]);
            start = index + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, c)| *c == '\n') {
                let (index, c) = iter.next().expect("peeked newline");
                start = index + c.len_utf8();
            }
        }
    }
    if start < value.len() {
        lines.push(&value[start..]);
    }
    lines
}
pub fn user_id(value: &str) -> Result<&str, ArchiveError> {
    if value.is_empty()
        || value.len() > 19
        || !value.bytes().all(|b| b.is_ascii_digit())
        || value.parse::<i64>().ok().is_none_or(|n| n <= 0)
    {
        Err(ArchiveError::new(400, "Enter a valid Discord user ID."))
    } else {
        Ok(value)
    }
}
pub fn image_url(uid: &str, hash: Option<&str>, kind: &str, size: usize) -> Option<String> {
    let hash = hash?;
    let body = hash.strip_prefix("a_").unwrap_or(hash);
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!(
        "https://cdn.discordapp.com/{kind}/{uid}/{hash}.{}?size={size}",
        if hash.starts_with("a_") { "gif" } else { "png" }
    ))
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}
pub(crate) fn fallback(values: &[&Value]) -> Value {
    values
        .iter()
        .find(|v| truthy(v))
        .or_else(|| values.last())
        .map_or(Value::Null, |v| (*v).clone())
}
fn object_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let mut value = serde_json::Map::new();
    for (i, name) in row.as_ref().column_names().iter().enumerate() {
        let field = match row.get_ref(i)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(n) => json!(n),
            rusqlite::types::ValueRef::Real(n) => json!(n),
            rusqlite::types::ValueRef::Text(s) => json!(String::from_utf8_lossy(s)),
            _ => Value::Null,
        };
        value.insert((*name).to_owned(), field);
    }
    Ok(Value::Object(value))
}
fn exists(db: &Connection, table: &str) -> rusqlite::Result<bool> {
    Ok(db
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE name=? AND type='table'",
            [table],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}
fn valid_asset_url(url: &str) -> bool {
    [
        "https://cdn.discordapp.com/",
        "https://cdn.discordapp.net/",
        "https://media.discordapp.com/",
        "https://media.discordapp.net/",
    ]
    .iter()
    .any(|prefix| url.starts_with(prefix))
}
impl Archive {
    pub fn summary(&self) -> Result<Value, ArchiveError> {
        let session = self.connect()?;
        let db = session.database();
        let total: i64 = db
            .query_row(
                "SELECT count FROM stats_counts WHERE kind='total' AND key=''",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let servers: i64 = db.query_row(
            "SELECT COUNT(*) FROM stats_counts WHERE kind='guild' AND key!='' AND count>0",
            [],
            |r| r.get(0),
        )?;
        Ok(json!({"messages":total,"servers":servers}))
    }
    pub fn profile(&self, uid: &str) -> Result<Value, ArchiveError> {
        let session = self.connect()?;
        let uid = user_id(uid)?;
        let db = session.database();
        let author: Option<Option<String>> = db
            .query_row("SELECT name FROM authors WHERE id=?", [uid], |r| r.get(0))
            .optional()?;
        let basic = db
            .query_row("SELECT * FROM profiles WHERE user_id=?", [uid], object_row)
            .optional()?;
        if author.is_none() && basic.is_none() {
            return Err(ArchiveError::new(404, "This user is not in the archive."));
        }
        let extended = if exists(db, "profile_details")? {
            db.query_row(
                "SELECT payload,fetched_at FROM profile_details WHERE user_id=?",
                [uid],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
        } else {
            None
        };
        let payload: Value = if let Some((body, _)) = &extended {
            serde_json::from_str(body).map_err(|_| {
                ArchiveError::new(
                    503,
                    "The archive could not be read. Check its location and access permissions.",
                )
            })?
        } else {
            json!({})
        };
        let scraped = basic.is_some();
        let basic = basic.unwrap_or_else(|| json!({}));
        let user = &payload["user"];
        let details = &payload["user_profile"];
        let author = match author {
            Some(value) => json!(value),
            None => json!(uid),
        };
        let fetched_at = extended
            .as_ref()
            .map_or_else(|| basic["fetched_at"].clone(), |(_, at)| json!(at));
        let age = fetched_at
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|at| (Utc::now().signed_duration_since(at).num_seconds()).max(0));
        let avatar = image_url(
            uid,
            fallback(&[&user["avatar"], &basic["avatar_hash"]]).as_str(),
            "avatars",
            256,
        );
        let banner = image_url(
            uid,
            fallback(&[&details["banner"], &user["banner"], &basic["banner_hash"]]).as_str(),
            "banners",
            1024,
        );
        let mut statuses = serde_json::Map::new();
        for (kind, url) in [("avatar", &avatar), ("banner", &banner)] {
            if let Some(url) = url {
                statuses.insert(kind.into(), media::status(&self.path, url, false));
            }
        }
        let presence = &payload["presence"];
        let online = presence["status"]
            .as_str()
            .filter(|s| ["online", "idle", "dnd", "offline"].contains(s));
        let custom = presence["activities"]
            .as_array()
            .and_then(|a| a.iter().find(|a| a["type"] == 4 && a["state"].is_string()))
            .and_then(|a| a["state"].as_str());
        let count: i64 = db
            .query_row(
                "SELECT count FROM stats_counts WHERE kind='author' AND key=?",
                [uid],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let created = DateTime::<Utc>::from_timestamp_millis(
            (uid.parse::<i64>().unwrap_or(0) >> 22) + 1_420_070_400_000,
        )
        .ok_or_else(|| ArchiveError::new(400, "Enter a valid Discord user ID."))?
        .to_rfc3339_opts(SecondsFormat::AutoSi, false);
        let created = if created.contains('.')
            && created.rfind('.').is_some_and(|i| created.len() - i == 10)
        {
            created.replace("+00:00", "000+00:00")
        } else {
            created
        };
        Ok(
            json!({"id":uid,"username":fallback(&[&user["username"],&basic["username"],&author]),
            "display_name":fallback(&[&user["global_name"],&basic["global_name"],&author]),
            "avatar_url":avatar.as_deref().map(|s|media::local_url(&self.path,s)),"banner_url":banner.as_deref().map(|s|media::local_url(&self.path,s)),"media":statuses,
            "bio":fallback(&[&details["bio"],&user["bio"],&json!("")]),"online_status":online,"custom_status":custom,
            "pronouns":fallback(&[&details["pronouns"],&json!("")]),"accent_color":fallback(&[&details["accent_color"],&user["accent_color"],&basic["accent_color"]]),
            "badges":fallback(&[&payload["badges"],&json!([])]),"connections":fallback(&[&payload["connected_accounts"],&json!([])]),
            "bot":truthy(user.get("bot").unwrap_or(&basic["bot"])),"public_flags":user.get("public_flags").unwrap_or(&basic["public_flags"]),
            "created_at":created,"fetched_at":fetched_at,"profile_age_seconds":age,"profile_stale":age.is_none_or(|age|age>=86400),
            "observation_kind":if extended.is_some(){"profile_fetch"}else{"message_author"},"presence_observed_at":if online.is_some()||custom.is_some_and(|s|!s.is_empty()){fetched_at.clone()}else{Value::Null},
            "extended":extended.is_some(),"scraped":scraped,"message_count":count,"premium_since":payload["premium_since"],"details":payload}),
        )
    }
    pub fn profile_servers(
        &self,
        uid: &str,
        q: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value, ArchiveError> {
        let session = self.connect()?;
        let uid = user_id(uid)?;
        let db = session.database();
        db.create_scalar_function(
            "search_label",
            1,
            FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                Ok(search_label(
                    ctx.get::<Option<String>>(0)?.as_deref().unwrap_or(""),
                ))
            },
        )?;
        let pattern = format!("%{}%", search::like_literal(&search_label(q)));
        let values: Vec<Value> = if session.decoder.is_some() {
            db.prepare("SELECT s.guild_id id,COALESCE(g.name,'Unknown server') name,SUM(m.messages) messages,CAST(record_id(MAX(m.latest)) AS TEXT) latest_id
                FROM (SELECT source_id,COUNT(*) messages,MAX(rowid) latest FROM message_index WHERE author_id=(SELECT ref FROM ref_values WHERE value=?) GROUP BY source_id) m
                JOIN message_sources s ON s.ref=m.source_id LEFT JOIN guilds g ON g.id=s.guild_id WHERE s.guild_id IS NOT NULL AND (search_label(g.name) LIKE ? ESCAPE '!' OR s.guild_id=?)
                GROUP BY s.guild_id ORDER BY messages DESC,CAST(s.guild_id AS INTEGER) LIMIT ? OFFSET ?")?
                .query_map(params![uid,pattern,search::strip(q),limit as i64+1,offset as i64],object_row)?.collect::<rusqlite::Result<_>>()?
        } else {
            db.prepare("SELECT CAST(m.guild_id AS TEXT) id,COALESCE(g.name,'Unknown server') name,m.messages,CAST(m.latest_id AS TEXT) latest_id
                FROM (SELECT guild_id,COUNT(*) messages,MAX(id) latest_id FROM messages WHERE author_id=? AND guild_id IS NOT NULL GROUP BY guild_id) m
                LEFT JOIN guilds g ON g.id=CAST(m.guild_id AS TEXT) WHERE search_label(g.name) LIKE ? ESCAPE '!' OR CAST(m.guild_id AS TEXT)=?
                ORDER BY messages DESC,m.guild_id LIMIT ? OFFSET ?")?
                .query_map(params![uid,pattern,search::strip(q),limit as i64+1,offset as i64],object_row)?.collect::<rusqlite::Result<_>>()?
        };
        Ok(
            json!({"servers":values[..limit.min(values.len())],"has_more":values.len()>limit,"offset":offset,"observation_kind":"archived_messages","current_membership":null}),
        )
    }
    pub fn suggestions(
        &self,
        kind: &str,
        q: &str,
        guild_id: Option<&str>,
    ) -> Result<Value, ArchiveError> {
        if !["server", "channel", "author"].contains(&kind) {
            return Err(ArchiveError::new(422, "Invalid suggestion kind"));
        }
        let q = search::strip(q);
        if kind == "author" && q.chars().count() < 2 && !unicode_digits(q) {
            return Ok(json!([]));
        }
        if kind == "author" && q.is_empty() {
            return Ok(json!([]));
        }
        let session = self.connect()?;
        let db = session.database();
        let pattern = format!("{}%", search::like_literal(q));
        let values: Vec<Value> = if kind == "author" {
            db.prepare("SELECT a.id,a.name,p.username FROM authors a LEFT JOIN profiles p ON p.user_id=a.id WHERE a.id IN (
            SELECT id FROM authors WHERE name LIKE ? ESCAPE '!' UNION SELECT user_id FROM profiles WHERE username LIKE ? ESCAPE '!' UNION SELECT id FROM authors WHERE id=?) ORDER BY a.name COLLATE NOCASE,a.id LIMIT 20")?
            .query_map(params![pattern,pattern,q],object_row)?.collect::<rusqlite::Result<_>>()?
        } else {
            db.create_scalar_function(
                "search_label",
                1,
                FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
                |ctx| {
                    Ok(search_label(
                        ctx.get::<Option<String>>(0)?.as_deref().unwrap_or(""),
                    ))
                },
            )?;
            let table = if kind == "server" {
                "guilds"
            } else {
                "channels"
            };
            let scoped = kind == "channel" && guild_id.is_some_and(|g| !g.is_empty());
            let sql = format!(
                "SELECT id,name FROM {table} WHERE (search_label(name) LIKE ? ESCAPE '!' OR id=?) {} ORDER BY name COLLATE NOCASE,id LIMIT 20",
                if scoped { "AND guild_id=?" } else { "" }
            );
            let mut params = vec![
                format!("{}%", search::like_literal(&search_label(q))),
                q.to_owned(),
            ];
            if scoped {
                params.push(guild_id.unwrap_or_default().to_owned());
            }
            db.prepare(&sql)?
                .query_map(params_from_iter(params), object_row)?
                .collect::<rusqlite::Result<_>>()?
        };
        Ok(json!(values))
    }
    pub fn search(&self, options: &SearchOptions) -> Result<Value, ArchiveError> {
        let started = Instant::now();
        let result = match self.search_once(options) {
            Err(error) if error.status == 408 && !options.scan => {
                let mut scan = options.clone();
                scan.scan = true;
                self.search_once(&scan)
            }
            result => result,
        };
        result.map(|mut result| {
            result["elapsed_ms"] =
                json!((started.elapsed().as_secs_f64() * 10000.0).round() / 10.0);
            result
        })
    }
    fn search_once(&self, options: &SearchOptions) -> Result<Value, ArchiveError> {
        let (session, page) = self.search_session(options)?;
        let db = session.database();
        let payload_table = if session.decoder.is_some() {
            "message_attachments"
        } else {
            "message_payloads"
        };
        let has_payload = exists(db, payload_table)?;
        let mut messages = vec![];
        for record in page.records {
            let uid = record.author_id.as_deref().unwrap_or("");
            let avatar: Option<Option<String>> = db
                .query_row(
                    "SELECT avatar_hash FROM profiles WHERE user_id=?",
                    [uid],
                    |r| r.get(0),
                )
                .optional()?;
            let avatar = if !uid.is_empty() && uid.bytes().all(|b| b.is_ascii_digit()) {
                image_url(uid, avatar.flatten().as_deref(), "avatars", 64)
            } else {
                None
            };
            let urls = record.image_urls.clone().unwrap_or_default();
            let id = record.id.clone();
            let mut message = serde_json::to_value(record).map_err(|_| {
                ArchiveError::new(
                    503,
                    "The archive could not be read. Check its location and access permissions.",
                )
            })?;
            message
                .as_object_mut()
                .expect("record object")
                .remove("image_urls");
            message["avatar_url"] =
                json!(avatar.as_deref().map(|s| media::local_url(&self.path, s)));
            message["attachments"] = json!(
                splitlines(&urls)
                    .into_iter()
                    .enumerate()
                    .filter(|(_, url)| valid_asset_url(url))
                    .map(
                        |(i, url)| if media::status(&self.path, url, false)["status"] == "saved" {
                            media::local_url(&self.path, url)
                        } else if options.refresh_images {
                            format!("/api/images/{id}/{i}")
                        } else {
                            url.to_owned()
                        }
                    )
                    .collect::<Vec<_>>()
            );
            let raw: Option<String> = if has_payload {
                db.query_row(
                    &format!("SELECT payload FROM {payload_table} WHERE message_id=?"),
                    [&id],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                None
            };
            let payload: Value = raw
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| {
                    ArchiveError::new(
                        503,
                        "The archive could not be read. Check its location and access permissions.",
                    )
                })?
                .unwrap_or(Value::Null);
            let files = if session.decoder.is_some() {
                &payload
            } else {
                &payload["attachments"]
            };
            message["attachment_files"]=json!(files.as_array().into_iter().flatten().filter_map(|attachment|{let url=attachment["url"].as_str()?;if !valid_asset_url(url){return None;}
                Some(json!({"filename":fallback(&[&attachment["filename"],&json!("attachment")]),"url":media::local_url(&self.path,url),"media":media::status(&self.path,url,false)}))}).collect::<Vec<_>>());
            messages.push(message);
        }
        Ok(
            json!({"messages":messages,"has_more":page.has_more,"next_cursor":page.next_cursor,"scan":page.scan,"partial":page.partial,
            "elapsed_ms":0.0}),
        )
    }
}
