//! Ephemeral, account-bound DM cleanup. Deletion jobs never resume on restart.
use crate::{
    Result,
    discord::{Discord, Request},
    discovery::http_error,
    invites::job_id,
    pacing::{Clock, Control, Options},
    storage,
};
use reqwest::Method;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
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

const CAPACITY: usize = 1000;
const RETENTION: f64 = 60.0;
const UNEXPECTED: &str =
    "Deletion stopped after an unexpected response; check Discord before retrying.";

#[derive(Clone, Default)]
struct Counts {
    deleted: u64,
    failed: u64,
    already_missing: u64,
    scanned: u64,
}
impl Counts {
    fn event(&self, kind: &str) -> Value {
        json!({"type":kind,"deleted":self.deleted,"failed":self.failed,"already_missing":self.already_missing,"scanned":self.scanned})
    }
}
struct Job {
    id: String,
    channel_id: String,
    cancelled: AtomicBool,
    control: Control,
    state: Mutex<JobState>,
    task: Mutex<Option<JoinHandle<Result<()>>>>,
    events: broadcast::Sender<Value>,
}
struct JobState {
    running: bool,
    counts: Counts,
    terminal: Option<Value>,
    expires: Option<f64>,
}
struct State {
    jobs: HashMap<String, Arc<Job>>,
    shutdown: bool,
}
struct Inner<C: Clock> {
    path: PathBuf,
    client: Discord<C>,
    clock: Arc<C>,
    state: Mutex<State>,
    operation: Mutex<()>,
}
pub struct DmCleanup<C: Clock> {
    inner: Arc<Inner<C>>,
}
impl<C: Clock> Clone for DmCleanup<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<C: Clock> DmCleanup<C> {
    /// The archive must already exist. No credentials or deletion work are persisted.
    pub fn new(path: PathBuf, client: Discord<C>, clock: Arc<C>) -> Self {
        Self {
            inner: Arc::new(Inner {
                path,
                client,
                clock,
                state: Mutex::new(State {
                    jobs: HashMap::new(),
                    shutdown: false,
                }),
                operation: Mutex::new(()),
            }),
        }
    }
    async fn prune(&self, state: &mut State) {
        let now = self.inner.clock.now();
        let mut expired = Vec::new();
        for (id, job) in &state.jobs {
            if job
                .state
                .lock()
                .await
                .expires
                .is_some_and(|expires| expires <= now)
            {
                expired.push(id.clone());
            }
        }
        for id in expired {
            state.jobs.remove(&id);
        }
    }
    async fn job(&self, id: &str) -> Result<Arc<Job>> {
        let mut state = self.inner.state.lock().await;
        self.prune(&mut state).await;
        state
            .jobs
            .get(id)
            .cloned()
            .ok_or_else(|| http_error(404, "Job not found"))
    }
    pub async fn start(&self, channel_id: &str, token: String) -> Result<Value> {
        self.start_result(channel_id, Ok(token)).await
    }
    /// The HTTP start endpoint creates a job before its account lookup failure is emitted.
    pub async fn start_error(&self, channel_id: &str, message: String) -> Result<Value> {
        self.start_result(channel_id, Err(runtime(message))).await
    }
    async fn start_result(&self, channel_id: &str, token: Result<String>) -> Result<Value> {
        let this = self.clone();
        let channel_id = channel_id.to_owned();
        tokio::spawn(async move {
            let _operation = this.inner.operation.lock().await;
            if channel_id.is_empty()
                || channel_id.len() > 19
                || !channel_id.bytes().all(|c| c.is_ascii_digit())
                || !channel_id.parse::<i64>().is_ok_and(|n| n >= 1)
            {
                return Err(http_error(422, "Channel ID must be a Discord ID"));
            }
            let mut state = this.inner.state.lock().await;
            this.prune(&mut state).await;
            if state.shutdown {
                return Err(http_error(503, "Deletion jobs are shutting down."));
            }
            for job in state.jobs.values() {
                if job.channel_id == channel_id && job.state.lock().await.running {
                    return Err(http_error(409, "This DM is already being cleared"));
                }
            }
            let id = job_id(true)?;
            let job = Arc::new(Job {
                id: id.clone(),
                channel_id,
                cancelled: AtomicBool::new(false),
                control: Control::default(),
                state: Mutex::new(JobState {
                    running: true,
                    counts: Counts::default(),
                    terminal: None,
                    expires: None,
                }),
                task: Mutex::new(None),
                events: broadcast::channel(CAPACITY).0,
            });
            state.jobs.insert(id.clone(), job.clone());
            let runner = this.clone();
            let run_job = job.clone();
            *job.task.lock().await = Some(tokio::spawn(
                async move { runner.run(run_job, token).await },
            ));
            Ok(json!({"job_id":id}))
        })
        .await?
    }
    /// Cooperative stop; an accepted deletion/tombstone is joined by `barrier` or shutdown.
    pub async fn stop(&self, id: &str) -> Result<Value> {
        let job = self.job(id).await?;
        job.cancelled.store(true, Ordering::SeqCst);
        job.control.stop();
        Ok(json!({"ok":true}))
    }
    fn snapshot(job: &Job, state: &JobState) -> Value {
        json!({"job_id":job.id,"channel_id":job.channel_id,"cancelled":job.cancelled.load(Ordering::SeqCst),"running":state.running,"counts":state.counts.event("progress"),"terminal":state.terminal})
    }
    pub async fn state(&self, id: &str) -> Result<Value> {
        let job = self.job(id).await?;
        let state = job.state.lock().await;
        Ok(Self::snapshot(&job, &state))
    }
    pub async fn has_jobs(&self) -> bool {
        let mut state = self.inner.state.lock().await;
        self.prune(&mut state).await;
        !state.jobs.is_empty()
    }
    pub async fn has_running(&self) -> bool {
        let mut state = self.inner.state.lock().await;
        self.prune(&mut state).await;
        for job in state.jobs.values() {
            if job.state.lock().await.running {
                return true;
            }
        }
        false
    }
    /// Atomic snapshot and bounded subscription. `terminal` lets late subscribers finish.
    /// On `Lagged`, callers can refresh `state`; no queue can grow without bound.
    pub async fn subscribe(&self, id: &str) -> Result<(Value, broadcast::Receiver<Value>)> {
        let job = self.job(id).await?;
        let state = job.state.lock().await;
        Ok((Self::snapshot(&job, &state), job.events.subscribe()))
    }
    async fn emit(job: &Job, event: Value) {
        let _state = job.state.lock().await;
        let _ = job.events.send(event);
    }
    pub async fn barrier(&self, id: &str) -> Result<()> {
        let job = self.job(id).await?;
        let mut task = job.task.lock().await;
        if let Some(handle) = task.as_mut() {
            let result = handle.await;
            task.take();
            result??;
        }
        Ok(())
    }
    /// Stops and joins every accepted request and SQLite write. This instance stays closed.
    pub async fn shutdown(&self) -> Result<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let _operation = this.inner.operation.lock().await;
            let jobs = {
                let mut state = this.inner.state.lock().await;
                state.shutdown = true;
                state.jobs.values().cloned().collect::<Vec<_>>()
            };
            for job in &jobs {
                job.cancelled.store(true, Ordering::SeqCst);
                job.control.stop();
            }
            let mut first = None;
            for job in jobs {
                let mut task = job.task.lock().await;
                if let Some(handle) = task.as_mut() {
                    let result = handle.await;
                    task.take();
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            if first.is_none() {
                                first = Some(error);
                            }
                        }
                        Err(error) => {
                            if first.is_none() {
                                first = Some(error.into());
                            }
                        }
                    }
                }
            }
            this.inner.state.lock().await.jobs.clear();
            match first {
                Some(error) => Err(error),
                None => Ok(()),
            }
        })
        .await?
    }
    async fn wait(&self, job: &Job, seconds: f64) -> Result<()> {
        let end = self.inner.clock.now() + seconds;
        loop {
            if job.cancelled.load(Ordering::SeqCst) {
                return Err(stopped());
            }
            let remaining = end - self.inner.clock.now();
            if remaining <= 0.0 {
                return Ok(());
            }
            self.inner.clock.sleep(remaining.min(0.1)).await;
        }
    }
    async fn request(
        &self,
        job: &Job,
        method: Method,
        path: String,
        token: &str,
        params: Vec<(String, String)>,
        retry: bool,
    ) -> Result<crate::pacing::Response> {
        if job.cancelled.load(Ordering::SeqCst) {
            return Err(stopped());
        }
        let mut request = Request::new(method, path, token);
        request.params = params;
        self.inner
            .client
            .request(
                request,
                Options {
                    control: job.control.clone(),
                    retry_rate_limits: retry,
                    ..Options::default()
                },
            )
            .await
    }
    async fn tombstone(&self, id: i64, source: &'static str) -> Result<()> {
        let path = self.inner.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            db.busy_timeout(Duration::from_secs(30))?;
            let tx = db.transaction()?;
            storage::mark_deleted(&tx, &[id], source)?;
            tx.commit()?;
            Ok(())
        })
        .await?
    }
    async fn run(&self, job: Arc<Job>, token: Result<String>) -> Result<()> {
        let mut counts = Counts::default();
        let result = match token {
            Ok(token) => self.run_pages(&job, &token, &mut counts).await,
            Err(error) => Err(error),
        };
        let cancelled = job.cancelled.load(Ordering::SeqCst);
        let error = result.err().filter(|_| !cancelled);
        let mut terminal = counts.event(if error.is_some() {
            "error"
        } else if cancelled {
            "cancelled"
        } else {
            "complete"
        });
        if let Some(error) = &error {
            // Only deliberate runtime errors are exposed. Transport and parsing errors stay generic.
            terminal["message"] = json!(
                error
                    .downcast_ref::<CleanupError>()
                    .map(|e| e.0.as_str())
                    .unwrap_or(UNEXPECTED)
            );
        }
        {
            let mut state = job.state.lock().await;
            state.running = false;
            state.counts = counts;
            state.expires = Some(self.inner.clock.now() + RETENTION);
            state.terminal = Some(terminal.clone());
            let _ = job.events.send(terminal);
        }
        // The terminal event records operational errors; storage failures are also
        // returned so archive shutdown cannot silently discard failed tombstones.
        match error {
            Some(error)
                if error.downcast_ref::<CleanupError>().is_none()
                    && error.downcast_ref::<reqwest::Error>().is_none()
                    && error.downcast_ref::<serde_json::Error>().is_none() =>
            {
                return Err(error);
            }
            _ => {}
        }
        Ok(())
    }
    async fn run_pages(&self, job: &Job, token: &str, counts: &mut Counts) -> Result<()> {
        let response = self
            .request(job, Method::GET, "/users/@me".into(), token, vec![], true)
            .await?;
        if response.status != 200 {
            return Err(runtime(format!(
                "Failed to fetch user: HTTP {}",
                response.status
            )));
        }
        let user = response.json()?;
        let self_id = user
            .get("id")
            .filter(|id| !id.is_null())
            .ok_or_else(|| runtime(UNEXPECTED))?
            .clone();
        let mut before: Option<String> = None;
        while !job.cancelled.load(Ordering::SeqCst) {
            let mut params = vec![("limit".into(), "100".into())];
            if let Some(before) = &before {
                params.push(("before".into(), before.clone()));
            }
            let response = self
                .request(
                    job,
                    Method::GET,
                    format!("/channels/{}/messages", job.channel_id),
                    token,
                    params,
                    true,
                )
                .await?;
            if response.status != 200 {
                return Err(runtime(format!(
                    "Could not read messages: HTTP {}",
                    response.status
                )));
            }
            let page = response.json()?;
            let messages = page
                .as_array()
                .ok_or_else(|| runtime("Discord returned an invalid message page"))?;
            if messages.is_empty() {
                break;
            }
            counts.scanned += messages.len() as u64;
            for message in messages {
                if job.cancelled.load(Ordering::SeqCst) {
                    return Err(stopped());
                }
                let author = message
                    .get("author")
                    .and_then(|a| a.get("id"))
                    .ok_or_else(|| runtime(UNEXPECTED))?;
                if author != &self_id {
                    continue;
                }
                let id_value = message.get("id").ok_or_else(|| runtime(UNEXPECTED))?;
                let id = id_value
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .or_else(|| id_value.as_i64())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| runtime(UNEXPECTED))?;
                let msg_id = id_value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string());
                let mut status = None;
                let mut missing = false;
                for attempt in 0..4 {
                    match self
                        .request(
                            job,
                            Method::DELETE,
                            format!("/channels/{}/messages/{msg_id}", job.channel_id),
                            token,
                            vec![],
                            false,
                        )
                        .await
                    {
                        Ok(response) => {
                            status = Some(response.status);
                            missing = response.status == 404
                                && response.json().ok().is_some_and(|v| v["code"] == 10008);
                        }
                        Err(error) => {
                            if job.cancelled.load(Ordering::SeqCst) {
                                return Err(stopped());
                            }
                            if error.downcast_ref::<reqwest::Error>().is_none() {
                                return Err(error);
                            }
                            status = None;
                            missing = false;
                        }
                    }
                    let transient = status
                        .is_none_or(|s| [408, 425, 429].contains(&s) || (500..=599).contains(&s));
                    if !transient || attempt == 3 {
                        break;
                    }
                    Self::emit(job, json!({"type":"warning","message":format!("Retrying delete {msg_id} ({}/3).", attempt + 1)})).await;
                    self.wait(job, (1u32 << attempt) as f64).await?;
                }
                if status == Some(204) {
                    counts.deleted += 1;
                } else if missing {
                    counts.already_missing += 1;
                } else {
                    counts.failed += 1;
                    let cause = status
                        .map(|s| format!("HTTP {s}"))
                        .unwrap_or_else(|| "connection failure".into());
                    Self::emit(job, json!({"type":"warning","message":format!("Could not confirm delete {msg_id}: {cause}")})).await;
                }
                if status == Some(204) || missing {
                    self.tombstone(
                        id,
                        if status == Some(204) {
                            "confirmed_delete"
                        } else {
                            "confirmed_missing"
                        },
                    )
                    .await?;
                }
                {
                    let mut state = job.state.lock().await;
                    state.counts = counts.clone();
                    let _ = job.events.send(counts.event("progress"));
                }
                if status.is_some_and(|s| s == 401 || s == 403) {
                    return Err(runtime(format!(
                        "Discord denied deletion: HTTP {}",
                        status.unwrap()
                    )));
                }
                self.wait(job, 0.45).await?;
            }
            before = Some(
                messages
                    .iter()
                    .map(|m| {
                        m["id"]
                            .as_str()
                            .and_then(|s| s.parse::<i64>().ok())
                            .or_else(|| m["id"].as_i64())
                            .ok_or_else(|| runtime(UNEXPECTED))
                    })
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .min()
                    .ok_or_else(|| runtime(UNEXPECTED))?
                    .to_string(),
            );
            if messages.len() < 100 {
                break;
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
struct CleanupError(String);
impl std::fmt::Display for CleanupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CleanupError {}
fn runtime(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(CleanupError(message.into()))
}
fn stopped() -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "Deletion stopped").into()
}
