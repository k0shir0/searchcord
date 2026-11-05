//! Collector HTTP boundary. Workers own jobs; requests own validation and presentation.
use crate::{
    PageContext, Result,
    accounts::Accounts,
    analytics,
    discord::{Discord, Request as DiscordRequest},
    discovery::{Discovery, HttpError, http_error},
    dm_cleanup::DmCleanup,
    invites::{Invites, parse_invites},
    media_collection::{MediaCollector, clear_media},
    monitor::Monitors,
    pacing::{Clock, Control, Options},
    profiles::{ProfileError, Profiles},
    scrapes::{Scrapes, new_id, validate_channels},
    writer::Writer,
};
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_core::Stream;
use rusqlite::OptionalExtension;
use searchcord_archive::{Archive, ArchiveError, SearchOptions, media};
use searchcord_viewer::{Validation, assets};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{Mutex, broadcast, mpsc};
use tower::ServiceExt;
use tower_http::compression::predicate::Predicate;

struct Boundaries {
    admission: Mutex<()>,
    collection: Mutex<()>,
    archive_clearing: AtomicBool,
    credentials_clearing: AtomicBool,
    closed: AtomicBool,
    instance: String,
    streams: Control,
}
pub struct App<C: Clock> {
    pub path: PathBuf,
    pub writer: Writer,
    pub discord: Discord<C>,
    pub profiles: Profiles<C>,
    pub scrapes: Scrapes<C>,
    pub accounts: Accounts,
    pub invites: Invites<C>,
    pub dm: DmCleanup<C>,
    pub monitor: Monitors<C>,
    pub media: MediaCollector,
    clock: Arc<C>,
    boundaries: Arc<Boundaries>,
}
impl<C: Clock> Clone for App<C> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            writer: self.writer.clone(),
            discord: self.discord.clone(),
            profiles: self.profiles.clone(),
            scrapes: self.scrapes.clone(),
            accounts: self.accounts.clone(),
            invites: self.invites.clone(),
            dm: self.dm.clone(),
            monitor: self.monitor.clone(),
            media: self.media.clone(),
            clock: self.clock.clone(),
            boundaries: self.boundaries.clone(),
        }
    }
}
impl<C: Clock> App<C> {
    pub fn instance_id(&self) -> &str {
        &self.boundaries.instance
    }
    /// Constructors recover paused work and never issue upstream requests.
    pub async fn open(
        path: PathBuf,
        client: Discord<C>,
        clock: Arc<C>,
        writer: Writer,
    ) -> Result<Self> {
        let media = MediaCollector::new(path.clone())?;
        Self::open_with_media(path, client, clock, writer, media).await
    }
    /// Explicit media fixture injection keeps HTTP tests independent of external CDNs.
    pub async fn open_with_media(
        path: PathBuf,
        client: Discord<C>,
        clock: Arc<C>,
        writer: Writer,
        media: MediaCollector,
    ) -> Result<Self> {
        let profiles = Profiles::new(path.clone(), client.clone()).with_media(media.clone());
        let accounts = Accounts::new(path.clone(), writer.clone());
        Ok(Self {
            scrapes: Scrapes::open(
                path.clone(),
                writer.clone(),
                client.clone(),
                profiles.clone(),
            )
            .await?
            .with_accounts(accounts.clone())
            .with_media(media.clone()),
            accounts: accounts.clone(),
            invites: Invites::open(path.clone(), client.clone(), clock.clone()).await?,
            dm: DmCleanup::new(path.clone(), client.clone(), clock.clone()),
            monitor: Monitors::open(path.clone(), writer.clone(), client.clone(), clock.clone())
                .await?
                .with_accounts(accounts.clone())
                .with_media(media.clone()),
            media,
            path,
            writer,
            discord: client,
            profiles,
            clock,
            boundaries: Arc::new(Boundaries {
                admission: Mutex::new(()),
                collection: Mutex::new(()),
                archive_clearing: AtomicBool::new(false),
                credentials_clearing: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                instance: new_id()?.replace('-', ""),
                streams: Control::default(),
            }),
        })
    }
    fn available(&self) -> Result<()> {
        if self.boundaries.archive_clearing.load(Ordering::Acquire) {
            return Err(http_error(
                409,
                "Archive clear is in progress; retry after it finishes",
            ));
        }
        if self.boundaries.closed.load(Ordering::Acquire) {
            return Err(http_error(503, "Collector is shutting down"));
        }
        Ok(())
    }
    fn profile_available(&self) -> Result<()> {
        if self.boundaries.archive_clearing.load(Ordering::Acquire) {
            return Err(http_error(409, "Archive clear is in progress"));
        }
        self.available()
    }
    async fn selected(&self) -> Result<(String, String)> {
        if self.boundaries.credentials_clearing.load(Ordering::Acquire) {
            return Err(http_error(
                409,
                "Credential removal is in progress; retry after it finishes",
            ));
        }
        self.accounts.selected().await
    }
    pub async fn shutdown(&self) -> Result<()> {
        let app = self.clone();
        tokio::spawn(async move {
            let _admission = app.boundaries.admission.lock().await;
            app.boundaries.closed.store(true, Ordering::Release);
            let mut first = None;
            for result in [
                app.scrapes.suspend().await,
                app.monitor.suspend().await,
                app.invites.shutdown(true).await,
                app.dm.shutdown().await,
            ] {
                if let Err(error) = result
                    && first.is_none()
                {
                    first = Some(error);
                }
            }
            app.profiles.stop().await;
            let _profiles = app.profiles.exclusion().await;
            if let Err(error) = app.writer.barrier().await
                && first.is_none()
            {
                first = Some(error);
            }
            app.boundaries.streams.stop();
            match first {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
        .await?
    }
    async fn clear(&self, tokens: bool) -> Result<Value> {
        let app = self.clone();
        tokio::spawn(async move {
            let _admission=app.boundaries.admission.lock().await;
            app.available()?;
            let active=!app.scrapes.active().await.is_empty() || !app.monitor.status().await["channels"].as_array().is_none_or(Vec::is_empty);
            if active || if tokens {app.dm.has_jobs().await} else {app.dm.has_running().await} {
                return Err(http_error(409,if tokens {"Stop scrapes, DM cleanup and live monitors before removing tokens"} else {"Stop active and paused scrapes, live monitors and DM cleanup before clearing the archive"}));
            }
            let flag=if tokens {&app.boundaries.credentials_clearing} else {&app.boundaries.archive_clearing};
            flag.store(true,Ordering::Release); let _reset=Reset(flag);
            let _collection=app.boundaries.collection.lock().await;
            if tokens {
                if let Some(id)=app.invites.status().await["job"]["id"].as_str() {app.invites.stop(id).await?;}
                app.profiles.stop().await;
                let _profiles=app.profiles.exclusion().await;
                app.accounts.clear().await
            } else {
                app.profiles.stop().await;
                let _profiles=app.profiles.exclusion().await;
                app.writer.write(|store| {
                    let tx=store.connection_mut().transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    for table in ["message_revisions","message_payloads","messages","message_history","name_values","stats_counts","scrape_cursors","channel_authors","profile_details","profiles","authors","channels","guilds"] {tx.execute(&format!("DELETE FROM {table}"),[])?;}
                    tx.execute("DELETE FROM collection_jobs WHERE kind IN ('scrape','monitor')",[])?;tx.commit()?;Ok(Value::Null)
                }).await.map_err(|_|http_error(503,"Could not clear the archive; retry when storage is available"))?;
                let path=app.path.clone();blocking(move||clear_media(&path)).await.map_err(|_|http_error(503,"Archive rows cleared, but cached media could not be removed; retry clear to finish"))?;
                Ok(json!({"ok":true}))
            }
        }).await?
    }
    async fn archive(&self, id: i64, after: i64, limit: usize) -> Result<Value> {
        if id < 1 {
            return Err(http_error(400, "Invalid message ID"));
        }
        self.writer
            .write(move |store| {
                store
                    .read_message(id, after, limit)?
                    .ok_or_else(|| http_error(404, "Message not archived"))
            })
            .await
    }
    async fn refresh(&self, id: i64) -> Result<Value> {
        self.profile_available()?;
        if id < 1 {
            return Err(http_error(400, "Invalid message ID"));
        }
        let _collection = self.boundaries.collection.lock().await;
        self.profile_available()?;
        let row=self.writer.write(move|store| {
            let row=store.connection().query_row("SELECT channel_id,channel_name,guild_id,guild_name FROM message_records WHERE id=?",[id],|r|Ok(json!([r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?]))).optional()?;
            row.ok_or_else(||http_error(404,"Message not archived"))
        }).await?;
        let (_, token) = self.selected().await?;
        let response = self
            .discord
            .request(
                DiscordRequest::new(
                    reqwest::Method::GET,
                    format!(
                        "/channels/{}/messages/{id}",
                        row[0].as_str().unwrap_or("None")
                    ),
                    &token,
                ),
                Options::default(),
            )
            .await?;
        if response.status == 200 {
            let payload = response.json()?;
            let saved = payload.clone();
            self.writer
                .write(move |store| {
                    store.save_page(
                        &[saved],
                        &PageContext {
                            channel_id: row[0].as_str().unwrap_or("None"),
                            channel_name: row[1].as_str().unwrap_or(""),
                            guild_id: row[2].as_str(),
                            guild_name: row[3].as_str().unwrap_or(""),
                            ..Default::default()
                        },
                    )?;
                    Ok(Value::Null)
                })
                .await?;
            self.media.archive_message_media(&[payload], None).await?;
        } else if response.status == 404 && response.json().ok().is_some_and(|v| v["code"] == 10008)
        {
            let accessible = self
                .discord
                .request(
                    DiscordRequest::new(
                        reqwest::Method::GET,
                        format!("/channels/{}", row[0].as_str().unwrap_or("None")),
                        &token,
                    ),
                    Options::default(),
                )
                .await?;
            if accessible.status != 200 {
                return Err(http_error(
                    409,
                    "Channel access could not be confirmed; deletion was not recorded",
                ));
            }
            self.writer
                .write(move |store| {
                    store.mark_deleted(&[id], "upstream_not_found")?;
                    Ok(Value::Null)
                })
                .await?;
        } else {
            return Err(http_error(
                response.status,
                "Could not refresh this message; archived contents were kept",
            ));
        }
        self.archive(id, 0, 100).await
    }
    async fn image(&self, id: i64, index: i64) -> Result<Response> {
        self.profile_available()?;
        let _collection = self.boundaries.collection.lock().await;
        self.profile_available()?;
        let path = self.path.clone();
        let image = blocking(move || analytics::image_lookup(path, id, index)).await?;
        let path = self.path.clone();
        let original = image.url.clone();
        let cached = blocking(move || Ok(media::local_url(&path, &original))).await?;
        if cached != image.url {
            return Ok(redirect(&cached));
        }
        let mut url = image.url;
        let parsed = url::Url::parse(&url).ok();
        let expiry = parsed.as_ref().and_then(|u| {
            u.query_pairs()
                .find(|(k, _)| k == "ex")
                .map(|(_, v)| v.into_owned())
        });
        if expiry.is_some_and(|expiry| {
            u128::from_str_radix(expiry.trim().trim_start_matches('+'), 16)
                .map_or(true, |n| n as f64 <= self.clock.wall() + 60.0)
        }) {
            let (_, token) = self.selected().await?;
            let response = self
                .discord
                .request(
                    DiscordRequest::new(
                        reqwest::Method::GET,
                        format!(
                            "/channels/{}/messages/{id}",
                            image
                                .channel_id
                                .map(|n| n.to_string())
                                .unwrap_or("None".into())
                        ),
                        token,
                    ),
                    Options::default(),
                )
                .await?;
            if response.status != 200 {
                return Err(http_error(404, "Image is no longer available"));
            }
            let original = parsed.as_ref().map(|u| u.path().to_owned());
            url = crate::storage::image_urls(&response.json()?["attachments"])
                .and_then(|urls| {
                    urls.split('\n')
                        .find(|fresh| {
                            url::Url::parse(fresh)
                                .ok()
                                .is_some_and(|u| Some(u.path()) == original.as_deref())
                        })
                        .map(str::to_owned)
                })
                .ok_or_else(|| http_error(404, "Image is no longer available"))?;
        }
        self.media.cache_media(&url, None).await?;
        let path = self.path.clone();
        let cached = blocking(move || Ok(media::local_url(&path, &url))).await?;
        Ok(redirect(&cached))
    }
}
struct Reset<'a>(&'a AtomicBool);
impl Drop for Reset<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
pub fn router<C: Clock>(app: App<C>) -> Router {
    Router::new().fallback(dispatch::<C>).with_state(app).layer(
        tower_http::compression::CompressionLayer::new()
            .gzip(true)
            .quality(tower_http::CompressionLevel::Precise(5))
            .compress_when(
                tower_http::compression::predicate::DefaultPredicate::new()
                    .and(tower_http::compression::predicate::SizeAbove::new(1024)),
            ),
    )
}
fn json_response(status: u16, value: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        axum::Json(value),
    )
        .into_response()
}
fn profile_state(state: impl serde::Serialize) -> Result<Value> {
    let mut value = serde_json::to_value(state)?;
    if value["wait_seconds"].as_f64() == Some(0.0) {
        value["wait_seconds"] = json!(0);
    }
    Ok(value)
}
fn redirect(url: &str) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::FOUND;
    if let Ok(value) = url.parse() {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}
#[derive(Debug)]
struct ApiError {
    status: u16,
    body: Value,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.status)
    }
}
impl std::error::Error for ApiError {}
fn invalid(errors: Vec<Value>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(ApiError {
        status: 422,
        body: json!({"detail":errors}),
    })
}
fn error(error: Box<dyn std::error::Error + Send + Sync>) -> Response {
    if let Some(e) = error.downcast_ref::<ApiError>() {
        return json_response(e.status, e.body.clone());
    }
    if let Some(e) = error.downcast_ref::<HttpError>() {
        return json_response(e.status, json!({"detail":e.detail}));
    }
    if let Some(e) = error.downcast_ref::<ArchiveError>() {
        return json_response(e.status, json!({"detail":e.detail}));
    }
    if let Some(e) = error.downcast_ref::<ProfileError>() {
        return json_response(e.status, json!({"detail":e.detail}));
    }
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}
struct Model {
    input: Value,
    errors: Vec<Value>,
}
impl Model {
    fn new(input: Value) -> Result<Self> {
        if !input.is_object() {
            return Err(invalid(vec![
                json!({"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":input}),
            ]));
        }
        Ok(Self {
            input,
            errors: vec![],
        })
    }
    fn field(&mut self, key: &str, required: bool) -> Option<Value> {
        match self.input.get(key) {
            Some(v) => Some(v.clone()),
            None => {
                if required {
                    self.errors.push(json!({"type":"missing","loc":["body",key],"msg":"Field required","input":self.input}));
                }
                None
            }
        }
    }
    fn add(&mut self, key: &str, kind: &str, msg: &str, input: Value, ctx: Option<Value>) {
        let mut e = json!({"type":kind,"loc":["body",key],"msg":msg,"input":input});
        if let Some(ctx) = ctx {
            e["ctx"] = ctx;
        }
        self.errors.push(e);
    }
    fn string(&mut self, key: &str, required: bool) -> Option<String> {
        let value = self.field(key, required)?;
        if !required && value.is_null() {
            return None;
        }
        match value.as_str() {
            Some(s) => Some(s.into()),
            None => {
                self.add(
                    key,
                    "string_type",
                    "Input should be a valid string",
                    value,
                    None,
                );
                None
            }
        }
    }
    fn list(&mut self, key: &str, required: bool) -> Value {
        let Some(value) = self.field(key, required) else {
            return json!([]);
        };
        if value.is_array() {
            value
        } else {
            self.add(
                key,
                "list_type",
                "Input should be a valid list",
                value,
                None,
            );
            json!([])
        }
    }
    fn boolean(&mut self, key: &str, default: bool) -> bool {
        let Some(value) = self.field(key, false) else {
            return default;
        };
        let result = match &value {
            Value::Bool(b) => Some(*b),
            Value::Number(n) if n.as_i64() == Some(0) => Some(false),
            Value::Number(n) if n.as_i64() == Some(1) => Some(true),
            Value::String(s) => match s.to_lowercase().as_str() {
                "true" | "1" | "on" | "yes" | "t" | "y" => Some(true),
                "false" | "0" | "off" | "no" | "f" | "n" => Some(false),
                _ => None,
            },
            _ => None,
        };
        result.unwrap_or_else(|| {
            self.add(
                key,
                if value.is_string() || value.is_number() {
                    "bool_parsing"
                } else {
                    "bool_type"
                },
                if value.is_string() || value.is_number() {
                    "Input should be a valid boolean, unable to interpret input"
                } else {
                    "Input should be a valid boolean"
                },
                value,
                None,
            );
            default
        })
    }
    fn integer(&mut self, key: &str, default: u64) -> u64 {
        let Some(value) = self.field(key, false) else {
            return default;
        };
        let n = match &value {
            Value::Bool(b) => Some(i128::from(*b)),
            Value::Number(n) => n
                .as_i64()
                .map(i128::from)
                .or_else(|| n.as_u64().map(i128::from))
                .or_else(|| {
                    n.as_f64()
                        .filter(|v| v.is_finite() && v.fract() == 0.0)
                        .map(|v| v as i128)
                }),
            Value::String(s) => s.trim().parse::<i128>().ok().or_else(|| {
                s.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && v.fract() == 0.0 && !s.contains(['e', 'E']))
                    .map(|v| v as i128)
            }),
            _ => None,
        };
        match n {
            Some(n) if n >= 0 => u64::try_from(n).unwrap_or(u64::MAX),
            Some(_) => {
                self.add(
                    key,
                    "greater_than_equal",
                    "Input should be greater than or equal to 0",
                    value,
                    Some(json!({"ge":0})),
                );
                default
            }
            None => {
                let (kind, msg) = if value.is_string() {
                    (
                        "int_parsing",
                        "Input should be a valid integer, unable to parse string as an integer",
                    )
                } else if value.is_f64() {
                    (
                        "int_from_float",
                        "Input should be a valid integer, got a number with a fractional part",
                    )
                } else {
                    ("int_type", "Input should be a valid integer")
                };
                self.add(key, kind, msg, value, None);
                default
            }
        }
    }
    fn finish(self) -> Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(invalid(self.errors))
        }
    }
}
async fn body(request: Request) -> Result<Model> {
    let json_content = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if !json_content {
        return Err(http_error(
            415,
            "JSON requests require Content-Type: application/json",
        ));
    }
    let raw = to_bytes(request.into_body(), usize::MAX).await?;
    if raw.is_empty() {
        return Err(invalid(vec![
            json!({"type":"missing","loc":["body"],"msg":"Field required","input":null}),
        ]));
    }
    let input=serde_json::from_slice(&raw).map_err(|e|invalid(vec![json!({"type":"json_invalid","loc":["body",e.column().saturating_sub(1)],"msg":"JSON decode error","input":{},"ctx":{"error":"Expecting value"}})]))?;
    Model::new(input)
}
fn valid(v: &Validation) -> Result<()> {
    if v.errors.is_empty() {
        Ok(())
    } else {
        Err(invalid(v.errors.clone()))
    }
}
fn path_number(value: &str, key: &str) -> Result<i64> {
    value.trim().parse().map_err(|_|invalid(vec![json!({"type":"int_parsing","loc":["path",key],"msg":"Input should be a valid integer, unable to parse string as an integer","input":value})]))
}
fn one<'a>(path: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    let value = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    (!value.is_empty() && !value.contains('/')).then_some(value)
}
fn api_method(path: &str) -> Option<&'static str> {
    Some(match path {
        "/api/health"
        | "/api/token/validate"
        | "/api/guilds"
        | "/api/dms"
        | "/api/scrape/active"
        | "/api/live/status"
        | "/api/live/events"
        | "/api/export/participants"
        | "/api/search"
        | "/api/stats"
        | "/api/stats/contributors"
        | "/api/stats/servers"
        | "/api/search/filters"
        | "/api/search/authors"
        | "/api/invites/status" => "GET",
        "/api/settings" | "/api/profile-backfill" => "GET,POST",
        "/api/settings/tokens" | "/api/messages" => "DELETE",
        "/api/scrape/start"
        | "/api/dm-clear/start"
        | "/api/live/start"
        | "/api/live/stop"
        | "/api/export/chatml"
        | "/api/invites/start"
        | "/api/profile-backfill/stop" => "POST",
        _ if one(path, "/api/guilds/", "/channels").is_some()
            || one(path, "/api/scrape/progress/", "").is_some()
            || one(path, "/api/dm-clear/progress/", "").is_some()
            || one(path, "/api/media/", "").is_some()
            || one(path, "/api/profiles/", "/servers").is_some()
            || one(path, "/api/profiles/", "").is_some()
            || one(path, "/api/messages/", "/archive").is_some()
            || path
                .strip_prefix("/api/images/")
                .is_some_and(|s| s.split('/').count() == 2) =>
        {
            "GET"
        }
        _ if ["pause", "resume", "stop"]
            .iter()
            .any(|s| one(path, "/api/scrape/", &format!("/{s}")).is_some())
            || one(path, "/api/dm-clear/", "/stop").is_some()
            || ["fetch", "refresh"]
                .iter()
                .any(|s| one(path, "/api/profiles/", &format!("/{s}")).is_some())
            || one(path, "/api/messages/", "/refresh").is_some()
            || ["stop", "resume"]
                .iter()
                .any(|s| one(path, "/api/invites/", &format!("/{s}")).is_some()) =>
        {
            "POST"
        }
        _ => return None,
    })
}

