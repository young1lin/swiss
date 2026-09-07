//! In-process MongoDB adapter.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use mongodb::bson::{self, doc, Bson, Document};
use mongodb::Client;
use serde_json::{json, Map, Value};

use super::direct::{def_bool, Lazy};
use super::mongo_resources::{client_lazy, parse_filter, readonly, MongoResources};
use super::resources::ResourceProvider;
use super::tool_server::{Engine, ServerMeta, ToolDef};
use crate::config::ServerDef;

const MAX_ROWS: i64 = 1000;

pub struct MongoEngine {
    def: ServerDef,
    name: Arc<RwLock<String>>,
    url: String,
    database: String,
    client: Arc<Lazy<Client>>,
    resources: Arc<MongoResources>,
}

impl MongoEngine {
    pub fn new(def: &ServerDef, name: &str) -> Result<Self, String> {
        let url = def.get_str("url").unwrap_or("").trim().to_string();
        if url.is_empty() {
            return Err("mongo url is required".into());
        }
        let database = def.get_str("database").unwrap_or("").trim().to_string();
        if database.is_empty() {
            return Err("mongo database is required".into());
        }
        let client = client_lazy(def);
        let resources = Arc::new(MongoResources::new(
            client.clone(),
            database.clone(),
            readonly(def),
        ));
        Ok(Self {
            def: def.clone(),
            name: Arc::new(RwLock::new(name.into())),
            url,
            database,
            client,
            resources,
        })
    }

    async fn db(&self) -> Result<mongodb::Database, String> {
        Ok(self.client.get().await?.database(&self.database))
    }

    async fn list_collections(&self, args: &Value) -> Result<Value, String> {
        crate::dbbrowser::MongoBrowser::list_collections(&*self.resources, args).await
    }

    async fn find(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        let filter = parse_filter(args.get("filterJson").or_else(|| args.get("filter")))?;
        let limit = integer(args.get("limit"), 100).clamp(1, MAX_ROWS);
        let skip = integer(args.get("skip"), 0).max(0) as u64;
        let cursor = self
            .db()
            .await?
            .collection::<Document>(&collection)
            .find(filter)
            .skip(skip)
            .limit(limit)
            .await
            .map_err(|e| e.to_string())?;
        let docs: Vec<Document> = futures_util::TryStreamExt::try_collect(cursor)
            .await
            .map_err(|e| e.to_string())?;
        let rows: Vec<Value> = docs
            .into_iter()
            .map(|d| bson::from_document::<Value>(d).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        Ok(json!({"rows": rows, "count": rows.len(), "collection": collection}))
    }

    async fn command(&self, args: &Value) -> Result<Value, String> {
        let command = args
            .get("command")
            .ok_or_else(|| "command is required".to_string())?;
        let document = to_document(command)?;
        let result = self
            .db()
            .await?
            .run_command(document)
            .await
            .map_err(|e| e.to_string())?;
        bson::from_document::<Value>(result).map_err(|e| e.to_string())
    }

    async fn insert(&self, args: &Value) -> Result<Value, String> {
        self.ensure_writable()?;
        let collection = required_str(args, "collection")?;
        let document = to_document(
            args.get("document")
                .or_else(|| args.get("doc"))
                .ok_or_else(|| "document is required".to_string())?,
        )?;
        let result = self
            .db()
            .await?
            .collection::<Document>(&collection)
            .insert_one(document)
            .await
            .map_err(|e| e.to_string())?;
        Ok(
            json!({"insertedId": bson::from_bson::<Value>(Bson::ObjectId(result.inserted_id.as_object_id().ok_or_else(|| "inserted id is not an ObjectId".to_string())?)).map_err(|e| e.to_string())?}),
        )
    }

    async fn update(&self, args: &Value) -> Result<Value, String> {
        self.ensure_writable()?;
        let collection = required_str(args, "collection")?;
        let filter = parse_filter(args.get("filterJson").or_else(|| args.get("filter")))?;
        let update = to_document(
            args.get("updateJson")
                .or_else(|| args.get("update"))
                .ok_or_else(|| "update is required".to_string())?,
        )?;
        let result = self
            .db()
            .await?
            .collection::<Document>(&collection)
            .update_many(filter, update)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({"matchedCount": result.matched_count, "modifiedCount": result.modified_count}))
    }

