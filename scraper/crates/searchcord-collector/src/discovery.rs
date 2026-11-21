//! Source discovery preserves the collector's permission and probe semantics.
use crate::{
    Result,
    discord::{Discord, Request},
    pacing::{Clock, Options},
};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const READABLE: u64 = (1 << 10) | (1 << 16);
pub const MANAGE_THREADS: u64 = 1 << 34;
const THREADS: [i64; 3] = [10, 11, 12];

#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub detail: String,
}
impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for HttpError {}
pub fn http_error(
    status: u16,
    detail: impl Into<String>,
) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(HttpError {
        status,
        detail: detail.into(),
    })
}
fn invalid(detail: &str) -> Box<dyn std::error::Error + Send + Sync> {
    http_error(502, detail)
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| invalid("Discord returned incomplete channel permissions. Try again."))
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array()
        .ok_or_else(|| invalid("Discord returned incomplete channel permissions. Try again."))
}
fn kind(v: &Value) -> Result<i64> {
    v["type"]
        .as_i64()
        .ok_or_else(|| invalid("Discord returned incomplete channel permissions. Try again."))
}
// Only the three flags consulted by discovery are needed. Reduce decimal
// strings modulo 2^64 to accept Discord's arbitrary-width permission integers.
fn bits(v: &Value) -> Result<u64> {
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    let raw = v
        .as_str()
        .ok_or_else(|| invalid("Discord returned invalid channel permissions. Try again."))?
        .trim();
    let (negative, raw) = raw.strip_prefix('-').map_or((false, raw), |s| (true, s));
    let raw = raw.strip_prefix('+').unwrap_or(raw);
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid(
            "Discord returned invalid channel permissions. Try again.",
        ));
    }
    let n = raw.bytes().fold(0u64, |n, b| {
        n.wrapping_mul(10).wrapping_add((b - b'0') as u64)
    });
    Ok(if negative { n.wrapping_neg() } else { n })
}
pub fn permissions_for(
    guild: &Value,
    member: &Value,
    channel: &Value,
    channels: &HashMap<String, Value>,
) -> Result<u64> {
    let uid = text(&member["user"], "id")?;
    let gid = text(guild, "id")?;
    let roles: HashMap<_, _> = array(&guild["roles"])?
        .iter()
        .map(|r| Ok((text(r, "id")?, bits(&r["permissions"])?)))
        .collect::<Result<_>>()?;
    let member_roles: HashSet<_> = array(&member["roles"])?
        .iter()
        .map(|r| {
            r.as_str().ok_or_else(|| {
                invalid("Discord returned incomplete channel permissions. Try again.")
            })
        })
        .collect::<Result<_>>()?;
    let mut p = *roles
        .get(gid)
        .ok_or_else(|| invalid("Discord returned incomplete channel permissions. Try again."))?;
    for role in &member_roles {
        p |= roles.get(role).ok_or_else(|| {
            invalid("Discord returned incomplete channel permissions. Try again.")
        })?;
    }
    if uid == text(guild, "owner_id")? || p & 8 != 0 {
        return Ok(u64::MAX);
    }
    let channel = if THREADS.contains(&kind(channel)?) {
        channels
            .get(text(channel, "parent_id")?)
            .ok_or_else(|| invalid("Discord returned incomplete channel permissions. Try again."))?
    } else {
        channel
    };
    let overwrites = array(&channel["permission_overwrites"])?;
    let apply =
        |p: u64, o: &Value| -> Result<u64> { Ok((p & !bits(&o["deny"])?) | bits(&o["allow"])?) };
    if let Some(o) = overwrites.iter().find(|o| o["id"] == gid && o["type"] == 0) {
        p = apply(p, o)?;
    }
    let (mut allow, mut deny) = (0, 0);
    for o in overwrites {
        if o["type"] == 0 && member_roles.contains(text(o, "id")?) {
            allow |= bits(&o["allow"])?;
            deny |= bits(&o["deny"])?;
        }
    }
    p = (p & !deny) | allow;
    if let Some(o) = overwrites.iter().find(|o| o["id"] == uid && o["type"] == 1) {
        p = apply(p, o)?;
    }
    Ok(p)
}

