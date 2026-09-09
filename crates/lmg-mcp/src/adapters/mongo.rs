//! In-process MongoDB adapter — port of `adapters/mongo.ts`.
//!
//! Read-only is enforced by tool presence (the three write tools drop out of tools/list) plus a
//! pipeline guard that refuses `$out`/`$merge` — Mongo has no equivalent of Postgres'
//! `default_transaction_read_only`, so unlike SQL there is no session-level boundary to lean on.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures_util::TryStreamExt;
use mongodb::bson::{doc, Bson, Document};
use serde_json::{json, Map, Value};

use super::mongo_resources::{
    bson_plain, client_lazy, describe_collection, document_plain, parse_filter, readonly,
    MongoLike, MongoResources,
};
use super::resources::{human_bytes, ResourceProvider};
use super::sql::{clamp_row_limit, drop_null_columns, DEFAULT_ROW_LIMIT, MAX_ROW_LIMIT};
use super::tool_server::{Engine, ServerMeta, ToolDef};
use lmg_host::config::ServerDef;

/// At most this many documents per `mongo_insert_many` call.
const MAX_INSERT_DOCS: usize = 1000;

/// Stage operators that write to a collection, so an aggregate can write even though it looks
/// like a read.
const WRITE_STAGES: &[&str] = &["$out", "$merge"];

/// True if an aggregation pipeline contains a writing stage. Pure, so it is unit-testable
/// without a DB.
pub fn writes_via_aggregate(pipeline: &Value) -> bool {
    let Some(stages) = pipeline.as_array() else {
        return false;
    };
    stages.iter().any(|stage| match stage {
        Value::Object(map) => map.keys().any(|k| WRITE_STAGES.contains(&k.as_str())),
        _ => false,
    })
}

pub struct MongoEngine {
    def: ServerDef,
    name: Arc<RwLock<String>>,
    url: String,
    database: String,
    readonly: bool,
    resources: Arc<MongoResources>,
}

