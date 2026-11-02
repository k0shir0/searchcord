//! One durable invite queue. Interrupted joins require a check in Discord.
use crate::{
    Result,
    discord::{Discord, Request},
    discovery::http_error,
    pacing::{Clock, Control, Options},
};
use reqwest::Method;
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, broadcast},
    task::JoinHandle,
};

const INTERVAL: f64 = 30.0;
const CAPACITY: usize = 1000;

pub(crate) fn job_id(hyphenated: bool) -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("Could not create job identifier: {e}"))?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(if hyphenated {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        )
    } else {
        hex
    })
}
fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 100
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn link_code(item: &str) -> Option<&str> {
    let lower = item.to_ascii_lowercase();
    let offset = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        0
    };
    let offset = offset + usize::from(lower[offset..].starts_with("www.")) * 4;
    let prefixes = [
        "discord.gg/",
        "discord.com/invite/",
        "discordapp.com/invite/",
        "canary.discord.com/invite/",
        "ptb.discord.com/invite/",
        "canary.discordapp.com/invite/",
        "ptb.discordapp.com/invite/",
    ];
    let prefix = prefixes
        .into_iter()
        .find(|prefix| lower[offset..].starts_with(prefix))?;
    let tail = &item[offset + prefix.len()..];
    let end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
    let code = &tail[..end];
    let suffix = tail[end..].strip_prefix('/').unwrap_or(&tail[end..]);
    (valid_code(code) && (suffix.is_empty() || suffix.starts_with(['?', '#']))).then_some(code)
}
/// Python-compatible splitting, wrapper stripping and case-sensitive deduplication.
pub fn parse_invites(text: &str) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut codes = Vec::new();
    // Split retains empty segments in Python; those still count toward Item N.
    let mut tokens = Vec::new();
    let mut start = 0;
    let trimmed = searchcord_archive::search::strip(text);
    let mut in_separator = false;
    for (i, c) in trimmed.char_indices() {
        if c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}' | ',' | ';') {
            if !in_separator {
                tokens.push(&trimmed[start..i]);
            }
            in_separator = true;
            start = i + c.len_utf8();
        } else {
            in_separator = false;
        }
    }
    tokens.push(&trimmed[start..]);
    for (index, item) in tokens.into_iter().enumerate() {
        let item = item
            .trim_matches(['<', '>', '"', '\'', '`', '[', ']', '(', ')'])
            .trim_end_matches('.');
        if item.is_empty() {
            continue;
        }
        let code = link_code(item).unwrap_or(item);
        if !valid_code(code) {
            return Err(http_error(
                422,
                format!("Item {} is not a Discord invite link or code.", index + 1),
            ));
        }
        if seen.insert(code.to_owned()) {
            codes.push(code.to_owned());
        }
    }
    if codes.is_empty() {
        return Err(http_error(
            422,
            "Enter at least one Discord invite link or code.",
        ));
    }
    if codes.len() > 200 {
        return Err(http_error(422, "Use at most 200 unique invites per queue."));
    }
    Ok(codes)
}

