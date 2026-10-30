//! Credential-free HTTP adapter. SQLite and digest operations run on blocking workers.
mod assets;
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use searchcord_archive::{Archive, ArchiveError, SearchOptions, media};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write};
use tower::{Layer, ServiceExt};
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data: https://cdn.discordapp.com; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";
pub fn router(archive: Archive) -> Router {
    Router::new().fallback(dispatch).with_state(archive)
}
fn json_response(status: u16, value: Value) -> Response {
    (StatusCode::from_u16(status).unwrap(), axum::Json(value)).into_response()
}
fn error(e: ArchiveError) -> Response {
    json_response(e.status, json!({"detail":e.detail}))
}
fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "/" | "/index.html" => (
            include_bytes!("../../../static/search/index.html"),
            "text/html; charset=utf-8",
        ),
        "/search.css" => (
            include_bytes!("../../../static/search/search.css"),
            "text/css; charset=utf-8",
        ),
        "/search.js" => (
            include_bytes!("../../../static/search/search.js"),
            "text/javascript; charset=utf-8",
        ),
        "/privacy.html" => (
            include_bytes!("../../../static/search/privacy.html"),
            "text/html; charset=utf-8",
        ),
        "/privacy.css" => (
            include_bytes!("../../../static/search/privacy.css"),
            "text/css; charset=utf-8",
        ),
        "/bauhaus.css" => (
            include_bytes!("../../../static/bauhaus.css"),
            "text/css; charset=utf-8",
        ),
        "/profile-view.css" => (
            include_bytes!("../../../static/profile-view.css"),
            "text/css; charset=utf-8",
        ),
        "/profile-view.js" => (
            include_bytes!("../../../static/profile-view.js"),
            "text/javascript; charset=utf-8",
        ),
        _ => return None,
    })
}
#[derive(Default)]
struct Validation {
    params: BTreeMap<String, String>,
    errors: Vec<Value>,
}
impl Validation {
    fn new(query: &str) -> Self {
        Self {
            params: url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect(),
            ..Self::default()
        }
    }
    fn string(&self, key: &str) -> Option<String> {
        self.params.get(key).cloned()
    }
    fn text(&mut self, key: &str, max: usize) -> String {
        let value = self.string(key).unwrap_or_default();
        if value.chars().count() > max {
            self.errors.push(json!({"type":"string_too_long","loc":["query",key],"msg":format!("String should have at most {max} characters"),"input":value,"ctx":{"max_length":max}}));
        }
        value
    }
    fn number(&mut self, key: &str, default: Option<i64>, min: i64, max: i64) -> Option<i64> {
        let Some(raw) = self.string(key) else {
            return default;
        };
        // Pydantic accepts whitespace, + signs and integral decimal strings.
        let numeric = raw.trim();
        let integer = if let Some((whole, fraction)) = numeric.split_once('.') {
            if !fraction.is_empty() && fraction.bytes().all(|b| b == b'0') {
                whole
            } else {
                ""
            }
        } else {
            numeric
        };
        let parsed = integer.parse::<i128>().ok().or_else(|| {
            let digits = integer.trim_start_matches(['+', '-']);
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                Some(if integer.starts_with('-') {
                    i128::MIN
                } else {
                    i128::MAX
                })
            } else {
                None
            }
        });
        match parsed {
            Some(n) if n >= min as i128 && n <= max as i128 => Some(n as i64),
            Some(n) => {
                let (kind, msg, ctx) = if n < min as i128 {
                    (
                        "greater_than_equal",
                        format!("Input should be greater than or equal to {min}"),
                        json!({"ge":min}),
                    )
                } else {
                    (
                        "less_than_equal",
                        format!("Input should be less than or equal to {max}"),
                        json!({"le":max}),
                    )
                };
                self.errors
                    .push(json!({"type":kind,"loc":["query",key],"msg":msg,"input":raw,"ctx":ctx}));
                default
            }
            None => {
                self.errors.push(json!({"type":"int_parsing","loc":["query",key],"msg":"Input should be a valid integer, unable to parse string as an integer","input":raw}));
                default
            }
        }
    }
    fn boolean(&mut self, key: &str) -> bool {
        let Some(raw) = self.string(key) else {
            return false;
        };
        match raw.to_lowercase().as_str() {
            "true" | "1" | "on" | "yes" | "t" | "y" => true,
            "false" | "0" | "off" | "no" | "f" | "n" => false,
            _ => {
                self.errors.push(json!({"type":"bool_parsing","loc":["query",key],"msg":"Input should be a valid boolean, unable to interpret input","input":raw}));
                false
            }
        }
    }
}
fn api(archive: &Archive, path: &str, query: &str) -> Response {
    let mut v = Validation::new(query);
    let operation: Box<dyn FnOnce() -> Result<Value, ArchiveError> + '_> = match path {
        "/api/summary" => Box::new(|| archive.summary()),
        "/api/search" => {
            let options = SearchOptions {
                q: v.text("q", 200),
                guild_id: v.string("guild_id"),
                channel_id: v.string("channel_id"),
                author_id: v.string("author_id"),
                date_from: v.string("date_from"),
                date_to: v.string("date_to"),
                before: v.number("before", None, 1, i64::MAX),
                limit: v.number("limit", Some(40), 1, 100).unwrap_or(40) as usize,
                scan: v.boolean("scan"),
                ..SearchOptions::default()
            };
            Box::new(move || archive.search(&options))
        }
        _ if path.starts_with("/api/suggestions/")
            && !path[17..].is_empty()
            && !path[17..].contains('/') =>
        {
            let kind = &path[17..];
            let q = v.text("q", 100);
            let guild = v.string("guild_id");
            if !["server", "channel", "author"].contains(&kind) {
                v.errors.insert(0,json!({"type":"literal_error","loc":["path","kind"],"msg":"Input should be 'server', 'channel' or 'author'","input":kind,"ctx":{"expected":"'server', 'channel' or 'author'"}}));
            }
            Box::new(move || archive.suggestions(kind, &q, guild.as_deref()))
        }
        _ if path.starts_with("/api/profiles/") => {
            let tail = &path[14..];
            if let Some(uid) = tail.strip_suffix("/servers").filter(|s| !s.contains('/')) {
                let q = v.text("q", 100);
                let offset = v.number("offset", Some(0), 0, i64::MAX).unwrap_or(0) as usize;
                let limit = v.number("limit", Some(30), 1, 100).unwrap_or(30) as usize;
                Box::new(move || archive.profile_servers(uid, &q, offset, limit))
            } else if !tail.contains('/') && !tail.is_empty() {
                Box::new(move || archive.profile(tail))
            } else {
                return json_response(404, json!({"detail":"Not Found"}));
            }
        }
        _ => return json_response(404, json!({"detail":"Not Found"})),
    };
    if !v.errors.is_empty() {
        return json_response(422, json!({"detail":v.errors}));
    }
    match operation() {
        Ok(value) => json_response(200, value),
        Err(e) => error(e),
    }
}
async fn dispatch(State(archive): State<Archive>, mut req: Request) -> Response {
    let path = percent_encoding::percent_decode_str(req.uri().path())
        .decode_utf8_lossy()
        .into_owned();
    let method = req.method().clone();
    let query = req.uri().query().unwrap_or("").to_owned();
    let gzip = req
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("gzip"));
    let mut response = if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        json_response(405, json!({"detail":"Method Not Allowed"}))
    } else if method == axum::http::Method::HEAD
        && (path.starts_with("/api/")
            || ["/bauhaus.css", "/profile-view.css", "/profile-view.js"].contains(&path.as_str()))
    {
        json_response(404, json!({"detail":"Not Found"}))
    } else if let Some(key) = path
        .strip_prefix("/api/media/")
        .filter(|key| !key.is_empty() && !key.contains('/'))
    {
        let database = archive.path.clone();
        let key = key.to_owned();
        match tokio::task::spawn_blocking(move || media::verified_file(&database, &key)).await {
            Ok(Ok(file)) => {
                // Explicit FileResponse routes do not use StaticFiles conditional caching.
                req.headers_mut().remove(header::IF_NONE_MATCH);
                req.headers_mut().remove(header::IF_MODIFIED_SINCE);
                let etag = format!("\"{}\"", file.digest);
                if let Some(value) = req
                    .headers()
                    .get(header::IF_RANGE)
                    .and_then(|v| v.to_str().ok())
                {
                    if value == etag {
                        req.headers_mut().remove(header::IF_RANGE);
                    } else if value.starts_with('"') || value.starts_with("W/") {
                        req.headers_mut().remove(header::RANGE);
                        req.headers_mut().remove(header::IF_RANGE);
                    }
                }
                let inline = file.inline;
                let size = file.bytes;
                let service = tower_http::compression::CompressionLayer::new()
                    .gzip(true)
                    .quality(tower_http::CompressionLevel::Precise(5))
                    .compress_when(
                        move |_: StatusCode,
                              _: axum::http::Version,
                              _: &axum::http::HeaderMap,
                              _: &axum::http::Extensions| {
                            !inline && size >= 1024
                        },
                    )
                    .layer(tower_http::services::ServeFile::new(&file.path));
                match service.oneshot(req).await {
                    Ok(r) => {
                        let mut r = r.map(Body::new);
                        r.headers_mut().insert(header::ETAG, etag.parse().unwrap());
                        r.headers_mut()
                            .insert(header::CONTENT_TYPE, file.content_type.parse().unwrap());
                        r.headers_mut().insert(
                            header::CONTENT_DISPOSITION,
                            if file.inline {
                                "inline"
                            } else {
                                "attachment; filename=\"attachment\""
                            }
                            .parse()
                            .unwrap(),
                        );
                        if file.inline {
                            r.headers_mut().remove(header::CONTENT_DISPOSITION);
                        }
                        r
                    }
                    Err(_) => error(ArchiveError::new(404, "Archived media not found.")),
                }
            }
            Ok(Err(e)) => error(e),
            Err(_) => error(ArchiveError::new(
                503,
                "The archive could not be read. Check its location and access permissions.",
            )),
        }
    } else if path.starts_with("/api/") {
        let p = path.clone();
        match tokio::task::spawn_blocking(move || api(&archive, &p, &query)).await {
            Ok(r) => r,
            Err(_) => json_response(500, json!({"detail":"Internal Server Error"})),
        }
    } else if let Some((bytes, mime)) = asset(&path) {
        let request_headers = req.headers().clone();
        let p = path.clone();
        let head = method == axum::http::Method::HEAD;
        tokio::task::spawn_blocking(move || assets::serve(bytes, mime, &p, head, &request_headers))
            .await
            .unwrap_or_else(|_| json_response(500, json!({"detail":"Internal Server Error"})))
    } else {
        json_response(
            404,
            json!({"detail":if path.trim_start_matches('/').contains('/') {"Not Found"}else{"Not found"}}),
        )
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        if path.starts_with("/api/") {
            "no-store"
        } else {
            "no-cache"
        }
        .parse()
        .unwrap(),
    );
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert(header::CONTENT_SECURITY_POLICY, CSP.parse().unwrap());
    if !path.starts_with("/api/media/") && method != axum::http::Method::HEAD {
        let (mut parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .unwrap_or_default();
        if bytes.len() >= 1024 {
            parts
                .headers
                .insert(header::VARY, "Accept-Encoding".parse().unwrap());
        }
        let output = if gzip && bytes.len() >= 1024 {
            let source = bytes.to_vec();
            match tokio::task::spawn_blocking(move || {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(5));
                e.write_all(&source)?;
                e.finish()
            })
            .await
            {
                Ok(Ok(compressed)) => {
                    parts
                        .headers
                        .insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
                    parts
                        .headers
                        .insert(header::VARY, "Accept-Encoding".parse().unwrap());
                    compressed
                }
                _ => bytes.to_vec(),
            }
        } else {
            bytes.to_vec()
        };
        parts.headers.insert(
            header::CONTENT_LENGTH,
            output.len().to_string().parse().unwrap(),
        );
        response = Response::from_parts(parts, Body::from(output));
    } else if method == axum::http::Method::HEAD {
        let (parts, _) = response.into_parts();
        response = Response::from_parts(parts, Body::empty());
    }
    response
}
