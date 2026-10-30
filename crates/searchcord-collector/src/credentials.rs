//! Existing external sealed vault format. Archive files never contain secrets.
use fernet::Fernet;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fmt, fs,
    io::Write,
    path::{Component, Path, PathBuf},
};
#[derive(Debug)]
pub struct CredentialError(pub String);
impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CredentialError {}
impl From<std::io::Error> for CredentialError {
    fn from(_: std::io::Error) -> Self {
        Self("Could not access the credential vault; saved credentials were kept".into())
    }
}
type Result<T> = std::result::Result<T, CredentialError>;
fn failure(message: &str) -> CredentialError {
    CredentialError(message.into())
}
pub fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let path = expand(path);
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
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy();
        resolved = if let Some(tail) = text.strip_prefix(r"\\?\UNC\") {
            PathBuf::from(format!(r"\\{tail}"))
        } else if let Some(tail) = text.strip_prefix(r"\\?\") {
            PathBuf::from(tail)
        } else {
            resolved
        };
    }
    Ok(resolved)
}
fn expand(path: &Path) -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default();
    match path.to_str() {
        Some("~") => home,
        Some(raw) if raw.starts_with("~/") => home.join(&raw[2..]),
        _ => path.to_owned(),
    }
}
fn normalized(path: &Path) -> String {
    let raw = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        raw.replace('/', "\\").to_lowercase()
    } else {
        raw
    }
}
#[derive(Clone, Debug)]
pub struct Paths {
    pub database: PathBuf,
    pub vault: PathBuf,
    pub key: PathBuf,
}
impl Paths {
    pub fn new(database: &Path, vault: &Path, key: &Path) -> Result<Self> {
        let database = resolve(database)?;
        let vault = resolve(vault)?;
        let key = resolve(key)?;
        let parent = database
            .parent()
            .ok_or_else(|| failure("Invalid archive path"))?;
        let within = |path: &Path| {
            PathBuf::from(normalized(path)).starts_with(PathBuf::from(normalized(parent)))
        };
        if within(&vault) || within(&key) {
            return Err(failure(
                "Keep the credential vault and encryption key outside the archive directory",
            ));
        }
        if normalized(&vault) == normalized(&key) {
            return Err(failure(
                "The credential vault and encryption key must use different files",
            ));
        }
        Ok(Self {
            database,
            vault,
            key,
        })
    }
    pub fn archive_key(database: &Path) -> Result<String> {
        let archive = resolve(database)?;
        Ok(format!("{:x}", Sha256::digest(normalized(&archive).as_bytes()))[..24].to_owned())
    }
    pub fn for_database(database: &Path) -> Result<Self> {
        let archive = resolve(database)?;
        let digest = format!("{:x}", Sha256::digest(normalized(&archive).as_bytes()));
        let root = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .unwrap_or_else(|| expand(Path::new("~")).join(".local/share"));
        let vault = std::env::var_os("SEARCHCORD_CREDENTIAL_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                root.join("Searchcord/credentials")
                    .join(format!("{}.vault", &digest[..24]))
            });
        let key = std::env::var_os("SEARCHCORD_CREDENTIAL_KEY_FILE")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| vault.with_extension("key"));
        Self::new(&archive, &vault, &key)
    }
}
fn private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(failure(
                "Credential files must be readable only by their owner (chmod 600)",
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("Invalid credential path"))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(parent)?;
    Ok(())
}
pub fn validate(state: &Value) -> Result<()> {
    let tokens = state
        .get("tokens")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("The credential vault is invalid"))?;
    let mut ids = HashSet::new();
    for token in tokens {
        if !token.is_object()
            || ["id", "label", "token"].iter().any(|k| {
                token
                    .get(*k)
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
            })
        {
            return Err(failure("The credential vault is invalid"));
        }
        if !ids.insert(token["id"].as_str().unwrap()) {
            return Err(failure("The credential vault is invalid"));
        }
    }
    if state
        .get("active_token_id")
        .is_some_and(|id| !id.is_null() && !id.as_str().is_some_and(|id| ids.contains(id)))
    {
        return Err(failure("The active credential is missing from the vault"));
    }
    Ok(())
}
pub fn metadata(tokens: &[Value]) -> Value {
    json!(
        tokens
            .iter()
            .map(|t| json!({"id":t["id"],"label":t["label"]}))
            .collect::<Vec<_>>()
    )
}
pub struct Vault {
    pub paths: Paths,
    #[cfg(windows)]
    portable: bool,
}
impl Vault {
    pub fn portable(paths: Paths) -> Self {
        Self {
            paths,
            #[cfg(windows)]
            portable: true,
        }
    }
    pub fn platform(paths: Paths) -> Self {
        Self {
            paths,
            #[cfg(windows)]
            portable: false,
        }
    }
    fn fernet(&self, create: bool) -> Result<Fernet> {
        if !self.paths.key.exists() {
            if !create {
                return Err(failure(
                    "The external credential encryption key is missing; restore it or clear credentials",
                ));
            }
            directory(&self.paths.key)?;
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&self.paths.key) {
                Ok(mut file) => {
                    file.write_all(Fernet::generate_key().as_bytes())?;
                    file.sync_all()?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        private(&self.paths.key)?;
        let key = fs::read_to_string(&self.paths.key)
            .map_err(|_| failure("The external credential encryption key is invalid"))?;
        Fernet::new(key.trim())
            .ok_or_else(|| failure("The external credential encryption key is invalid"))
    }
    fn unseal(&self, data: &[u8]) -> Result<Value> {
        let (header, ciphertext) = data
            .iter()
            .position(|b| *b == b'\n')
            .map(|at| (&data[..at], &data[at + 1..]))
            .ok_or_else(|| {
                failure("Could not unlock the credential vault; saved credentials were kept")
            })?;
        let plaintext = match header {
            b"SEARCHCORD-FERNET-1" => {
                let fernet = self.fernet(false)?;
                let cipher = std::str::from_utf8(ciphertext).map_err(|_| {
                    failure("Could not unlock the credential vault; saved credentials were kept")
                })?;
                fernet.decrypt(cipher).map_err(|_| {
                    failure("Could not unlock the credential vault; saved credentials were kept")
                })?
            }
            #[cfg(windows)]
            b"SEARCHCORD-DPAPI-1" if !self.portable => crate::dpapi::transform(ciphertext, true)?,
            _ => {
                return Err(failure(
                    "This credential vault needs its original OS user or encryption key",
                ));
            }
        };
        let value: Value = serde_json::from_slice(&plaintext).map_err(|_| {
            failure("Could not unlock the credential vault; saved credentials were kept")
        })?;
        validate(&value)?;
        Ok(value)
    }
    pub fn load(&self) -> Result<Value> {
        if !self.paths.vault.exists() {
            return Ok(json!({"tokens":[],"active_token_id":null}));
        }
        private(&self.paths.vault)?;
        self.unseal(&fs::read(&self.paths.vault)?)
    }
    pub fn save(&self, tokens: &Value, active: &Value) -> Result<()> {
        self.save_with_hook(tokens, active, |_| Ok(()))
    }
    pub fn save_with_hook(
        &self,
        tokens: &Value,
        active: &Value,
        before_verify: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<()> {
        let state = json!({"tokens":tokens,"active_token_id":active});
        validate(&state)?;
        let exists = self.paths.vault.exists();
        if exists {
            self.load()?;
        }
        let plaintext =
            serde_json::to_vec(&state).map_err(|_| failure("The credential vault is invalid"))?;
        let data = self.seal(&plaintext, !exists)?;
        directory(&self.paths.vault)?;
        let parent = self.paths.vault.parent().unwrap();
        let mut temporary = tempfile::Builder::new()
            .prefix(".credential-")
            .tempfile_in(parent)?;
        temporary.write_all(&data)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        before_verify(temporary.path())?;
        if self.unseal(&fs::read(temporary.path())?)? != state {
            return Err(failure(
                "The credential vault did not verify; archive settings were kept",
            ));
        }
        temporary.persist(&self.paths.vault).map_err(|_| {
            failure("Could not replace the credential vault; saved credentials were kept")
        })?;
        #[cfg(unix)]
        {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
    fn seal(&self, plaintext: &[u8], create: bool) -> Result<Vec<u8>> {
        #[cfg(windows)]
        if !self.portable {
            let mut data = b"SEARCHCORD-DPAPI-1\n".to_vec();
            data.extend_from_slice(&crate::dpapi::transform(plaintext, false)?);
            return Ok(data);
        }
        let fernet = self.fernet(create)?;
        Ok(format!("SEARCHCORD-FERNET-1\n{}", fernet.encrypt(plaintext)).into_bytes())
    }
    pub fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.paths.vault) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
