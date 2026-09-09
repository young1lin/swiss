//! MongoDB collections as MCP resources, and the read-only Data browser — port of
//! `adapters/mongo-resources.ts`.
//!
//! Mongo is schemaless: there is no `CREATE TABLE` to read the way mysql/pg do, so a collection's
//! "schema" is not a fact the server holds. Two things ARE facts — its indexes and its `$jsonSchema`
//! validator (if any) — and those are cheap and exact. What fields actually live in the documents is
//! not, so it is *inferred* from a bounded sample (Mongo-Compass style): fetch up to [`SAMPLE_SIZE`]
//! docs, walk them, and report each top-level field's dominant type and prevalence. That is a best
//! effort with a known ceiling, never a scan of the whole collection.
//!
//! As with the other adapters, nothing is cached and nothing is interpolated: a collection name
//! reaches the driver as a value (a command field, a `collection(name)` argument), never spliced
//! into a string, so — unlike SQL `SHOW CREATE TABLE` — it needs no identifier escaping.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use futures_util::TryStreamExt;
use mongodb::bson::{self, doc, Bson, Document};
use mongodb::results::CollectionType;
use mongodb::Client;
use serde_json::{json, Map, Value};

use super::direct::{def_bool, BoxFut, Lazy};
use super::resources::{
    collapse_shards, describe_family, human_bytes, page, split_uri, Family, ResourceBody,
    ResourceEntry, ResourceFault, ResourcePage, ResourceProvider, ResourceTemplate, TableFact,
    SHARD_MIN,
};
use lmg_host::config::ServerDef;
use lmg_host::dbbrowser::{
    browse_offset, browse_page_size, MongoBrowser, BROWSE_DEFAULT_PAGE, BROWSE_MAX_PAGE,
};

/// Documents sampled to infer a schema. 100 keeps the read bounded yet representative.
pub const SAMPLE_SIZE: i64 = 100;
/// Top-level fields reported; the rest are dropped so a 1000-field sparse collection cannot flood.
pub const MAX_SCHEMA_FIELDS: usize = 50;

/// The largest int64 a JavaScript number holds exactly (`Number.MAX_SAFE_INTEGER`).
///
/// Load-bearing for output parity, not decoration: the Node driver promotes an int64 to a plain
/// number while it fits this range (`promoteLongs`' default) and leaves anything wider as a `Long`,
/// which [`bson_plain`] then renders as an exact decimal string. Both halves of that rule are
/// reproduced here so a snowflake id crosses the wire as the same JSON in both builds.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

// --- the facts a collection reports --------------------------------------------------------------

/// What the adapter tells the provider about one collection's catalog presence.
#[derive(Debug, Clone, Default)]
pub struct CollectionStat {
    pub name: String,
    /// "collection" | "view" | "timeseries" — straight from listCollections.
    pub type_: String,
    /// Estimated document count; 0 when the server has none (e.g. a view).
    pub count: u64,
    /// On-disk storage bytes; 0 when unknown.
    pub bytes: u64,
}

/// One index on a collection, from listIndexes.
#[derive(Debug, Clone, Default)]
pub struct IndexInfo {
    pub name: String,
    /// The indexed key spec, e.g. `{ email: 1 }`, `{ "a.b": -1, c: 1 }`, or `{ body: "text" }`.
    pub keys: Value,
    pub unique: bool,
    pub sparse: bool,
    /// Seconds, on a TTL index.
    pub ttl: Option<u64>,
}

impl IndexInfo {
    /// Node spreads `unique`/`sparse`/`ttl` in only when set, so absent is absent — not `false`.
    fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("name".into(), json!(self.name));
        out.insert("keys".into(), self.keys.clone());
        if self.unique {
            out.insert("unique".into(), json!(true));
        }
        if self.sparse {
            out.insert("sparse".into(), json!(true));
        }
        if let Some(ttl) = self.ttl {
            out.insert("ttl".into(), json!(ttl));
        }
        Value::Object(out)
    }
}

