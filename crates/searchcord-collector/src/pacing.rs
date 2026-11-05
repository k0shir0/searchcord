//! One collector-wide request gate, including restart-safe scrape budgets.
//!
//! Clone a Gate for every Discord service. Waiting and paused requests release
//! the gate; an accepted attempt owns it until its checkpoint commit finishes.
//! Stop the request controls and await their callers (or `barrier`) before
//! closing storage. Dropping a caller never abandons an accepted checkpoint.
use crate::Result;
use reqwest::header::HeaderMap;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    future::Future,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, watch};

/// The same wall-clock JSON fields used by Python's discord-pacing job.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Checkpoint {
    pub discord_ready_at: f64,
    pub last_scrape_request: f64,
    pub scrape_break_until: f64,
    pub scrape_confirmed_requests: u32,
}
impl Checkpoint {
    fn validate(&self) -> Result<()> {
        if ![
            self.discord_ready_at,
            self.last_scrape_request,
            self.scrape_break_until,
        ]
        .into_iter()
        .all(f64::is_finite)
            || self.scrape_confirmed_requests >= 100
        {
            return Err("Invalid saved Discord pacing state".into());
        }
        Ok(())
    }
}

/// Explicit time and random draws make pacing testable without real waits.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> f64;
    fn wall(&self) -> f64;
    fn uniform(&self, low: f64, high: f64) -> Result<f64>;
    fn sleep(&self, seconds: f64) -> impl Future<Output = ()> + Send;
}
#[derive(Clone)]
pub struct SystemClock(tokio::time::Instant);
impl Default for SystemClock {
    fn default() -> Self {
        Self(tokio::time::Instant::now())
    }
}
impl Clock for SystemClock {
    fn now(&self) -> f64 {
        1.0 + self.0.elapsed().as_secs_f64()
    }
    fn wall(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }
    fn uniform(&self, low: f64, high: f64) -> Result<f64> {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|e| format!("Could not draw request interval: {e}"))?;
        let unit = (u64::from_le_bytes(bytes) >> 11) as f64 / (1u64 << 53) as f64;
        Ok(low + (high - low) * unit)
    }
    async fn sleep(&self, seconds: f64) {
        tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Running,
    Paused,
    Stopped,
}
#[derive(Clone)]
pub struct Control(watch::Sender<Status>);
impl Default for Control {
    fn default() -> Self {
        Self(watch::channel(Status::Running).0)
    }
}
impl Control {
    pub fn is_paused(&self) -> bool {
        *self.0.borrow() == Status::Paused
    }
    pub fn is_stopped(&self) -> bool {
        *self.0.borrow() == Status::Stopped
    }
    pub async fn ready(&self) -> Result<()> {
        running(&mut self.0.subscribe()).await
    }
    pub async fn stopped(&self) {
        until_stopped(&mut self.0.subscribe()).await;
    }
    pub fn pause(&self) {
        self.update(Status::Paused);
    }
    pub fn resume(&self) {
        self.update(Status::Running);
    }
    pub fn stop(&self) {
        self.update(Status::Stopped);
    }
    fn update(&self, status: Status) {
        self.0.send_modify(|current| {
            if *current != Status::Stopped {
                *current = status;
            }
        });
    }
}
fn stopped() -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "Discord request stopped").into()
}
async fn running(receiver: &mut watch::Receiver<Status>) -> Result<()> {
    loop {
        let status = *receiver.borrow_and_update();
        match status {
            Status::Running => return Ok(()),
            Status::Stopped => return Err(stopped()),
            Status::Paused => {
                receiver.changed().await.map_err(|_| stopped())?;
            }
        }
    }
}
async fn until_stopped(receiver: &mut watch::Receiver<Status>) {
    loop {
        if *receiver.borrow_and_update() == Status::Stopped {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WaitEvent {
    Paused,
    Waiting(f64),
    ScrapeBreak(u64),
    BreakFinished,
}
#[derive(Clone)]
pub struct Options {
    pub scrape: bool,
    pub retry_rate_limits: bool,
    pub control: Control,
    pub on_wait: Option<Arc<dyn Fn(WaitEvent) + Send + Sync>>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            scrape: false,
            retry_rate_limits: true,
            control: Control::default(),
            on_wait: None,
        }
    }
}
impl Options {
    pub(crate) async fn ready(&self) -> Result<()> {
        if self.control.is_paused() {
            self.emit(WaitEvent::Paused);
        }
        self.control.ready().await
    }
    fn emit(&self, event: WaitEvent) {
        if let Some(callback) = &self.on_wait {
            callback(event);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&self.body)?)
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }
}
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(value) => Some(u8::from(*value) as f64),
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}
/// JSON retry_after takes precedence, with the same tolerant header fallback.
pub fn retry_after(response: &Response) -> f64 {
    let payload = response.json().ok();
    let value = payload
        .as_ref()
        .and_then(|v| v.get("retry_after"))
        .filter(|v| !v.is_null());
    let seconds = match value {
        Some(value) => numeric(value),
        None => response
            .header("retry-after")
            .unwrap_or("1")
            .trim()
            .parse()
            .ok(),
    };
    seconds
        .filter(|s| s.is_finite())
        .map(|s| s.max(0.0))
        .unwrap_or(1.0)
}

