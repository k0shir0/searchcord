//! Credential-free local media identity, manifests and digest validation.
pub fn split_lines(value: &str) -> Vec<&str> {
    crate::presentation::splitlines(value)
}
pub fn profile_references(uid: &str, payload: &serde_json::Value) -> Vec<String> {
    let avatar = crate::image_url(uid, payload["user"]["avatar"].as_str(), "avatars", 256);
    let banner = crate::presentation::fallback(&[
        &payload["user_profile"]["banner"],
        &payload["user"]["banner"],
    ]);
    let banner = crate::image_url(uid, banner.as_str(), "banners", 1024);
    [avatar, banner].into_iter().flatten().collect()
}
use crate::ArchiveError;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

pub fn profile_images_enabled() -> bool {
    std::env::var("SEARCHCORD_PROFILE_IMAGES")
        .unwrap_or_else(|_| "original".into())
        .eq_ignore_ascii_case("original")
}
pub fn media_kind(url: &str) -> &'static str {
    match url::Url::parse(url).ok().as_ref().map(|url| url.path()) {
        Some(path) if path.starts_with("/avatars/") => "avatar",
        Some(path) if path.starts_with("/banners/") => "banner",
        _ => "attachment",
    }
}

fn quote(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                output.push(byte as char)
            }
            b' ' => output.push('+'),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}
pub fn canonical_url(value: &str) -> Option<String> {
    if value.chars().count() > 8192 || value.chars().any(|c| (c as u32) < 32) {
        return None;
    }
    let value = value.trim_start_matches(' ');
    let (scheme, rest) = value.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut authority = &rest[..authority_end];
    if let Some(host) = authority
        .strip_prefix(":@")
        .or_else(|| authority.strip_prefix('@'))
    {
        authority = host;
    }
    if authority.contains('@') {
        return None;
    }
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(h, p)| (h, Some(p)));
    if let Some(port) = port.filter(|s| !s.is_empty())
        && (!port.bytes().all(|b| b.is_ascii_digit()) || port.parse::<u16>().ok() != Some(443))
    {
        return None;
    }
    let host = host.to_ascii_lowercase();
    if !["cdn.discordapp.com", "media.discordapp.net"].contains(&host.as_str()) {
        return None;
    }
    let target = rest[authority_end..].split('#').next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if !path.starts_with('/') {
        return None;
    }
    let profile = path.starts_with("/avatars/") || path.starts_with("/banners/");
    let mut values: Vec<_> = url::form_urlencoded::parse(query.as_bytes())
        .filter(|(k, v)| {
            let key = k.to_lowercase();
            let ignored = ["ex", "is", "hm"].contains(&key.as_str()) || (profile && key == "size");
            !v.is_empty() && !ignored
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    values.sort();
    let query = values
        .iter()
        .map(|(k, v)| format!("{}={}", quote(k), quote(v)))
        .collect::<Vec<_>>()
        .join("&");
    Some(format!(
        "https://{host}{path}{}",
        if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        }
    ))
}
pub fn media_key(url: &str) -> Option<String> {
    canonical_url(url).map(|v| format!("{:x}", Sha256::digest(v.as_bytes())))
}
pub fn directory(path: &Path) -> PathBuf {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    resolved.parent().unwrap_or(Path::new(".")).join("media")
}
fn symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|s| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            s.file_attributes() & 0x400 != 0
        }
        #[cfg(not(windows))]
        {
            s.file_type().is_symlink()
        }
    })
}
pub fn binary_path(directory: &Path, key: &str, data: &Value) -> std::io::Result<PathBuf> {
    let fail = || std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid media filename");
    let Some(name) = data.get("storage_file").filter(|value| !value.is_null()) else {
        return Ok(directory.join(format!("{key}.bin")));
    };
    let digest = data["sha256"].as_str().ok_or_else(fail)?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || name.as_str() != Some(&format!("{key}.{digest}.webp"))
    {
        return Err(fail());
    }
    Ok(directory.join(name.as_str().ok_or_else(fail)?))
}
fn file_valid(directory: &Path, key: &str, data: &Value, verify: bool) -> std::io::Result<bool> {
    let binary = binary_path(directory, key, data)?;
    if symlink(directory) || symlink(&binary) {
        return Ok(false);
    }
    if data["status"] != "saved" || data["bytes"].as_u64() != Some(fs::metadata(&binary)?.len()) {
        return Ok(false);
    }
    if verify {
        return Ok(data["sha256"].as_str() == Some(&file_digest(&binary)?));
    }
    Ok(true)
}
pub fn file_digest(path: &Path) -> std::io::Result<String> {
    let mut source = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn status(path: &Path, url: &str, verify: bool) -> Value {
    let Some(key) = media_key(url) else {
        return json!({"status":"unavailable","reason":"unsupported_host"});
    };
    let directory = directory(path);
    let manifest = directory.join(format!("{key}.json"));
    if symlink(&directory) || symlink(&manifest) {
        return json!({"status":"unavailable","reason":"unsafe_path"});
    }
    let read = || -> Option<Value> {
        let data: Value = serde_json::from_slice(&fs::read(manifest).ok()?).ok()?;
        data.as_object()?;
        if data["status"] == "saved" && !file_valid(&directory, &key, &data, verify).ok()? {
            return Some(json!({"status":"unavailable","reason":"damaged_file"}));
        }
        Some(data)
    };
    read().unwrap_or_else(|| json!({"status":"unavailable","reason":"not_collected"}))
}
pub fn local_url(path: &Path, url: &str) -> String {
    if status(path, url, false)["status"] == "saved" {
        format!("/api/media/{}", media_key(url).unwrap_or_default())
    } else {
        url.to_owned()
    }
}
pub struct MediaFile {
    pub digest: String,
    pub path: PathBuf,
    pub content_type: String,
    pub inline: bool,
    pub bytes: u64,
}
pub fn verified_file(path: &Path, key: &str) -> Result<MediaFile, ArchiveError> {
    let fail = || ArchiveError::new(404, "Archived media not found.");
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(fail());
    }
    let directory = directory(path);
    let manifest = directory.join(format!("{key}.json"));
    if symlink(&directory) || symlink(&manifest) {
        return Err(fail());
    }
    let data: Value =
        serde_json::from_slice(&fs::read(manifest).map_err(|_| fail())?).map_err(|_| fail())?;
    if !file_valid(&directory, key, &data, true).map_err(|_| fail())? {
        return Err(fail());
    }
    let content_type = data["content_type"].as_str().unwrap_or("");
    let inline = [
        "image/png",
        "image/jpeg",
        "image/gif",
        "image/webp",
        "image/avif",
    ]
    .contains(&content_type);
    Ok(MediaFile {
        digest: data["sha256"].as_str().ok_or_else(fail)?.to_owned(),
        path: binary_path(&directory, key, &data).map_err(|_| fail())?,
        content_type: if inline {
            content_type.to_owned()
        } else {
            "application/octet-stream".into()
        },
        inline,
        bytes: data["bytes"].as_u64().ok_or_else(fail)?,
    })
}
