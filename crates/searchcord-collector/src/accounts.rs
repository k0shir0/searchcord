//! Account selection and sealed credentials share one serialization boundary.
use crate::{
    Result,
    credentials::{Paths, Vault, metadata},
    discovery::http_error,
    scrapes::new_id,
    writer::Writer,
};
use rusqlite::params;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
#[derive(Clone)]
pub struct Accounts {
    path: PathBuf,
    writer: Writer,
    lock: Arc<Mutex<()>>,
    paths: Option<Paths>,
}
impl Accounts {
    pub fn new(path: PathBuf, writer: Writer) -> Self {
        Self {
            path,
            writer,
            lock: Arc::new(Mutex::new(())),
            paths: None,
        }
    }
    pub fn with_paths(paths: Paths, writer: Writer) -> Self {
        Self {
            path: paths.database.clone(),
            writer,
            lock: Arc::new(Mutex::new(())),
            paths: Some(paths),
        }
    }
    fn vault_paths(&self) -> Result<Paths> {
        Ok(match &self.paths {
            Some(paths) => paths.clone(),
            None => Paths::for_database(&self.path)?,
        })
    }
    async fn load(&self) -> Result<Value> {
        let paths = self.vault_paths()?;
        tokio::task::spawn_blocking(move || -> Result<Value> {
            Vault::platform(paths).load().map_err(|_| {
                http_error(
                    503,
                    "Could not unlock saved credentials; the credential vault was kept",
                )
            })
        })
        .await?
    }
    pub async fn selected(&self) -> Result<(String, String)> {
        let _guard = self.lock.lock().await;
        let state = self.load().await?;
        let tokens = state["tokens"].as_array().ok_or("Invalid vault state")?;
        let active = state["active_token_id"]
            .as_str()
            .or_else(|| tokens.first()?.get("id")?.as_str());
        let selected = tokens.iter().find(|t| t["id"].as_str() == active);
        match selected {
            Some(t) => Ok((
                t["id"].as_str().unwrap().into(),
                t["token"].as_str().unwrap().into(),
            )),
            None => Err(http_error(401, "No token configured")),
        }
    }
    pub async fn settings(&self) -> Result<Value> {
        let _guard = self.lock.lock().await;
        let state = self.load().await?;
        let tokens = state["tokens"].as_array().ok_or("Invalid vault state")?;
        let metadata = metadata(tokens);
        let active = state["active_token_id"]
            .as_str()
            .or_else(|| tokens.first()?.get("id")?.as_str())
            .map(str::to_owned);
        self.writer.write(move|store|{let mut result=json!({});let mut q=store.connection().prepare("SELECT key,value FROM settings WHERE key NOT IN ('token','saved_tokens','active_token_id')")?;for row in q.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?{let(k,v)=row?;result[k]=json!(v);}result["tokens"]=metadata;result["active_token_id"]=json!(active);result["token_set"]=json!(!tokens_empty(&result));Ok(result)}).await
    }
    pub async fn update(&self, input: Value) -> Result<Value> {
        let this = self.clone();
        tokio::spawn(async move { this.update_inner(input).await }).await?
    }
    async fn update_inner(&self, input: Value) -> Result<Value> {
        let token = input["token"].as_str();
        let selected = input["active_token_id"].as_str();
        if token.is_none() == selected.is_none() {
            return Err(http_error(400, "Provide a token or a saved token ID"));
        }
        let token = token.map(str::trim);
        let label = input["token_label"].as_str().unwrap_or("").trim();
        if token.is_some_and(|s| s.is_empty() || s.chars().count() > 4096) {
            return Err(http_error(400, "Token must contain 1 to 4096 characters"));
        }
        if label.chars().count() > 40 {
            return Err(http_error(400, "Token name must be at most 40 characters"));
        }
        let _guard = self.lock.lock().await;
        let mut state = self.load().await?;
        let tokens = state["tokens"]
            .as_array_mut()
            .ok_or("Invalid vault state")?;
        let active = if let Some(token) = token {
            if let Some(existing) = tokens.iter_mut().find(|t| t["token"] == token) {
                if !label.is_empty() {
                    existing["label"] = json!(label);
                }
                existing["id"].as_str().unwrap().to_owned()
            } else {
                let id = new_id()?.replace('-', "");
                tokens.push(json!({"id":id,"token":token,"label":if label.is_empty(){format!("Token {}",tokens.len()+1)}else{label.into()}}));
                id
            }
        } else {
            let selected = selected.unwrap();
            if !tokens.iter().any(|t| t["id"] == selected) {
                return Err(http_error(404, "Saved token not found"));
            }
            selected.to_owned()
        };
        let tokens = json!(tokens);
        let paths = self.vault_paths()?;
        let id = active.clone();
        self.writer
            .write(move |store| {
                let tx = store
                    .connection_mut()
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                Vault::platform(paths)
                    .save(&tokens, &json!(id))
                    .map_err(|_| {
                        http_error(
                            503,
                            "Could not save credentials; the credential vault was kept",
                        )
                    })?;
                let public = metadata(tokens.as_array().unwrap());
                tx.execute(
                    "INSERT OR REPLACE INTO settings VALUES ('saved_tokens',?)",
                    [public.to_string()],
                )?;
                tx.execute(
                    "INSERT OR REPLACE INTO settings VALUES ('active_token_id',?)",
                    [&id],
                )?;
                tx.execute("DELETE FROM settings WHERE key='token'", [])?;
                tx.commit()?;
                Ok(json!({"ok":true,"active_token_id":id,"tokens":public}))
            })
            .await
    }
    pub async fn clear(&self) -> Result<Value> {
        let this = self.clone();
        tokio::spawn(async move{let _guard=this.lock.lock().await;let paths=this.vault_paths()?;this.writer.write(move|store|{Vault::platform(paths).clear()?;store.connection().execute("DELETE FROM settings WHERE key IN ('token','saved_tokens','active_token_id')",params![])?;Ok(json!({"ok":true,"tokens":[],"active_token_id":null,"token_set":false}))}).await.map_err(|_|http_error(503,"Could not remove saved tokens; retry when storage is available"))}).await?
    }
}
fn tokens_empty(settings: &Value) -> bool {
    settings["tokens"].as_array().is_none_or(Vec::is_empty)
}