#[derive(Default)]
struct State {
    persistence_error: Option<String>,
    last_discord_request: f64,
    ready_at: f64,
    last_scrape_request: f64,
    break_until: f64,
    confirmed: u32,
}
impl State {
    fn recover(saved: Checkpoint, now: f64, wall: f64) -> Self {
        Self {
            ready_at: now + (saved.discord_ready_at - wall).max(0.0),
            last_scrape_request: now + saved.last_scrape_request - wall,
            break_until: now + (saved.scrape_break_until - wall).max(0.0),
            confirmed: saved.scrape_confirmed_requests,
            ..Self::default()
        }
    }
    fn checkpoint(&self, now: f64, wall: f64) -> Checkpoint {
        Checkpoint {
            discord_ready_at: wall + (self.ready_at - now).max(0.0),
            last_scrape_request: if self.last_scrape_request == 0.0 {
                0.0
            } else {
                wall + self.last_scrape_request - now
            },
            scrape_break_until: wall + (self.break_until - now).max(0.0),
            scrape_confirmed_requests: self.confirmed,
        }
    }
}
fn connection(path: &PathBuf) -> Result<Connection> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(30))?;
    Ok(db)
}
async fn load(path: PathBuf) -> Result<Option<Checkpoint>> {
    tokio::task::spawn_blocking(move || {
        let raw: Option<String> = connection(&path)?.query_row(
            "SELECT state_json FROM collection_jobs WHERE id='discord-pacing' AND kind='pacing'", [], |r| r.get(0)
        ).optional()?;
        raw.map(|raw| {
            let saved: Checkpoint = serde_json::from_str(&raw)?;
            saved.validate()?;
            Ok(saved)
        }).transpose()
    }).await?
}
async fn persist(path: PathBuf, saved: Checkpoint, wall: f64) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        connection(&path)?.execute(
            "INSERT INTO collection_jobs(id,kind,state_json,updated_at) VALUES ('discord-pacing','pacing',?,?)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,state_json=excluded.state_json,updated_at=excluded.updated_at",
            params![serde_json::to_string(&saved)?, wall]
        )?;
        Ok(())
    }).await?
}