impl MongoEngine {
    pub fn new(def: &ServerDef, name: &str) -> Result<Self, String> {
        let url = def.get_str("url").unwrap_or("").trim().to_string();
        if url.is_empty() {
            return Err("mongo url is required".into());
        }
        // An explicit `database` on the def, else the URI's path — the Node build accepts both, and
        // `mongodb://host/app` is the form most connection strings are copied in.
        let database = match def
            .get_str("database")
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(explicit) => explicit.to_string(),
            None => database_from_uri(&url),
        };
        if database.is_empty() {
            return Err(
                "mongo database is required (set `database`, or put it in the url path)".into(),
            );
        }
        let readonly = readonly(def);
        let client = client_lazy(def);
        let resources = Arc::new(MongoResources::new(client, database.clone(), readonly));
        Ok(Self {
            def: def.clone(),
            name: Arc::new(RwLock::new(name.into())),
            url,
            database,
            readonly,
            resources,
        })
    }

    async fn db(&self) -> Result<mongodb::Database, String> {
        self.resources.db().await
    }

    // --- the tools ---------------------------------------------------------------------------

    async fn find(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        let requested = args.get("limit").filter(|v| !v.is_null());
        let limit = clamp_row_limit(requested, DEFAULT_ROW_LIMIT);
        let filter = parse_filter(args.get("filter").or_else(|| args.get("filterJson")))?;
        let db = self.db().await?;
        let coll = db.collection::<Document>(&collection);
        let mut find = coll.find(filter).limit(limit);
        if let Some(projection) = args.get("projection").filter(|v| !v.is_null()) {
            find = find.projection(to_document(projection, "projection")?);
        }
        if let Some(sort) = args.get("sort").filter(|v| !v.is_null()) {
            find = find.sort(to_document(sort, "sort")?);
        }
        if let Some(skip) = args.get("skip").filter(|v| !v.is_null()) {
            find = find.skip(integer(Some(skip), 0).max(0) as u64);
        }
        let cursor = find.await.map_err(|e| e.to_string())?;
        let docs: Vec<Document> = cursor.try_collect().await.map_err(|e| e.to_string())?;
        let found = docs.len() as i64;
        let documents = drop_null_columns(docs.iter().map(document_plain).collect());
        // Only an UNREQUESTED limit that actually bit is worth a note; an explicit one is the
        // caller's own ceiling and saying so back is noise.
        if requested.is_none() && found >= limit {
            return Ok(json!({
                "documents": documents,
                "note": format!("no limit requested, so a default limit of {limit} was applied — there may be more documents."),
            }));
        }
        Ok(json!({ "documents": documents }))
    }

    async fn aggregate(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        let pipeline = args
            .get("pipeline")
            .ok_or_else(|| "pipeline must be an array of stages".to_string())?;
        let Some(stages) = pipeline.as_array() else {
            return Err("pipeline must be an array of stages".into());
        };
        if self.readonly && writes_via_aggregate(pipeline) {
            return Err(
                "refused: this mongo MCP is readonly and the pipeline writes ($out/$merge)".into(),
            );
        }
        let stages: Vec<Document> = stages
            .iter()
            .map(|s| to_document(s, "pipeline stage"))
            .collect::<Result<_, _>>()?;
        let cursor = self
            .db()
            .await?
            .collection::<Document>(&collection)
            .aggregate(stages)
            .await
            .map_err(|e| e.to_string())?;
        let docs: Vec<Document> = cursor.try_collect().await.map_err(|e| e.to_string())?;
        let rows: Vec<Value> = docs.iter().map(document_plain).collect();
        if rows.len() as i64 > MAX_ROW_LIMIT {
            return Ok(json!({
                "result": rows[..MAX_ROW_LIMIT as usize],
                "note": format!("result capped at {MAX_ROW_LIMIT} rows — there may be more."),
            }));
        }
        Ok(json!({ "result": rows }))
    }

    async fn list_collections(&self) -> Result<Value, String> {
        let mut stats = self.resources.collections().await?;
        // Largest first, ties by name — the order that puts "what needs a limit" at the top.
        stats.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        Ok(Value::Array(
            stats
                .into_iter()
                .map(|s| {
                    let mut row = Map::new();
                    row.insert("name".into(), json!(s.name));
                    row.insert("type".into(), json!(s.type_));
                    if s.count > 0 {
                        row.insert("count".into(), json!(s.count));
                    }
                    if s.bytes > 0 {
                        row.insert("size".into(), json!(human_bytes(s.bytes)));
                    }
                    Value::Object(row)
                })
                .collect(),
        ))
    }

    async fn describe_collection(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        describe_collection(self.resources.as_ref(), &self.database, &collection).await
    }

    async fn insert_many(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        let Some(Value::Array(documents)) = args.get("documents") else {
            return Err("documents must be an array".into());
        };
        if documents.is_empty() {
            return Err("documents must not be empty".into());
        }
        if documents.len() > MAX_INSERT_DOCS {
            return Err(format!(
                "documents must be at most {MAX_INSERT_DOCS} per call"
            ));
        }
        let docs: Vec<Document> = documents
            .iter()
            .map(|d| to_document(d, "document"))
            .collect::<Result<_, _>>()?;
        let db = self.db().await?;
        let coll = db.collection::<Document>(&collection);
        let mut insert = coll.insert_many(docs);
        if args.get("ordered") == Some(&Value::Bool(false)) {
            insert = insert.ordered(false);
        }
        let result = insert.await.map_err(|e| e.to_string())?;
        // A BTreeMap keyed by input index: values() is already in insertion order, like
        // Object.values(insertedIds) over the driver's integer-keyed object.
        let ids: Vec<Value> = result.inserted_ids.values().map(bson_plain).collect();
        Ok(json!({ "insertedCount": ids.len(), "insertedIds": ids }))
    }

    async fn update_many(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        let filter = parse_filter(args.get("filter").or_else(|| args.get("filterJson")))?;
        let update = args
            .get("update")
            .or_else(|| args.get("updateJson"))
            .ok_or_else(|| "update is required".to_string())?;
        // A document ({ $set: … }) or an aggregation pipeline — the driver accepts both, so the
        // tool does too rather than making the caller guess which shape this adapter wants.
        let modifications: mongodb::options::UpdateModifications = match update {
            Value::Array(stages) => mongodb::options::UpdateModifications::Pipeline(
                stages
                    .iter()
                    .map(|s| to_document(s, "update pipeline stage"))
                    .collect::<Result<_, _>>()?,
            ),
            other => mongodb::options::UpdateModifications::Document(to_document(other, "update")?),
        };
        let db = self.db().await?;
        let coll = db.collection::<Document>(&collection);
        let mut update_call = coll.update_many(filter, modifications);
        if args.get("upsert").and_then(Value::as_bool) == Some(true) {
            update_call = update_call.upsert(true);
        }
        let result = update_call.await.map_err(|e| e.to_string())?;
        let mut out = Map::new();
        out.insert("matchedCount".into(), json!(result.matched_count));
        out.insert("modifiedCount".into(), json!(result.modified_count));
        if let Some(id) = &result.upserted_id {
            out.insert("upsertedId".into(), bson_plain(id));
        }
        Ok(Value::Object(out))
    }

    async fn delete_many(&self, args: &Value) -> Result<Value, String> {
        let collection = required_str(args, "collection")?;
        let filter = parse_filter(args.get("filter").or_else(|| args.get("filterJson")))?;
        let result = self
            .db()
            .await?
            .collection::<Document>(&collection)
            .delete_many(filter)
            .await
            .map_err(|e| e.to_string())?;
        Ok(json!({ "deletedCount": result.deleted_count }))
    }
}