/// Everything `describe()` returns in one round — the certain metadata.
#[derive(Debug, Clone, Default)]
pub struct CollectionInfo {
    pub name: String,
    pub type_: String,
    pub count: u64,
    pub bytes: u64,
    pub indexes: Vec<IndexInfo>,
    /// The collection's `$jsonSchema` validator, when one is set; `None` otherwise.
    pub validator: Option<Value>,
}

/// Database-level totals for the overview.
#[derive(Debug, Clone, Default)]
pub struct DbStats {
    pub collections: u64,
    pub data_size: u64,
    pub storage_size: u64,
}

/// The subset of a MongoClient the resource provider needs.
///
/// A trait rather than a concrete client (like the SQL `query` fn and the redis `RedisLike`) so the
/// provider is pure logic testable with a fake. Each method is one or a few commands, never a scan.
#[async_trait]
pub trait MongoLike: Send + Sync {
    async fn stats(&self) -> Result<DbStats, String>;
    async fn collections(&self) -> Result<Vec<CollectionStat>, String>;
    async fn describe(&self, name: &str) -> Result<CollectionInfo, String>;
    async fn sample(&self, name: &str, size: i64) -> Result<Vec<Document>, String>;
}

// --- BSON to the JSON Node emits ------------------------------------------------------------------

/// Render one BSON value as the JSON the Node build produces for it.
///
/// Not `bson::from_document::<Value>`: that path emits Extended JSON (`{"$oid": …}`,
/// `{"$date": …}`), while the Node driver hands the panel an ObjectId's hex string and a Date's ISO
/// string, because each of those classes carries a `toJSON`. The two 64-bit wrappers are the
/// exception the Node source calls out — `Long` and `Timestamp` have NO `toJSON`, so `bsonPlain`
/// stringifies them by hand or the number is lost entirely. This function is that whole rule.
pub fn bson_plain(value: &Bson) -> Value {
    match value {
        Bson::Double(n) => json!(n),
        Bson::String(s) => json!(s),
        Bson::Array(a) => Value::Array(a.iter().map(bson_plain).collect()),
        Bson::Document(d) => document_plain(d),
        Bson::Boolean(b) => json!(b),
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::Int32(n) => json!(n),
        // Inside the safe range the JS driver has already promoted this to a number; beyond it the
        // value arrives as a Long and bsonPlain renders it as an exact decimal string.
        Bson::Int64(n) => {
            if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(n) {
                json!(n)
            } else {
                json!(n.to_string())
            }
        }
        // Timestamp is a Long in the JS driver, and is never promoted — always the decimal string.
        Bson::Timestamp(t) => {
            json!((((t.time as u64) << 32) | t.increment as u64).to_string())
        }
        Bson::ObjectId(id) => json!(id.to_hex()),
        Bson::DateTime(dt) => match dt.try_to_rfc3339_string() {
            Ok(s) => json!(s),
            // Dates outside the representable range: keep the epoch millis rather than drop the field.
            Err(_) => json!(dt.timestamp_millis()),
        },
        // bson's Binary.toJSON() in the JS driver is its base64 text, so that is what crosses.
        Bson::Binary(b) => json!(base64::engine::general_purpose::STANDARD.encode(&b.bytes)),
        Bson::Decimal128(d) => json!({ "$numberDecimal": d.to_string() }),
        Bson::RegularExpression(r) => json!({ "pattern": r.pattern, "options": r.options }),
        Bson::JavaScriptCode(c) => json!({ "code": c }),
        Bson::JavaScriptCodeWithScope(c) => {
            json!({ "code": c.code, "scope": document_plain(&c.scope) })
        }
        Bson::MinKey => json!({ "$minKey": 1 }),
        Bson::MaxKey => json!({ "$maxKey": 1 }),
        Bson::DbPointer(_) | Bson::Symbol(_) => json!(value.to_string()),
    }
}