#[derive(Clone)]
struct Active {
    control: Control,
    cancelled: Arc<AtomicBool>,
    preserve: Arc<AtomicBool>,
}
impl Active {
    fn new() -> Self {
        Self {
            control: Control::default(),
            cancelled: Arc::new(AtomicBool::new(false)),
            preserve: Arc::new(AtomicBool::new(false)),
        }
    }
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    fn stop(&self, preserve: bool) {
        self.preserve.store(preserve, Ordering::SeqCst);
        self.cancelled.store(true, Ordering::SeqCst);
        self.control.stop();
    }
    fn options(&self) -> Options {
        Options {
            control: self.control.clone(),
            retry_rate_limits: false,
            ..Options::default()
        }
    }
}
struct State {
    job: Option<Value>,
    next_at: f64,
    active: Option<Active>,
    error: Option<String>,
    shutdown: bool,
}
struct Inner<C: Clock> {
    path: PathBuf,
    client: Discord<C>,
    clock: Arc<C>,
    state: Mutex<State>,
    operation: Mutex<()>,
    task: Mutex<Option<JoinHandle<Result<()>>>>,
    events: broadcast::Sender<Value>,
}
pub struct Invites<C: Clock> {
    inner: Arc<Inner<C>>,
}
impl<C: Clock> Clone for Invites<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<C: Clock> Invites<C> {
    /// The archive and pacing gate must already be initialized. Recovery never starts HTTP work.
    pub async fn open(path: PathBuf, client: Discord<C>, clock: Arc<C>) -> Result<Self> {
        let load_path = path.clone();
        let saved: Option<Value> = tokio::task::spawn_blocking(move || -> Result<Option<Value>> {
            let db = connection(&load_path)?;
            let mut statement =
                db.prepare("SELECT state_json FROM collection_jobs WHERE kind='invite'")?;
            let mut last = None;
            for row in statement.query_map([], |row| row.get::<_, String>(0))? {
                last = Some(serde_json::from_str(&row?)?);
            }
            Ok(last)
        })
        .await??;
        let mut next_at = 0.0;
        let mut job = None;
        let mut recovered = false;
        if let Some(saved) = saved {
            next_at = clock.now()
                + (saved["next_attempt_at"]
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .unwrap_or(0.0)
                    - clock.wall())
                .max(0.0);
            let mut restored = saved
                .get("job")
                .filter(|v| v.is_object())
                .cloned()
                .ok_or("Invalid saved invite queue")?;
            if restored["running"] == true {
                for item in restored["items"]
                    .as_array_mut()
                    .ok_or("Invalid saved invite items")?
                {
                    if item["status"] == "joining" {
                        item["status"] = json!("uncertain");
                        item["message"] = json!(
                            "Request interrupted. Check Discord; this invite will not be replayed."
                        );
                    }
                }
                restored["paused"] = json!(true);
                restored["status"] = json!("paused");
                restored["message"] =
                    json!("Recovered after restart. Resume pending invites when ready.");
                recovered = true;
            }
            job = Some(restored);
        }
        let service = Self {
            inner: Arc::new(Inner {
                path,
                client,
                clock,
                state: Mutex::new(State {
                    job,
                    next_at,
                    active: None,
                    error: None,
                    shutdown: false,
                }),
                operation: Mutex::new(()),
                task: Mutex::new(None),
                events: broadcast::channel(CAPACITY).0,
            }),
        };
        if recovered {
            service.persist().await?;
        }
        Ok(service)
    }
    fn snapshot(&self, state: &State) -> Value {
        let mut job = state.job.clone();
        if let Some(job) = job.as_mut() {
            job["wait_seconds"] = json!(if job["running"] == true {
                (state.next_at - self.inner.clock.now()).max(0.0).ceil() as u64
            } else {
                0
            });
        }
        json!({"job": job})
    }
    fn publish(&self, state: &State) {
        let _ = self.inner.events.send(self.snapshot(state));
    }
    pub async fn status(&self) -> Value {
        let state = self.inner.state.lock().await;
        self.snapshot(&state)
    }
    /// Subscribe under the state lock, returning the initial snapshot without an event gap.
    /// A lagging receiver reports `Lagged`; obtain a fresh snapshot to resynchronize.
    pub async fn subscribe(&self) -> (Value, broadcast::Receiver<Value>) {
        let state = self.inner.state.lock().await;
        (self.snapshot(&state), self.inner.events.subscribe())
    }
    async fn persist(&self) -> Result<()> {
        let saved = {
            let state = self.inner.state.lock().await;
            state.job.as_ref().map(|job| json!({"job":job,"next_attempt_at":self.inner.clock.wall() + (state.next_at - self.inner.clock.now()).max(0.0)}))
        };
        let Some(saved) = saved else {
            return Ok(());
        };
        let path = self.inner.path.clone();
        let wall = self.inner.clock.wall();
        tokio::task::spawn_blocking(move || -> Result<()> {
            connection(&path)?.execute("INSERT INTO collection_jobs(id,kind,state_json,updated_at) VALUES ('invite-queue','invite',?,?) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,state_json=excluded.state_json,updated_at=excluded.updated_at", params![serde_json::to_string(&saved)?, wall])?;
            Ok(())
        }).await?
    }
    async fn join(&self) -> Result<()> {
        let mut task = self.inner.task.lock().await;
        if let Some(handle) = task.as_mut() {
            let result = handle.await;
            task.take();
            result??;
        }
        Ok(())
    }
    /// A cancelled caller cannot abandon an accepted start/checkpoint.
    pub async fn start(
        &self,
        text: &str,
        credential_id: Option<String>,
        token: String,
    ) -> Result<Value> {
        let this = self.clone();
        let text = text.to_owned();
        tokio::spawn(async move {
            let _operation = this.inner.operation.lock().await;
            {
                let state = this.inner.state.lock().await;
                if state.shutdown { return Err(http_error(503, "Invite queues are shutting down.")); }
                if state.job.as_ref().is_some_and(|j| j["running"] == true) { return Err(http_error(409, "An invite queue is already running.")); }
            }
            if text.is_empty() || text.chars().count() > 50000 { return Err(http_error(422, "Invites must contain between 1 and 50000 characters.")); }
            let codes = parse_invites(&text)?;
            this.join().await?;
            let active = Active::new();
            {
                let mut state = this.inner.state.lock().await;
                state.job = Some(json!({"id":job_id(false)?,"running":true,"status":"running","paused":false,"credential_id":credential_id,"message":"Starting invite queue.","items":codes.into_iter().map(|code|json!({"code":code,"status":"pending","message":"Waiting."})).collect::<Vec<_>>()}));
                state.active = Some(active.clone()); state.error = None;
                this.publish(&state);
            }
            if let Err(error) = this.persist().await { this.persistence_failed(error.as_ref()).await; return Err(error); }
            let runner = this.clone();
            *this.inner.task.lock().await = Some(tokio::spawn(async move { runner.run(token, active).await }));
            Ok(this.status().await)
        }).await?
    }
    pub async fn resume(
        &self,
        id: &str,
        credential_id: Option<String>,
        token: String,
    ) -> Result<Value> {
        let this = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let _operation = this.inner.operation.lock().await;
            {
                let state = this.inner.state.lock().await;
                if state.shutdown {
                    return Err(http_error(503, "Invite queues are shutting down."));
                }
                let job = matching(&state, &id)?;
                if job["paused"] != true {
                    return Err(http_error(409, "Invite queue is not paused."));
                }
                if job["credential_id"] != json!(credential_id) {
                    return Err(http_error(
                        409,
                        "Select the account used to start this queue before resuming.",
                    ));
                }
            }
            this.join().await?;
            let active = Active::new();
            {
                let mut state = this.inner.state.lock().await;
                let job = state.job.as_mut().unwrap();
                job["paused"] = json!(false);
                job["running"] = json!(true);
                job["status"] = json!("running");
                job["message"] = json!("Resuming pending invites.");
                state.active = Some(active.clone());
                this.publish(&state);
            }
            if let Err(error) = this.persist().await {
                this.persistence_failed(error.as_ref()).await;
                return Err(error);
            }
            let runner = this.clone();
            *this.inner.task.lock().await =
                Some(tokio::spawn(async move { runner.run(token, active).await }));
            Ok(this.status().await)
        })
        .await?
    }
    pub async fn stop(&self, id: &str) -> Result<Value> {
        let this = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let _operation = this.inner.operation.lock().await;
            {
                let state = this.inner.state.lock().await;
                matching(&state, &id)?;
                if let Some(active) = &state.active {
                    active.stop(false);
                }
            }
            this.join().await?;
            {
                let mut state = this.inner.state.lock().await;
                let job = state.job.as_mut().unwrap();
                job["running"] = json!(false);
                job["paused"] = json!(false);
                job["status"] = json!("stopped");
                job["message"] = json!("Invite queue stopped. Completed joins are kept.");
                this.publish(&state);
            }
            this.persist().await?;
            Ok(this.status().await)
        })
        .await?
    }
    /// Shutdown joins network/checkpoint tasks. Preserved queues recover paused.
    pub async fn shutdown(&self, preserve: bool) -> Result<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let _operation = this.inner.operation.lock().await;
            {
                let mut state = this.inner.state.lock().await;
                state.shutdown = true;
                if let Some(active) = &state.active {
                    active.stop(preserve);
                }
                // A recovered paused queue has no worker to perform this update.
                if !preserve
                    && state.job.as_ref().is_some_and(|j| j["running"] == true)
                    && state.active.is_none()
                {
                    let job = state.job.as_mut().unwrap();
                    job["running"] = json!(false);
                    job["paused"] = json!(false);
                    job["status"] = json!("stopped");
                    job["message"] = json!("Invite queue stopped. Completed joins are kept.");
                    this.publish(&state);
                }
            }
            this.join().await?;
            this.persist().await
        })
        .await?
    }
    /// Join the worker and its accepted SQLite writes (call stop first for a running queue).
    pub async fn barrier(&self) -> Result<()> {
        let _operation = self.inner.operation.lock().await;
        self.join().await?;
        match &self.inner.state.lock().await.error {
            Some(e) => Err(e.clone().into()),
            None => Ok(()),
        }
    }
    async fn persistence_failed(&self, error: &(dyn std::error::Error + Send + Sync)) {
        let mut state = self.inner.state.lock().await;
        state.error = Some(error.to_string());
        if let Some(job) = state.job.as_mut() {
            job["running"] = json!(false);
            job["status"] = json!("failed");
            job["message"] = json!("Invite queue stopped after an unexpected response.");
        }
        self.publish(&state);
    }
    async fn run(&self, token: String, active: Active) -> Result<()> {
        let mut current = None;
        let result = self.run_items(&token, &active, &mut current).await;
        {
            let mut state = self.inner.state.lock().await;
            let job = state.job.as_mut().unwrap();
            if active.stopped() {
                let preserve = active.preserve.load(Ordering::SeqCst);
                if let Some(i) = current.filter(|&i| job["items"][i]["status"] == "joining") {
                    job["items"][i]["status"] =
                        json!(if preserve { "uncertain" } else { "stopped" });
                    job["items"][i]["message"] = json!(if preserve {
                        "Request interrupted. Check Discord; this invite will not be replayed."
                    } else {
                        "Stopped during request; check Discord before retrying."
                    });
                }
                job["paused"] = json!(preserve);
                job["status"] = json!(if preserve { "paused" } else { "stopped" });
                job["message"] = json!(if preserve {
                    "Recovered queue paused. Resume pending invites when ready."
                } else {
                    "Invite queue stopped. Completed joins are kept."
                });
            } else if let Err(error) = &result {
                let connection = error.downcast_ref::<reqwest::Error>().is_some();
                if let Some(i) = current {
                    job["items"][i]["status"] = json!("failed");
                    job["items"][i]["message"] = json!(if connection {
                        "Connection lost; check Discord before retrying."
                    } else {
                        "Could not confirm join; check Discord before retrying."
                    });
                }
                job["status"] = json!("failed");
                job["message"] = json!(if connection {
                    "Invite queue stopped after a connection failure."
                } else {
                    "Invite queue stopped after an unexpected response."
                });
            }
            job["running"] = json!(job["paused"] == true);
            self.publish(&state);
        }
        if let Err(error) = self.persist().await {
            self.persistence_failed(error.as_ref()).await;
            return Err(error);
        }
        Ok(())
    }
    async fn run_items(
        &self,
        token: &str,
        active: &Active,
        current: &mut Option<usize>,
    ) -> Result<()> {
        let count = self.inner.state.lock().await.job.as_ref().unwrap()["items"]
            .as_array()
            .unwrap()
            .len();
        for i in 0..count {
            let (code, mut attempts) = {
                let state = self.inner.state.lock().await;
                let item = &state.job.as_ref().unwrap()["items"][i];
                if !["pending", "waiting"].contains(&item["status"].as_str().unwrap_or("")) {
                    continue;
                }
                (
                    item["code"]
                        .as_str()
                        .ok_or("Invalid saved invite code")?
                        .to_owned(),
                    item["attempts"].as_u64().unwrap_or(0),
                )
            };
            *current = Some(i);
            loop {
                loop {
                    if active.stopped() {
                        return Ok(());
                    }
                    let wait = {
                        let mut state = self.inner.state.lock().await;
                        let wait = state.next_at - self.inner.clock.now();
                        if wait > 0.0 {
                            state.job.as_mut().unwrap()["message"] =
                                json!("Waiting before the next join.");
                            self.publish(&state);
                        }
                        wait
                    };
                    if wait <= 0.0 {
                        break;
                    }
                    self.inner.clock.sleep(wait.min(0.1)).await;
                }
                if active.stopped() {
                    return Ok(());
                }
                {
                    let mut state = self.inner.state.lock().await;
                    state.next_at = self.inner.clock.now() + INTERVAL;
                    let job = state.job.as_mut().unwrap();
                    job["items"][i]["status"] = json!("joining");
                    job["message"] = json!("Checking invite and joining server.");
                    self.publish(&state);
                }
                self.persist().await?;
                let invite = self
                    .inner
                    .client
                    .request(
                        Request::new(Method::GET, format!("/invites/{code}"), token),
                        active.options(),
                    )
                    .await?;
                let response = if invite.status == 200 {
                    let data = invite.json().unwrap_or(Value::Null);
                    if !data["guild"].is_object() || !truthy(&data["guild"]["id"]) {
                        self.item(i, "failed", "This invite does not identify a server.")
                            .await;
                        break;
                    }
                    {
                        let mut state = self.inner.state.lock().await;
                        state.next_at = self.inner.clock.now() + INTERVAL;
                        let item = &mut state.job.as_mut().unwrap()["items"][i];
                        item["guild_id"] = data["guild"]["id"].clone();
                        item["guild_name"] = data["guild"]
                            .get("name")
                            .cloned()
                            .unwrap_or(json!("Server"));
                        self.publish(&state);
                    }
                    self.persist().await?;
                    let mut request = Request::new(Method::POST, format!("/invites/{code}"), token);
                    request.json = Some(json!({}));
                    let response = self.inner.client.request(request, active.options()).await?;
                    self.inner.state.lock().await.next_at = self.inner.clock.now() + INTERVAL;
                    response
                } else {
                    invite
                };
                let payload = response
                    .json()
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or(json!({}));
                let discord_code = payload["code"].as_i64();
                if let Some(code) = discord_code {
                    self.inner.state.lock().await.job.as_mut().unwrap()["items"][i]["discord_code"] =
                        json!(code);
                }
                let account_error = match discord_code {
                    Some(20001) => Some("Invite joining requires a user account, not a bot token."),
                    Some(30001) => Some(
                        "This account has reached its server limit. Free a server slot in Discord, then retry.",
                    ),
                    Some(40001 | 50014) => Some("Discord rejected the selected token."),
                    Some(40002) => Some("Complete account verification in Discord, then retry."),
                    Some(340015) => Some(
                        "Discord has restricted this account from joining new servers. Check Account Standing in Discord before retrying.",
                    ),
                    _ => None,
                };
                if payload.get("captcha_key").is_some()
                    || response.status == 401
                    || account_error.is_some()
                {
                    let message = if payload.get("captcha_key").is_some() {
                        "Complete Discord verification, then retry."
                    } else {
                        account_error.unwrap_or("Discord rejected the selected token.")
                    };
                    self.item(i, "blocked", message).await;
                    let mut state = self.inner.state.lock().await;
                    let job = state.job.as_mut().unwrap();
                    job["status"] = json!("blocked");
                    job["message"] = json!(message);
                    self.publish(&state);
                    return Ok(());
                }
                if response.status == 429 {
                    attempts += 1;
                    let mut state = self.inner.state.lock().await;
                    state.job.as_mut().unwrap()["items"][i]["attempts"] = json!(attempts);
                    if attempts <= 5 {
                        let wait = payload.get("retry_after").map(number).unwrap_or_else(|| {
                            response
                                .headers
                                .get("retry-after")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.trim().parse::<f64>().ok())
                        });
                        if let Some(wait) = wait.filter(|n| n.is_finite()) {
                            state.next_at =
                                state.next_at.max(self.inner.clock.now() + wait.max(0.0));
                        }
                        let item = &mut state.job.as_mut().unwrap()["items"][i];
                        item["status"] = json!("waiting");
                        item["message"] = json!("Discord rate limit; retrying this invite.");
                        self.publish(&state);
                        drop(state);
                        self.persist().await?;
                        continue;
                    }
                }
                if [200, 201, 204].contains(&response.status) {
                    self.item(
                        i,
                        "joined",
                        if truthy(&payload["pending"]) {
                            "Joined; complete server screening in Discord."
                        } else {
                            "Joined server."
                        },
                    )
                    .await;
                } else {
                    let message = if discord_code == Some(40007) {
                        "This account is banned from the invited server.".into()
                    } else {
                        match response.status {
                            400 => "Discord could not accept this invite.".into(),
                            403 => {
                                "Discord denied this join. Check access or server limits.".into()
                            }
                            404 => "Invite is invalid or expired.".into(),
                            429 => "Invite is still rate limited. Retry it later.".into(),
                            status => format!(
                                "Could not confirm join (HTTP {status}); check Discord before retrying."
                            ),
                        }
                    };
                    self.item(i, "failed", &message).await;
                }
                break;
            }
            self.persist().await?;
            *current = None;
        }
        if !active.stopped() {
            let mut state = self.inner.state.lock().await;
            let job = state.job.as_mut().unwrap();
            let failures = job["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|i| i["status"] != "joined")
                .count();
            job["status"] = json!("complete");
            job["message"] = json!(if failures == 0 {
                "Invite queue finished.".into()
            } else {
                format!("Invite queue finished; {failures} invite(s) need attention.")
            });
            self.publish(&state);
        }
        Ok(())
    }
    async fn item(&self, i: usize, status: &str, message: &str) {
        let mut state = self.inner.state.lock().await;
        let item = &mut state.job.as_mut().unwrap()["items"][i];
        item["status"] = json!(status);
        item["message"] = json!(message);
        self.publish(&state);
    }
}
fn matching<'a>(state: &'a State, id: &str) -> Result<&'a Value> {
    state
        .job
        .as_ref()
        .filter(|j| j["id"] == id)
        .ok_or_else(|| http_error(404, "Invite queue not found."))
}
fn connection(path: &PathBuf) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(30))?;
    Ok(db)
}
fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(v) => Some(u8::from(*v) as f64),
        Value::Number(v) => v.as_f64(),
        Value::String(v) => v.trim().parse().ok(),
        _ => None,
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