async fn api<C: Clock>(
    app: &App<C>,
    path: &str,
    query: &str,
    request: Request,
) -> Result<Response> {
    let method = request.method().clone();
    let mut v = Validation::new(query);
    let result = match path {
        "/api/health" => {
            let mut r = (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "searchcord-ready",
            )
                .into_response();
            r.headers_mut()
                .insert("x-searchcord-instance", app.boundaries.instance.parse()?);
            return Ok(r);
        }
        "/api/token/validate" => match app.selected().await {
            Ok((_, token)) => Discovery::new(app.discord.clone()).validate(&token).await?,
            Err(_) => json!({"valid":false}),
        },
        "/api/guilds" => {
            let (_, token) = app.selected().await?;
            Discovery::new(app.discord.clone()).guilds(&token).await?
        }
        "/api/dms" => {
            let (_, token) = app.selected().await?;
            Discovery::new(app.discord.clone()).dms(&token).await?
        }
        "/api/settings" if method == Method::GET => app.accounts.settings().await?,
        "/api/settings" => {
            let mut m = body(request).await?;
            let token = m.string("token", false);
            let label = m.string("token_label", false);
            let id = m.string("active_token_id", false);
            m.finish()?;
            let _admission = app.boundaries.admission.lock().await;
            app.accounts
                .update(json!({"token":token,"token_label":label,"active_token_id":id}))
                .await?
        }
        "/api/settings/tokens" | "/api/messages" => {
            let confirm = v.boolean("confirm");
            valid(&v)?;
            let tokens = path.ends_with("tokens");
            if !confirm {
                return Err(http_error(
                    400,
                    if tokens {
                        "Confirm removal of all saved tokens"
                    } else {
                        "Confirm removal of all archived messages, profiles, revisions and cached media"
                    },
                ));
            }
            app.clear(tokens).await?
        }
        "/api/search" => {
            let options = SearchOptions {
                q: v.text("q", 200),
                guild_id: v.string("guild_id"),
                channel_id: v.string("channel_id"),
                author_id: v.string("author_id"),
                date_from: v.string("date_from"),
                date_to: v.string("date_to"),
                ..Default::default()
            };
            let page = v.number("page", Some(1), 1, i64::MAX).unwrap_or(1);
            let limit = v.number("limit", Some(50), 1, 100).unwrap_or(50);
            let cursor = v.boolean("cursor");
            let scan = v.boolean("scan");
            let before = v.number("before", None, 1, i64::MAX);
            valid(&v)?;
            let options = SearchOptions {
                limit: limit as usize,
                before,
                scan,
                refresh_images: true,
                ..options
            };
            let path = app.path.clone();
            blocking(move || {
                if cursor {
                    let mut result = Archive::new(path, false).search(&options)?;
                    result["total"] = Value::Null;
                    result["pages"] = Value::Null;
                    result["page"] = json!(page);
                    result["limit"] = json!(limit);
                    Ok(result)
                } else {
                    analytics::counted_search(path, &options, page)
                }
            })
            .await?
        }
        "/api/stats" => {
            let days = v.number("days", Some(90), 0, 3650).unwrap_or(90);
            valid(&v)?;
            let path = app.path.clone();
            blocking(move || analytics::stats(path, days)).await?
        }
        "/api/stats/contributors" | "/api/stats/servers" => {
            let offset = v.number("offset", Some(0), 0, i64::MAX).unwrap_or(0);
            let limit = v.number("limit", Some(50), 1, 100).unwrap_or(50);
            let q = v.text("q", 100);
            valid(&v)?;
            let servers = path == "/api/stats/servers";
            let path = app.path.clone();
            blocking(move || {
                if servers {
                    analytics::ranked_servers(path, offset, limit, &q)
                } else {
                    analytics::contributors(path, offset, limit, &q)
                }
            })
            .await?
        }
        "/api/search/filters" => {
            let include = if v.string("include_users").is_some() {
                v.boolean("include_users")
            } else {
                true
            };
            valid(&v)?;
            let path = app.path.clone();
            blocking(move || analytics::search_filters(path, include)).await?
        }
        "/api/search/authors" => {
            let q = v.string("q").unwrap_or_default();
            let path = app.path.clone();
            blocking(move || analytics::search_authors(path, &q)).await?
        }
        "/api/export/participants" => {
            let cid=v.string("channel_id").ok_or_else(||invalid(vec![json!({"type":"missing","loc":["query","channel_id"],"msg":"Field required","input":null})]))?;
            let path = app.path.clone();
            blocking(move || analytics::participants(path, &cid)).await?
        }
        "/api/export/chatml" => {
            let mut m = body(request).await?;
            let cid = m.string("channel_id", true).unwrap_or_default();
            let target = m.string("target_id", true).unwrap_or_default();
            let other = m.string("other_name", false);
            let system = m.string("system_prompt", false);
            m.finish()?;
            let path = app.path.clone();
            let export = blocking(move || {
                analytics::prepare_chatml(
                    path,
                    &analytics::ChatMLExportRequest {
                        channel_id: cid,
                        target_id: target,
                        other_name: other,
                        system_prompt: system,
                    },
                )
            })
            .await?;
            let name = format!(
                "attachment; filename=\"chatml_{}.jsonl\"",
                export.channel_id
            );
            let (tx, rx) = mpsc::channel(8);
            tokio::task::spawn_blocking(move || {
                let _ = export.stream(|line| {
                    tx.blocking_send(Ok(Bytes::from(line)))
                        .map_err(|_| "Export receiver disconnected".into())
                });
            });
            let mut r = Response::new(Body::from_stream(ReceiverStream::new(
                rx,
                app.boundaries.streams.clone(),
            )));
            r.headers_mut()
                .insert(header::CONTENT_TYPE, "application/x-ndjson".parse()?);
            r.headers_mut()
                .insert(header::CONTENT_DISPOSITION, name.parse()?);
            return Ok(r);
        }
        "/api/profile-backfill" if method == Method::GET => profile_state(app.profiles.state())?,
        "/api/profile-backfill" => {
            let stale = v.boolean("refresh_stale");
            valid(&v)?;
            app.profile_available()?;
            let _collection = app.boundaries.collection.lock().await;
            app.profile_available()?;
            if app.profiles.state().running {
                return Err(http_error(409, "Profile collection is already running."));
            }
            let (_, token) = app.selected().await?;
            profile_state(app.profiles.start(&token, stale).await?)?
        }
        "/api/profile-backfill/stop" => profile_state(app.profiles.stop().await)?,
        "/api/scrape/active" => json!(app.scrapes.active().await),
        "/api/scrape/start" => {
            let mut m = body(request).await?;
            let channels = m.list("channels", true);
            let limit = m.integer("limit", 0);
            let harvest = m.boolean("harvest_profiles", false);
            m.finish()?;
            validate_channels(&channels)?;
            let _admission = app.boundaries.admission.lock().await;
            app.available()?;
            let (id, token) = app.selected().await?;
            json!({"job_id":app.scrapes.start(channels,limit,harvest,id,token).await?})
        }
        "/api/dm-clear/start" => {
            let mut m = body(request).await?;
            let cid = m.string("channel_id", true).unwrap_or_default();
            if m.errors.is_empty()
                && (cid.is_empty() || cid.len() > 19 || !cid.bytes().all(|b| b.is_ascii_digit()))
            {
                m.add(
                    "channel_id",
                    "string_pattern_mismatch",
                    "String should match pattern '^[0-9]{1,19}$'",
                    json!(cid),
                    Some(json!({"pattern":"^[0-9]{1,19}$"})),
                );
            }
            m.finish()?;
            let _admission = app.boundaries.admission.lock().await;
            app.available()?;
            match app.selected().await {
                Ok((_, token)) => app.dm.start(&cid, token).await?,
                Err(error) => {
                    let message = error.downcast_ref::<HttpError>().map(|e|format!("{}: {}",e.status,e.detail)).unwrap_or_else(||"Deletion stopped after an unexpected response; check Discord before retrying.".into());
                    app.dm.start_error(&cid, message).await?
                }
            }
        }
        "/api/live/status" => app.monitor.status().await,
        "/api/live/start" => {
            let mut m = body(request).await?;
            let channels = m.list("channels", true);
            m.finish()?;
            let _admission = app.boundaries.admission.lock().await;
            app.available()?;
            validate_channels(&channels)?;
            let (id, token) = app.selected().await?;
            app.monitor.start(channels, id, token).await?
        }
        "/api/live/stop" => {
            let mut m = body(request).await?;
            let ids = m.list("channel_ids", false);
            m.finish()?;
            let selected: Vec<String> = ids
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if ids.as_array().unwrap().is_empty() {
                app.monitor.stop(vec![]).await?
            } else {
                if !selected.is_empty() {
                    app.monitor.stop(selected).await?;
                }
                json!({"stopped":ids})
            }
        }
        "/api/live/events" => {
            let (initial, events) = app.monitor.subscribe().await;
            return Ok(sse(
                initial,
                events,
                20,
                false,
                app.boundaries.streams.clone(),
            ));
        }
        "/api/invites/status" => app.invites.status().await,
        "/api/invites/start" => {
            let mut m = body(request).await?;
            let text = m.string("invites", true).unwrap_or_default();
            if m.errors.is_empty() && text.is_empty() {
                m.add(
                    "invites",
                    "string_too_short",
                    "String should have at least 1 character",
                    json!(text),
                    Some(json!({"min_length":1})),
                );
            }
            if text.chars().count() > 50000 {
                m.add(
                    "invites",
                    "string_too_long",
                    "String should have at most 50000 characters",
                    json!(text),
                    Some(json!({"max_length":50000})),
                );
            }
            m.finish()?;
            if app.invites.status().await["job"]["running"] == true {
                return Err(http_error(409, "An invite queue is already running."));
            }
            parse_invites(&text)?;
            let _admission = app.boundaries.admission.lock().await;
            let (id, token) = app.selected().await?;
            app.invites.start(&text, Some(id), token).await?
        }
        _ if one(path, "/api/guilds/", "/channels").is_some() => {
            let include = v.boolean("include_threads");
            valid(&v)?;
            let (_, token) = app.selected().await?;
            Discovery::new(app.discord.clone())
                .channels(
                    one(path, "/api/guilds/", "/channels").unwrap(),
                    &token,
                    include,
                )
                .await?
        }
        _ if one(path, "/api/profiles/", "/servers").is_some() => {
            let uid = one(path, "/api/profiles/", "/servers").unwrap().to_owned();
            let q = v.text("q", 100);
            let offset = v.number("offset", Some(0), 0, i64::MAX).unwrap_or(0);
            let limit = v.number("limit", Some(30), 1, 100).unwrap_or(30);
            valid(&v)?;
            let path = app.path.clone();
            blocking(move || {
                Ok(Archive::new(path, false).profile_servers(
                    &uid,
                    &q,
                    offset as usize,
                    limit as usize,
                )?)
            })
            .await?
        }
        _ if one(path, "/api/profiles/", "").is_some() => {
            let uid = one(path, "/api/profiles/", "").unwrap().to_owned();
            let path = app.path.clone();
            blocking(move || Ok(Archive::new(path, false).profile(&uid)?)).await?
        }
        _ if one(path, "/api/profiles/", "/fetch")
            .or_else(|| one(path, "/api/profiles/", "/refresh"))
            .is_some() =>
        {
            let uid = one(path, "/api/profiles/", "/fetch")
                .or_else(|| one(path, "/api/profiles/", "/refresh"))
                .unwrap()
                .to_owned();
            app.profile_available()?;
            let _collection = app.boundaries.collection.lock().await;
            app.profile_available()?;
            let path = app.path.clone();
            let uid_read = uid.clone();
            blocking(move || Ok(Archive::new(path, false).profile(&uid_read)?)).await?;
            let (_, token) = app.selected().await?;
            app.profiles.fetch(&uid, &token, Control::default()).await?;
            let path = app.path.clone();
            blocking(move || Ok(Archive::new(path, false).profile(&uid)?)).await?
        }
        _ if one(path, "/api/messages/", "/archive").is_some() => {
            let id = path_number(
                one(path, "/api/messages/", "/archive").unwrap(),
                "message_id",
            )?;
            let after = v
                .number("after_revision", Some(0), 0, i64::MAX)
                .unwrap_or(0);
            let limit = v.number("limit", Some(100), 1, 100).unwrap_or(100);
            valid(&v)?;
            app.archive(id, after, limit as usize).await?
        }
        _ if one(path, "/api/messages/", "/refresh").is_some() => {
            app.refresh(path_number(
                one(path, "/api/messages/", "/refresh").unwrap(),
                "message_id",
            )?)
            .await?
        }
        _ if path.starts_with("/api/images/") => {
            let values: Vec<_> = path[12..].split('/').collect();
            let id = path_number(values[0], "message_id")?;
            let index = path_number(values[1], "image_index")?;
            return app.image(id, index).await;
        }
        _ if one(path, "/api/scrape/progress/", "").is_some() => {
            let id = one(path, "/api/scrape/progress/", "").unwrap();
            let (snapshot, events) = app.scrapes.subscribe(id).await?;
            let terminal = snapshot["last_event"].clone();
            let mut initial = vec![snapshot];
            if terminal_type(&terminal) {
                initial.push(terminal);
            }
            return Ok(sse(
                initial,
                events,
                25,
                true,
                app.boundaries.streams.clone(),
            ));
        }
        _ if one(path, "/api/dm-clear/progress/", "").is_some() => {
            let id = one(path, "/api/dm-clear/progress/", "").unwrap();
            let (state, events) = app.dm.subscribe(id).await?;
            let initial = if state["terminal"].is_object() {
                vec![state["terminal"].clone()]
            } else {
                vec![]
            };
            return Ok(sse(
                initial,
                events,
                25,
                true,
                app.boundaries.streams.clone(),
            ));
        }
        _ if one(path, "/api/dm-clear/", "/stop").is_some() => {
            app.dm
                .stop(one(path, "/api/dm-clear/", "/stop").unwrap())
                .await?
        }
        _ if one(path, "/api/invites/", "/stop").is_some() => {
            app.invites
                .stop(one(path, "/api/invites/", "/stop").unwrap())
                .await?
        }
        _ if one(path, "/api/invites/", "/resume").is_some() => {
            let (id, token) = app.selected().await?;
            app.invites
                .resume(
                    one(path, "/api/invites/", "/resume").unwrap(),
                    Some(id),
                    token,
                )
                .await?
        }
        _ if one(path, "/api/scrape/", "/pause").is_some() => {
            app.scrapes
                .pause(one(path, "/api/scrape/", "/pause").unwrap())
                .await?
        }
        _ if one(path, "/api/scrape/", "/stop").is_some() => {
            app.scrapes
                .stop(one(path, "/api/scrape/", "/stop").unwrap())
                .await?
        }
        _ if one(path, "/api/scrape/", "/resume").is_some() => {
            let (id, token) = app.selected().await?;
            app.scrapes
                .resume(one(path, "/api/scrape/", "/resume").unwrap(), &id, token)
                .await?
        }
        _ => return Err(http_error(404, "Not Found")),
    };
    Ok(json_response(200, result))
}
struct ReceiverStream {
    receiver: mpsc::Receiver<std::result::Result<Bytes, std::io::Error>>,
    stopped: Pin<Box<dyn Future<Output = ()> + Send>>,
}
impl ReceiverStream {
    fn new(
        receiver: mpsc::Receiver<std::result::Result<Bytes, std::io::Error>>,
        control: Control,
    ) -> Self {
        Self {
            receiver,
            stopped: Box::pin(async move { control.stopped().await }),
        }
    }
}
impl Stream for ReceiverStream {
    type Item = std::result::Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.stopped.as_mut().poll(cx).is_ready() {
            self.receiver.close();
            return Poll::Ready(None);
        }
        self.receiver.poll_recv(cx)
    }
}
fn terminal_type(v: &Value) -> bool {
    matches!(v["type"].as_str(), Some("complete" | "cancelled" | "error"))
}
fn sse(
    initial: Vec<Value>,
    mut events: broadcast::Receiver<Value>,
    heartbeat: u64,
    terminal: bool,
    control: Control,
) -> Response {
    let (tx, rx) = mpsc::channel(8);
    let body_control = control.clone();
    tokio::spawn(async move {
        for event in initial {
            let done = terminal && terminal_type(&event);
            if !send_event(&tx, &control, event).await || done {
                return;
            }
        }
        loop {
            let event = tokio::select! {_=control.stopped()=>return,_=tx.closed()=>return,event=tokio::time::timeout(Duration::from_secs(heartbeat),events.recv())=>match event{Ok(Ok(event))=>event,Ok(Err(broadcast::error::RecvError::Lagged(_)))=>continue,Ok(Err(broadcast::error::RecvError::Closed))=>return,Err(_)=>json!({"type":"ping"})}};
            let done = terminal && terminal_type(&event);
            if !send_event(&tx, &control, event).await || done {
                return;
            }
        }
    });
    let mut r = Response::new(Body::from_stream(ReceiverStream::new(rx, body_control)));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        "text/event-stream; charset=utf-8".parse().unwrap(),
    );
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    r.headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    r
}
async fn send_event(
    tx: &mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    control: &Control,
    event: Value,
) -> bool {
    tokio::select! {biased;_=control.stopped()=>false,result=tx.send(Ok(Bytes::from(format!("data: {event}\n\n"))))=>result.is_ok()}
}