/// [`bson_plain`] over a whole document, preserving field order.
pub fn document_plain(doc: &Document) -> Value {
    let mut out = Map::new();
    for (k, v) in doc {
        out.insert(k.clone(), bson_plain(v));
    }
    Value::Object(out)
}

// --- schema inference ------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SchemaField {
    pub field: String,
    /// Dominant type across the sample ("null" excluded from dominance).
    pub type_: String,
    /// Percent of sampled documents that contain this field, 0–100.
    pub prevalence: i64,
    /// Set when null was seen alongside a real type.
    pub nullable: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SampledSchema {
    pub sampled: usize,
    pub fields: Vec<SchemaField>,
}

impl SampledSchema {
    fn to_json(&self) -> Value {
        let fields: Vec<Value> = self
            .fields
            .iter()
            .map(|f| {
                let mut out = Map::new();
                out.insert("field".into(), json!(f.field));
                out.insert("type".into(), json!(f.type_));
                out.insert("prevalence".into(), json!(f.prevalence));
                if f.nullable {
                    out.insert("nullable".into(), json!(true));
                }
                Value::Object(out)
            })
            .collect();
        json!({ "sampled": self.sampled, "fields": fields })
    }
}

/// A Mongo/BSON-aware type name for one value, for schema inference.
///
/// Mirrors the Node `fieldType`, including its int64 rule: the JS driver promotes a small int64 to
/// a plain number, so only a value too wide for a double reads as "long" there.
fn field_type(v: &Bson) -> &'static str {
    match v {
        Bson::Null => "null",
        Bson::Array(_) => "array",
        Bson::DateTime(_) => "date",
        Bson::ObjectId(_) => "objectId",
        Bson::Int64(n) if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(n) => "long",
        Bson::Int64(_) | Bson::Int32(_) | Bson::Double(_) => "number",
        Bson::Decimal128(_) => "decimal",
        Bson::Binary(_) => "binary",
        Bson::String(_) => "string",
        Bson::Boolean(_) => "boolean",
        Bson::Undefined => "undefined",
        // Everything else is a class instance the JS `typeof` reports as "object" — Timestamp,
        // RegExp, Code, MinKey/MaxKey and plain sub-documents alike.
        _ => "object",
    }
}

/// Infer a top-level field schema from a sample of documents.
///
/// A field present in 98 of 100 docs reports prevalence 98 and its dominant type; if any sampled
/// value was null alongside a real type, it is flagged nullable. "null" never wins dominance, so a
/// string-or-null field reads as `string` with `nullable`, not as `null`. Nested object shapes are
/// not expanded (reported as type "object") — top-level fields are what a model needs to write a
/// first query.
pub fn infer_schema(docs: &[Document], max_fields: usize) -> SampledSchema {
    let sampled = docs.len();
    if sampled == 0 {
        return SampledSchema {
            sampled: 0,
            fields: Vec::new(),
        };
    }

    // Insertion-ordered so a tie in the sort below resolves the same way Node's Map iteration does.
    let mut present: Vec<(String, usize)> = Vec::new();
    let mut types: Vec<Vec<(&'static str, usize)>> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for doc in docs {
        for (field, value) in doc {
            let slot = match index.get(field.as_str()) {
                Some(i) => *i,
                None => {
                    let i = present.len();
                    present.push((field.clone(), 0));
                    types.push(Vec::new());
                    index.insert(field.clone(), i);
                    i
                }
            };
            present[slot].1 += 1;
            let t = field_type(value);
            match types[slot].iter_mut().find(|(name, _)| *name == t) {
                Some(entry) => entry.1 += 1,
                None => types[slot].push((t, 1)),
            }
        }
    }

    let mut fields: Vec<SchemaField> = present
        .iter()
        .enumerate()
        .map(|(i, (field, count))| {
            let tm = &types[i];
            let nullable = tm.iter().any(|(name, n)| *name == "null" && *n > 0);
            // First-seen wins a tie, matching the JS loop's strict `>`.
            let mut best = "null";
            let mut best_n: i64 = -1;
            for (name, n) in tm {
                if *name != "null" && (*n as i64) > best_n {
                    best = name;
                    best_n = *n as i64;
                }
            }
            SchemaField {
                field: field.clone(),
                type_: best.to_string(),
                prevalence: ((*count as f64 / sampled as f64) * 100.0).round() as i64,
                nullable: nullable && best != "null",
            }
        })
        .collect();

    fields.sort_by(|a, b| {
        b.prevalence
            .cmp(&a.prevalence)
            .then_with(|| a.field.cmp(&b.field))
    });
    fields.truncate(max_fields);
    SampledSchema { sampled, fields }
}