pub struct Discovery<C: Clock> {
    client: Discord<C>,
}
impl<C: Clock> Discovery<C> {
    pub fn new(client: Discord<C>) -> Self {
        Self { client }
    }
    async fn get(
        &self,
        path: &str,
        token: &str,
        optional: bool,
        params: Vec<(String, String)>,
    ) -> Result<Option<Value>> {
        let mut request = Request::new(Method::GET, path, token);
        request.params = params;
        let response = self.client.request(request, Options::default()).await?;
        if optional && [403, 404].contains(&response.status) {
            if response.json().ok().is_some_and(|v| v["code"] == 40002) {
                return Err(http_error(
                    403,
                    "Complete account verification in Discord before checking channels.",
                ));
            }
            return Ok(None);
        }
        if response.status != 200 {
            return Err(http_error(
                response.status,
                format!(
                    "Could not verify channel access (HTTP {}). Try again.",
                    response.status
                ),
            ));
        }
        response
            .json()
            .map(Some)
            .map_err(|_| invalid("Discord returned invalid channel access data. Try again."))
    }
    pub async fn validate(&self, token: &str) -> Result<Value> {
        let response = self
            .client
            .request(
                Request::new(Method::GET, "/users/@me", token),
                Options::default(),
            )
            .await?;
        if response.status != 200 {
            return Ok(json!({"valid":false}));
        }
        let user = response.json()?;
        let name = user["global_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(Value::from)
            .unwrap_or_else(|| user["username"].clone());
        Ok(json!({"valid":true,"username":name,"id":user["id"]}))
    }
    pub async fn guilds(&self, token: &str) -> Result<Value> {
        let response = self
            .client
            .request(
                Request::new(Method::GET, "/users/@me/guilds", token),
                Options::default(),
            )
            .await?;
        if response.status != 200 {
            return Err(http_error(response.status, "Failed to fetch guilds"));
        }
        let payload = response.json()?;
        let mut result = Vec::new();
        for g in array(&payload)? {
            let id = text(g, "id")?;
            let icon = g["icon"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!("https://cdn.discordapp.com/icons/{id}/{s}.png?size=64"));
            result.push(json!({"id":id,"name":g["name"],"icon":icon}));
        }
        Ok(json!(result))
    }
    pub async fn dms(&self, token: &str) -> Result<Value> {
        let response = self
            .client
            .request(
                Request::new(Method::GET, "/users/@me/channels", token),
                Options::default(),
            )
            .await?;
        if response.status != 200 {
            return Err(http_error(response.status, "Failed to fetch DMs"));
        }
        let payload = response.json()?;
        let mut result = Vec::new();
        for c in array(&payload)? {
            let recipients = c
                .get("recipients")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let name = |r: &Value| {
                r["global_name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .or_else(|| r["username"].as_str())
                    .map(str::to_owned)
            };
            let label = match kind(c)? {
                1 => recipients
                    .first()
                    .map(&name)
                    .unwrap_or(Some("Unknown User".into())),
                3 => c["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .or_else(|| {
                        let s = recipients
                            .iter()
                            .filter_map(name)
                            .collect::<Vec<_>>()
                            .join(", ");
                        Some(if s.is_empty() { "Group DM".into() } else { s })
                    }),
                _ => continue,
            };
            result.push(json!({"id":text(c,"id")?,"name":label,"type":c["type"]}));
        }
        Ok(json!(result))
    }
    pub async fn channels(&self, gid: &str, token: &str, include_threads: bool) -> Result<Value> {
        let payload = self
            .get(&format!("/guilds/{gid}/channels"), token, false, vec![])
            .await?
            .unwrap();
        let raw = payload
            .as_array()
            .ok_or_else(|| invalid("Discord returned an invalid channel list."))?;
        if raw.is_empty() {
            return Ok(json!([]));
        }
        let guild = self
            .get(&format!("/guilds/{gid}"), token, false, vec![])
            .await?
            .unwrap();
        let user = self.get("/users/@me", token, false, vec![]).await?.unwrap();
        let uid = user["id"]
            .as_str()
            .ok_or_else(|| invalid("Discord returned invalid account data."))?;
        let member = self
            .get(
                &format!("/guilds/{gid}/members/{uid}"),
                token,
                false,
                vec![],
            )
            .await?
            .unwrap();
        if guild["id"] != gid || member["user"]["id"] != uid {
            return Err(invalid(
                "Discord returned incomplete channel permissions. Try again.",
            ));
        }
        let by_id: HashMap<String, Value> = raw
            .iter()
            .map(|c| Ok((text(c, "id")?.into(), c.clone())))
            .collect::<Result<_>>()?;
        let permitted: HashMap<_, _> = raw
            .iter()
            .map(|c| Ok((text(c, "id")?, permissions_for(&guild, &member, c, &by_id)?)))
            .collect::<Result<_>>()?;
        let parents: Vec<_> = raw
            .iter()
            .filter(|c| {
                include_threads
                    && [0, 5, 15, 16].contains(&c["type"].as_i64().unwrap_or(-1))
                    && permitted
                        .get(c["id"].as_str().unwrap_or(""))
                        .is_some_and(|p| p & READABLE == READABLE)
            })
            .collect();
        let mut candidates = BTreeMap::new();
        for c in raw {
            if [0, 5, 10, 11, 12].contains(&kind(c)?) {
                candidates.insert(text(c, "id")?.to_owned(), c.clone());
            }
        }
        if !parents.is_empty()
            && let Some(active) = self
                .get(
                    &format!("/guilds/{gid}/threads/active"),
                    token,
                    true,
                    vec![],
                )
                .await?
        {
            for t in active["threads"]
                .as_array()
                .ok_or_else(|| invalid("Discord returned an invalid thread list."))?
            {
                candidates.insert(text(t, "id")?.to_owned(), t.clone());
            }
        }
        for parent in parents {
            let pid = text(parent, "id")?;
            let mut paths = vec![format!("/channels/{pid}/threads/archived/public")];
            if kind(parent)? == 0 {
                paths.push(format!(
                    "/channels/{pid}/users/@me/threads/archived/private"
                ));
                if permitted[pid] & MANAGE_THREADS != 0 {
                    paths.push(format!("/channels/{pid}/threads/archived/private"));
                }
            }
            for path in paths {
                let mut before: Option<String> = None;
                let mut seen = HashSet::new();
                loop {
                    let mut params = vec![("limit".into(), "100".into())];
                    if let Some(ref b) = before {
                        params.push(("before".into(), b.clone()));
                    }
                    let Some(page) = self.get(&path, token, true, params).await? else {
                        break;
                    };
                    let batch = page["threads"].as_array().ok_or_else(|| {
                        invalid("Discord returned incomplete archived threads. Try again.")
                    })?;
                    for t in batch {
                        candidates.insert(text(t, "id")?.to_owned(), t.clone());
                    }
                    let more = page["has_more"].as_bool().ok_or_else(|| {
                        invalid("Discord returned incomplete archived threads. Try again.")
                    })?;
                    if !more {
                        break;
                    }
                    let last = batch.last().ok_or_else(|| {
                        invalid("Discord returned incomplete archived threads. Try again.")
                    })?;
                    let cursor = if path.contains("/users/@me/") {
                        &last["id"]
                    } else {
                        &last["thread_metadata"]["archive_timestamp"]
                    };
                    let cursor = cursor.as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
                        invalid("Discord returned incomplete archived threads. Try again.")
                    })?;
                    if !seen.insert(cursor.to_owned()) {
                        return Err(invalid(
                            "Discord returned incomplete archived threads. Try again.",
                        ));
                    }
                    before = Some(cursor.to_owned());
                }
            }
        }
        let mut candidates: Vec<_> = candidates.into_values().collect();
        candidates.sort_by(|a, b| {
            (
                a["parent_id"].as_str().unwrap_or(""),
                a["position"].as_i64().unwrap_or(0),
                a["id"].as_str().unwrap_or(""),
            )
                .cmp(&(
                    b["parent_id"].as_str().unwrap_or(""),
                    b["position"].as_i64().unwrap_or(0),
                    b["id"].as_str().unwrap_or(""),
                ))
        });
        let mut result = Vec::new();
        for c in candidates {
            if THREADS.contains(&kind(&c)?)
                && c["parent_id"]
                    .as_str()
                    .is_none_or(|s| !by_id.contains_key(s))
            {
                continue;
            }
            if permissions_for(&guild, &member, &c, &by_id)? & READABLE != READABLE {
                continue;
            }
            let id = text(&c, "id")?;
            let Some(probe) = self
                .get(
                    &format!("/channels/{id}/messages"),
                    token,
                    true,
                    vec![("limit".into(), "1".into())],
                )
                .await?
            else {
                continue;
            };
            if !probe.is_array() {
                return Err(invalid(
                    "Discord returned an invalid channel access probe. Try again.",
                ));
            }
            let parent = c["parent_id"].as_str().and_then(|s| by_id.get(s));
            result.push(json!({"id":id,"name":c["name"],"type":c["type"],"category":parent.and_then(|p| p["name"].as_str()).unwrap_or(""),"category_id":c["parent_id"],"nsfw":c.get("nsfw").unwrap_or(&Value::Bool(false))}));
        }
        Ok(json!(result))
    }
}