    async fn delete(&self, args: &Value) -> Result<Value, String> {
        self.ensure_writable()?;
        let collection = required_str(args, "collection")?;
        let filter = parse_filter(args.get("filterJson").or_else(|| args.get("filter")))?;
        let result = self
            .db()
            .await?
            .collection::<Document>(&collection)
            .delete_many(filter)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({"deletedCount": result.deleted_count}))
    }

    fn ensure_writable(&self) -> Result<(), String> {
        if def_bool(&self.def, "readonly") {
            Err("MongoDB adapter is read-only".into())
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl Engine for MongoEngine {
    fn kind(&self) -> &'static str {
        "mongo"
    }

    fn tools(&self) -> Vec<ToolDef> {
        let object = |properties: Map<String, Value>, required: Vec<&str>| json!({"type":"object", "properties":properties, "required":required});
        vec![
            ToolDef {
                name: "mongo_list_collections".into(),
                description: "List MongoDB collections, optionally filtered by name.".into(),
                input_schema: object(
                    {
                        let mut p = Map::new();
                        p.insert("grep".into(), json!({"type":"string"}));
                        p
                    },
                    vec![],
                ),
            },
            ToolDef {
                name: "mongo_find".into(),
                description: "Find documents in a MongoDB collection with a JSON filter.".into(),
                input_schema: object(
                    {
                        let mut p = Map::new();
                        p.insert("collection".into(), json!({"type":"string"}));
                        p.insert("filterJson".into(), json!({"type":"string"}));
                        p.insert("skip".into(), json!({"type":"integer"}));
                        p.insert("limit".into(), json!({"type":"integer"}));
                        p
                    },
                    vec!["collection"],
                ),
            },
            ToolDef {
                name: "mongo_command".into(),
                description: "Run a MongoDB command and return its JSON result.".into(),
                input_schema: object(
                    {
                        let mut p = Map::new();
                        p.insert("command".into(), json!({"type":"object"}));
                        p
                    },
                    vec!["command"],
                ),
            },
            ToolDef {
                name: "mongo_insert".into(),
                description: "Insert one MongoDB document.".into(),
                input_schema: object(
                    {
                        let mut p = Map::new();
                        p.insert("collection".into(), json!({"type":"string"}));
                        p.insert("document".into(), json!({"type":"object"}));
                        p
                    },
                    vec!["collection", "document"],
                ),
            },
            ToolDef {
                name: "mongo_update".into(),
                description: "Update MongoDB documents matching a JSON filter.".into(),
                input_schema: object(
                    {
                        let mut p = Map::new();
                        p.insert("collection".into(), json!({"type":"string"}));
                        p.insert("filterJson".into(), json!({"type":"string"}));
                        p.insert("updateJson".into(), json!({"type":"string"}));
                        p
                    },
                    vec!["collection", "updateJson"],
                ),
            },
            ToolDef {
                name: "mongo_delete".into(),
                description: "Delete MongoDB documents matching a JSON filter.".into(),
                input_schema: object(
                    {
                        let mut p = Map::new();
                        p.insert("collection".into(), json!({"type":"string"}));
                        p.insert("filterJson".into(), json!({"type":"string"}));
                        p
                    },
                    vec!["collection"],
                ),
            },
        ]
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "mongo_list_collections" => self.list_collections(args).await,
            "mongo_find" => self.find(args).await,
            "mongo_command" => self.command(args).await,
            "mongo_insert" => self.insert(args).await,
            "mongo_update" => self.update(args).await,
            "mongo_delete" => self.delete(args).await,
            _ => Err(format!("unknown tool: {tool}")),
        }
    }

    fn meta(&self) -> ServerMeta {
        ServerMeta {
            name: self.name.read().ok().map(|v| v.clone()),
            description: self.def.get_str("description").map(str::to_string),
            target: Some(format!("{} @ {}", self.database, self.url_host())),
            limits: None,
        }
    }

    fn resources(&self) -> Option<Arc<dyn ResourceProvider>> {
        Some(self.resources.clone())
    }

    fn browser(&self) -> Option<crate::dbbrowser_api::BrowserFlavor> {
        Some(crate::dbbrowser_api::BrowserFlavor::Mongo(
            self.resources.browser(),
        ))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let result = match self.db().await {
            Ok(db) => db
                .run_command(doc! {"ping": 1})
                .await
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Err(err) => Err(err),
        };
        Some(result)
    }

    async fn close(&self) {
        self.client.dispose(|_| Box::pin(async {})).await;
    }

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.into();
        }
    }
}

impl MongoEngine {
    fn url_host(&self) -> String {
        self.url
            .split("//")
            .nth(1)
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or("")
            .to_string()
    }
}

fn required_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("{key} is required"))
}
fn integer(value: Option<&Value>, fallback: i64) -> i64 {
    value
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(fallback)
}
fn to_document(value: &Value) -> Result<Document, String> {
    match bson::to_bson(value).map_err(|e| e.to_string())? {
        Bson::Document(d) => Ok(d),
        _ => Err("MongoDB value must be an object".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_the_mongo_tool_surface() {
        let def = ServerDef(
            json!({"type": "mongo", "url": "mongodb://127.0.0.1:27017", "database": "app"})
                .as_object()
                .cloned()
                .unwrap(),
        );
        let engine = MongoEngine::new(&def, "m").expect("valid definition");
        let names: Vec<String> = engine.tools().into_iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            vec![
                "mongo_list_collections",
                "mongo_find",
                "mongo_command",
                "mongo_insert",
                "mongo_update",
                "mongo_delete"
            ]
        );
    }

    #[test]
    fn rejects_empty_mongo_url() {
        let def = ServerDef(json!({"type": "mongo"}).as_object().cloned().unwrap());
        assert_eq!(
            MongoEngine::new(&def, "m").err().as_deref(),
            Some("mongo url is required")
        );
    }
}