// --- the shared bodies -----------------------------------------------------------------------------

fn stat_facts(stats: &[CollectionStat]) -> Vec<TableFact> {
    stats
        .iter()
        .map(|s| TableFact {
            name: s.name.clone(),
            rows: s.count,
            bytes: s.bytes,
        })
        .collect()
}

/// Collections collapsed into logical families (sharded/time-series suffixes folded), largest first.
pub async fn collection_families(like: &dyn MongoLike) -> Result<Vec<Family>, String> {
    let stats = like.collections().await?;
    Ok(collapse_shards(&stat_facts(&stats), SHARD_MIN))
}

/// The overview body (returned for `mongo://<db>`), shared by the resource read.
pub async fn overview_body(like: &dyn MongoLike, database: &str) -> Result<Value, String> {
    let fams = collection_families(like).await?;
    let st = like.stats().await?;
    let physical: usize = fams.iter().map(|f| f.members.len()).sum();
    let largest: Vec<Value> = fams
        .iter()
        .take(30)
        .map(|f| {
            let mut row = Map::new();
            row.insert("collection".into(), json!(f.name));
            if f.rows > 0 {
                row.insert("docs".into(), json!(f.rows));
            }
            if f.bytes > 0 {
                row.insert("size".into(), json!(human_bytes(f.bytes)));
            }
            Value::Object(row)
        })
        .collect();
    let collapsed: Vec<Value> = fams
        .iter()
        .filter(|f| f.members.len() > 1)
        .map(|f| json!({ "collection": f.name, "shards": f.members.len() }))
        .collect();
    Ok(json!({
        "database": database,
        "collections": fams.len(),
        "physicalCollections": physical,
        "dataSize": human_bytes(st.data_size),
        "storageSize": human_bytes(st.storage_size),
        "largest": largest,
        "collapsedShardSets": collapsed,
        "notes": [
            format!("Read mongo://{database}/<collection> for indexes, the $jsonSchema validator and a sampled field schema."),
            "Collection names reach the driver as values, never interpolated, so no identifier escaping is needed.",
        ],
    }))
}

/// One collection's body (returned for `mongo://<db>/<collection>`), shared by the resource read
/// and the `mongo_describe_collection` tool.
pub async fn describe_collection(
    like: &dyn MongoLike,
    database: &str,
    name: &str,
) -> Result<Value, String> {
    let info = like.describe(name).await?;
    let docs = like.sample(name, SAMPLE_SIZE).await?;
    let mut out = Map::new();
    out.insert("database".into(), json!(database));
    out.insert("collection".into(), json!(name));
    out.insert("type".into(), json!(info.type_));
    // Node spreads `count || undefined`, so a view's zero drops the key rather than showing 0.
    if info.count > 0 {
        out.insert("count".into(), json!(info.count));
    }
    out.insert("size".into(), json!(human_bytes(info.bytes)));
    out.insert(
        "indexes".into(),
        Value::Array(info.indexes.iter().map(IndexInfo::to_json).collect()),
    );
    if let Some(validator) = &info.validator {
        out.insert("validator".into(), validator.clone());
    }
    out.insert(
        "sampledSchema".into(),
        infer_schema(&docs, MAX_SCHEMA_FIELDS).to_json(),
    );
    out.insert("notes".into(), json!([
        format!("Schema is inferred from up to {SAMPLE_SIZE} sampled documents; Mongo collections are schemaless, so absent fields may still occur and types may vary per document."),
        "Only top-level fields are listed; nested object shapes are not expanded.",
    ]));
    Ok(Value::Object(out))
}

