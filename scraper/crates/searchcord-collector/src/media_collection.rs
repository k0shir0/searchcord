//! Credential-free bounded collection beside the archive. Identity stays in SQLite.
use crate::{Result, pacing::Control};
use reqwest::{blocking::Client, redirect::Policy};
use searchcord_archive::media;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

pub const MAX_BYTES: u64 = 25 * 1024 * 1024;
pub const FILE_SECONDS: u64 = 15;
pub const BATCH_SECONDS: u64 = 30;
pub const PARALLEL_DOWNLOADS: usize = 4;

type ObjectLocks = HashMap<(PathBuf, String), Weak<Mutex<()>>>;
fn object_lock(path: &Path, key: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<ObjectLocks>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    let identity = (media::directory(path), key.to_owned());
    if let Some(lock) = locks.get(&identity).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(identity, Arc::downgrade(&lock));
    lock
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub max_bytes: u64,
    pub file_time: Duration,
    pub batch_time: Duration,
    pub parallel_downloads: usize,
    /// Deterministic disk-space fixture. Production always queries the volume.
    pub disk_free_bytes: Option<u64>,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            file_time: Duration::from_secs(FILE_SECONDS),
            batch_time: Duration::from_secs(BATCH_SECONDS),
            parallel_downloads: PARALLEL_DOWNLOADS,
            disk_free_bytes: None,
        }
    }
}

#[derive(Clone)]
pub struct MediaCollector {
    path: PathBuf,
    fixture: Option<url::Url>,
    limits: Limits,
}

fn stopped() -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::new(io::ErrorKind::Interrupted, "Media collection stopped").into()
}
fn cancelled(control: &Control, stop_on_pause: bool) -> bool {
    control.is_stopped() || (stop_on_pause && control.is_paused())
}
pub(crate) fn symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            metadata.file_attributes() & 0x400 != 0
        }
        #[cfg(not(windows))]
        {
            metadata.file_type().is_symlink()
        }
    })
}
fn safe_directory(directory: &Path) -> io::Result<()> {
    if symlink(directory) {
        return Err(io::Error::other(
            "Media directory cannot be a symbolic link",
        ));
    }
    fs::create_dir_all(directory)
}
fn record(directory: &Path, key: &str, data: &Value) -> io::Result<()> {
    safe_directory(directory)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".searchcord-media-")
        .suffix(".tmp")
        .tempfile_in(directory)?;
    serde_json::to_writer(&mut temporary, data)?;
    temporary.flush()?;
    safe_directory(directory)?;
    temporary
        .persist(directory.join(format!("{key}.json")))
        .map_err(|e| e.error)?;
    Ok(())
}

#[cfg(windows)]
fn free_space(directory: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<_> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0;
    // Both pointers are valid for the synchronous call; no handles are retained.
    let success = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut free,
        )
    };
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(free)
    }
}
#[cfg(unix)]
fn free_space(directory: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(directory.as_os_str().as_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs initializes the output on success and does not retain its pointers.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let stats = unsafe { stats.assume_init() };
    let bytes = (stats.f_bavail as u128).saturating_mul(stats.f_frsize as u128);
    Ok(bytes.min(u128::from(u64::MAX)) as u64)
}

fn client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(FILE_SECONDS))
        .build()?)
}

#[derive(Debug)]
struct BodyError(io::Error);
impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for BodyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
fn timeout(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_timeout)
            || error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::TimedOut)
        {
            return true;
        }
        current = error.source();
    }
    false
}

