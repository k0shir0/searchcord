use crate::{Report, Result, SnapshotError, build_database_with_hook};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use searchcord_archive::{codec, image_url, media};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::Instant,
};

pub fn resolve_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            part => resolved.push(part.as_os_str()),
        }
        if resolved.exists() {
            resolved = resolved.canonicalize()?;
        }
    }
    Ok(resolved)
}
struct OwnedFiles {
    directory: Option<PathBuf>,
    files: Vec<PathBuf>,
    armed: bool,
}
impl Drop for OwnedFiles {
    fn drop(&mut self) {
        if self.armed {
            for file in self.files.iter().rev() {
                let _ = fs::remove_file(file);
            }
            if let Some(directory) = &self.directory {
                let _ = fs::remove_dir(directory);
            }
        }
    }
}
pub fn build_snapshot(source: &Path, destination: &Path, block_rows: usize) -> Result<Report> {
    publish_with_hooks(source, destination, block_rows, |_, _| Ok(()), |_| Ok(()))
}
pub fn publish_with_hooks(
    source: &Path,
    destination: &Path,
    block_rows: usize,
    progress: impl FnMut(u64, u64) -> Result<()>,
    before_publish: impl FnOnce(&Path) -> Result<()>,
) -> Result<Report> {
    let started = Instant::now();
    let source = resolve_path(source)?;
    let destination = resolve_path(destination)?;
    if !source.is_file() {
        return Err(SnapshotError("Source archive does not exist".into()));
    }
    if source == destination || destination.exists() || destination.is_symlink() {
        return Err(SnapshotError(
            "Refusing to replace an existing file or the source archive".into(),
        ));
    }
    let stamp = fs::metadata(&source)?;
    let source_stamp = (stamp.len(), stamp.modified()?);
    let mut name = destination
        .file_name()
        .ok_or_else(|| SnapshotError("Invalid destination path".into()))?
        .to_os_string();
    name.push(".building");
    let temporary = destination.with_file_name(name);
    let mut report = build_database_with_hook(&source, &temporary, block_rows, progress)?;
    let mut cleanup = OwnedFiles {
        directory: None,
        files: vec![temporary.clone()],
        armed: true,
    };
    before_publish(&destination)?;
    fs::hard_link(&temporary, &destination)?;
    fs::remove_file(&temporary)?;
    cleanup.armed = false;
    let stamp = fs::metadata(&source)?;
    report.source_main_file_unchanged = source_stamp == (stamp.len(), stamp.modified()?);
    report.build_seconds = format!("{:.2}", started.elapsed().as_secs_f64())
        .parse()
        .unwrap_or(0.0);
    Ok(report)
}
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaReport {
    pub copied: u64,
    pub unavailable: u64,
    pub bytes: u64,
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
pub fn export_media(source: &Path, destination: &Path) -> Result<MediaReport> {
    export_media_with_hook(source, destination, |_| Ok(()))
}
pub fn export_media_with_hook(
    source: &Path,
    destination: &Path,
    mut after_copy: impl FnMut(&Path) -> Result<()>,
) -> Result<MediaReport> {
    let directory = media::directory(destination);
    if directory.exists() || directory.is_symlink() {
        return Err(SnapshotError(
            "Destination media directory already exists; choose a new snapshot directory".into(),
        ));
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    let mut cleanup = OwnedFiles {
        directory: Some(directory.clone()),
        files: vec![],
        armed: true,
    };
    let db = Connection::open_with_flags(
        destination,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    codec::install_reader(&db)?;
    let mut report = MediaReport::default();
    let mut seen = HashSet::new();
    let source_directory = media::directory(source);
    {
        let mut copy = |url: &str| -> Result<()> {
            let Some(key) = media::media_key(url) else {
                return Ok(());
            };
            if !seen.insert(key.clone()) {
                return Ok(());
            }
            let data = media::status(source, url, true);
            if data["status"] != "saved" {
                report.unavailable += 1;
                return Ok(());
            }
            let path = directory.join(format!("{key}.bin"));
            {
                let mut original = fs::File::open(source_directory.join(format!("{key}.bin")))?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                cleanup.files.push(path.clone());
                let mut buffer = vec![0_u8; 1024 * 1024];
                loop {
                    let n = original.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    output.write_all(&buffer[..n])?;
                }
            }
            after_copy(&path)?;
            let bytes = fs::metadata(&path)?.len();
            if data["bytes"].as_u64() != Some(bytes)
                || data["sha256"].as_str() != Some(&media::file_digest(&path)?)
            {
                return Err(SnapshotError(
                    "Copied media did not match its recorded digest".into(),
                ));
            }
            let manifest = directory.join(format!("{key}.json"));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&manifest)?;
            cleanup.files.push(manifest);
            serde_json::to_writer(&mut file, &data)?;
            file.flush()?;
            report.copied += 1;
            report.bytes += bytes;
            Ok(())
        };
        {
            let mut statement = db.prepare("SELECT image_urls FROM message_records")?;
            let rows = statement.query_map([], |r| r.get::<_, Option<String>>(0))?;
            for row in rows {
                for url in searchcord_archive::media::split_lines(&row?.unwrap_or_default()) {
                    copy(url)?;
                }
            }
        }
        if exists(&db, "message_attachments")? {
            let mut statement = db.prepare("SELECT payload FROM message_attachments")?;
            let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                let payload: Value = serde_json::from_str(&row?)?;
                if let Some(attachments) = payload.as_array() {
                    for attachment in attachments {
                        if let Some(url) = attachment["url"].as_str() {
                            copy(url)?;
                        }
                    }
                }
            }
        }
        {
            let mut statement =
                db.prepare("SELECT user_id,avatar_hash,banner_hash FROM profiles")?;
            let rows = statement.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?;
            for row in rows {
                let (uid, avatar, banner) = row?;
                for url in [
                    image_url(&uid, avatar.as_deref(), "avatars", 256),
                    image_url(&uid, banner.as_deref(), "banners", 1024),
                ]
                .into_iter()
                .flatten()
                {
                    copy(&url)?;
                }
            }
        }
        {
            let mut statement = db.prepare("SELECT user_id,payload FROM profile_details")?;
            let rows = statement
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (uid, payload) = row?;
                let payload: Value = serde_json::from_str(&payload)?;
                for url in media::profile_references(&uid, &payload) {
                    copy(&url)?;
                }
            }
        }
    }
    cleanup.armed = false;
    Ok(report)
}
