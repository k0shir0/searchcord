//! Explicit profile collection. Clones share exclusion through the committed save.
use crate::{
    Result,
    discord::{Discord, Request},
    pacing::{Clock, Control, Options, Response, SystemClock, WaitEvent},
    storage::canonical_json,
};
use reqwest::Method;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::Value as SqlValue};
use serde::Serialize;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

const BATCH_SIZE: i64 = 100;
pub const STALE_AFTER_SECONDS: i64 = 24 * 60 * 60;
type Error = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub struct ProfileError {
    pub status: u16,
    pub detail: String,
    pub retry_after: f64,
}
impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for ProfileError {}
fn error(status: u16, detail: impl Into<String>, retry_after: f64) -> Error {
    Box::new(ProfileError {
        status,
        detail: detail.into(),
        retry_after,
    })
}
fn stopped() -> Error {
    std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "Profile collection stopped",
    )
    .into()
}
fn validate_id(uid: &str) -> Result<()> {
    if uid.is_empty()
        || uid.len() > 19
        || !uid.bytes().all(|b| b.is_ascii_digit())
        || uid.parse::<i64>().ok().is_none_or(|n| n <= 0)
    {
        return Err(error(400, "Enter a valid Discord user ID.", 0.0));
    }
    Ok(())
}
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(v) => Some(u8::from(*v) as f64),
        Value::Number(v) => v.as_f64(),
        Value::String(v) => v.trim().parse().ok(),
        _ => None,
    }
}
fn retry_seconds(response: &Response) -> f64 {
    let payload = response.json().ok();
    let supplied = payload
        .as_ref()
        .and_then(|v| v.get("retry_after"))
        .filter(|v| !v.is_null());
    supplied
        .map_or_else(
            || {
                response
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse().ok())
            },
            numeric,
        )
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
        .max(0.0)
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn scalar(value: &Value) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(v) => SqlValue::Integer(i64::from(*v)),
        Value::Number(v) if v.is_i64() => SqlValue::Integer(v.as_i64().unwrap()),
        Value::Number(v) if v.is_f64() => SqlValue::Real(v.as_f64().unwrap()),
        Value::String(v) => SqlValue::Text(v.clone()),
        _ => {
            return Err(error(
                502,
                "Discord returned invalid profile fields. Saved data was kept.",
                0.0,
            ));
        }
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct RetryInfo {
    pub user_id: String,
    pub reason: String,
    pub attempt: u32,
    pub wait_seconds: f64,
}
pub type OnRetry = Arc<dyn Fn(RetryInfo) + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
pub struct BackfillState {
    pub running: bool,
    pub saved: u64,
    pub failed: u64,
    pub cancelled: bool,
    pub status: String,
    pub current_user: Option<String>,
    pub retries: u64,
    pub wait_seconds: f64,
    pub last_error: Option<String>,
}
impl Default for BackfillState {
    fn default() -> Self {
        Self {
            running: false,
            saved: 0,
            failed: 0,
            cancelled: false,
            status: "idle".into(),
            current_user: None,
            retries: 0,
            wait_seconds: 0.0,
            last_error: None,
        }
    }
}
#[derive(Default)]
struct Job {
    control: Option<Control>,
    task: Option<JoinHandle<()>>,
}

