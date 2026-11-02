//! Durable scrape ownership. Progress is published after its SQLite checkpoint.
use crate::{
    PageContext, Result,
    discord::{Discord, Request},
    discovery::{HttpError, http_error},
    pacing::{Clock, Control, Options, WaitEvent, retry_after},
    profiles::Profiles,
    writer::Writer,
};
use reqwest::Method;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, broadcast};

pub fn save_job(db: &Connection, id: &str, kind: &str, state: &Value) -> Result<()> {
    db.execute("INSERT INTO collection_jobs VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,state_json=excluded.state_json,updated_at=excluded.updated_at",params![id,kind,state.to_string(),chrono::Utc::now().timestamp_millis() as f64/1000.0])?;
    Ok(())
}
pub fn new_id() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    b[6] = (b[6] & 15) | 64;
    b[8] = (b[8] & 63) | 128;
    let h = b.iter().map(|v| format!("{v:02x}")).collect::<String>();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}
pub fn validate_channels(channels: &Value) -> Result<Vec<Value>> {
    let channels = channels
        .as_array()
        .ok_or_else(|| http_error(422, "Select between 1 and 2000 channels"))?;
    if !(1..=2000).contains(&channels.len()) {
        return Err(http_error(422, "Select between 1 and 2000 channels"));
    }
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for c in channels {
        if !c.is_object() {
            return Err(http_error(422, "Each channel must provide its ID and name"));
        }
        let fail = || {
            http_error(
                422,
                "Channel IDs must be Discord IDs and names must contain at most 200 characters",
            )
        };
        let valid = |s: &str| {
            !s.is_empty()
                && s.len() <= 19
                && s.bytes().all(|b| b.is_ascii_digit())
                && s.parse::<i64>().is_ok_and(|n| n > 0)
        };
        let id = c["id"].as_str().ok_or_else(fail)?;
        let name = c["name"].as_str().ok_or_else(fail)?;
        let gname = c
            .get("guild_name")
            .map_or(Some(""), Value::as_str)
            .ok_or_else(fail)?;
        if !valid(id)
            || name.chars().count() > 200
            || gname.chars().count() > 200
            || c.get("guild_id")
                .is_some_and(|g| !g.is_null() && !g.as_str().is_some_and(valid))
        {
            return Err(fail());
        }
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let gid = c["guild_id"].as_str();
        result.push(json!({"id":id,"name":name,"guild_id":gid,"guild_name":gname}));
    }
    Ok(result)
}
fn project(state: &Value, event: Option<&Value>, changes: &Value) -> Value {
    let mut result = state.clone();
    if let Some(changes) = changes.as_object() {
        for (k, v) in changes {
            result[k] = v.clone();
        }
    }
    if let Some(event) = event {
        result["last_event"] = event.clone();
        let kind = event["type"].as_str().unwrap_or("");
        if kind == "channel_start" {
            result["index"] = event["index"].clone();
        }
        if let Some(total) = event.get("total_messages") {
            result["total_messages"] = total.clone();
        }
        if kind == "channel_complete" {
            result["completed"] = json!(result["completed"].as_u64().unwrap_or(0) + 1);
        }
        if [
            "progress",
            "channel_start",
            "channel_complete",
            "channel_error",
        ]
        .contains(&kind)
            && let Some(index) = result["index"].as_u64().and_then(|n| n.checked_sub(1))
        {
            if kind == "channel_complete"
                && result["rows"][index as usize]["type"] == "channel_error"
            {
                result["rows"][index as usize]["messages"] = event["messages"].clone();
            } else if let Some(rows) = result["rows"].as_array_mut()
                && let Some(row) = rows.get_mut(index as usize)
            {
                *row = event.clone();
            }
        }
    }
    result
}
struct Job {
    id: String,
    state: Arc<Mutex<Value>>,
    control: Control,
    events: broadcast::Sender<Value>,
    notice: std::sync::Mutex<Option<Value>>,
    recovered: AtomicBool,
    suspending: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    commands: Mutex<()>,
    on_break: AtomicBool,
}
struct Inner<C: Clock> {
    path: PathBuf,
    writer: Writer,
    client: Discord<C>,
    profiles: Profiles<C>,
    jobs: Mutex<Vec<Arc<Job>>>,
    closing: AtomicBool,
    accounts: std::sync::Mutex<Option<crate::accounts::Accounts>>,
    media: std::sync::Mutex<Option<crate::media_collection::MediaCollector>>,
}
pub struct Scrapes<C: Clock>(Arc<Inner<C>>);
impl<C: Clock> Clone for Scrapes<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<C: Clock> Scrapes<C> {
    pub async fn open(
        path: PathBuf,
        writer: Writer,
        client: Discord<C>,
        profiles: Profiles<C>,
    ) -> Result<Self> {
        let owner = Self(Arc::new(Inner {
            path,
            writer,
            client,
            profiles,
            jobs: Mutex::new(Vec::new()),
            closing: AtomicBool::new(false),
            accounts: std::sync::Mutex::new(None),
            media: std::sync::Mutex::new(None),
        }));
        owner.recover().await?;
        Ok(owner)
    }
    pub fn with_accounts(self, accounts: crate::accounts::Accounts) -> Self {
        *self.0.accounts.lock().unwrap() = Some(accounts);
        self
    }
    pub fn with_media(self, media: crate::media_collection::MediaCollector) -> Self {
        *self.0.media.lock().unwrap() = Some(media);
        self
    }
    fn job(id: String, state: Value, recovered: bool) -> Arc<Job> {
        let control = Control::default();
        if recovered {
            control.pause();
        }
        Arc::new(Job {
            id,
            state: Arc::new(Mutex::new(state)),
            control,
            events: broadcast::channel(1000).0,
            notice: std::sync::Mutex::new(None),
            recovered: AtomicBool::new(recovered),
            suspending: AtomicBool::new(false),
            task: Mutex::new(None),
            commands: Mutex::new(()),
            on_break: AtomicBool::new(false),
        })
    }
    async fn get(&self, id: &str) -> Result<Arc<Job>> {
        self.0
            .jobs
            .lock()
            .await
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .ok_or_else(|| http_error(404, "Job not found"))
    }
    async fn recover(&self) -> Result<()> {
        let path = self.0.path.clone();
        let saved = tokio::task::spawn_blocking(move || -> Result<Vec<(String, Value)>> {
            let db = Connection::open(path)?;
            let mut q =
                db.prepare("SELECT id,state_json FROM collection_jobs WHERE kind='scrape'")?;
            let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut result = vec![];
            for row in rows {
                let (id, raw) = row?;
                let mut state: Value = serde_json::from_str(&raw)?;
                if state["running"] == true && state["cancelled"] != true {
                    if !state["channels"].is_array() {
                        return Err("Invalid saved scrape checkpoint".into());
                    }
                    state["paused"] = json!(true);
                    state["pause_requested"] = json!(true);
                    result.push((id, state));
                }
            }
            Ok(result)
        })
        .await??;
        let mut jobs = self.0.jobs.lock().await;
        for (id, state) in saved {
            jobs.push(Self::job(id, state, true));
        }
        Ok(())
    }
    async fn snapshot_locked(job: &Job, state: &Value) -> Value {
        let mut snapshot = json!({"type":"snapshot","job_id":job.id,"recovered":job.recovered.load(Ordering::Acquire),"break_seconds":0});
        for k in [
            "running",
            "cancelled",
            "pause_requested",
            "paused",
            "channels",
            "limit",
            "harvest_profiles",
            "index",
            "total_messages",
            "completed",
            "rows",
        ] {
            snapshot[k] = state[k].clone();
        }
        snapshot["last_event"] = job
            .notice
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| state["last_event"].clone());
        snapshot
    }
    async fn snapshot_job(job: &Job) -> Value {
        let state = job.state.lock().await;
        Self::snapshot_locked(job, &state).await
    }
    pub async fn snapshot(&self, id: &str) -> Result<Value> {
        let job = self.get(id).await?;
        let mut snapshot = Self::snapshot_job(&job).await;
        snapshot["break_seconds"] = json!(self.0.client.break_seconds());
        Ok(snapshot)
    }
    pub async fn active(&self) -> Vec<Value> {
        let jobs = self.0.jobs.lock().await.clone();
        let mut result = vec![];
        for job in jobs {
            let mut snapshot = Self::snapshot_job(&job).await;
            snapshot["break_seconds"] = json!(self.0.client.break_seconds());
            if snapshot["running"] == true {
                result.push(snapshot);
            }
        }
        result
    }
    pub async fn subscribe(&self, id: &str) -> Result<(Value, broadcast::Receiver<Value>)> {
        let job = self.get(id).await?;
        let _commands = job.commands.lock().await;
        let state = job.state.lock().await;
        let events = job.events.subscribe();
        let mut snapshot = Self::snapshot_locked(&job, &state).await;
        snapshot["break_seconds"] = json!(self.0.client.break_seconds());
        Ok((snapshot, events))
    }
    async fn transition(&self, job: Arc<Job>, event: Option<Value>, changes: Value) -> Result<()> {
        let writer = self.0.writer.clone();
        tokio::spawn(async move {
            let mut state = job.state.lock().await;
            let next = project(&state, event.as_ref(), &changes);
            let saved = next.clone();
            let id = job.id.clone();
            writer
                .write(move |store| {
                    save_job(store.connection(), &id, "scrape", &saved)?;
                    Ok(Value::Null)
                })
                .await?;
            *state = next;
            if let Some(event) = event {
                *job.notice.lock().unwrap() = None;
                let _ = job.events.send(event);
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await?
    }
    fn report(job: &Job, event: Value) {
        *job.notice.lock().unwrap() = Some(event.clone());
        let _ = job.events.send(event);
    }
    fn options(&self, job: Arc<Job>) -> Options {
        let owner = self.clone();
        let control = job.control.clone();
        Options {
            scrape: true,
            control,
            on_wait: Some(Arc::new(move |event| match event {
                WaitEvent::Paused => {
                    let owner = owner.clone();
                    let job = job.clone();
                    tokio::spawn(async move {
                        let mut state = job.state.lock().await;
                        if !job.control.is_paused()
                            || state["pause_requested"] != true
                            || state["paused"] == true
                            || state["running"] != true
                        {
                            return;
                        }
                        let event = json!({"type":"paused"});
                        let next = project(
                            &state,
                            Some(&event),
                            &json!({"paused":true,"pause_requested":true}),
                        );
                        let saved = next.clone();
                        let id = job.id.clone();
                        if let Err(error) = owner
                            .0
                            .writer
                            .write(move |store| {
                                save_job(store.connection(), &id, "scrape", &saved)?;
                                Ok(Value::Null)
                            })
                            .await
                        {
                            eprintln!("Could not pause scrape {}: {error}", job.id);
                            return;
                        }
                        *state = next;
                        *job.notice.lock().unwrap() = None;
                        let _ = job.events.send(event);
                    });
                }
                WaitEvent::ScrapeBreak(seconds) => {
                    if !job.on_break.swap(true, Ordering::AcqRel) {
                        Self::report(&job, json!({"type":"scrape_break","wait_seconds":seconds}));
                    }
                }
                WaitEvent::BreakFinished => {
                    if job.on_break.swap(false, Ordering::AcqRel) {
                        Self::report(&job, json!({"type":"scrape_break_end"}));
                    }
                }
                WaitEvent::Waiting(_) => {}
            })),
            ..Options::default()
        }
    }
    pub async fn start(
        &self,
        channels: Value,
        limit: u64,
        harvest: bool,
        credential_id: String,
        token: String,
    ) -> Result<String> {
        let owner = self.clone();
        tokio::spawn(async move {
            owner
                .start_inner(channels, limit, harvest, credential_id, token)
                .await
        })
        .await?
    }
    async fn start_inner(
        &self,
        channels: Value,
        limit: u64,
        harvest: bool,
        credential_id: String,
        token: String,
    ) -> Result<String> {
        let channels = validate_channels(&channels)?;
        let mut jobs = self.0.jobs.lock().await;
        if self.0.closing.load(Ordering::Acquire) {
            return Err(http_error(409, "Collector is shutting down"));
        }
        for job in jobs.iter() {
            let state = job.state.lock().await;
            if state["running"] == true
                && channels.iter().any(|c| {
                    state["channels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|old| old["id"] == c["id"])
                })
            {
                return Err(http_error(409, "A selected channel is still being scraped"));
            }
        }
        let id = new_id()?;
        let state = json!({"running":true,"cancelled":false,"pause_requested":false,"paused":false,"channels":channels,"limit":limit,"harvest_profiles":harvest,"index":0,"total_messages":0,"completed":0,"rows":vec![json!({});channels.len()],"last_event":null,"next_index":0,"channel_state":null,"failed_channels":0,"credential_id":credential_id,"profile_auth_failed":false});
        let job = Self::job(id.clone(), state, false);
        self.transition(job.clone(), None, json!({})).await?;
        jobs.push(job.clone());
        drop(jobs);
        self.spawn(job, token).await;
        Ok(id)
    }
    async fn spawn(&self, job: Arc<Job>, token: String) {
        let owner = self.clone();
        let running = job.clone();
        *job.task.lock().await = Some(tokio::spawn(async move {
            owner.run(running, token).await;
        }));
    }
    pub async fn pause(&self, id: &str) -> Result<Value> {
        let owner = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move { owner.pause_inner(&id).await }).await?
    }
    async fn pause_inner(&self, id: &str) -> Result<Value> {
        let job = self.get(id).await?;
        let _guard = job.commands.lock().await;
        if job.state.lock().await["running"] != true || job.control.is_stopped() {
            return Err(http_error(409, "Scrape is no longer running"));
        }
        self.transition(job.clone(), None, json!({"pause_requested":true}))
            .await?;
        job.control.pause();
        Ok(json!({"ok":true,"paused":job.state.lock().await["paused"]}))
    }
    pub async fn resume(&self, id: &str, credential_id: &str, token: String) -> Result<Value> {
        let owner = self.clone();
        let id = id.to_owned();
        let credential_id = credential_id.to_owned();
        tokio::spawn(async move { owner.resume_inner(&id, &credential_id, token).await }).await?
    }
    async fn resume_inner(&self, id: &str, credential_id: &str, token: String) -> Result<Value> {
        let job = self.get(id).await?;
        let _guard = job.commands.lock().await;
        if self.0.closing.load(Ordering::Acquire) {
            return Err(http_error(409, "Collector is shutting down"));
        }
        let state = job.state.lock().await;
        if state["credential_id"] != credential_id {
            return Err(http_error(
                409,
                "Select the account used to start this job before resuming",
            ));
        }
        if state["running"] != true || job.control.is_stopped() {
            return Err(http_error(409, "Scrape is no longer running"));
        }
        let was_paused = state["pause_requested"] == true;
        drop(state);
        self.transition(
            job.clone(),
            was_paused
                .then(|| json!({"type":"resumed","break_seconds":self.0.client.break_seconds()})),
            json!({"pause_requested":false,"paused":false}),
        )
        .await?;
        job.recovered.store(false, Ordering::Release);
        job.suspending.store(false, Ordering::Release);
        job.control.resume();
        let mut task = job.task.lock().await;
        if task.as_ref().is_none_or(|t| t.is_finished()) {
            let owner = self.clone();
            let running = job.clone();
            *task = Some(tokio::spawn(async move {
                owner.run(running, token).await;
            }));
        }
        Ok(json!({"ok":true}))
    }
    pub async fn stop(&self, id: &str) -> Result<Value> {
        let owner = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move { owner.stop_inner(&id).await }).await?
    }
    async fn stop_inner(&self, id: &str) -> Result<Value> {
        let job = self.get(id).await?;
        let _guard = job.commands.lock().await;
        job.control.stop();
        if let Some(task) = job.task.lock().await.take() {
            task.await?;
        }
        if job.state.lock().await["running"] == true {
            let total = job.state.lock().await["total_messages"].clone();
            self.transition(
                job.clone(),
                Some(json!({"type":"cancelled","total_messages":total})),
                json!({"running":false,"cancelled":true,"paused":false,"pause_requested":false}),
            )
            .await?;
        }
        self.0.writer.barrier().await?;
        Ok(json!({"ok":true}))
    }
    pub async fn suspend(&self) -> Result<()> {
        let owner = self.clone();
        tokio::spawn(async move { owner.suspend_inner().await }).await?
    }
    async fn suspend_inner(&self) -> Result<()> {
        self.0.closing.store(true, Ordering::Release);
        let jobs = self.0.jobs.lock().await.clone();
        for job in &jobs {
            job.suspending.store(true, Ordering::Release);
            job.control.stop();
        }
        for job in jobs {
            let _guard = job.commands.lock().await;
            if let Some(task) = job.task.lock().await.take() {
                task.await?;
            }
            if job.state.lock().await["running"] == true {
                self.transition(
                    job.clone(),
                    Some(json!({"type":"paused"})),
                    json!({"paused":true,"pause_requested":true}),
                )
                .await?;
            }
        }
        self.0.writer.barrier().await
    }
    async fn checkpoint(&self, job: Arc<Job>) -> Result<()> {
        let needs_pause = {
            let state = job.state.lock().await;
            state["pause_requested"] == true && state["paused"] != true
        };
        if needs_pause {
            self.transition(
                job.clone(),
                Some(json!({"type":"paused"})),
                json!({"paused":true,"pause_requested":true}),
            )
            .await?;
        }
        job.control.ready().await
    }
    async fn wait(&self, job: Arc<Job>, seconds: f64, pause: bool) -> Result<()> {
        let mut remaining = seconds.max(0.0);
        while remaining > 0.0 {
            let before = tokio::time::Instant::now();
            if pause {
                self.checkpoint(job.clone()).await?;
            }
            let sleep = (remaining - before.elapsed().as_secs_f64()).clamp(0.0, 0.1);
            if sleep == 0.0 {
                return Ok(());
            }
            tokio::select! {_=job.control.stopped()=>return Err("Scrape stopped".into()),_=tokio::time::sleep(std::time::Duration::from_secs_f64(sleep))=>{}}
            remaining -= before.elapsed().as_secs_f64();
        }
        Ok(())
    }
    async fn fetch(
        &self,
        job: Arc<Job>,
        channel: &str,
        params: Vec<(String, String)>,
        token: &str,
    ) -> Result<Vec<Value>> {
        let mut attempts = 0u32;
        loop {
            self.checkpoint(job.clone()).await?;
            let mut request =
                Request::new(Method::GET, format!("/channels/{channel}/messages"), token);
            request.params = params.clone();
            let result = self
                .0
                .client
                .request(request, self.options(job.clone()))
                .await;
            let (reason, delay) = match result {
                Ok(response) if response.status == 200 => match response.json() {
                    Ok(Value::Array(messages)) => {
                        if attempts > 0 {
                            Self::report(
                                &job,
                                json!({"type":"retry_resumed","channel_id":channel,"stage":"fetch","attempts":attempts}),
                            );
                        }
                        return Ok(messages);
                    }
                    _ => ("Invalid JSON response".to_owned(), 0.0),
                },
                Ok(response)
                    if [408, 425, 429].contains(&response.status)
                        || (500..=599).contains(&response.status) =>
                {
                    (
                        format!("HTTP {}", response.status),
                        if [429, 503].contains(&response.status) {
                            retry_after(&response)
                        } else {
                            0.0
                        },
                    )
                }
                Ok(response) => {
                    return Err(http_error(
                        response.status,
                        match response.status {
                            401 => "Token rejected (401)".to_owned(),
                            403 => "No access (403)".to_owned(),
                            n => format!("HTTP {n}"),
                        },
                    ));
                }
                Err(error) => {
                    if job.control.is_stopped() {
                        return Err(error);
                    }
                    let transient = error
                        .downcast_ref::<reqwest::Error>()
                        .is_some_and(|e| !e.is_builder())
                        || error.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
                            matches!(
                                e.sqlite_error_code(),
                                Some(
                                    rusqlite::ErrorCode::DatabaseBusy
                                        | rusqlite::ErrorCode::DatabaseLocked
                                )
                            )
                        });
                    if !transient {
                        return Err(error);
                    }
                    (error.to_string(), 0.0)
                }
            };
            attempts += 1;
            let delay = delay.max((2f64.powi(attempts.min(10) as i32)).min(60.0));
            Self::report(
                &job,
                json!({"type":"retry_wait","channel_id":channel,"stage":"fetch","attempt":attempts,"wait_seconds":delay,"reason":reason}),
            );
            self.wait(job.clone(), delay, true).await?;
        }
    }
    async fn channel_state(&self, job: Arc<Job>, channel: &Value) -> Result<()> {
        if !job.state.lock().await["channel_state"].is_null() {
            return Ok(());
        }
        let cid = channel["id"].as_str().unwrap().to_owned();
        let cursor=self.0.writer.write(move|store|{let row=store.connection().query_row("SELECT newest_message_id,oldest_message_id,history_complete,pending_after_message_id,pending_before_message_id FROM scrape_cursors WHERE channel_id=?",[cid],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,bool>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?))).optional()?;Ok(match row {Some((newest,oldest,complete,anchor,before))=>json!({"stage":"newer","messages":0,"profiles":0,"pending_profiles":[],"anchor":anchor.as_ref().unwrap_or(&newest),"before":if anchor.is_some(){before}else{None},"newest":newest,"oldest":oldest,"history_complete":complete,"resuming_pending":anchor.is_some()}),None=>json!({"stage":"history","messages":0,"profiles":0,"pending_profiles":[],"anchor":null,"before":null,"newest":null,"oldest":null,"history_complete":false,"resuming_pending":false})})}).await?;
        self.transition(job, None, json!({"channel_state":cursor}))
            .await
    }
    async fn page(
        &self,
        job: Arc<Job>,
        channel: Value,
        messages: Vec<Value>,
        next: Value,
        history: bool,
        more: bool,
    ) -> Result<()> {
        let writer = self.0.writer.clone();
        tokio::spawn(async move {
            let mut state=job.state.lock().await;let old=state.clone();let id=job.id.clone();
            let saved=writer.write(move|store| {
                let cid=channel["id"].as_str().unwrap();let mut committed=Value::Null;
                let mut checkpoint=|db:&Connection,inserted:usize|->Result<()> {let mut channel_state=next.clone();channel_state["messages"]=json!(old["channel_state"]["messages"].as_u64().unwrap_or(0)+inserted as u64);let event=json!({"type":"progress","channel":channel["name"],"messages":channel_state["messages"],"total_messages":old["total_messages"].as_u64().unwrap_or(0)+inserted as u64});committed=project(&old,Some(&event),&json!({"channel_state":channel_state}));save_job(db,&id,"scrape",&committed)};
                if messages.is_empty(){let tx=store.connection_mut().transaction()?;if history{tx.execute("UPDATE scrape_cursors SET history_complete=1 WHERE channel_id=?",[cid])?;}else if !more{tx.execute("UPDATE scrape_cursors SET pending_after_message_id=NULL,pending_before_message_id=NULL WHERE channel_id=?",[cid])?;}checkpoint(&tx,0)?;tx.commit()?;}
                else {let pending=(!history&&more).then(||(old["channel_state"]["anchor"].as_str(),next["before"].as_str()));let context=PageContext {channel_id:cid,channel_name:channel["name"].as_str().unwrap_or(""),guild_id:channel["guild_id"].as_str(),guild_name:channel["guild_name"].as_str().unwrap_or(""),history_page:history,pending,clear_pending:!history&&!more,history_complete:history&&!more};store.save_page_with_hook(&messages,&context,checkpoint)?;}
                Ok(committed)
            }).await?;
            *state=saved;*job.notice.lock().unwrap()=None;let _=job.events.send(state["last_event"].clone());Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
        }).await?
    }
    async fn profiles(&self, job: Arc<Job>, channel: &Value, token: &str) -> Result<()> {
        loop {
            self.checkpoint(job.clone()).await?;
            let state = job.state.lock().await.clone();
            if state["profile_auth_failed"] == true {
                return Ok(());
            }
            let Some(uid) = state["channel_state"]["pending_profiles"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(Value::as_str)
            else {
                return Ok(());
            };
            let result = self
                .0
                .profiles
                .collect_missing(
                    uid,
                    token,
                    self.options(job.clone()),
                    {let progress_job=job.clone();let cid=channel["id"].clone();Some(Arc::new(move|info:crate::profiles::RetryInfo|{Self::report(&progress_job,json!({"type":"profile_retry","channel_id":cid,"user_id":info.user_id,"reason":info.reason,"attempt":info.attempt,"wait_seconds":info.wait_seconds}));}))},
                )
                .await;
            let saved = match result {
                Ok(saved) => saved,
                Err(error) => {
                    if job.control.is_stopped() {
                        return Err(error);
                    }
                    let status = error
                        .downcast_ref::<crate::profiles::ProfileError>()
                        .map(|e| e.status)
                        .or_else(|| error.downcast_ref::<HttpError>().map(|e| e.status))
                        .unwrap_or(500);
                    if status == 401 {
                        self.transition(
                            job.clone(),
                            Some(json!({"type":"profile_warning","message":"Profile HTTP 401"})),
                            json!({"profile_auth_failed":true}),
                        )
                        .await?;
                        return Ok(());
                    }
                    Self::report(
                        &job,
                        json!({"type":"profile_warning","message":format!("Profile HTTP {status}")}),
                    );
                    false
                }
            };
            let mut next = state["channel_state"].clone();
            next["profiles"] = json!(next["profiles"].as_u64().unwrap_or(0) + u64::from(saved));
            next["pending_profiles"].as_array_mut().unwrap().remove(0);
            self.transition(job.clone(),Some(json!({"type":"profile_progress","channel_id":channel["id"],"user_id":uid,"saved":next["profiles"]})),json!({"channel_state":next})).await?;
        }
    }
    async fn channel(&self, job: Arc<Job>, channel: &Value, token: &str) -> Result<()> {
        self.channel_state(job.clone(), channel).await?;
        loop {
            if job.state.lock().await["harvest_profiles"] == true {
                self.profiles(job.clone(), channel, token).await?;
            }
            self.checkpoint(job.clone()).await?;
            let state = job.state.lock().await.clone();
            let cs = &state["channel_state"];
            let limit = state["limit"].as_u64().unwrap_or(0);
            let count = cs["messages"].as_u64().unwrap_or(0);
            let fetch = if limit > 0 {
                limit.saturating_sub(count).min(100)
            } else {
                100
            };
            if fetch == 0 || cs["stage"] == "done" {
                return Ok(());
            }
            let mut params = vec![("limit".into(), fetch.to_string())];
            if let Some(before) = cs["before"].as_str() {
                params.push(("before".into(), before.into()));
            } else if cs["stage"] == "newer" {
                params.push(("after".into(), cs["newest"].as_str().unwrap_or("0").into()));
            }
            let messages = self
                .fetch(job.clone(), channel["id"].as_str().unwrap(), params, token)
                .await?;
            let history = cs["stage"] == "history";
            let anchor = cs["anchor"]
                .as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let mut fresh = vec![];
            for message in &messages {
                let id = message["id"]
                    .as_str()
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or("Invalid message ID in Discord response")?;
                if history || id > anchor {
                    fresh.push(message.clone());
                }
            }
            let more = !fresh.is_empty()
                && messages.len() == fetch as usize
                && fresh.len() == messages.len();
            let mut next = cs.clone();
            let mut users = vec![];
            let mut seen = HashSet::new();
            if state["harvest_profiles"] == true {
                for m in &fresh {
                    if m.get("webhook_id").is_none_or(|v| v.is_null() || v == "")
                        && let Some(uid) = m["author"]["id"].as_str()
                        && seen.insert(uid.to_owned())
                    {
                        users.push(uid.to_owned());
                    }
                }
            }
            next["pending_profiles"] = json!(users);
            let before = fresh
                .iter()
                .filter_map(|m| m["id"].as_str()?.parse::<i64>().ok())
                .min()
                .map(|n| n.to_string());
            if history {
                if let Some(before) = before {
                    next["before"] = json!(before);
                }
                next["stage"] = json!(if more { "history" } else { "done" });
            } else if more {
                next["before"] = json!(before);
            } else if cs["resuming_pending"] == true {
                next["anchor"] = cs["newest"].clone();
                next["before"] = Value::Null;
                next["resuming_pending"] = json!(false);
            } else {
                next["stage"] = json!(if cs["history_complete"] == true {
                    "done"
                } else {
                    "history"
                });
                next["before"] = cs["oldest"].clone();
            }
            // Fetched data is committed even if pause was requested in flight.
            let mut attempts = 0u32;
            loop {
                match self
                    .page(
                        job.clone(),
                        channel.clone(),
                        fresh.clone(),
                        next.clone(),
                        history,
                        more,
                    )
                    .await
                {
                    Ok(()) => break,
                    Err(e)
                        if e.downcast_ref::<rusqlite::Error>().is_some_and(|e| {
                            matches!(
                                e.sqlite_error_code(),
                                Some(
                                    rusqlite::ErrorCode::DatabaseBusy
                                        | rusqlite::ErrorCode::DatabaseLocked
                                )
                            )
                        }) =>
                    {
                        attempts += 1;
                        let delay = 2f64.powi(attempts.min(10) as i32).min(60.0);
                        Self::report(
                            &job,
                            json!({"type":"retry_wait","channel_id":channel["id"],"stage":"save","attempt":attempts,"wait_seconds":delay,"reason":"database busy"}),
                        );
                        self.wait(job.clone(), delay, false).await?;
                    }
                    Err(e) => return Err(e),
                }
            }
            let media = self.0.media.lock().unwrap().clone();
            if !fresh.is_empty()
                && let Some(media) = media
            {
                let _ = media
                    .archive_scrape_media(&fresh, job.control.clone())
                    .await;
            }
        }
    }
    async fn run(&self, job: Arc<Job>, token: String) {
        let accounts = self.0.accounts.lock().unwrap().clone();
        let token = if let Some(accounts) = accounts {
            match accounts.selected().await {
                Ok((id, token)) if job.state.lock().await["credential_id"] == id => token,
                _ => {
                    job.control.pause();
                    if let Err(error)=self.transition(job.clone(),Some(json!({"type":"paused","message":"Select this job's saved account before resuming."})),json!({"paused":true,"pause_requested":true})).await { eprintln!("Could not pause scrape {}: {error}",job.id); }
                    return;
                }
            }
        } else {
            token
        };
        let result:Result<Value>=async {
            let channels=job.state.lock().await["channels"].as_array().unwrap().clone();let first=job.state.lock().await["next_index"].as_u64().unwrap_or(0)as usize;
            for(index,channel)in channels.iter().enumerate().skip(first){self.checkpoint(job.clone()).await?;let messages=job.state.lock().await["channel_state"]["messages"].as_u64().unwrap_or(0);self.transition(job.clone(),Some(json!({"type":"channel_start","channel":channel["name"],"guild":channel["guild_name"],"index":index+1,"total":channels.len(),"messages":messages})),json!({})).await?;
                if let Err(error)=self.channel(job.clone(),channel,&token).await {if job.control.is_stopped(){return Err(error);}let failed=job.state.lock().await["failed_channels"].as_u64().unwrap_or(0)+1;let message=if error.downcast_ref::<HttpError>().is_some(){error.to_string()}else{"Collection failed; see terminal".into()};eprintln!("scrape {} channel {}: {error}",job.id,channel["id"]);self.transition(job.clone(),Some(json!({"type":"channel_error","channel":channel["name"],"message":message})),json!({"failed_channels":failed})).await?;}
                let messages=job.state.lock().await["channel_state"]["messages"].as_u64().unwrap_or(0);self.transition(job.clone(),Some(json!({"type":"channel_complete","channel":channel["name"],"messages":messages})),json!({"next_index":index+1,"channel_state":null})).await?;
            }
            let state=job.state.lock().await;Ok(json!({"type":"complete","total_messages":state["total_messages"],"channels":channels.len(),"failed_channels":state["failed_channels"]}))
        }.await;
        let changes;
        let event;
        if job.suspending.load(Ordering::Acquire) {
            event = json!({"type":"paused"});
            changes = json!({"paused":true,"pause_requested":true});
        } else if job.control.is_stopped() {
            event = json!({"type":"cancelled","total_messages":job.state.lock().await["total_messages"]});
            changes =
                json!({"running":false,"cancelled":true,"paused":false,"pause_requested":false});
        } else {
            event=result.unwrap_or_else(|e|{eprintln!("scrape {}: {e}",job.id);json!({"type":"error","message":"Scrape stopped; see terminal for details. Saved cursors remain available."})});
            changes =
                json!({"running":false,"cancelled":false,"paused":false,"pause_requested":false});
        }
        if let Err(e) = self.transition(job.clone(), Some(event), changes).await {
            eprintln!("Could not checkpoint scrape {}: {e}", job.id);
            return;
        }
        if !job.suspending.load(Ordering::Acquire) {
            let owner = self.clone();
            let id = job.id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                owner.0.jobs.lock().await.retain(|job| job.id != id);
            });
        }
    }
}