// --- the tool schemas --------------------------------------------------------------------------

fn collection_arg() -> Value {
    json!({
        "type": "string",
        "description": "Collection name. Passed to the driver as a value, so it needs no escaping.",
    })
}

fn object_schema(properties: Vec<(&str, Value)>, required: Vec<&str>) -> Value {
    let mut props = Map::new();
    for (k, v) in properties {
        props.insert(k.into(), v);
    }
    json!({ "type": "object", "properties": props, "required": required })
}

/// The read + discovery tools, always present. Mirrors the pg trio's intent — find (the common
/// lookup), aggregate (the one Mongo needs that SQL does not: grouping, `$lookup` joins,
/// `$unwind`), and the two things a model cannot guess: what collections exist (with sizes) and
/// what shape one has.
///
/// No separate indexes/count/distinct tool — each is an aggregate the model can write itself, and
/// every tool schema is re-sent on every request across endpoints.
fn read_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "mongo_find".into(),
            description: format!(
                "Query a collection with a filter. Returns {{ documents }}. Projection narrows the \
                 fields; sort and skip page through results. A find with no `limit` is capped at \
                 {DEFAULT_ROW_LIMIT} documents and says so in the reply — raise `limit` (max \
                 {MAX_ROW_LIMIT}) to read more. NULL/absent fields are dropped from each document; \
                 run mongo_describe_collection for the full field picture."
            ),
            input_schema: object_schema(
                vec![
                    ("collection", collection_arg()),
                    ("filter", json!({"type":"object","description":"A MongoDB query document, e.g. { status: 'active', age: { $gte: 18 } }. Defaults to {}."})),
                    ("projection", json!({"type":"object","description":"Fields to include/exclude, e.g. { email: 1, _id: 0 }."})),
                    ("sort", json!({"type":"object","description":"Sort spec, e.g. { createdAt: -1 }."})),
                    ("limit", json!({"type":"number","description":format!("Document cap (default {DEFAULT_ROW_LIMIT}, max {MAX_ROW_LIMIT}).")})),
                    ("skip", json!({"type":"number","description":"Documents to skip (offset)."})),
                ],
                vec!["collection"],
            ),
        },
        ToolDef {
            name: "mongo_aggregate".into(),
            description: format!(
                "Run an aggregation pipeline — grouping, $lookup joins, $unwind, $project \
                 reshaping. Returns {{ result }}. Output is capped at {MAX_ROW_LIMIT} rows and says \
                 so if it bit. When this MCP is readonly, $out and $merge stages are refused (they \
                 write)."
            ),
            input_schema: object_schema(
                vec![
                    ("collection", collection_arg()),
                    ("pipeline", json!({"type":"array","description":"Pipeline stages, e.g. [{ $match: { ... } }, { $group: { _id: '$x', n: { $sum: 1 } } }]."})),
                ],
                vec!["collection", "pipeline"],
            ),
        },
        ToolDef {
            name: "mongo_list_collections".into(),
            description: "List collections with their type, approximate document count and \
                          on-disk size — use this before querying, to know what exists and what \
                          is big enough to need a limit."
                .into(),
            input_schema: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "mongo_describe_collection".into(),
            description: "Shape of one collection: indexes, the $jsonSchema validator if any, and \
                          a field schema inferred from a 100-document sample (field, dominant \
                          type, prevalence). Mongo is schemaless, so this is a best-effort \
                          picture, not a constraint."
                .into(),
            input_schema: object_schema(
                vec![("collection", collection_arg())],
                vec!["collection"],
            ),
        },
    ]
}