// --- the provider over a real client ---------------------------------------------------------------

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

    pub async fn db(&self) -> Result<mongodb::Database, String> {
        if self.database.is_empty() {
            return Err("mongo database is required".into());
        }
        Ok(self.client.get().await?.database(&self.database))
    }

    pub fn browser(self: &Arc<Self>) -> Arc<dyn MongoBrowser> {
        self.clone()
    }

    /// Drop the shared client on shutdown. The driver closes its own pool on drop, so this only
    /// has to let go of it — best effort, like every other adapter's close.
    pub async fn close(&self) {
        self.client.dispose(|_| Box::pin(async {})).await;
    }
}

/// `Number(v)` with Node's `> 0` guard: a missing, negative or non-numeric stat reads as 0.
fn num(v: Option<&Bson>) -> u64 {
    let n = match v {
        Some(Bson::Int32(n)) => *n as f64,
        Some(Bson::Int64(n)) => *n as f64,
        Some(Bson::Double(n)) => *n,
        Some(Bson::String(s)) => s.parse::<f64>().unwrap_or(f64::NAN),
        _ => f64::NAN,
    };
    if n.is_finite() && n > 0.0 {
        n as u64
    } else {
        0
    }
}

fn collection_type_name(t: &CollectionType) -> String {
    match t {
        CollectionType::View => "view".into(),
        CollectionType::Collection => "collection".into(),
        CollectionType::Timeseries => "timeseries".into(),
        // The enum is non_exhaustive: a future server type reports itself rather than panicking.
        other => format!("{other:?}").to_lowercase(),
    }
}

#[async_trait]
impl MongoLike for MongoResources {
    async fn stats(&self) -> Result<DbStats, String> {
        let db = self.db().await?;
        // dbStats can be refused on some hosted clusters; the overview degrades to zeros.
        let Ok(s) = db.run_command(doc! { "dbStats": 1 }).await else {
            return Ok(DbStats::default());
        };
        Ok(DbStats {
            collections: num(s.get("collections")),
            data_size: num(s.get("dataSize")),
            storage_size: num(s.get("storageSize")),
        })
    }

    async fn collections(&self) -> Result<Vec<CollectionStat>, String> {
        let db = self.db().await?;
        let specs: Vec<mongodb::results::CollectionSpecification> = db
            .list_collections()
            .await
            .map_err(|e| e.to_string())?
            .try_collect()
            .await
            .map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(specs.len());
        for spec in specs {
            let mut count = 0;
            let mut bytes = 0;
            // Views and some time-series shapes refuse collStats; report them with no size.
            if let Ok(st) = db.run_command(doc! { "collStats": &spec.name }).await {
                count = num(st.get("count"));
                bytes = match st.get("storageSize") {
                    Some(v) => num(Some(v)),
                    None => num(st.get("size")),
                };
            }
            out.push(CollectionStat {
                name: spec.name,
                type_: collection_type_name(&spec.collection_type),
                count,
                bytes,
            });
        }
        Ok(out)
    }

