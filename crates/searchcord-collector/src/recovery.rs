//! Sanitized and verified recovery copies; publication never overwrites another copy.
use crate::storage::Result;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
struct Owned(PathBuf, bool);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.1 {
            let _ = fs::remove_file(&self.0);
        }
    }
}
pub fn backup(path: &Path) -> Result<PathBuf> {
    let path = crate::credentials::resolve(path)?;
    let mut name = path
        .file_name()
        .ok_or("Invalid backup source")?
        .to_os_string();
    name.push(format!(
        ".pre-compact-{}.bak",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%6fZ")
    ));
    let target = path.with_file_name(name);
    let mut private_name = target.as_os_str().to_os_string();
    private_name.push(".private-tmp");
    let private = PathBuf::from(private_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(options.open(&private)?);
    let mut cleanup = Owned(private.clone(), true);
    {
        let source = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.backup("main", &private, None)?;
        let destination = Connection::open(&private)?;
        destination.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA secure_delete=ON;")?;
        if destination.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='settings'",
            [],
            |r| r.get::<_, i64>(0),
        )? > 0
        {
            destination.execute(
                "DELETE FROM settings WHERE key IN ('token','saved_tokens','active_token_id')",
                [],
            )?;
        }
        destination.execute_batch("VACUUM")?;
        if destination.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))? != "ok" {
            return Err("Recovery backup failed integrity check".into());
        }
    }
    fs::hard_link(&private, &target)?;
    fs::remove_file(&private)?;
    cleanup.1 = false;
    Ok(target)
}
pub fn pack_backup(path: &Path) -> Result<PathBuf> {
    pack_backup_with_hook(path, |_| Ok(()))
}
pub fn pack_backup_with_hook(
    path: &Path,
    before_verify: impl FnOnce(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".gz");
    let packed = PathBuf::from(name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let output = options.open(&packed)?;
    let mut cleanup = Owned(packed.clone(), true);
    let mut encoder = flate2::write::GzEncoder::new(output, flate2::Compression::new(6));
    let mut original = fs::File::open(path)?;
    let mut expected = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let size = original.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        expected.update(&buffer[..size]);
        encoder.write_all(&buffer[..size])?;
    }
    drop(original);
    encoder.finish()?.sync_all()?;
    before_verify(&packed)?;
    let mut decoder = flate2::read::GzDecoder::new(fs::File::open(&packed)?);
    let mut actual = Sha256::new();
    loop {
        let size = decoder.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        actual.update(&buffer[..size]);
    }
    drop(decoder);
    if expected.finalize() != actual.finalize() {
        return Err("Compressed recovery backup did not verify".into());
    }
    fs::remove_file(path)?;
    cleanup.1 = false;
    Ok(packed)
}