/// Write tools, present only when the MCP is not readonly (hidden from tools/list otherwise).
fn write_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "mongo_insert_many".into(),
            description: format!(
                "Insert documents into a collection. Returns {{ insertedCount, insertedIds }}. At \
                 most {MAX_INSERT_DOCS} documents per call. An empty `documents` array is an \
                 error, not a no-op."
            ),
            input_schema: object_schema(
                vec![
                    ("collection", collection_arg()),
                    (
                        "documents",
                        json!({"type":"array","description":"Documents to insert.","items":{}}),
                    ),
                    (
                        "ordered",
                        json!({"type":"boolean","description":"Stop on first error (default true)."}),
                    ),
                ],
                vec!["collection", "documents"],
            ),
        },
        ToolDef {
            name: "mongo_update_many".into(),
            description: "Update every document matching `filter`. Returns { matchedCount, \
                          modifiedCount, upsertedId? }. An empty filter updates the whole \
                          collection — state that explicitly when you mean it."
                .into(),
            input_schema: object_schema(
                vec![
                    ("collection", collection_arg()),
                    (
                        "filter",
                        json!({"type":"object","description":"Which documents to update."}),
                    ),
                    (
                        "update",
                        json!({"type":"object","description":"Update document or aggregation pipeline, e.g. { $set: { status: 'x' } }."}),
                    ),
                    (
                        "upsert",
                        json!({"type":"boolean","description":"Insert a document when none match (default false)."}),
                    ),
                ],
                vec!["collection", "filter", "update"],
            ),
        },
        ToolDef {
            name: "mongo_delete_many".into(),
            description: "Delete every document matching `filter`. Returns { deletedCount }. An \
                          empty filter deletes the whole collection — state that explicitly when \
                          you mean it."
                .into(),
            input_schema: object_schema(
                vec![
                    ("collection", collection_arg()),
                    (
                        "filter",
                        json!({"type":"object","description":"Which documents to delete."}),
                    ),
                ],
                vec!["collection", "filter"],
            ),
        },
    ]
}

#[async_trait]
impl Engine for MongoEngine {
    fn kind(&self) -> &'static str {
        "mongo"
    }

    /// Read tools always; write tools only when the MCP is writable.
    fn tools(&self) -> Vec<ToolDef> {
        let mut tools = read_tools();
        if !self.readonly {
            tools.extend(write_tools());
        }
        tools
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "mongo_find" => self.find(args).await,
            "mongo_aggregate" => self.aggregate(args).await,
            "mongo_list_collections" => self.list_collections().await,
            "mongo_describe_collection" => self.describe_collection(args).await,
            // The write tools are absent from tools/list on a readonly MCP, but a client that
            // remembers a name from a previous config must still be refused, not served.
            "mongo_insert_many" | "mongo_update_many" | "mongo_delete_many" if self.readonly => {
                Err(format!(
                    "refused: this mongo MCP is readonly ({tool} writes)"
                ))
            }
            "mongo_insert_many" => self.insert_many(args).await,
            "mongo_update_many" => self.update_many(args).await,
            "mongo_delete_many" => self.delete_many(args).await,
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

    fn browser(&self) -> Option<lmg_host::dbbrowser::BrowserFlavor> {
        Some(lmg_host::dbbrowser::BrowserFlavor::Mongo(
            self.resources.browser(),
        ))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        let result = match self.db().await {
            Ok(db) => db
                .run_command(doc! {"ping": 1})
                .await
                .map_err(|e| e.to_string())
                .and_then(|res| match res.get_f64("ok") {
                    Ok(1.0) => Ok(()),
                    _ => Err("mongo ping did not return ok".into()),
                }),
            Err(err) => Err(err),
        };
        Some(result)
    }

    async fn close(&self) {
        self.resources.close().await;
    }

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.into();
        }
    }
}

impl MongoEngine {
    /// Where this endpoint points — "app @ localhost:27017". Shown to clients; never a password.
    fn url_host(&self) -> String {
        self.url
            .split("//")
            .nth(1)
            .unwrap_or(&self.url)
            .split('/')
            .next()
            .unwrap_or("")
            // A userinfo prefix carries the password; the host is what is being named.
            .rsplit('@')
            .next()
            .unwrap_or("")
            .to_string()
    }
}