    async fn describe(&self, name: &str) -> Result<CollectionInfo, String> {
        let db = self.db().await?;
        let specs: Vec<mongodb::results::CollectionSpecification> = db
            .list_collections()
            .filter(doc! { "name": name })
            .await
            .map_err(|e| e.to_string())?
            .try_collect()
            .await
            .map_err(|e| e.to_string())?;
        let spec = specs.into_iter().next();
        let stats = db.run_command(doc! { "collStats": name }).await.ok();
        let indexes = match db
            .collection::<Document>(name)
            .list_indexes()
            .await
            .map_err(|e| e.to_string())
        {
            Ok(cursor) => cursor
                .try_collect::<Vec<mongodb::IndexModel>>()
                .await
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let bytes = stats
            .as_ref()
            .map(|s| match s.get("storageSize") {
                Some(v) => num(Some(v)),
                None => num(s.get("size")),
            })
            .unwrap_or(0);
        Ok(CollectionInfo {
            name: name.to_string(),
            type_: spec
                .as_ref()
                .map(|s| collection_type_name(&s.collection_type))
                .unwrap_or_else(|| "collection".into()),
            count: stats.as_ref().map(|s| num(s.get("count"))).unwrap_or(0),
            bytes,
            indexes: indexes
                .into_iter()
                .map(|i| {
                    let opts = i.options.unwrap_or_default();
                    IndexInfo {
                        name: opts.name.unwrap_or_default(),
                        keys: document_plain(&i.keys),
                        unique: opts.unique.unwrap_or(false),
                        sparse: opts.sparse.unwrap_or(false),
                        ttl: opts.expire_after.map(|d| d.as_secs()),
                    }
                })
                .collect(),
            validator: spec
                .and_then(|s| s.options.validator)
                .map(|v| document_plain(&v)),
        })
    }

    async fn sample(&self, name: &str, size: i64) -> Result<Vec<Document>, String> {
        let cursor = self
            .db()
            .await?
            .collection::<Document>(name)
            .aggregate(vec![doc! { "$sample": { "size": size } }])
            .await
            .map_err(|e| e.to_string())?;
        cursor.try_collect().await.map_err(|e| e.to_string())
    }
}

#[async_trait]
impl ResourceProvider for MongoResources {
    async fn list(&self, cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
        let fams = collection_families(self).await.map_err(ResourceFault)?;
        let (slice, next_cursor) = page(&fams, cursor)?;
        let overview_uri = format!("mongo://{}", self.database);
        let mut resources: Vec<ResourceEntry> = Vec::new();
        // The overview leads the first page only — a cursored page is a continuation, not a restart.
        if cursor.is_none_or(str::is_empty) {
            resources.push(ResourceEntry {
                uri: overview_uri.clone(),
                name: self.database.clone(),
                description: Some(format!(
                    "Collection overview: {} collections, live from the catalog",
                    fams.len()
                )),
                mime_type: Some("application/json".into()),
            });
        }
        for f in slice {
            let described = describe_family(f);
            resources.push(ResourceEntry {
                uri: format!("{overview_uri}/{}", f.name),
                name: f.name.clone(),
                description: (!described.is_empty()).then_some(described),
                mime_type: Some("application/json".into()),
            });
        }
        Ok(ResourcePage {
            resources,
            next_cursor,
        })
    }

    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![ResourceTemplate {
            uri_template: format!("mongo://{}/{{collection}}", self.database),
            name: format!("{} collection schema", self.database),
            description: Some(
                "Indexes, $jsonSchema validator and a sampled field schema of any collection."
                    .into(),
            ),
            mime_type: Some("application/json".into()),
        }]
    }