pub struct Gate<C: Clock = SystemClock> {
    state: Arc<Mutex<State>>,
    clock: Arc<C>,
    path: PathBuf,
    break_deadline: Arc<AtomicU64>,
}
impl<C: Clock> Clone for Gate<C> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            clock: self.clock.clone(),
            path: self.path.clone(),
            break_deadline: self.break_deadline.clone(),
        }
    }
}
impl<C: Clock> Gate<C> {
    /// Storage must already have been initialized; this never creates archives.
    pub async fn open(path: PathBuf, clock: C) -> Result<Self> {
        let state = match load(path.clone()).await? {
            Some(saved) => State::recover(saved, clock.now(), clock.wall()),
            None => State::default(),
        };
        Ok(Self {
            break_deadline: Arc::new(AtomicU64::new(state.break_until.to_bits())),
            state: Arc::new(Mutex::new(state)),
            clock: Arc::new(clock),
            path,
        })
    }
    pub async fn checkpoint(&self) -> Checkpoint {
        self.state
            .lock()
            .await
            .checkpoint(self.clock.now(), self.clock.wall())
    }
    pub fn break_seconds(&self) -> u64 {
        (f64::from_bits(self.break_deadline.load(Ordering::Acquire)) - self.clock.now())
            .max(0.0)
            .ceil() as u64
    }
    /// After stopping callers, join any accepted attempt and checkpoint write.
    pub async fn barrier(&self) -> Result<()> {
        let state = self.state.lock().await;
        match &state.persistence_error {
            Some(error) => Err(format!("Discord pacing checkpoint failed: {error}").into()),
            None => Ok(()),
        }
    }
    fn draw(&self, low: f64, high: f64) -> Result<f64> {
        let value = self.clock.uniform(low, high)?;
        if !value.is_finite() || !(low..=high).contains(&value) {
            return Err("Invalid pacing random draw".into());
        }
        Ok(value)
    }
    /// send must return a fully buffered response, as httpx.AsyncClient does.
    pub async fn request<F, Fut>(&self, options: Options, mut send: F) -> Result<Response>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = Result<Response>> + Send + 'static,
    {
        let mut receiver = options.control.0.subscribe();
        for attempt in 0..=5 {
            let interval = if options.scrape {
                self.draw(0.5, 1.5)?
            } else {
                0.0
            };
            let response = loop {
                if *receiver.borrow() == Status::Paused {
                    options.emit(WaitEvent::Paused);
                }
                running(&mut receiver).await?;
                let mut guard = tokio::select! {
                    guard = self.state.clone().lock_owned() => guard,
                    _ = receiver.changed() => continue,
                };
                if *receiver.borrow() != Status::Running {
                    continue;
                }
                let mut ready = (guard.last_discord_request + 0.025).max(guard.ready_at);
                if options.scrape {
                    ready = ready
                        .max(guard.last_scrape_request + interval)
                        .max(guard.break_until);
                }
                let now = self.clock.now();
                let wait = ready - now;
                if wait > 0.0 {
                    let remaining_break = guard.break_until - now;
                    drop(guard);
                    if options.scrape && remaining_break > 0.0 {
                        options.emit(WaitEvent::ScrapeBreak(remaining_break.ceil() as u64));
                    }
                    if wait >= 0.1 {
                        options.emit(WaitEvent::Waiting(wait));
                    }
                    tokio::select! { _ = self.clock.sleep(wait.min(0.25)) => {}, _ = receiver.changed() => {} }
                    continue;
                }
                guard.last_discord_request = now;
                let future = send();
                let gate = self.clone();
                let options = options.clone();
                let mut receiver = receiver.clone();
                // The task owns the permit through persistence even if its caller
                // is cancelled. Only an explicit stop cancels the network attempt.
                break tokio::spawn(async move {
                    let mut result = tokio::select! {
                        biased;
                        _ = until_stopped(&mut receiver) => Err(stopped()),
                        response = future => response,
                    };
                    let now = gate.clock.now();
                    let mut draw_error = None;
                    if options.scrape {
                        guard.last_scrape_request = now;
                    }
                    if let Ok(response) = &result {
                        if options.scrape && (200..300).contains(&response.status) {
                            guard.confirmed += 1;
                            if guard.confirmed == 100 {
                                guard.confirmed = 0;
                                let seconds = match gate.draw(60.0, 90.0) {
                                    Ok(seconds) => seconds,
                                    Err(error) => {
                                        // Keep the full break and persist the
                                        // accepted attempt if entropy fails.
                                        draw_error = Some(error);
                                        90.0
                                    }
                                };
                                guard.break_until = now + seconds;
                            }
                        }
                        if response.status == 429 {
                            guard.ready_at = guard.ready_at.max(now + retry_after(response));
                        } else if response.header("x-ratelimit-remaining") == Some("0")
                            && let Some(seconds) = response
                                .header("x-ratelimit-reset-after")
                                .and_then(|s| s.trim().parse::<f64>().ok())
                                .filter(|s| s.is_finite())
                        {
                            guard.ready_at = guard.ready_at.max(now + seconds.max(0.0));
                        }
                    }
                    let wall = gate.clock.wall();
                    gate.break_deadline
                        .store(guard.break_until.to_bits(), Ordering::Release);
                    if let Err(error) =
                        persist(gate.path.clone(), guard.checkpoint(now, wall), wall).await
                    {
                        guard.persistence_error = Some(error.to_string());
                        return Err(error);
                    }
                    guard.persistence_error = None;
                    if let Some(error) = draw_error {
                        result = Err(error);
                    }
                    result
                })
                .await??;
            };
            if options.scrape {
                options.emit(WaitEvent::BreakFinished);
            }
            if response.status != 429 || !options.retry_rate_limits || attempt == 5 {
                return Ok(response);
            }
        }
        unreachable!("the last rate-limited response is returned")
    }
}
