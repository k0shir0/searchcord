//! Restart-safe live observations. Pages and monitor cursors share one transaction.
use crate::{
    PageContext, Result,
    discord::{Discord, Request},
    discovery::http_error,
    pacing::{Clock, Control, Options},
    scrapes::{save_job, validate_channels},
    writer::Writer,
};
use reqwest::Method;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
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

pub const POLL_SECONDS: f64 = 3.0;
const SUBSCRIBER_CAPACITY: usize = 500;
struct Monitor {
    id: String,
    state: Mutex<Value>,
    control: Control,
    suspending: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
struct Inner<C: Clock> {
    writer: Writer,
    discord: Discord<C>,
    clock: Arc<C>,
    // Preserve the insertion order exposed by Python's channel registry.
    jobs: Mutex<Vec<Arc<Monitor>>>,
    commands: Mutex<()>,
    events: broadcast::Sender<Value>,
    closing: AtomicBool,
    accounts: std::sync::Mutex<Option<crate::accounts::Accounts>>,
    media: std::sync::Mutex<Option<crate::media_collection::MediaCollector>>,
}
pub struct Monitors<C: Clock>(Arc<Inner<C>>);
impl<C: Clock> Clone for Monitors<C> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
fn id(value: &Value) -> Result<i64> {
    value
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| "Invalid message ID in monitor response".into())
}
fn channel(state: &Value, cid: &str) -> Value {
    json!({"id":cid,"name":state["channel_name"],"guild_id":state["guild_id"],"guild_name":state["guild_name"]})
}
fn event(kind: &str, cid: &str, channel: &Value) -> Value {
    json!({"type":kind,"channel_id":cid,"channel":channel["name"],"guild":channel["guild_name"],"guild_id":channel["guild_id"]})
}
fn avatar(uid: &str, hash: &Value) -> Option<String> {
    if !searchcord_archive::media::profile_images_enabled() {
        return None;
    }
    let hash = hash.as_str()?;
    let plain = hash.strip_prefix("a_").unwrap_or(hash);
    if plain.is_empty() || !plain.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!(
        "https://cdn.discordapp.com/avatars/{uid}/{hash}.{}?size=64",
        if hash.starts_with("a_") { "gif" } else { "png" }
    ))
}
fn message_event(message: &Value, cid: &str, channel: &Value) -> Value {
    let user = &message["author"];
    let name = user["global_name"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(Value::from)
        .unwrap_or_else(|| user["username"].clone());
    let attachments = crate::storage::image_urls(&message["attachments"])
        .map(|urls| {
            urls.split('\n')
                .enumerate()
                .map(|(index, _)| {
                    format!(
                        "/api/images/{}/{index}",
                        message["id"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"type":"message","id":message["id"],"channel_id":cid,"channel":channel["name"],"guild_id":channel["guild_id"],"guild":channel["guild_name"],"author_id":user["id"],"author":name,"avatar_url":avatar(user["id"].as_str().unwrap_or(""),&user["avatar"]),"content":message.get("content").cloned().unwrap_or_else(||json!("")),"timestamp":message["timestamp"],"attachments":attachments})
}
impl<C: Clock> Monitors<C> {
    pub async fn open(
        path: PathBuf,
        writer: Writer,
        discord: Discord<C>,
        clock: Arc<C>,
    ) -> Result<Self> {
        let recovered = tokio::task::spawn_blocking(move || -> Result<Vec<Arc<Monitor>>> {
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut statement = db.prepare(
                "SELECT id,state_json FROM collection_jobs WHERE kind='monitor' ORDER BY rowid",
            )?;
            let mut jobs = vec![];
            for row in
                statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            {
                let (cid, raw) = row?;
                let mut state: Value = serde_json::from_str(&raw)?;
                if state["running"] != true {
                    continue;
                }
                validate_channels(&json!([channel(&state, &cid)]))?;
                if !state["credential_id"].is_string() {
                    return Err("Invalid saved monitor account binding".into());
                }
                for key in [
                    "last_id",
                    "pending_before",
                    "pending_anchor",
                    "pending_newest",
                ] {
                    if !state[key].is_null() {
                        id(&state[key])?;
                    }
                }
                if !state["pending_before"].is_null()
                    && (state["pending_anchor"].is_null() || state["pending_newest"].is_null())
                {
                    return Err("Invalid saved monitor gap cursor".into());
                }
                state["paused"] = json!(true);
                jobs.push(Self::job(cid, state));
            }
            Ok(jobs)
        })
        .await??;
        Ok(Self(Arc::new(Inner {
            writer,
            discord,
            clock,
            jobs: Mutex::new(recovered),
            commands: Mutex::new(()),
            events: broadcast::channel(SUBSCRIBER_CAPACITY).0,
            closing: AtomicBool::new(false),
            accounts: std::sync::Mutex::new(None),
            media: std::sync::Mutex::new(None),
        })))
    }
    pub fn with_accounts(self, accounts: crate::accounts::Accounts) -> Self {
        *self.0.accounts.lock().unwrap() = Some(accounts);
        self
    }
    pub fn with_media(self, media: crate::media_collection::MediaCollector) -> Self {
        *self.0.media.lock().unwrap() = Some(media);
        self
    }
    fn job(cid: String, state: Value) -> Arc<Monitor> {
        Arc::new(Monitor {
            id: cid,
            state: Mutex::new(state),
            control: Control::default(),
            suspending: AtomicBool::new(false),
            task: Mutex::new(None),
        })
    }
    pub async fn status(&self) -> Value {
        let jobs = self.0.jobs.lock().await.clone();
        let mut channels = vec![];
        for job in jobs {
            let state = job.state.lock().await;
            channels.push(json!({"channel_id":job.id,"channel_name":state["channel_name"],"guild_name":state["guild_name"],"guild_id":state["guild_id"],"paused":state["paused"]==true}));
        }
        json!({"channels":channels})
    }
    pub async fn subscribe(&self) -> (Vec<Value>, broadcast::Receiver<Value>) {
        let receiver = self.0.events.subscribe();
        let jobs = self.0.jobs.lock().await.clone();
        let mut initial = vec![];
        for job in jobs.into_iter().take(SUBSCRIBER_CAPACITY) {
            let state = job.state.lock().await;
            initial.push(event(
                if state["paused"] == true {
                    "monitor_paused"
                } else {
                    "monitor_start"
                },
                &job.id,
                &channel(&state, &job.id),
            ));
        }
        (initial, receiver)
    }
    async fn persist(&self, job: &Monitor, next: Value) -> Result<()> {
        let mut state = job.state.lock().await;
        let saved = next.clone();
        let cid = job.id.clone();
        self.0
            .writer
            .write(move |store| {
                save_job(store.connection(), &cid, "monitor", &saved)?;
                Ok(Value::Null)
            })
            .await?;
        *state = next;
        Ok(())
    }
    pub async fn start(
        &self,
        channels: Value,
        credential_id: String,
        token: String,
    ) -> Result<Value> {
        let owner = self.clone();
        tokio::spawn(async move { owner.start_inner(channels, credential_id, token).await }).await?
    }
    async fn start_inner(
        &self,
        channels: Value,
        credential_id: String,
        token: String,
    ) -> Result<Value> {
        let channels = validate_channels(&channels)?;
        let _commands = self.0.commands.lock().await;
        if self.0.closing.load(Ordering::Acquire) {
            return Err(http_error(409, "Collector is shutting down"));
        }
        for channel in channels {
            let cid = channel["id"].as_str().unwrap();
            let existing = self
                .0
                .jobs
                .lock()
                .await
                .iter()
                .find(|job| job.id == cid)
                .cloned();
            let state = if let Some(job) = &existing {
                let state = job.state.lock().await;
                if state["paused"] != true {
                    continue;
                }
                if state["credential_id"] != credential_id {
                    return Err(http_error(
                        409,
                        "Select the account used to start this monitor before resuming",
                    ));
                }
                state.clone()
            } else {
                json!({"channel_name":channel["name"],"guild_id":channel["guild_id"],"guild_name":channel["guild_name"],"credential_id":credential_id,"last_id":null,"pending_before":null,"pending_anchor":null,"pending_newest":null})
            };
            let mut next = state;
            if let Some(previous) = existing
                && let Some(task) = previous.task.lock().await.take()
            {
                task.await?;
            }
            next["running"] = json!(true);
            next["paused"] = json!(false);
            // Each resumed run receives a fresh, terminal-stop-safe control.
            let job = Self::job(cid.to_owned(), next.clone());
            self.persist(&job, next).await?;
            {
                let mut jobs = self.0.jobs.lock().await;
                if let Some(index) = jobs.iter().position(|old| old.id == cid) {
                    jobs[index] = job.clone();
                } else {
                    jobs.push(job.clone());
                }
            }
            let owner = self.clone();
            let running = job.clone();
            let token = token.clone();
            *job.task.lock().await = Some(tokio::spawn(async move {
                owner.run(running, channel, token).await;
            }));
        }
        let active: Vec<_> = self
            .0
            .jobs
            .lock()
            .await
            .iter()
            .map(|job| job.id.clone())
            .collect();
        Ok(json!({"active":active}))
    }
    pub async fn stop(&self, channel_ids: Vec<String>) -> Result<Value> {
        let owner = self.clone();
        tokio::spawn(async move { owner.stop_inner(channel_ids, false).await }).await?
    }
    pub async fn suspend(&self) -> Result<()> {
        let owner = self.clone();
        tokio::spawn(async move { owner.stop_inner(vec![], true).await.map(|_| ()) }).await?
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.suspend().await
    }
    async fn stop_inner(&self, channel_ids: Vec<String>, suspend: bool) -> Result<Value> {
        let _commands = self.0.commands.lock().await;
        if suspend {
            self.0.closing.store(true, Ordering::Release);
        }
        let jobs = self.0.jobs.lock().await.clone();
        let ids = if channel_ids.is_empty() {
            jobs.iter().map(|job| job.id.clone()).collect()
        } else {
            channel_ids
        };
        for job in &jobs {
            if ids.contains(&job.id) {
                job.suspending.store(suspend, Ordering::Release);
                job.control.stop();
            }
        }
        for job in jobs {
            if !ids.contains(&job.id) {
                continue;
            }
            if let Some(task) = job.task.lock().await.take() {
                task.await?;
            }
            // Recovered monitors have no task to finish their durable transition.
            if suspend {
                let mut state = job.state.lock().await.clone();
                state["paused"] = json!(true);
                self.persist(&job, state).await?;
            } else {
                self.remove(&job).await?;
            }
        }
        self.0.writer.barrier().await?;
        Ok(json!({"stopped":ids}))
    }
    async fn remove(&self, job: &Arc<Monitor>) -> Result<()> {
        let cid = job.id.clone();
        self.0
            .writer
            .write(move |store| {
                store.connection().execute(
                    "DELETE FROM collection_jobs WHERE id=? AND kind='monitor'",
                    [cid],
                )?;
                Ok(Value::Null)
            })
            .await?;
        self.0
            .jobs
            .lock()
            .await
            .retain(|old| !Arc::ptr_eq(old, job));
        Ok(())
    }
    async fn request(
        &self,
        job: &Monitor,
        path: String,
        params: Vec<(String, String)>,
        token: &str,
    ) -> Result<crate::pacing::Response> {
        let mut request = Request::new(Method::GET, path, token);
        request.params = params;
        self.0
            .discord
            .request(
                request,
                Options {
                    control: job.control.clone(),
                    ..Options::default()
                },
            )
            .await
    }
    async fn page(
        &self,
        job: &Monitor,
        channel: &Value,
        messages: Vec<Value>,
        next: Value,
    ) -> Result<()> {
        let mut state = job.state.lock().await;
        let cid = job.id.clone();
        let channel = channel.clone();
        let saved = next.clone();
        self.0
            .writer
            .write(move |store| {
                let context = PageContext {
                    channel_id: &cid,
                    channel_name: channel["name"].as_str().unwrap_or(""),
                    guild_id: channel["guild_id"].as_str(),
                    guild_name: channel["guild_name"].as_str().unwrap_or(""),
                    ..PageContext::default()
                };
                store.save_page_with_hook(&messages, &context, |db, _| {
                    save_job(db, &cid, "monitor", &saved)
                })?;
                Ok(Value::Null)
            })
            .await?;
        *state = next;
        Ok(())
    }
    async fn absent(
        &self,
        cid: &str,
        range: Option<(i64, i64, HashSet<String>)>,
    ) -> Result<Option<String>> {
        let cid = cid.to_owned();
        let value = self.0.writer.write(move |store| {
            let db = store.connection();
            let absent = if let Some((low,high,current)) = range {
                db.prepare("SELECT m.id FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.channel_id=? AND m.id>=? AND m.id<=? AND p.deleted_at IS NULL ORDER BY m.id DESC LIMIT 51")?.query_map(params![cid,low,high],|r|r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().map(|id|id.to_string()).find(|id|!current.contains(id))
            } else {
                db.query_row("SELECT m.id FROM messages m LEFT JOIN message_payloads p ON p.message_id=m.id WHERE m.channel_id=? AND p.deleted_at IS NULL ORDER BY m.id DESC LIMIT 1",[cid],|r|r.get::<_,i64>(0)).optional()?.map(|id|id.to_string())
            };
            Ok(json!(absent))
        }).await?;
        Ok(value.as_str().map(str::to_owned))
    }
    async fn observe_absent(
        &self,
        job: &Monitor,
        channel: &Value,
        absent: String,
        token: &str,
        save_observed: bool,
    ) -> Result<()> {
        let response = self
            .request(
                job,
                format!("/channels/{}/messages/{absent}", job.id),
                vec![],
                token,
            )
            .await?;
        if response.status == 404
            && response
                .json()
                .ok()
                .is_some_and(|v| v["code"].as_i64() == Some(10008))
        {
            let id = absent.parse::<i64>()?;
            self.0
                .writer
                .write(move |store| {
                    store.mark_deleted(&[id], "upstream_not_found")?;
                    Ok(Value::Null)
                })
                .await?;
        } else if save_observed && response.status == 200 {
            let saved = job.state.lock().await.clone();
            self.page(job, channel, vec![response.json()?], saved)
                .await?;
        }
        Ok(())
    }
    async fn poll(&self, job: &Monitor, channel: &Value, token: &str) -> Result<()> {
        let state = job.state.lock().await.clone();
        let draining = state["pending_before"].is_string();
        let mut params = vec![("limit".into(), "50".into())];
        if let Some(before) = state["pending_before"].as_str() {
            params.push(("before".into(), before.into()));
        }
        let response = self
            .request(job, format!("/channels/{}/messages", job.id), params, token)
            .await?;
        if response.status != 200 {
            return Ok(());
        }
        let payload = response.json()?;
        let mut messages = payload
            .as_array()
            .cloned()
            .ok_or("Invalid monitor message list")?;
        if messages.is_empty() {
            if draining {
                let mut next = state;
                next["last_id"] = next["pending_newest"].clone();
                for key in ["pending_before", "pending_anchor", "pending_newest"] {
                    next[key] = Value::Null;
                }
                self.persist(job, next).await?;
            }
            if let Some(absent) = self.absent(&job.id, None).await? {
                self.observe_absent(job, channel, absent, token, false)
                    .await?;
            }
            return Ok(());
        }
        if messages.len() > 50 {
            return Err("Oversized monitor message list".into());
        }
        for message in &messages {
            id(&message["id"])?;
        }
        messages.sort_by_key(|m| id(&m["id"]).unwrap());
        let anchor = if draining {
            &state["pending_anchor"]
        } else {
            &state["last_id"]
        };
        let anchor = if anchor.is_null() {
            None
        } else {
            Some(id(anchor)?)
        };
        let fresh: Vec<_> = messages
            .iter()
            .filter(|m| anchor.is_none_or(|anchor| id(&m["id"]).unwrap() > anchor))
            .cloned()
            .collect();
        let newest = if draining {
            state["pending_newest"].clone()
        } else {
            messages.last().unwrap()["id"].clone()
        };
        let mut next = state.clone();
        for key in ["pending_before", "pending_anchor", "pending_newest"] {
            next[key] = Value::Null;
        }
        if messages.len() == 50 && fresh.len() == 50 && anchor.is_some() {
            next["pending_anchor"] = json!(anchor.unwrap().to_string());
            next["pending_before"] = messages[0]["id"].clone();
            next["pending_newest"] = newest;
        } else if state["last_id"].is_null() || id(&newest)? > id(&state["last_id"])? {
            next["last_id"] = newest;
        }
        self.page(job, channel, messages.clone(), next.clone())
            .await?;
        let media = self.0.media.lock().unwrap().clone();
        if let Some(media) = media {
            let _ = media
                .archive_message_media(&messages, Some(job.control.clone()))
                .await;
        }
        for message in &fresh {
            let _ = self.0.events.send(message_event(message, &job.id, channel));
        }
        if !draining {
            let current = messages
                .iter()
                .map(|m| m["id"].as_str().unwrap().to_owned())
                .collect();
            let high = id(&messages.last().unwrap()["id"])?.max(
                next["last_id"]
                    .as_str()
                    .and_then(|id| id.parse::<i64>().ok())
                    .unwrap_or(0),
            );
            if let Some(absent) = self
                .absent(&job.id, Some((id(&messages[0]["id"])?, high, current)))
                .await?
            {
                self.observe_absent(job, channel, absent, token, true)
                    .await?;
            }
        }
        Ok(())
    }
    async fn run(&self, job: Arc<Monitor>, channel: Value, token: String) {
        let accounts = self.0.accounts.lock().unwrap().clone();
        let token = if let Some(accounts) = accounts {
            match accounts.selected().await {
                Ok((id, token)) if job.state.lock().await["credential_id"] == id => Some(token),
                _ => None,
            }
        } else {
            Some(token)
        };
        let anchored: Result<()> = async {
            let token = token.as_deref().ok_or("Monitor account is unavailable")?;
            let state = job.state.lock().await.clone();
            if state["last_id"].is_null() {
                let response = self
                    .request(
                        &job,
                        format!("/channels/{}/messages", job.id),
                        vec![("limit".into(), "1".into())],
                        token,
                    )
                    .await?;
                if response.status != 200 {
                    return Err("Could not establish the monitor cursor".into());
                }
                let payload = response.json()?;
                let messages = payload
                    .as_array()
                    .ok_or("Invalid monitor anchor response")?;
                let mut next = state;
                if let Some(message) = messages.first() {
                    id(&message["id"])?;
                    next["last_id"] = message["id"].clone();
                }
                self.persist(&job, next).await?;
            }
            Ok(())
        }
        .await;
        if anchored.is_err() && !job.control.is_stopped() {
            job.suspending.store(true, Ordering::Release);
        }
        if anchored.is_ok()
            && !job.control.is_stopped()
            && let Some(token) = token.as_deref()
        {
            let _ = self
                .0
                .events
                .send(event("monitor_start", &job.id, &channel));
            loop {
                tokio::select! {
                    _=job.control.stopped()=>break,
                    _=self.0.clock.sleep(POLL_SECONDS)=>{},
                }
                if job.control.is_stopped() {
                    break;
                }
                // Transient HTTP, JSON and SQLite errors keep the monitor alive.
                let _ = self.poll(&job, &channel, token).await;
            }
        }
        let mut paused = job.suspending.load(Ordering::Acquire);
        let result = if paused {
            let mut state = job.state.lock().await.clone();
            state["paused"] = json!(true);
            self.persist(&job, state).await
        } else {
            self.remove(&job).await
        };
        if let Err(error) = result {
            // The worker has exited even if storage could not record its pause.
            // Keep it resumable without advancing the last committed cursor.
            job.state.lock().await["paused"] = json!(true);
            job.suspending.store(true, Ordering::Release);
            paused = true;
            eprintln!("Could not checkpoint monitor {}: {error}", job.id);
        }
        let _=self.0.events.send(json!({"type":if paused {"monitor_paused"} else {"monitor_stop"},"channel_id":job.id,"channel":channel["name"]}));
    }
}
