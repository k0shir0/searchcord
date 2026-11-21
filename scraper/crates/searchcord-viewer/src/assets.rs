use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::time::UNIX_EPOCH;
pub fn serve(
    bytes: &'static [u8],
    mime: &str,
    path: &str,
    head: bool,
    request: &HeaderMap,
) -> Response {
    let etag = format!("\"{:x}\"", Sha256::digest(bytes));
    let modified = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .unwrap_or(UNIX_EPOCH);
    let date = DateTime::<Utc>::from(modified)
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let text = |name: header::HeaderName| request.get(name).and_then(|v| v.to_str().ok());
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, mime.parse().unwrap());
    headers.insert(
        header::CONTENT_LENGTH,
        bytes.len().to_string().parse().unwrap(),
    );
    headers.insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    headers.insert(header::ETAG, etag.parse().unwrap());
    headers.insert(header::LAST_MODIFIED, date.parse().unwrap());
    // Only the StaticFiles route evaluates conditional cache requests.
    if (path == "/" || head)
        && (text(header::IF_NONE_MATCH).is_some_and(|s| {
            s.split(',')
                .any(|v| v.trim().trim_start_matches("W/") == etag)
        }) || text(header::IF_NONE_MATCH).is_none()
            && text(header::IF_MODIFIED_SINCE)
                .and_then(|s| DateTime::parse_from_rfc2822(s).ok())
                .is_some_and(|d| d.timestamp() >= DateTime::<Utc>::from(modified).timestamp()))
    {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response.headers_mut().remove(header::CONTENT_LENGTH);
        response.headers_mut().remove(header::CONTENT_TYPE);
        *response.body_mut() = Body::empty();
        return response;
    }
    if let Some(range) = text(header::RANGE)
        .filter(|_| text(header::IF_RANGE).is_none_or(|v| v == etag || v == date))
    {
        match ranges(range, bytes.len()) {
            Err(status) => {
                *response.status_mut() = status;
                response.headers_mut().remove(header::CONTENT_LENGTH);
                if status == StatusCode::RANGE_NOT_SATISFIABLE {
                    response.headers_mut().insert(
                        header::CONTENT_RANGE,
                        format!("bytes */{}", bytes.len()).parse().unwrap(),
                    );
                }
                *response.body_mut() = Body::from(if status == StatusCode::BAD_REQUEST {
                    "Malformed range header."
                } else {
                    ""
                });
            }
            Ok(ranges) => {
                *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                if ranges.len() == 1 {
                    let (start, end) = ranges[0];
                    response.headers_mut().insert(
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{}/{}", end - 1, bytes.len())
                            .parse()
                            .unwrap(),
                    );
                    response.headers_mut().insert(
                        header::CONTENT_LENGTH,
                        (end - start).to_string().parse().unwrap(),
                    );
                    *response.body_mut() = Body::from(&bytes[start..end]);
                } else {
                    let boundary = "searchcord-native-range";
                    let mut out = Vec::new();
                    for (start, end) in ranges {
                        out.extend_from_slice(format!("--{boundary}\nContent-Type: {mime}\nContent-Range: bytes {start}-{}/{}\n\n",end-1,bytes.len()).as_bytes());
                        out.extend_from_slice(&bytes[start..end]);
                        out.extend_from_slice(b"\n");
                    }
                    out.extend_from_slice(format!("\n--{boundary}--\n").as_bytes());
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        format!("multipart/byteranges; boundary={boundary}")
                            .parse()
                            .unwrap(),
                    );
                    response.headers_mut().insert(
                        header::CONTENT_LENGTH,
                        out.len().to_string().parse().unwrap(),
                    );
                    *response.body_mut() = Body::from(out);
                }
            }
        }
    }
    response
}
fn ranges(raw: &str, len: usize) -> Result<Vec<(usize, usize)>, StatusCode> {
    let fail = StatusCode::BAD_REQUEST;
    let mut ranges = Vec::new();
    for part in raw.strip_prefix("bytes=").ok_or(fail)?.split(',') {
        let (start, end) = part.trim().split_once('-').ok_or(fail)?;
        let (start, end) = if start.is_empty() {
            let n = end.parse::<usize>().map_err(|_| fail)?;
            (len.saturating_sub(n), len)
        } else {
            let start = start.parse::<usize>().map_err(|_| fail)?;
            let end = if end.is_empty() {
                len
            } else {
                end.parse::<usize>()
                    .map_err(|_| fail)?
                    .saturating_add(1)
                    .min(len)
            };
            (start, end)
        };
        if start >= len || start >= end {
            return Err(StatusCode::RANGE_NOT_SATISFIABLE);
        }
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(previous) = merged.last_mut().filter(|p| start <= p.1) {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    Ok(merged)
}