/// The database named by a connection string's path, `mongodb://host:27017/app` → `app`.
fn database_from_uri(url: &str) -> String {
    let after_scheme = match url.split_once("//") {
        Some((_, rest)) => rest,
        None => url,
    };
    let path = match after_scheme.split_once('/') {
        Some((_, rest)) => rest,
        None => return String::new(),
    };
    let name = path.split(['?', '#']).next().unwrap_or("");
    super::resources::percent_decode(name)
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

fn to_document(value: &Value, what: &str) -> Result<Document, String> {
    match mongodb::bson::to_bson(value).map_err(|e| e.to_string())? {
        Bson::Document(d) => Ok(d),
        _ => Err(format!("{what} must be a JSON object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(extra: Value) -> MongoEngine {
        let mut def =
            json!({"type": "mongo", "url": "mongodb://127.0.0.1:27017", "database": "app"});
        if let (Value::Object(base), Value::Object(more)) = (&mut def, extra) {
            for (k, v) in more {
                base.insert(k, v);
            }
        }
        MongoEngine::new(&ServerDef(def.as_object().cloned().unwrap()), "m")
            .expect("valid definition")
    }

    fn names(e: &MongoEngine) -> Vec<String> {
        e.tools().into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn declares_the_node_tool_surface() {
        assert_eq!(
            names(&engine(json!({}))),
            vec![
                "mongo_find",
                "mongo_aggregate",
                "mongo_list_collections",
                "mongo_describe_collection",
                "mongo_insert_many",
                "mongo_update_many",
                "mongo_delete_many",
            ]
        );
    }

    #[test]
    fn a_readonly_mcp_hides_the_write_tools() {
        let ro = engine(json!({"readonly": true}));
        assert_eq!(
            names(&ro),
            vec![
                "mongo_find",
                "mongo_aggregate",
                "mongo_list_collections",
                "mongo_describe_collection",
            ]
        );
    }

    #[tokio::test]
    async fn a_readonly_mcp_refuses_a_write_tool_it_no_longer_lists() {
        // Hiding a tool is not a boundary on its own: a client with a remembered name must be
        // refused before the driver is ever reached.
        let ro = engine(json!({"readonly": true}));
        let err = ro
            .call("mongo_delete_many", &json!({"collection":"c","filter":{}}))
            .await
            .expect_err("readonly must refuse");
        assert!(err.contains("readonly"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn a_readonly_mcp_refuses_a_writing_pipeline() {
        let ro = engine(json!({"readonly": true}));
        let err = ro
            .call(
                "mongo_aggregate",
                &json!({"collection":"c","pipeline":[{"$match":{}},{"$out":"copy"}]}),
            )
            .await
            .expect_err("$out must be refused");
        assert!(err.contains("$out/$merge"), "unexpected error: {err}");
    }

    #[test]
    fn spots_a_writing_stage_anywhere_in_a_pipeline() {
        assert!(writes_via_aggregate(&json!([{"$out": "c"}])));
        assert!(writes_via_aggregate(
            &json!([{"$match": {}}, {"$merge": {}}])
        ));
        assert!(!writes_via_aggregate(&json!([{"$match": {}}])));
        assert!(!writes_via_aggregate(&json!([])));
        // A non-array is not a pipeline; the call path rejects it with its own message.
        assert!(!writes_via_aggregate(&json!({"$out": "c"})));
        assert!(!writes_via_aggregate(&json!(["$out"])));
    }

    #[test]
    fn takes_the_database_from_the_uri_path_when_the_def_omits_it() {
        let def = ServerDef(
            json!({"type":"mongo","url":"mongodb://127.0.0.1:27017/shop"})
                .as_object()
                .cloned()
                .unwrap(),
        );
        let e = MongoEngine::new(&def, "m").expect("uri path names the database");
        assert_eq!(e.database, "shop");
    }

    #[test]
    fn rejects_a_def_with_no_database_anywhere() {
        let def = ServerDef(
            json!({"type":"mongo","url":"mongodb://127.0.0.1:27017"})
                .as_object()
                .cloned()
                .unwrap(),
        );
        let err = match MongoEngine::new(&def, "m") {
            Err(err) => err,
            Ok(_) => panic!("a def with no database anywhere must be refused"),
        };
        assert!(
            err.contains("database is required"),
            "unexpected error: {err}"
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

    #[test]
    fn names_the_target_without_leaking_the_password() {
        let def = ServerDef(
            json!({"type":"mongo","url":"mongodb://user:s3cret@db.internal:27017/shop"})
                .as_object()
                .cloned()
                .unwrap(),
        );
        let e = MongoEngine::new(&def, "m").expect("valid definition");
        let target = e.meta().target.expect("a target");
        assert_eq!(target, "shop @ db.internal:27017");
        assert!(!target.contains("s3cret"));
    }
}