impl MediaCollector {
    fn mode(&self) -> Result<String> {
        let mode = std::env::var("SEARCHCORD_MEDIA_MODE")
            .unwrap_or_else(|_| "original".into())
            .to_ascii_lowercase();
        if !["original", "compact", "none"].contains(&mode.as_str()) {
            return Err("SEARCHCORD_MEDIA_MODE must be original, compact or none".into());
        }
        let profiles = std::env::var("SEARCHCORD_PROFILE_IMAGES")
            .unwrap_or_else(|_| "original".into())
            .to_ascii_lowercase();
        if !["original", "shapes"].contains(&profiles.as_str()) {
            return Err("SEARCHCORD_PROFILE_IMAGES must be original or shapes".into());
        }
        Ok(mode)
    }
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            path: crate::credentials::resolve(path.as_ref())?,
            fixture: None,
            limits: Limits::default(),
        })
    }
    /// Rewrite approved CDN URLs to an explicit numeric HTTP loopback fixture.
    /// Keys/manifests always retain the production CDN identity.
    pub fn for_loopback(path: impl AsRef<Path>, origin: &str, limits: Limits) -> Result<Self> {
        let origin = url::Url::parse(origin)?;
        if origin.scheme() != "http"
            || !origin.username().is_empty()
            || origin.password().is_some()
            || !origin.host_str().is_some_and(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            })
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || limits.parallel_downloads == 0
            || limits.parallel_downloads > PARALLEL_DOWNLOADS
            || limits.max_bytes > MAX_BYTES
        {
            return Err(
                "Media fixture origin must be numeric HTTP loopback with bounded limits".into(),
            );
        }
        Ok(Self {
            path: crate::credentials::resolve(path.as_ref())?,
            fixture: Some(origin),
            limits,
        })
    }
    pub async fn cache_media(&self, url: &str, control: Option<Control>) -> Result<Value> {
        let this = self.clone();
        let url = url.to_owned();
        let control = control.unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            let client = client()?;
            this.cache_sync(&url, &client, &control, None, false)
        })
        .await?
    }
    pub async fn archive_message_media(
        &self,
        messages: &[Value],
        control: Option<Control>,
    ) -> Result<Vec<Value>> {
        self.archive_media(messages, control, false).await
    }
    /// Scrape pages stop caching on pause; profiles and monitors remain stop-only.
    pub async fn archive_scrape_media(
        &self,
        messages: &[Value],
        control: Control,
    ) -> Result<Vec<Value>> {
        self.archive_media(messages, Some(control), true).await
    }
    async fn archive_media(
        &self,
        messages: &[Value],
        control: Option<Control>,
        stop_on_pause: bool,
    ) -> Result<Vec<Value>> {
        if self.mode()? == "none" {
            return Ok(vec![]);
        }
        let urls: Vec<_> = message_media_urls(messages)
            .into_iter()
            .filter(|url| media::media_kind(url) == "attachment" || media::profile_images_enabled())
            .collect();
        if urls.is_empty() {
            return Ok(vec![]);
        }
        let pending = Arc::new(Mutex::new(VecDeque::from(urls)));
        let results = Arc::new(Mutex::new(vec![]));
        let deadline = Instant::now() + self.limits.batch_time;
        let control = control.unwrap_or_default();
        let mut workers = tokio::task::JoinSet::new();
        for _ in 0..self.limits.parallel_downloads {
            let this = self.clone();
            let pending = pending.clone();
            let results = results.clone();
            let control = control.clone();
            workers.spawn_blocking(move || -> Result<()> {
                // A client's internal runtime is built and dropped on this worker.
                let client = client()?;
                loop {
                    if cancelled(&control, stop_on_pause) {
                        return Ok(());
                    }
                    let Some(url) = pending.lock().unwrap().pop_front() else {
                        return Ok(());
                    };
                    let result =
                        this.cache_sync(&url, &client, &control, Some(deadline), stop_on_pause)?;
                    results.lock().unwrap().push(result);
                }
            });
        }
        // Drain every accepted worker, including after cancellation or an error.
        let mut failure = None;
        while let Some(result) = workers.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error.into());
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(std::mem::take(&mut *results.lock().unwrap()))
    }
    fn request_url(&self, url: &str) -> Result<String> {
        let Some(origin) = &self.fixture else {
            return Ok(url.to_owned());
        };
        let original = url::Url::parse(url)?;
        let mut target = origin.clone();
        target.set_path(original.path());
        target.set_query(original.query());
        Ok(target.to_string())
    }
    fn cache_sync(
        &self,
        url: &str,
        client: &Client,
        control: &Control,
        deadline: Option<Instant>,
        stop_on_pause: bool,
    ) -> Result<Value> {
        let Some(key) = media::media_key(url) else {
            return Ok(json!({"status":"unavailable","reason":"unsupported_host"}));
        };
        let mode = self.mode()?;
        if mode == "none"
            || (media::media_kind(url) != "attachment" && !media::profile_images_enabled())
        {
            return Ok(json!({"status":"unavailable","reason":"disabled_by_policy"}));
        }
        // Distinct objects retain their parallelism. For the same archive/key,
        // include cache checks, both publications and original retirement.
        let object = object_lock(&self.path, &key);
        let _publication = object.lock().unwrap();
        let directory = media::directory(&self.path);
        let existing = if symlink(&directory) {
            json!({"status":"unavailable","reason":"unsafe_path"})
        } else {
            media::status(&self.path, url, true)
        };
        if existing["status"] == "saved" {
            return Ok(existing);
        }
        let mut data = json!({"source_url":media::canonical_url(url),"status":"unavailable","reason":null,"observed_at":crate::storage::now()});
        if cancelled(control, stop_on_pause) {
            return Err(stopped());
        }
        let attempt = (|| -> Result<()> {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                data["reason"] = json!("batch_time_limit");
                return Ok(());
            }
            safe_directory(&directory)?;
            let free = self
                .limits
                .disk_free_bytes
                .map_or_else(|| free_space(&directory), Ok)?;
            if free < self.limits.max_bytes {
                data["reason"] = json!("disk_space");
                return Ok(());
            }
            let mut seconds = self.limits.file_time;
            if let Some(deadline) = deadline {
                let Some(remaining) = deadline
                    .checked_duration_since(Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                else {
                    data["reason"] = json!("batch_time_limit");
                    return Ok(());
                };
                seconds = seconds.min(remaining);
            }
            seconds = seconds.max(Duration::from_millis(1));
            let file_deadline = Instant::now() + seconds;
            let response = client.get(self.request_url(url)?).timeout(seconds).send()?;
            let status = response.status().as_u16();
            if status != 200 {
                data["reason"] = json!(format!("http_{status}"));
                return Ok(());
            }
            let length = response
                .headers()
                .get("content-length")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| s.trim().parse::<i128>().ok())
                .unwrap_or(0);
            if length > self.limits.max_bytes as i128 {
                data["reason"] = json!("size_limit");
                return Ok(());
            }
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|s| s.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .to_lowercase();
            let encoding = response
                .headers()
                .get("content-encoding")
                .and_then(|s| s.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let mut source: Box<dyn Read> = Box::new(response);
            for encoding in encoding.split(',').rev().map(str::trim) {
                source = match encoding.to_ascii_lowercase().as_str() {
                    "gzip" => Box::new(flate2::read::MultiGzDecoder::new(source)),
                    "deflate" => {
                        let mut buffered = io::BufReader::new(source);
                        let header = buffered.fill_buf().map_err(BodyError)?;
                        let zlib = header.len() >= 2
                            && header[0] & 15 == 8
                            && (u16::from(header[0]) * 256 + u16::from(header[1])) % 31 == 0;
                        if zlib {
                            Box::new(flate2::read::ZlibDecoder::new(buffered))
                        } else {
                            Box::new(flate2::read::DeflateDecoder::new(buffered))
                        }
                    }
                    _ => source,
                };
            }
            let mut temporary = tempfile::Builder::new()
                .prefix(".searchcord-media-")
                .suffix(".tmp")
                .tempfile_in(&directory)?;
            let mut digest = Sha256::new();
            let mut size = 0u64;
            let mut buffer = [0u8; 64 * 1024];
            loop {
                if cancelled(control, stop_on_pause) {
                    return Err(stopped());
                }
                if Instant::now() >= file_deadline {
                    data["reason"] = json!("time_limit");
                    return Ok(());
                }
                let length = source.read(&mut buffer).map_err(BodyError)?;
                if length == 0 {
                    break;
                }
                size += length as u64;
                if size > self.limits.max_bytes {
                    data["reason"] = json!("size_limit");
                    return Ok(());
                }
                temporary.write_all(&buffer[..length])?;
                digest.update(&buffer[..length]);
            }
            temporary.flush()?;
            safe_directory(&directory)?;
            temporary
                .persist(directory.join(format!("{key}.bin")))
                .map_err(|error| error.error)?;
            data["status"] = json!("saved");
            data["bytes"] = json!(size);
            data["sha256"] = json!(format!("{:x}", digest.finalize()));
            data["content_type"] = json!(content_type);
            Ok(())
        })();
        if let Err(error) = attempt {
            if cancelled(control, stop_on_pause)
                && error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::Interrupted)
            {
                return Err(error);
            }
            data["reason"] = json!(if timeout(error.as_ref()) {
                "time_limit"
            } else if error.downcast_ref::<reqwest::Error>().is_some()
                || error.downcast_ref::<BodyError>().is_some()
            {
                "network_error"
            } else {
                "storage_error"
            });
        }
        if data["status"] != "saved" && !symlink(&directory) {
            let concurrent = media::status(&self.path, url, true);
            if concurrent["status"] == "saved" {
                return Ok(concurrent);
            }
        }
        if record(&directory, &key, &data).is_err() {
            data["status"] = json!("unavailable");
            data["reason"] = json!("storage_error");
        }
        if data["status"] == "saved" && mode == "compact" {
            // This runs on the joined blocking worker, never on the HTTP loop.
            // Conversion failure leaves the verified original manifest intact.
            if let Ok(Some(compact)) = crate::media_compact::optimize(&directory, &key, &data)
                && record(&directory, &key, &compact).is_ok()
            {
                let original = directory.join(format!("{key}.bin"));
                if !symlink(&original)
                    && media::file_digest(&original).ok().as_deref() == data["sha256"].as_str()
                {
                    let _ = fs::remove_file(original);
                }
                data = compact;
            }
        }
        Ok(data)
    }
}