async fn media_response(path: PathBuf, key: String, mut request: Request) -> Result<Response> {
    let file = blocking(move || Ok(media::verified_file(&path, &key)?)).await?;
    request.headers_mut().remove(header::IF_NONE_MATCH);
    request.headers_mut().remove(header::IF_MODIFIED_SINCE);
    let etag = format!("\"{}\"", file.digest);
    if let Some(value) = request
        .headers()
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
    {
        if value == etag {
            request.headers_mut().remove(header::IF_RANGE);
        } else if value.starts_with('"') || value.starts_with("W/") {
            request.headers_mut().remove(header::RANGE);
            request.headers_mut().remove(header::IF_RANGE);
        }
    }
    let response = tower_http::services::ServeFile::new(&file.path)
        .oneshot(request)
        .await?;
    let mut response = response.map(Body::new);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, file.content_type.parse()?);
    headers.insert(header::CACHE_CONTROL, "private, no-store".parse()?);
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; sandbox".parse()?,
    );
    headers.insert("x-content-type-options", "nosniff".parse()?);
    if !file.inline {
        headers.insert(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"attachment\"".parse()?,
        );
    }
    Ok(response)
}
fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "/" | "/index.html" => (
            include_bytes!("../../../static/index.html"),
            "text/html; charset=utf-8",
        ),
        "/style.css" => (
            include_bytes!("../../../static/style.css"),
            "text/css; charset=utf-8",
        ),
        "/home.css" => (
            include_bytes!("../../../static/home.css"),
            "text/css; charset=utf-8",
        ),
        "/workspace.css" => (
            include_bytes!("../../../static/workspace.css"),
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
        "/app.js" => (
            include_bytes!("../../../static/app.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/profiles.js" => (
            include_bytes!("../../../static/profiles.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/stats.js" => (
            include_bytes!("../../../static/stats.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/collection.js" => (
            include_bytes!("../../../static/collection.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/collection-queue.js" => (
            include_bytes!("../../../static/collection-queue.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/profile-view.js" => (
            include_bytes!("../../../static/profile-view.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/vendor/chart.umd.min.js" => (
            include_bytes!("../../../static/vendor/chart.umd.min.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/vendor/chart.LICENSE.md" => (
            include_bytes!("../../../static/vendor/chart.LICENSE.md"),
            "text/markdown; charset=utf-8",
        ),
        "/brand/searchcord-logo.png" => (
            include_bytes!("../../../static/brand/searchcord-logo.png"),
            "image/png",
        ),
        "/brand/searchcord-logo-dark.png" => (
            include_bytes!("../../../static/brand/searchcord-logo-dark.png"),
            "image/png",
        ),
        "/search/" | "/search/index.html" => (
            include_bytes!("../../../static/search/index.html"),
            "text/html; charset=utf-8",
        ),
        "/search/search.css" => (
            include_bytes!("../../../static/search/search.css"),
            "text/css; charset=utf-8",
        ),
        "/search/search.js" => (
            include_bytes!("../../../static/search/search.js"),
            searchcord_viewer::javascript_type(),
        ),
        "/search/privacy.html" => (
            include_bytes!("../../../static/search/privacy.html"),
            "text/html; charset=utf-8",
        ),
        "/search/privacy.css" => (
            include_bytes!("../../../static/search/privacy.css"),
            "text/css; charset=utf-8",
        ),
        _ => return None,
    })
}
fn origin_allowed(value: &str) -> bool {
    let Some(authority) = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
    else {
        return false;
    };
    ["localhost", "127.0.0.1"].into_iter().any(|host| {
        authority == host
            || authority
                .strip_prefix(host)
                .and_then(|tail| tail.strip_prefix(':'))
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
    })
}
async fn dispatch<C: Clock>(State(app): State<App<C>>, request: Request) -> Response {
    let path = percent_encoding::percent_decode_str(request.uri().path())
        .decode_utf8_lossy()
        .into_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let method = request.method().clone();
    let headers = request.headers().clone();
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let preflight = method == Method::OPTIONS
        && headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
        && origin.is_some();
    let outcome = if preflight {
        let allowed = origin.as_ref().is_some_and(|o| origin_allowed(o));
        let mut r = (
            if allowed {
                StatusCode::OK
            } else {
                StatusCode::BAD_REQUEST
            },
            if allowed {
                "OK"
            } else {
                "Disallowed CORS origin"
            },
        )
            .into_response();
        r.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            "DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"
                .parse()
                .unwrap(),
        );
        r.headers_mut()
            .insert(header::ACCESS_CONTROL_MAX_AGE, "600".parse().unwrap());
        r.headers_mut()
            .insert(header::VARY, "Origin".parse().unwrap());
        if let Some(value) = headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
            r.headers_mut()
                .insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value.clone());
        }
        Ok(r)
    } else if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS)
        && headers.contains_key(header::ORIGIN)
        && !origin.as_ref().is_some_and(|value| origin_allowed(value))
    {
        Err(http_error(403, "Disallowed request origin"))
    } else if ["/docs", "/redoc", "/docs/oauth2-redirect", "/openapi.json"].contains(&path.as_str())
        && (method == Method::GET || method == Method::HEAD)
    {
        Ok(match path.as_str() {
            "/openapi.json" => json_response(200, serde_json::from_str(OPENAPI).unwrap()),
            "/docs" => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], DOCS).into_response(),
            "/redoc" => {
                ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], REDOC).into_response()
            }
            _ => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], OAUTH).into_response(),
        })
    } else if api_method(&path)
        .is_some_and(|allowed| allowed.split(',').any(|m| m == method.as_str()))
    {
        if let Some(key) = one(&path, "/api/media/", "") {
            media_response(app.path.clone(), key.into(), request).await
        } else {
            api(&app, &path, &query, request).await
        }
    } else if method != Method::GET && method != Method::HEAD {
        Err(http_error(405, "Method Not Allowed"))
    } else if path == "/search" {
        Ok(redirect("/search/"))
    } else if let Some((bytes, mime)) = asset(&path) {
        Ok(assets::serve(
            bytes,
            mime,
            "/",
            method == Method::HEAD,
            &headers,
        ))
    } else {
        Err(http_error(404, "Not Found"))
    };
    let mut response = outcome.unwrap_or_else(error);
    if let Some(origin) = origin.filter(|o| origin_allowed(o)) {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.parse().unwrap());
        let vary = response
            .headers()
            .get(header::VARY)
            .and_then(|v| v.to_str().ok())
            .map(|s| format!("{s}, Origin"))
            .unwrap_or("Origin".into());
        response
            .headers_mut()
            .insert(header::VARY, vary.parse().unwrap());
    }
    if method == Method::HEAD {
        let (parts, _) = response.into_parts();
        response = Response::from_parts(parts, Body::empty());
    }
    response
}

// Pinned reference framework output, with no runtime Python dependency.
const OPENAPI: &str = r###"{"openapi":"3.1.0","info":{"title":"FastAPI","version":"0.1.0"},"paths":{"/api/token/validate":{"get":{"summary":"Validate Token","operationId":"validate_token_api_token_validate_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/guilds":{"get":{"summary":"Get Guilds","operationId":"get_guilds_api_guilds_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/guilds/{guild_id}/channels":{"get":{"summary":"Get Channels","operationId":"get_channels_api_guilds__guild_id__channels_get","parameters":[{"name":"guild_id","in":"path","required":true,"schema":{"type":"string","title":"Guild Id"}},{"name":"include_threads","in":"query","required":false,"schema":{"type":"boolean","default":false,"title":"Include Threads"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/dms":{"get":{"summary":"Get Dms","operationId":"get_dms_api_dms_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/scrape/start":{"post":{"summary":"Start Scrape","operationId":"start_scrape_api_scrape_start_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ScrapeRequest"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/scrape/active":{"get":{"summary":"Scrape Active","operationId":"scrape_active_api_scrape_active_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/scrape/{job_id}/pause":{"post":{"summary":"Pause Scrape","operationId":"pause_scrape_api_scrape__job_id__pause_post","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/scrape/{job_id}/resume":{"post":{"summary":"Resume Scrape","operationId":"resume_scrape_api_scrape__job_id__resume_post","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/scrape/{job_id}/stop":{"post":{"summary":"Stop Scrape","operationId":"stop_scrape_api_scrape__job_id__stop_post","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/scrape/progress/{job_id}":{"get":{"summary":"Scrape Progress","operationId":"scrape_progress_api_scrape_progress__job_id__get","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/profiles/{uid}":{"get":{"summary":"Profile","operationId":"profile_api_profiles__uid__get","parameters":[{"name":"uid","in":"path","required":true,"schema":{"type":"string","title":"Uid"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/profiles/{uid}/servers":{"get":{"summary":"Servers","operationId":"servers_api_profiles__uid__servers_get","parameters":[{"name":"uid","in":"path","required":true,"schema":{"type":"string","title":"Uid"}},{"name":"q","in":"query","required":false,"schema":{"type":"string","maxLength":100,"default":"","title":"Q"}},{"name":"offset","in":"query","required":false,"schema":{"type":"integer","minimum":0,"default":0,"title":"Offset"}},{"name":"limit","in":"query","required":false,"schema":{"type":"integer","maximum":100,"minimum":1,"default":30,"title":"Limit"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/profiles/{uid}/refresh":{"post":{"summary":"Fetch","operationId":"fetch_api_profiles__uid__refresh_post","parameters":[{"name":"uid","in":"path","required":true,"schema":{"type":"string","title":"Uid"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/profiles/{uid}/fetch":{"post":{"summary":"Fetch","operationId":"fetch_api_profiles__uid__fetch_post","parameters":[{"name":"uid","in":"path","required":true,"schema":{"type":"string","title":"Uid"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/profile-backfill":{"get":{"summary":"Progress","operationId":"progress_api_profile_backfill_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}},"post":{"summary":"Start","operationId":"start_api_profile_backfill_post","parameters":[{"name":"refresh_stale","in":"query","required":false,"schema":{"type":"boolean","default":false,"title":"Refresh Stale"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/profile-backfill/stop":{"post":{"summary":"Stop","operationId":"stop_api_profile_backfill_stop_post","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/media/{key}":{"get":{"summary":"Media","operationId":"media_api_media__key__get","parameters":[{"name":"key","in":"path","required":true,"schema":{"type":"string","title":"Key"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/invites/start":{"post":{"summary":"Start","operationId":"start_api_invites_start_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/InviteRequest"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/invites/status":{"get":{"summary":"Status","operationId":"status_api_invites_status_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/invites/{job_id}/stop":{"post":{"summary":"Stop","operationId":"stop_api_invites__job_id__stop_post","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/invites/{job_id}/resume":{"post":{"summary":"Resume","operationId":"resume_api_invites__job_id__resume_post","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/dm-clear/start":{"post":{"summary":"Start Dm Clear","operationId":"start_dm_clear_api_dm_clear_start_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/DmClearStartRequest"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/dm-clear/{job_id}/stop":{"post":{"summary":"Stop Dm Clear","operationId":"stop_dm_clear_api_dm_clear__job_id__stop_post","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/dm-clear/progress/{job_id}":{"get":{"summary":"Dm Clear Progress","operationId":"dm_clear_progress_api_dm_clear_progress__job_id__get","parameters":[{"name":"job_id","in":"path","required":true,"schema":{"type":"string","title":"Job Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/live/start":{"post":{"summary":"Live Start","operationId":"live_start_api_live_start_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/LiveStartRequest"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/live/stop":{"post":{"summary":"Live Stop","operationId":"live_stop_api_live_stop_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/LiveStopRequest"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/live/status":{"get":{"summary":"Live Status","operationId":"live_status_api_live_status_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/live/events":{"get":{"summary":"Live Events","operationId":"live_events_api_live_events_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}}},"/api/export/participants":{"get":{"summary":"Export Participants","operationId":"export_participants_api_export_participants_get","parameters":[{"name":"channel_id","in":"query","required":true,"schema":{"type":"string","title":"Channel Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/export/chatml":{"post":{"summary":"Export Chatml","operationId":"export_chatml_api_export_chatml_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/ChatMLExportRequest"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/search":{"get":{"summary":"Search","operationId":"search_api_search_get","parameters":[{"name":"q","in":"query","required":false,"schema":{"anyOf":[{"type":"string","maxLength":200},{"type":"null"}],"title":"Q"}},{"name":"guild_id","in":"query","required":false,"schema":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Guild Id"}},{"name":"channel_id","in":"query","required":false,"schema":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Channel Id"}},{"name":"author_id","in":"query","required":false,"schema":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Author Id"}},{"name":"date_from","in":"query","required":false,"schema":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Date From"}},{"name":"date_to","in":"query","required":false,"schema":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Date To"}},{"name":"page","in":"query","required":false,"schema":{"type":"integer","minimum":1,"default":1,"title":"Page"}},{"name":"limit","in":"query","required":false,"schema":{"type":"integer","maximum":100,"minimum":1,"default":50,"title":"Limit"}},{"name":"cursor","in":"query","required":false,"schema":{"type":"boolean","default":false,"title":"Cursor"}},{"name":"scan","in":"query","required":false,"schema":{"type":"boolean","default":false,"title":"Scan"}},{"name":"before","in":"query","required":false,"schema":{"anyOf":[{"type":"integer","maximum":9223372036854775807,"minimum":1},{"type":"null"}],"title":"Before"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/images/{message_id}/{image_index}":{"get":{"summary":"Open Image","description":"Redirect to a current attachment URL, refreshing expired signatures.","operationId":"open_image_api_images__message_id___image_index__get","parameters":[{"name":"message_id","in":"path","required":true,"schema":{"type":"integer","title":"Message Id"}},{"name":"image_index","in":"path","required":true,"schema":{"type":"integer","title":"Image Index"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/stats":{"get":{"summary":"Get Stats","operationId":"get_stats_api_stats_get","parameters":[{"name":"days","in":"query","required":false,"schema":{"type":"integer","maximum":3650,"minimum":0,"default":90,"title":"Days"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/stats/contributors":{"get":{"summary":"Contributors","description":"Bounded, deterministic leaderboard pages over the cached author counts.","operationId":"contributors_api_stats_contributors_get","parameters":[{"name":"offset","in":"query","required":false,"schema":{"type":"integer","minimum":0,"default":0,"title":"Offset"}},{"name":"limit","in":"query","required":false,"schema":{"type":"integer","maximum":100,"minimum":1,"default":50,"title":"Limit"}},{"name":"q","in":"query","required":false,"schema":{"type":"string","maxLength":100,"default":"","title":"Q"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/stats/servers":{"get":{"summary":"Ranked Servers","description":"Page through all archived servers using cached message counts.","operationId":"ranked_servers_api_stats_servers_get","parameters":[{"name":"offset","in":"query","required":false,"schema":{"type":"integer","minimum":0,"default":0,"title":"Offset"}},{"name":"limit","in":"query","required":false,"schema":{"type":"integer","maximum":100,"minimum":1,"default":50,"title":"Limit"}},{"name":"q","in":"query","required":false,"schema":{"type":"string","maxLength":100,"default":"","title":"Q"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/search/filters":{"get":{"summary":"Search Filters","operationId":"search_filters_api_search_filters_get","parameters":[{"name":"include_users","in":"query","required":false,"schema":{"type":"boolean","default":true,"title":"Include Users"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/search/authors":{"get":{"summary":"Search Authors","description":"Return a small prefix match set for the author text filter.","operationId":"search_authors_api_search_authors_get","parameters":[{"name":"q","in":"query","required":false,"schema":{"type":"string","default":"","title":"Q"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/settings":{"get":{"summary":"Get Settings","operationId":"get_settings_api_settings_get","responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}}}},"post":{"summary":"Update Settings","operationId":"update_settings_api_settings_post","requestBody":{"content":{"application/json":{"schema":{"$ref":"#/components/schemas/SettingsIn"}}},"required":true},"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/settings/tokens":{"delete":{"summary":"Clear Tokens","operationId":"clear_tokens_api_settings_tokens_delete","parameters":[{"name":"confirm","in":"query","required":false,"schema":{"type":"boolean","default":false,"title":"Confirm"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/messages":{"delete":{"summary":"Clear Messages","operationId":"clear_messages_api_messages_delete","parameters":[{"name":"confirm","in":"query","required":false,"schema":{"type":"boolean","default":false,"title":"Confirm"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/messages/{message_id}/archive":{"get":{"summary":"Message Archive","operationId":"message_archive_api_messages__message_id__archive_get","parameters":[{"name":"message_id","in":"path","required":true,"schema":{"type":"integer","title":"Message Id"}},{"name":"after_revision","in":"query","required":false,"schema":{"type":"integer","minimum":0,"default":0,"title":"After Revision"}},{"name":"limit","in":"query","required":false,"schema":{"type":"integer","maximum":100,"minimum":1,"default":100,"title":"Limit"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}},"/api/messages/{message_id}/refresh":{"post":{"summary":"Refresh Message","operationId":"refresh_message_api_messages__message_id__refresh_post","parameters":[{"name":"message_id","in":"path","required":true,"schema":{"type":"integer","title":"Message Id"}}],"responses":{"200":{"description":"Successful Response","content":{"application/json":{"schema":{}}}},"422":{"description":"Validation Error","content":{"application/json":{"schema":{"$ref":"#/components/schemas/HTTPValidationError"}}}}}}}},"components":{"schemas":{"ChatMLExportRequest":{"properties":{"channel_id":{"type":"string","title":"Channel Id"},"target_id":{"type":"string","title":"Target Id"},"other_name":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Other Name"},"system_prompt":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"System Prompt"}},"type":"object","required":["channel_id","target_id"],"title":"ChatMLExportRequest"},"DmClearStartRequest":{"properties":{"channel_id":{"type":"string","pattern":"^[0-9]{1,19}$","title":"Channel Id"}},"type":"object","required":["channel_id"],"title":"DmClearStartRequest"},"HTTPValidationError":{"properties":{"detail":{"items":{"$ref":"#/components/schemas/ValidationError"},"type":"array","title":"Detail"}},"type":"object","title":"HTTPValidationError"},"InviteRequest":{"properties":{"invites":{"type":"string","maxLength":50000,"minLength":1,"title":"Invites"}},"type":"object","required":["invites"],"title":"InviteRequest"},"LiveStartRequest":{"properties":{"channels":{"items":{},"type":"array","title":"Channels"}},"type":"object","required":["channels"],"title":"LiveStartRequest"},"LiveStopRequest":{"properties":{"channel_ids":{"items":{},"type":"array","title":"Channel Ids","default":[]}},"type":"object","title":"LiveStopRequest"},"ScrapeRequest":{"properties":{"channels":{"items":{},"type":"array","title":"Channels"},"limit":{"type":"integer","minimum":0.0,"title":"Limit","default":0},"harvest_profiles":{"type":"boolean","title":"Harvest Profiles","default":false}},"type":"object","required":["channels"],"title":"ScrapeRequest"},"SettingsIn":{"properties":{"token":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Token"},"token_label":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Token Label"},"active_token_id":{"anyOf":[{"type":"string"},{"type":"null"}],"title":"Active Token Id"}},"type":"object","title":"SettingsIn"},"ValidationError":{"properties":{"loc":{"items":{"anyOf":[{"type":"string"},{"type":"integer"}]},"type":"array","title":"Location"},"msg":{"type":"string","title":"Message"},"type":{"type":"string","title":"Error Type"},"input":{"title":"Input"},"ctx":{"type":"object","title":"Context"}},"type":"object","required":["loc","msg","type"],"title":"ValidationError"}}}}"###;
const DOCS: &str = r###"
    <!DOCTYPE html>
    <html>
    <head>
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <link type="text/css" rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css">
    <link rel="shortcut icon" href="https://fastapi.tiangolo.com/img/favicon.png">
    <title>FastAPI - Swagger UI</title>
    </head>
    <body>
    <div id="swagger-ui">
    </div>
    <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
    <!-- `SwaggerUIBundle` is now available on the page -->
    <script>
    const ui = SwaggerUIBundle({
        url: '/openapi.json',
    "dom_id": "#swagger-ui",
"layout": "BaseLayout",
"deepLinking": true,
"showExtensions": true,
"showCommonExtensions": true,
oauth2RedirectUrl: window.location.origin + '/docs/oauth2-redirect',
    presets: [
        SwaggerUIBundle.presets.apis,
        SwaggerUIBundle.SwaggerUIStandalonePreset
        ],
    })
    </script>
    </body>
    </html>
    "###;
const REDOC: &str = concat!(
    r###"
    <!DOCTYPE html>
    <html>
    <head>
    <title>FastAPI - ReDoc</title>
    <!-- needed for adaptive design -->
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1">
"###,
    "    \n",
    r###"    <link href="https://fonts.googleapis.com/css?family=Montserrat:300,400,700|Roboto:300,400,700" rel="stylesheet">
"###,
    "    \n",
    r###"    <link rel="shortcut icon" href="https://fastapi.tiangolo.com/img/favicon.png">
    <!--
    ReDoc doesn't change outer page styles
    -->
    <style>
      body {
        margin: 0;
        padding: 0;
      }
    </style>
    </head>
    <body>
    <noscript>
        ReDoc requires Javascript to function. Please enable it to browse the documentation.
    </noscript>
    <redoc spec-url="/openapi.json"></redoc>
    <script src="https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js"> </script>
    </body>
    </html>
    "###
);
const OAUTH: &str = r###"
    <!doctype html>
    <html lang="en-US">
    <head>
        <title>Swagger UI: OAuth2 Redirect</title>
    </head>
    <body>
    <script>
        'use strict';
        function run () {
            var oauth2 = window.opener.swaggerUIRedirectOauth2;
            var sentState = oauth2.state;
            var redirectUrl = oauth2.redirectUrl;
            var isValid, qp, arr;

            if (/code|token|error/.test(window.location.hash)) {
                qp = window.location.hash.substring(1).replace('?', '&');
            } else {
                qp = location.search.substring(1);
            }

            arr = qp.split("&");
            arr.forEach(function (v,i,_arr) { _arr[i] = '"' + v.replace('=', '":"') + '"';});
            qp = qp ? JSON.parse('{' + arr.join() + '}',
                    function (key, value) {
                        return key === "" ? value : decodeURIComponent(value);
                    }
            ) : {};

            isValid = qp.state === sentState;

            if ((
              oauth2.auth.schema.get("flow") === "accessCode" ||
              oauth2.auth.schema.get("flow") === "authorizationCode" ||
              oauth2.auth.schema.get("flow") === "authorization_code"
            ) && !oauth2.auth.code) {
                if (!isValid) {
                    oauth2.errCb({
                        authId: oauth2.auth.name,
                        source: "auth",
                        level: "warning",
                        message: "Authorization may be unsafe, passed state was changed in server. The passed state wasn't returned from auth server."
                    });
                }

                if (qp.code) {
                    delete oauth2.state;
                    oauth2.auth.code = qp.code;
                    oauth2.callback({auth: oauth2.auth, redirectUrl: redirectUrl});
                } else {
                    let oauthErrorMsg;
                    if (qp.error) {
                        oauthErrorMsg = "["+qp.error+"]: " +
                            (qp.error_description ? qp.error_description+ ". " : "no accessCode received from the server. ") +
                            (qp.error_uri ? "More info: "+qp.error_uri : "");
                    }

                    oauth2.errCb({
                        authId: oauth2.auth.name,
                        source: "auth",
                        level: "error",
                        message: oauthErrorMsg || "[Authorization failed]: no accessCode received from the server."
                    });
                }
            } else {
                oauth2.callback({auth: oauth2.auth, token: qp, isValid: isValid, redirectUrl: redirectUrl});
            }
            window.close();
        }

        if (document.readyState !== 'loading') {
            run();
        } else {
            document.addEventListener('DOMContentLoaded', function () {
                run();
            });
        }
    </script>
    </body>
    </html>
        "###;
