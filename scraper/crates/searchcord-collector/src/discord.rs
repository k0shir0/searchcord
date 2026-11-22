//! Buffered Discord REST requests. All callers share the same pacing Gate.
use crate::{
    Result,
    pacing::{Clock, Gate, Options, Response, SystemClock},
};
use reqwest::{
    Client, Method, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::Value;
use std::time::Duration;

pub struct Request {
    pub method: Method,
    pub path: String,
    pub params: Vec<(String, String)>,
    pub json: Option<Value>,
    token: String,
}
impl Request {
    pub fn new(method: Method, path: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            token: token.into(),
            params: Vec::new(),
            json: None,
        }
    }
}
pub struct Discord<C: Clock = SystemClock> {
    gate: Gate<C>,
    http: Client,
    origin: String,
}
impl<C: Clock> Clone for Discord<C> {
    fn clone(&self) -> Self {
        Self {
            gate: self.gate.clone(),
            http: self.http.clone(),
            origin: self.origin.clone(),
        }
    }
}
impl<C: Clock> Discord<C> {
    pub fn break_seconds(&self) -> u64 {
        self.gate.break_seconds()
    }
    pub fn new(gate: Gate<C>) -> Result<Self> {
        Self::client(gate, "https://discord.com/api/v10".into(), false)
    }
    /// Explicit numeric loopback endpoint for contract fixtures, without proxies.
    pub fn for_loopback(gate: Gate<C>, origin: &str) -> Result<Self> {
        let origin = Url::parse(origin)?;
        if origin.scheme() != "http"
            || !origin.username().is_empty()
            || origin.password().is_some()
            || !origin.host_str().is_some_and(|s| {
                s.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            })
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err("Discord fixture origin must be numeric HTTP loopback".into());
        }
        Self::client(
            gate,
            format!("{}/api/v10", origin.as_str().trim_end_matches('/')),
            true,
        )
    }
    fn client(gate: Gate<C>, origin: String, local: bool) -> Result<Self> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(10)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36");
        if local {
            builder = builder.no_proxy();
        }
        Ok(Self {
            gate,
            http: builder.build()?,
            origin,
        })
    }
    pub async fn request(&self, request: Request, options: Options) -> Result<Response> {
        if !request.path.starts_with('/') || request.path.starts_with("//") {
            return Err("Discord path must be relative to the API".into());
        }
        let mut token = HeaderValue::from_str(&request.token)?;
        token.set_sensitive(true);
        let mut builder = self
            .http
            .request(request.method, format!("{}{}", self.origin, request.path))
            .header(AUTHORIZATION, token)
            .header("Content-Type", "application/json")
            .query(&request.params);
        if let Some(json) = request.json {
            builder = builder.json(&json);
        }
        let request = builder.build()?;
        let http = self.http.clone();
        self.gate
            .request(options, move || {
                let request = request.try_clone().expect("buffered JSON request");
                let http = http.clone();
                async move {
                    let response = http.execute(request).await?;
                    let status = response.status().as_u16();
                    let headers = response.headers().clone();
                    Ok(Response {
                        status,
                        headers,
                        body: response.bytes().await?.to_vec(),
                    })
                }
            })
            .await
    }
}
