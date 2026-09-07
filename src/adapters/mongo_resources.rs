//! MongoDB resources and the read-only Data browser.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use mongodb::bson::{self, doc, Bson, Document};
use mongodb::Client;
use serde_json::{json, Value};

use super::direct::{def_bool, BoxFut, Lazy};
use super::resources::{
    ResourceBody, ResourceEntry, ResourceFault, ResourcePage, ResourceProvider, ResourceTemplate,
};
use crate::config::ServerDef;
use crate::dbbrowser::MongoBrowser;

const MAX_ROWS: i64 = 1000;

pub struct MongoResources {
    client: Arc<Lazy<Client>>,
    database: String,
    readonly: bool,
}

impl MongoResources {
    pub fn new(client: Arc<Lazy<Client>>, database: String, readonly: bool) -> Self {
        Self {
            client,
            database,
            readonly,
        }
    }

    async fn db(&self) -> Result<mongodb::Database, String> {
        let client = self.client.get().await?;
        if self.database.is_empty() {
            return Err("mongo database is required".into());
        }
        Ok(client.database(&self.database))
    }

    async fn names(&self) -> Result<Vec<String>, String> {
        self.db()
            .await?
            .list_collection_names()
            .await
            .map_err(|e| e.to_string())
    }

    async fn find_documents(
        &self,
        collection: &str,
        filter: Document,
        skip: u64,
        limit: i64,
    ) -> Result<Vec<Value>, String> {
        if collection.trim().is_empty() {
            return Err("collection is required".into());
        }
        let cursor = self
            .db()
            .await?
            .collection::<Document>(collection)
            .find(filter)
            .skip(skip)
            .limit(limit)
            .await
            .map_err(|e| e.to_string())?;
        let docs: Vec<Document> = cursor.try_collect().await.map_err(|e| e.to_string())?;
        docs.into_iter()
            .map(|d| bson::from_document::<Value>(d).map_err(|e| e.to_string()))
            .collect()
    }

    pub fn browser(self: &Arc<Self>) -> Arc<dyn MongoBrowser> {
        self.clone()
    }
}

#[async_trait]
impl ResourceProvider for MongoResources {
    async fn list(&self, cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        let names = self.names().await.map_err(ResourceFault)?;
        let start = cursor
            .unwrap_or("0")
            .parse::<usize>()
            .map_err(|_| ResourceFault("invalid cursor".into()))?;
        if start > names.len() {
            return Err(ResourceFault("cursor is past the end of the list".into()));
        }
        let end = (start + 200).min(names.len());
        let resources = names[start..end]
            .iter()
            .map(|name| ResourceEntry {
                uri: format!("mongo://{}/{}", self.database, name),
                name: name.clone(),
                description: Some(format!("MongoDB collection {name}")),
                mime_type: Some("application/json".into()),
            })
            .collect();
        Ok(ResourcePage {
            resources,
            next_cursor: (end < names.len()).then(|| end.to_string()),
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        let rest = uri
            .strip_prefix("mongo://")
            .ok_or_else(|| ResourceFault("invalid mongo resource URI".into()))?;
        let (database, collection) = rest
            .split_once('/')
            .ok_or_else(|| ResourceFault("invalid mongo resource URI".into()))?;
        if database != self.database || collection.is_empty() {
            return Err(ResourceFault("unknown mongo resource".into()));
        }
        let rows = self
            .find_documents(collection, doc! {}, 0, MAX_ROWS)
            .await
            .map_err(ResourceFault)?;
        let text = serde_json::to_string_pretty(&rows).map_err(|e| ResourceFault(e.to_string()))?;
        Ok(vec![ResourceBody {
            uri: uri.into(),
            mime_type: Some("application/json".into()),
            text,
        }])
    }
}

#[async_trait]
impl MongoBrowser for MongoResources {
    fn readonly(&self) -> bool {
        self.readonly
    }

    fn label(&self) -> String {
        if self.database.is_empty() {
            "MongoDB".into()
        } else {
            format!("MongoDB database {}", self.database)
        }
    }

    async fn list_collections(&self, options: &Value) -> Result<Value, String> {
        let grep = options
            .get("grep")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let mut names = self.names().await?;
        if !grep.is_empty() {
            names.retain(|n| n.to_lowercase().contains(&grep));
        }
        Ok(Value::Array(
            names
                .into_iter()
                .map(|name| json!({"name": name}))
                .collect(),
        ))
    }

    async fn read_collection(&self, options: &Value) -> Result<Value, String> {
        let collection = options
            .get("collection")
            .and_then(Value::as_str)
            .unwrap_or("");
        let filter = parse_filter(options.get("filterJson"))?;
        let offset = number(options.get("offset"), 0).max(0) as u64;
        let limit = number(options.get("limit"), 100).clamp(1, MAX_ROWS);
        let rows = self
            .find_documents(collection, filter, offset, limit)
            .await?;
        Ok(
            json!({"rows": rows, "offset": offset, "limit": limit, "hasMore": rows.len() as i64 == limit}),
        )
    }
}

pub(crate) fn parse_filter(value: Option<&Value>) -> Result<Document, String> {
    let Some(value) = value else {
        return Ok(Document::new());
    };
    let parsed = match value {
        Value::Object(_) => value.clone(),
        Value::String(text) if !text.trim().is_empty() => {
            serde_json::from_str(text).map_err(|e| format!("invalid filter JSON: {e}"))?
        }
        Value::String(_) | Value::Null => return Ok(Document::new()),
        _ => return Err("mongo filter must be a JSON object".into()),
    };
    match bson::to_bson(&parsed).map_err(|e| e.to_string())? {
        Bson::Document(document) => Ok(document),
        _ => Err("mongo filter must be a JSON object".into()),
    }
}

fn number(value: Option<&Value>, fallback: i64) -> i64 {
    value
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(fallback)
}

pub fn client_lazy(def: &ServerDef) -> Arc<Lazy<Client>> {
    let uri = def.get_str("url").unwrap_or("").to_string();
    Arc::new(Lazy::new(move || {
        let uri = uri.clone();
        Box::pin(async move { Client::with_uri_str(&uri).await.map_err(|e| e.to_string()) })
            as BoxFut<Result<Client, String>>
    }))
}

pub fn readonly(def: &ServerDef) -> bool {
    def_bool(def, "readonly")
}