pub struct Profiles<C: Clock = SystemClock> {
    path: PathBuf,
    discord: Discord<C>,
    exclusion: Arc<AsyncMutex<()>>,
    state: Arc<Mutex<BackfillState>>,
    job: Arc<AsyncMutex<Job>>,
}
impl<C: Clock> Clone for Profiles<C> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            discord: self.discord.clone(),
            exclusion: self.exclusion.clone(),
            state: self.state.clone(),
            job: self.job.clone(),
        }
    }
}
impl<C: Clock> Profiles<C> {
    /// Construct once per archive and clone for manual, scrape and backfill callers.
    /// Storage must already be initialized. Construction never starts collection.
    pub fn new(path: PathBuf, discord: Discord<C>) -> Self {
        Self {
            path,
            discord,
            exclusion: Arc::new(AsyncMutex::new(())),
            state: Arc::new(Mutex::new(BackfillState::default())),
            job: Arc::new(AsyncMutex::new(Job::default())),
        }
    }
    async fn database<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            db.busy_timeout(Duration::from_millis(250))?;
            action(&mut db)
        })
        .await?
    }
    async fn known(&self, uid: &str) -> Result<()> {
        let uid = uid.to_owned();
        self.database(move |db| {
            // Matches read_profile: a previously saved basic profile is known too.
            let known = db.query_row("SELECT 1 FROM authors WHERE id=? UNION ALL SELECT 1 FROM profiles WHERE user_id=? LIMIT 1", params![uid, uid], |_| Ok(())).optional()?.is_some();
            if !known { return Err(error(404, "This user is not in the archive.", 0.0)); }
            Ok(())
        }).await
    }
    async fn needed(&self, uid: &str, cutoff: Option<String>) -> Result<bool> {
        let uid = uid.to_owned();
        self.database(move |db| {
            Ok(db
                .query_row(
                    "SELECT 1 FROM profile_details WHERE user_id=? AND (? IS NULL OR fetched_at>?)",
                    params![uid, cutoff, cutoff],
                    |_| Ok(()),
                )
                .optional()?
                .is_none())
        })
        .await
    }
    async fn attempt(&self, uid: &str, token: &str, mut options: Options) -> Result<Value> {
        options.retry_rate_limits = false;
        let mut request = Request::new(Method::GET, format!("/users/{uid}/profile"), token);
        request.params = vec![
            ("with_mutual_guilds".into(), "true".into()),
            ("with_mutual_friends_count".into(), "false".into()),
        ];
        let response = self.discord.request(request, options).await?;
        if response.status != 200 {
            let status = if [401, 403, 404, 408, 425, 429].contains(&response.status)
                || (500..=599).contains(&response.status)
            {
                response.status
            } else {
                502
            };
            return Err(error(
                status,
                format!(
                    "Discord could not provide this profile (HTTP {}). Saved data was kept.",
                    response.status
                ),
                retry_seconds(&response),
            ));
        }
        let payload = response.json().map_err(|_| {
            error(
                502,
                "Discord returned an invalid profile response. Saved data was kept.",
                0.0,
            )
        })?;
        let user = payload.get("user").filter(|v| v.is_object());
        let returned_id = user.and_then(|v| v.get("id")).map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        });
        if returned_id.as_deref() != Some(uid) {
            return Err(error(
                502,
                "Discord returned a mismatched profile. Saved data was kept.",
                0.0,
            ));
        }
        let user = user.unwrap();
        let username = if truthy(&user["username"]) {
            scalar(&user["username"])?
        } else {
            SqlValue::Text(String::new())
        };
        let fields = [
            "global_name",
            "avatar",
            "banner",
            "accent_color",
            "public_flags",
        ]
        .map(|key| scalar(&user[key]))
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
        let bot = i64::from(truthy(&user["bot"]));
        let raw = canonical_json(&payload)?;
        let uid = uid.to_owned();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
        self.database(move |db| {
            let tx = db.transaction()?;
            tx.execute("INSERT INTO profiles(user_id,username,global_name,avatar_hash,banner_hash,accent_color,bot,public_flags,fetched_at) VALUES (?,?,?,?,?,?,?,?,?) ON CONFLICT(user_id) DO UPDATE SET username=excluded.username,global_name=excluded.global_name,avatar_hash=excluded.avatar_hash,banner_hash=excluded.banner_hash,accent_color=excluded.accent_color,bot=excluded.bot,public_flags=excluded.public_flags,fetched_at=excluded.fetched_at",
                params![uid,username,fields[0],fields[1],fields[2],fields[3],bot,fields[4],now])?;
            tx.execute("INSERT INTO profile_details(user_id,payload,fetched_at) VALUES (?,?,?) ON CONFLICT(user_id) DO UPDATE SET payload=excluded.payload,fetched_at=excluded.fetched_at", params![uid,raw,now])?;
            tx.commit()?;
            Ok(())
        }).await?;
        Ok(payload)
    }
    /// One manual fetch/refresh, only for known archive identities. Returns the
    /// full saved payload; presentation remains the archive reader's job.
    pub async fn fetch(&self, uid: &str, token: &str, control: Control) -> Result<Value> {
        validate_id(uid)?;
        control.ready().await?;
        self.known(uid).await?;
        self.run(
            uid,
            token,
            Options {
                control,
                ..Options::default()
            },
            false,
            None,
            None,
        )
        .await?
        .ok_or_else(|| error(502, "Profile was not collected.", 0.0))
    }
    /// Retry this user until committed, permanently unavailable, or stopped.
    pub async fn collect(
        &self,
        uid: &str,
        token: &str,
        options: Options,
        on_retry: Option<OnRetry>,
    ) -> Result<Value> {
        validate_id(uid)?;
        self.run(uid, token, options, true, None, on_retry)
            .await?
            .ok_or_else(|| error(502, "Profile was not collected.", 0.0))
    }
    /// Scrapes recheck for a full profile after acquiring shared exclusion.
    pub async fn collect_missing(
        &self,
        uid: &str,
        token: &str,
        options: Options,
        on_retry: Option<OnRetry>,
    ) -> Result<bool> {
        validate_id(uid)?;
        Ok(self
            .run(uid, token, options, true, Some(None), on_retry)
            .await?
            .is_some())
    }
    async fn run(
        &self,
        uid: &str,
        token: &str,
        options: Options,
        retry: bool,
        needed: Option<Option<String>>,
        on_retry: Option<OnRetry>,
    ) -> Result<Option<Value>> {
        validate_id(uid)?;
        options.control.ready().await?;
        let guard = tokio::select! {
            guard = self.exclusion.clone().lock_owned() => guard,
            _ = options.control.stopped() => return Err(stopped()),
        };
        let service = self.clone();
        let uid = uid.to_owned();
        let token = token.to_owned();
        // This task owns exclusion through the blocking commit even if its caller
        // is dropped. Stop controls cancel requests and retries, never an accepted save.
        tokio::spawn(async move {
            let _guard = guard;
            options.control.ready().await?;
            if let Some(cutoff) = needed
                && !service.needed(&uid, cutoff).await?
            {
                return Ok(None);
            }
            let mut attempts = 0u32;
            loop {
                options.control.ready().await?;
                match service.attempt(&uid, &token, options.clone()).await {
                    Ok(payload) => return Ok(Some(payload)),
                    Err(err) => {
                        if !retry || options.control.is_stopped() {
                            return Err(err);
                        }
                        let Some((reason, supplied)) = retryable(err.as_ref()) else {
                            return Err(err);
                        };
                        attempts = attempts.saturating_add(1);
                        let delay = supplied
                            .max((2.0 * 2f64.powi((attempts - 1).min(10) as i32)).min(60.0));
                        if let Some(callback) = &on_retry {
                            callback(RetryInfo {
                                user_id: uid.clone(),
                                reason,
                                attempt: attempts,
                                wait_seconds: delay,
                            });
                        }
                        // Chunk an untrusted Retry-After so even a huge finite
                        // value remains cancellable without overflowing timers.
                        let mut remaining = delay;
                        while remaining > 0.0 {
                            let seconds = remaining.min(60.0);
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs_f64(seconds)) => {},
                                _ = options.control.stopped() => return Err(stopped()),
                            }
                            remaining -= seconds;
                        }
                    }
                }
            }
        })
        .await?
    }
    pub fn state(&self) -> BackfillState {
        self.state.lock().unwrap().clone()
    }
    /// Join accepted profile requests and hold their write boundary during clear.
    pub async fn exclusion(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.exclusion.clone().lock_owned().await
    }
    pub async fn start(&self, token: &str, refresh_stale: bool) -> Result<BackfillState> {
        let mut job = self.job.lock().await;
        if self.state().running {
            return Err(error(409, "Profile collection is already running.", 0.0));
        }
        // Join a completed worker before reusing its state and control.
        if let Some(task) = job.task.take() {
            task.await?;
        }
        let control = Control::default();
        *self.state.lock().unwrap() = BackfillState {
            running: true,
            status: collecting(refresh_stale).into(),
            ..BackfillState::default()
        };
        let started = self.state();
        let service = self.clone();
        let token = token.to_owned();
        job.control = Some(control.clone());
        job.task = Some(tokio::spawn(async move {
            let result = service
                .backfill(&token, refresh_stale, control.clone())
                .await;
            let mut state = service.state.lock().unwrap();
            if control.is_stopped() {
                state.status = "Stopped. Saved profiles are kept.".into();
            } else if result.is_err() {
                state.status =
                    "Collection failed. Saved profiles are kept; retry to resume.".into();
            } else if !state.status.starts_with("Stopped after") {
                state.status = "Complete.".into();
            }
            state.running = false;
            state.current_user = None;
            state.wait_seconds = 0.0;
        }));
        Ok(started)
    }
    pub async fn stop(&self) -> BackfillState {
        let mut job = self.job.lock().await;
        let active = job.task.as_ref().is_some_and(|task| !task.is_finished());
        self.state.lock().unwrap().cancelled = true;
        if let Some(control) = job.control.take() {
            control.stop();
        }
        if let Some(task) = job.task.take() {
            let _ = task.await;
        }
        let mut state = self.state.lock().unwrap();
        if active {
            state.running = false;
            state.current_user = None;
            state.wait_seconds = 0.0;
            state.status = "Stopped. Saved profiles are kept.".into();
        }
        state.clone()
    }
    async fn backfill(&self, token: &str, refresh_stale: bool, control: Control) -> Result<()> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(STALE_AFTER_SECONDS))
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
        let mut after = String::new();
        loop {
            control.ready().await?;
            let (last, before) = (after.clone(), cutoff.clone());
            let ids: Vec<String> = self.database(move |db| {
                Ok(db.prepare("SELECT a.id FROM authors a LEFT JOIN profile_details p ON p.user_id=a.id WHERE (p.user_id IS NULL OR (? AND p.fetched_at<=?)) AND a.id>? ORDER BY a.id LIMIT ?")?.query_map(params![refresh_stale,before,last,BATCH_SIZE], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
            }).await?;
            if ids.is_empty() {
                return Ok(());
            }
            for uid in ids {
                control.ready().await?;
                after = uid.clone();
                {
                    let mut state = self.state.lock().unwrap();
                    state.current_user = Some(uid.clone());
                    state.wait_seconds = 0.0;
                    state.status = collecting(refresh_stale).into();
                }
                let state = self.state.clone();
                let retry: OnRetry = Arc::new(move |info| {
                    let mut state = state.lock().unwrap();
                    state.retries += 1;
                    state.wait_seconds = info.wait_seconds;
                    state.status = format!(
                        "{}; retrying user {} in {:.1}s…",
                        info.reason, info.user_id, info.wait_seconds
                    );
                });
                let state = self.state.clone();
                let options = Options {
                    control: control.clone(),
                    on_wait: Some(Arc::new(move |event| {
                        let seconds = match event {
                            WaitEvent::Waiting(v) => v,
                            WaitEvent::ScrapeBreak(v) => v as f64,
                            WaitEvent::BreakFinished => return,
                        };
                        let mut state = state.lock().unwrap();
                        state.retries += 1;
                        state.wait_seconds = seconds;
                        state.status = "Discord cooldown".into();
                    })),
                    ..Options::default()
                };
                match self
                    .run(
                        &uid,
                        token,
                        options,
                        true,
                        Some(refresh_stale.then(|| cutoff.clone())),
                        Some(retry),
                    )
                    .await
                {
                    Ok(Some(_)) => self.state.lock().unwrap().saved += 1,
                    Ok(None) => {}
                    Err(err) => {
                        if control.is_stopped() {
                            return Err(err);
                        }
                        let Some(err) = err.downcast_ref::<ProfileError>() else {
                            return Err(err);
                        };
                        let mut state = self.state.lock().unwrap();
                        state.failed += 1;
                        state.last_error = Some(format!("HTTP {} for user {uid}", err.status));
                        if err.status == 401 {
                            state.status = "Stopped after Discord HTTP 401. Check account access before retrying.".into();
                            return Ok(());
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        }
    }
}
fn collecting(stale: bool) -> &'static str {
    if stale {
        "Refreshing archived profiles…"
    } else {
        "Collecting missing extended profiles…"
    }
}
fn retryable(err: &(dyn std::error::Error + Send + Sync + 'static)) -> Option<(String, f64)> {
    if let Some(err) = err.downcast_ref::<ProfileError>() {
        return ([408, 425, 429].contains(&err.status) || (500..=599).contains(&err.status))
            .then(|| (format!("HTTP {}", err.status), err.retry_after));
    }
    if err.is::<reqwest::Error>() {
        return Some(("RequestError".into(), 0.0));
    }
    if let Some(rusqlite::Error::SqliteFailure(code, _)) = err.downcast_ref::<rusqlite::Error>()
        && [
            rusqlite::ErrorCode::DatabaseBusy,
            rusqlite::ErrorCode::DatabaseLocked,
        ]
        .contains(&code.code)
    {
        return Some(("database busy".into(), 0.0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification_keeps_permanent_failures_and_cancelled_work_terminal() {
        for status in [408, 425, 429, 500, 502, 503, 599] {
            assert_eq!(
                retryable(error(status, "fixture", 7.0).as_ref()),
                Some((format!("HTTP {status}"), 7.0))
            );
        }
        for status in [400, 401, 403, 404, 409, 422] {
            assert!(retryable(error(status, "fixture", 7.0).as_ref()).is_none());
        }
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            assert_eq!(retryable(&error), Some(("database busy".into(), 0.0)));
        }
        let error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            None,
        );
        assert!(retryable(&error).is_none());
        assert!(retryable(stopped().as_ref()).is_none());
    }
}
