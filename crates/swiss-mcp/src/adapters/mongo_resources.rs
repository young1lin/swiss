/*
 * Copyright 2026 young1lin
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! MongoDB as MCP resources (SPEC §mcp.db): the instance overview, one entry per database, one
//! per collection of the connection's own database, and a template for any other collection.
//! A collection's body is mongo_describe_collection's answer — the shape a client attaches with
//! `@` before it writes a query. Nothing is cached (resources.rs).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::direct::Lazy;
use super::mongo::{describe_collection, Ejson, MongoHandle, DESCRIBE_SAMPLE_DEFAULT};
use super::resources::{
    human_bytes, page, split_uri, ResourceBody, ResourceEntry, ResourceFault, ResourcePage,
    ResourceProvider, ResourceTemplate,
};

pub struct MongoResources {
    label: String,
    conn: Arc<Lazy<MongoHandle>>,
}

/// `db` or `db/collection` — a database name never holds a `/`, so the first one splits.
fn split_rest(rest: &str) -> (String, Option<String>) {
    match rest.split_once('/') {
        Some((db, coll)) if !coll.is_empty() => (db.to_string(), Some(coll.to_string())),
        Some((db, _)) => (db.to_string(), None),
        None => (rest.to_string(), None),
    }
}

impl MongoResources {
    pub fn new(label: String, conn: Arc<Lazy<MongoHandle>>) -> Self {
        Self { label, conn }
    }

    fn base(&self) -> String {
        format!("mongo://{}", self.label)
    }

    fn json(v: &Value) -> Result<String, ResourceFault> {
        serde_json::to_string_pretty(v).map_err(|e| ResourceFault(e.to_string()))
    }

    async fn handle(&self) -> Result<Arc<MongoHandle>, ResourceFault> {
        self.conn.get().await.map_err(ResourceFault)
    }

    async fn overview(&self) -> Result<String, ResourceFault> {
        let handle = self.handle().await?;
        let dbs = handle.databases().await.map_err(ResourceFault)?;
        let rows: Vec<Value> = dbs
            .iter()
            .map(|(name, size, empty)| {
                let mut row = json!({ "database": name });
                if let Some(s) = size {
                    row["size"] = json!(human_bytes((*s).max(0) as u64));
                }
                if *empty {
                    row["empty"] = json!(true);
                }
                row
            })
            .collect();
        let facts = handle.server_facts().await.ok();
        Self::json(&json!({
            "server": facts.as_ref().map(|f| json!({ "version": f.version, "topology": f.topology, "setName": f.set_name })),
            "defaultDatabase": handle.default_db(),
            "databases": rows,
            "notes": [
                format!("Read {}/<database> for its collections, and {}/<database>/<collection> for a collection's shape, indexes and size.", self.base(), self.base()),
            ],
        }))
    }

    async fn database(&self, db: &str) -> Result<String, ResourceFault> {
        swiss_host::mongobrowser::assert_db_name(db).map_err(ResourceFault)?;
        let handle = self.handle().await?;
        let (rows, omitted) = handle.collections(db, true, Ejson::Relaxed).await.map_err(ResourceFault)?;
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let mut out = json!({ "collection": r.get("name"), "type": r.get("type") });
                if let Some(n) = r.get("count") {
                    out["documents"] = n.clone();
                }
                if let Some(n) = r.get("size").and_then(Value::as_i64) {
                    out["size"] = json!(human_bytes(n.max(0) as u64));
                }
                if let Some(n) = r.get("indexes") {
                    out["indexes"] = n.clone();
                }
                if let Some(v) = r.get("viewOn") {
                    out["viewOn"] = v.clone();
                }
                out
            })
            .collect();
        let mut out = json!({ "database": db, "collections": rows });
        if omitted > 0 {
            out["statsOmitted"] = json!(omitted);
        }
        Self::json(&out)
    }
}

#[async_trait]
impl ResourceProvider for MongoResources {
    async fn list(&self, cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        let handle = self.handle().await?;
        let dbs = handle.databases().await.map_err(ResourceFault)?;
        // The flat list a picker pages through: every database, then the collections of the
        // connection's own database (other databases' collections are one template away).
        let mut entries: Vec<ResourceEntry> = dbs
            .iter()
            .map(|(name, size, _)| ResourceEntry {
                uri: format!("{}/{name}", self.base()),
                name: name.clone(),
                description: Some(match size {
                    Some(s) => format!("Database {name} ({}): its collections, live", human_bytes((*s).max(0) as u64)),
                    None => format!("Database {name}: its collections, live"),
                }),
                mime_type: Some("application/json".into()),
            })
            .collect();
        if let Some(db) = handle.default_db() {
            let (rows, _) = handle.collections(db, false, Ejson::Relaxed).await.map_err(ResourceFault)?;
            for r in rows {
                let Some(name) = r.get("name").and_then(Value::as_str) else { continue };
                let kind = r.get("type").and_then(Value::as_str).unwrap_or("collection");
                entries.push(ResourceEntry {
                    uri: format!("{}/{db}/{name}", self.base()),
                    name: format!("{db}.{name}"),
                    description: Some(format!("{kind}: inferred shape, indexes and size")),
                    mime_type: Some("application/json".into()),
                });
            }
        }
        let (slice, next_cursor) = page(&entries, cursor)?;
        let mut resources: Vec<ResourceEntry> = Vec::new();
        if cursor.is_none() {
            resources.push(ResourceEntry {
                uri: self.base(),
                name: self.label.clone(),
                description: Some(format!("MongoDB overview: {} databases, live", dbs.len())),
                mime_type: Some("application/json".into()),
            });
        }
        resources.extend(slice.into_iter().cloned());
        Ok(ResourcePage { resources, next_cursor })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate {
            uri_template: format!("{}/{{database}}/{{collection}}", self.base()),
            name: format!("{} collection", self.label),
            description: Some(
                "Inferred document shape (field paths, types, examples), indexes and size of any collection.".into(),
            ),
            mime_type: Some("application/json".into()),
        }]
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        let (authority, rest) = split_uri(uri, "mongo")?;
        if authority != self.label {
            return Err(ResourceFault(format!("this MCP is {}, not {authority}", self.label)));
        }
        let text = match rest.as_deref().map(split_rest) {
            None => self.overview().await?,
            Some((db, None)) => self.database(&db).await?,
            Some((db, Some(coll))) => {
                let handle = self.handle().await?;
                let body = describe_collection(&handle, &db, &coll, DESCRIBE_SAMPLE_DEFAULT)
                    .await
                    .map_err(ResourceFault)?;
                Self::json(&body)?
            }
        };
        Ok(vec![ResourceBody { uri: uri.to_string(), mime_type: Some("application/json".into()), text }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_of_a_uri_splits_into_database_and_collection() {
        assert_eq!(split_rest("shop"), ("shop".into(), None));
        assert_eq!(split_rest("shop/"), ("shop".into(), None));
        assert_eq!(split_rest("shop/orders"), ("shop".into(), Some("orders".into())));
        // A collection name may itself hold a dot or a slash; only the first slash splits.
        assert_eq!(split_rest("shop/a.b/c"), ("shop".into(), Some("a.b/c".into())));
    }
}