/// Preserve discovery order while the last signed URL for each object wins.
pub fn message_media_urls(messages: &[Value]) -> Vec<String> {
    let mut urls = vec![];
    let mut seen = HashMap::new();
    let mut add = |candidate: Option<&str>| {
        if let Some(url) = candidate
            && let Some(canonical) = media::canonical_url(url)
        {
            if let Some(&index) = seen.get(&canonical) {
                urls[index] = url.to_owned();
            } else {
                seen.insert(canonical, urls.len());
                urls.push(url.to_owned());
            }
        }
    };
    for message in messages {
        if let Some(attachments) = message["attachments"].as_array() {
            for attachment in attachments {
                add(attachment["url"].as_str());
            }
        }
        if let Some(embeds) = message["embeds"].as_array() {
            for embed in embeds {
                for kind in ["image", "thumbnail", "video"] {
                    let asset = &embed[kind];
                    add(asset["proxy_url"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .or_else(|| asset["url"].as_str()));
                }
            }
        }
        let user = &message["author"];
        let id = user["id"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| user["id"].as_u64().map(|n| n.to_string()));
        if let Some(id) = id.filter(|id| searchcord_archive::unicode_digits(id)) {
            for (kind, size) in [("avatars", 256), ("banners", 1024)] {
                let hash = if kind == "avatars" {
                    &user["avatar"]
                } else {
                    &user["banner"]
                };
                let url = searchcord_archive::image_url(&id, hash.as_str(), kind, size);
                add(url.as_deref());
            }
        }
    }
    urls
}

pub fn clear_media(path: impl AsRef<Path>) -> Result<usize> {
    let directory = media::directory(path.as_ref());
    if symlink(&directory) {
        return Err("Media directory resolves outside the archive directory".into());
    }
    if !directory.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(directory)? {
        let file = entry?.path();
        let name = file.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let hex = |key: &str| {
            key.len() == 64
                && key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        let compact = name
            .strip_suffix(".webp")
            .and_then(|name| name.split_once('.'))
            .is_some_and(|(key, digest)| hex(key) && hex(digest));
        let object = compact
            || name
                .strip_suffix(".bin")
                .or_else(|| name.strip_suffix(".json"))
                .is_some_and(|key| {
                    key.len() == 64
                        && key
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                });
        let temporary = name
            .strip_prefix(".searchcord-media-")
            .and_then(|s| s.strip_suffix(".tmp"))
            .is_some_and(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            });
        if file.is_file() && (object || temporary) {
            fs::remove_file(file)?;
            removed += 1;
        }
    }
    Ok(removed)
}