    async fn read(&self, uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
        let (authority, rest) = split_uri(uri, "mongo")?;
        if authority != self.database {
            return Err(ResourceFault(format!(
                "this MCP is connected to {}, not {authority}",
                self.database
            )));
        }
        let body = match rest {
            None => overview_body(self, &self.database).await,
            Some(collection) => describe_collection(self, &self.database, &collection).await,
        }
        .map_err(ResourceFault)?;
        Ok(vec![ResourceBody {
            uri: uri.into(),
            mime_type: Some("application/json".into()),
            text: serde_json::to_string_pretty(&body).map_err(|e| ResourceFault(e.to_string()))?,
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
        let mut stats = self.collections().await?;
        if !grep.is_empty() {
            stats.retain(|c| c.name.to_lowercase().contains(&grep));
        }
        stats.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Value::Array(
            stats
                .into_iter()
                .map(|c| {
                    json!({
                        "name": c.name,
                        "type": c.type_,
                        "approxDocs": c.count,
                        "size": human_bytes(c.bytes),
                    })
                })
                .collect(),
        ))
    }

    async fn read_collection(&self, options: &Value) -> Result<Value, String> {
        let collection = options
            .get("collection")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if collection.is_empty() {
            return Err("collection is required".into());
        }
        let filter = parse_filter(options.get("filterJson").or_else(|| options.get("filter")))?;
        let limit =
            browse_page_size(options.get("limit"), BROWSE_DEFAULT_PAGE).min(BROWSE_MAX_PAGE);
        let offset = browse_offset(options.get("offset"));
        let db = self.db().await?;
        let coll = db.collection::<Document>(&collection);
        let cursor = coll
            .find(filter.clone())
            .sort(doc! { "_id": 1 })
            .skip(offset.max(0) as u64)
            .limit(limit)
            .await
            .map_err(|e| e.to_string())?;
        let docs: Vec<Document> = cursor.try_collect().await.map_err(|e| e.to_string())?;
        let total = coll
            .count_documents(filter)
            .await
            .map_err(|e| e.to_string())?;
        // The page's column set: _id first, then every other field seen on the page, in
        // first-seen order.
        let mut fields: Vec<String> = vec!["_id".into()];
        for doc in &docs {
            for k in doc.keys() {
                if k != "_id" && !fields.iter().any(|f| f == k) {
                    fields.push(k.clone());
                }
            }
        }
        Ok(json!({
            "collection": collection,
            "documents": docs.iter().map(document_plain).collect::<Vec<Value>>(),
            "total": total,
            "offset": offset,
            "limit": limit,
            "fields": fields,
        }))
    }
}

/// A filter argument as either a JSON object or a JSON string, to one BSON document.
pub(crate) fn parse_filter(value: Option<&Value>) -> Result<Document, String> {
    let Some(value) = value else {
        return Ok(Document::new());
    };
    let parsed = match value {
        Value::Object(_) => value.clone(),
        Value::String(text) if !text.trim().is_empty() => {
            serde_json::from_str(text).map_err(|_| {
                "filter must be a valid JSON query document, e.g. {\"status\":\"active\"}"
                    .to_string()
            })?
        }
        Value::String(_) | Value::Null => return Ok(Document::new()),
        _ => return Err("filter must be a JSON object".into()),
    };
    match bson::to_bson(&parsed).map_err(|e| e.to_string())? {
        Bson::Document(document) => Ok(document),
        _ => Err("filter must be a JSON object".into()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn docs(raw: Vec<Document>) -> Vec<Document> {
        raw
    }

    #[test]
    fn infers_dominant_type_and_prevalence() {
        let sample = docs(vec![
            doc! { "a": "x", "b": 1 },
            doc! { "a": "y", "b": 2 },
            doc! { "a": "z" },
        ]);
        let schema = infer_schema(&sample, MAX_SCHEMA_FIELDS);
        assert_eq!(schema.sampled, 3);
        let a = &schema.fields[0];
        assert_eq!(a.field, "a");
        assert_eq!(a.type_, "string");
        assert_eq!(a.prevalence, 100);
        let b = schema.fields.iter().find(|f| f.field == "b").expect("b");
        assert_eq!(b.type_, "number");
        assert_eq!(b.prevalence, 67);
    }

    #[test]
    fn null_never_wins_dominance_but_flags_nullable() {
        let sample = docs(vec![
            doc! { "a": Bson::Null },
            doc! { "a": "x" },
            doc! { "a": Bson::Null },
        ]);
        let schema = infer_schema(&sample, MAX_SCHEMA_FIELDS);
        let a = &schema.fields[0];
        assert_eq!(a.type_, "string", "null is excluded from dominance");
        assert!(a.nullable);
    }

    #[test]
    fn an_all_null_field_stays_null_and_is_not_flagged_nullable() {
        let schema = infer_schema(&docs(vec![doc! { "a": Bson::Null }]), MAX_SCHEMA_FIELDS);
        assert_eq!(schema.fields[0].type_, "null");
        assert!(!schema.fields[0].nullable);
    }

    #[test]
    fn caps_the_reported_field_count() {
        let mut d = Document::new();
        for i in 0..80 {
            d.insert(format!("f{i:02}"), i);
        }
        let schema = infer_schema(&docs(vec![d]), MAX_SCHEMA_FIELDS);
        assert_eq!(schema.fields.len(), MAX_SCHEMA_FIELDS);
    }

    #[test]
    fn an_empty_sample_infers_nothing() {
        let schema = infer_schema(&[], MAX_SCHEMA_FIELDS);
        assert_eq!(schema.sampled, 0);
        assert!(schema.fields.is_empty());
    }

    #[test]
    fn names_bson_types_the_way_the_node_build_does() {
        assert_eq!(field_type(&Bson::Null), "null");
        assert_eq!(field_type(&Bson::Array(vec![])), "array");
        assert_eq!(field_type(&Bson::Int32(1)), "number");
        assert_eq!(field_type(&Bson::Double(1.5)), "number");
        assert_eq!(field_type(&Bson::Boolean(true)), "boolean");
        assert_eq!(field_type(&Bson::String("s".into())), "string");
        assert_eq!(field_type(&Bson::Document(Document::new())), "object");
        assert_eq!(
            field_type(&Bson::ObjectId(bson::oid::ObjectId::new())),
            "objectId"
        );
        // Inside the safe range the JS driver has already promoted it to a plain number.
        assert_eq!(field_type(&Bson::Int64(42)), "number");
        assert_eq!(field_type(&Bson::Int64(MAX_SAFE_INTEGER + 1)), "long");
    }

    #[test]
    fn renders_object_ids_and_dates_the_way_the_node_driver_does() {
        let oid = bson::oid::ObjectId::new();
        assert_eq!(bson_plain(&Bson::ObjectId(oid)), json!(oid.to_hex()));
        let dt = bson::DateTime::from_millis(0);
        assert_eq!(
            bson_plain(&Bson::DateTime(dt)),
            json!("1970-01-01T00:00:00Z")
        );
    }

    #[test]
    fn renders_wide_int64_as_an_exact_decimal_string() {
        // A snowflake id: JSON.stringify would lose it as a number, so both builds emit a string.
        let wide = MAX_SAFE_INTEGER + 1;
        assert_eq!(bson_plain(&Bson::Int64(wide)), json!(wide.to_string()));
        assert_eq!(bson_plain(&Bson::Int64(7)), json!(7));
    }

    #[test]
    fn parses_a_filter_from_either_an_object_or_a_json_string() {
        // serde_json widens every integer to i64; Mongo compares int32 and int64 numerically,
        // so the width is not observable in a query — the test just names the path's real output.
        assert_eq!(
            parse_filter(Some(&json!({"a": 1}))).expect("object filter"),
            doc! { "a": 1i64 }
        );
        assert_eq!(
            parse_filter(Some(&json!("{\"a\": 1}"))).expect("string filter"),
            doc! { "a": 1i64 }
        );
        assert_eq!(parse_filter(None).expect("absent"), Document::new());
        assert_eq!(
            parse_filter(Some(&json!(""))).expect("empty"),
            Document::new()
        );
        assert!(parse_filter(Some(&json!("{oops"))).is_err());
        assert!(parse_filter(Some(&json!([1]))).is_err());
    }
}
