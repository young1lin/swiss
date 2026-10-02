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

//! The MongoDB flavour of the Data view (SPEC §data.mongo): the browser trait the mongo engine
//! implements, and the pure halves the routes and the engine share.
//!
//! Same rule as `dbbrowser.rs`: no driver here. A document crosses this seam as **canonical
//! Extended JSON** in a `serde_json::Value` — `{"$oid": …}`, `{"$numberLong": "…"}`,
//! `{"$date": {"$numberLong": "…"}}` — so the host stays free of bson, every helper below is
//! testable without a server, and nothing about a value is lost on its way to the panel and
//! back: an Int64 past 2^53 stays a string, an Int32 stays an Int32, a Decimal128 keeps every
//! digit. The panel renders the shell spelling (`ObjectId('…')`) and sends canonical back.
//!
//! What lives here, in order: the namespace rules and option parsing (a caller's mistake is a
//! 400 before any lease, the house rule every browser route follows), the trait, the BSON type
//! reading of canonical Extended JSON, schema inference over a sample (SPEC §data.mongo-schema),
//! the explain summary (SPEC §data.mongo-pipeline), the optimistic edit filter
//! (SPEC §data.mongo-edits), the currentOp row, and the CSV flattening of an export.

use std::cmp::Ordering;
use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::dbbrowser::{js_number, EditError, EXPORT_ROW_CAP, IMPORT_ROW_CAP};

// --- limits --------------------------------------------------------------------------------------

/// The documents page sizes the panel offers; the default is smaller than the SQL grid's 50
/// because a document is usually several rows' worth of text.
pub const MONGO_PAGE_SIZES: [i64; 6] = [10, 20, 50, 100, 200, 500];
pub const MONGO_PAGE_DEFAULT: i64 = 20;
pub const MONGO_PAGE_MAX: i64 = 500;
/// The server-side time limit a browse read runs under unless the caller asks for another, and
/// the most it may ask for. A query the panel fires must end on its own: a forgotten COLLSCAN
/// over a billion documents would otherwise hold a pooled connection until the server gives up.
pub const MONGO_MAX_TIME_DEFAULT_MS: u64 = 15_000;
pub const MONGO_MAX_TIME_MAX_MS: u64 = 300_000;
/// A count is a second, optional question (the page answers first), so it gets less time.
pub const MONGO_COUNT_TIME_MS: u64 = 5_000;
/// Schema analysis samples this many documents by default (`$sample`), and at most this many.
pub const MONGO_SAMPLE_DEFAULT: i64 = 1_000;
pub const MONGO_SAMPLE_MAX: i64 = 10_000;
/// Stages one pipeline may carry through the browser. The server's own limit is 1000; a pipeline
/// a person builds in a panel is a few dozen at most, and the cap keeps a pasted mistake bounded.
pub const MONGO_PIPELINE_MAX: usize = 200;
/// Edits one commit may carry (the SQL grid's MAX_EDITS).
pub const MONGO_EDITS_MAX: usize = 1_000;
/// Documents one import may carry, and one export may stream (the SQL caps, SPEC §data.export).
pub const MONGO_IMPORT_MAX: usize = IMPORT_ROW_CAP;
pub const MONGO_EXPORT_CAP: i64 = EXPORT_ROW_CAP;
/// Collections whose storage statistics one listing gathers. Past it the rest list by name and
/// type only, and the reply says how many went without — a database with ten thousand
/// collections must not cost ten thousand round trips per sidebar refresh.
pub const MONGO_STATS_MAX: usize = 300;

// --- namespaces ----------------------------------------------------------------------------------

/// One collection address. Validated at parse time, so a browser never sees a name the server
/// would refuse for its shape alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MongoNs {
    pub db: String,
    pub coll: String,
}

/// MongoDB's database-name rules (the stricter Windows set, which the server applies
/// everywhere): non-empty, under 64 bytes, none of `/\. "$*<>:|?` and no NUL.
pub fn assert_db_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("db is required".into());
    }
    if name.len() >= 64 {
        return Err(format!("not a valid database name (64 bytes or more): {name}"));
    }
    if let Some(c) = name
        .chars()
        .find(|&c| matches!(c, '/' | '\\' | '.' | ' ' | '"' | '$' | '*' | '<' | '>' | ':' | '|' | '?' | '\0'))
    {
        return Err(format!("not a valid database name ('{c}' is not allowed): {name}"));
    }
    Ok(())
}

/// Collection-name rules: non-empty, no NUL, no `$` (the server reserves it), and the whole
/// namespace under 255 bytes. `system.*` stays readable — `system.profile` and `system.views`
/// are worth looking at — and the server itself refuses writes it does not allow there.
pub fn assert_coll_name(db: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("collection is required".into());
    }
    if name.contains('\0') || name.contains('$') {
        return Err(format!("not a valid collection name: {name}"));
    }
    if db.len() + 1 + name.len() > 255 {
        return Err(format!("namespace is longer than 255 bytes: {db}.{name}"));
    }
    Ok(())
}

fn str_field(o: &Value, key: &str) -> String {
    match o.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// `{db, collection}` out of an option object.
pub fn mongo_ns_of(o: &Value) -> Result<MongoNs, String> {
    let db = str_field(o, "db");
    assert_db_name(&db)?;
    let coll = str_field(o, "collection");
    assert_coll_name(&db, &coll)?;
    Ok(MongoNs { db, coll })
}

/// A document-valued option: absent and null are "not sent"; anything but an object is the
/// caller's mistake, named by the option it came in.
fn doc_opt(o: &Value, key: &str) -> Result<Option<Map<String, Value>>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m.clone())),
        Some(_) => Err(format!("{key} must be a document")),
    }
}

/// A non-negative whole number option, JavaScript-coerced like every other browser parameter.
fn count_opt(o: &Value, key: &str) -> Result<Option<i64>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v) => match js_number(v) {
            Some(n) if n.is_finite() && n >= 0.0 && n.fract() == 0.0 => Ok(Some(n as i64)),
            _ => Err(format!("{key} must be a non-negative integer")),
        },
    }
}

fn max_time_of(o: &Value, fallback: u64) -> Result<u64, String> {
    Ok(match count_opt(o, "maxTimeMS")? {
        Some(0) | None => fallback,
        Some(n) => (n as u64).min(MONGO_MAX_TIME_MAX_MS),
    })
}

/// The index hint: an index name or a key-pattern document.
fn hint_of(o: &Value) -> Result<Option<Value>, String> {
    match o.get("hint") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v @ Value::String(_)) | Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
        Some(_) => Err("hint must be an index name or a key pattern document".into()),
    }
}

/// One find, as the documents view asks it (SPEC §data.mongo).
#[derive(Clone, Debug, PartialEq)]
pub struct MongoFind {
    pub ns: MongoNs,
    pub filter: Map<String, Value>,
    pub projection: Option<Map<String, Value>>,
    pub sort: Option<Map<String, Value>>,
    pub collation: Option<Map<String, Value>>,
    pub hint: Option<Value>,
    pub skip: u64,
    pub limit: i64,
    pub max_time_ms: u64,
}

/// Parse a find. `limit` is clamped to the page cap; the export passes its own cap instead.
pub fn mongo_find_of(o: &Value, limit_cap: i64) -> Result<MongoFind, String> {
    let ns = mongo_ns_of(o)?;
    let limit = match count_opt(o, "limit")? {
        Some(0) | None => MONGO_PAGE_DEFAULT.min(limit_cap),
        Some(n) => n.min(limit_cap),
    };
    Ok(MongoFind {
        ns,
        filter: doc_opt(o, "filter")?.unwrap_or_default(),
        projection: doc_opt(o, "projection")?.filter(|m| !m.is_empty()),
        sort: doc_opt(o, "sort")?.filter(|m| !m.is_empty()),
        collation: doc_opt(o, "collation")?.filter(|m| !m.is_empty()),
        hint: hint_of(o)?,
        skip: count_opt(o, "skip")?.unwrap_or(0) as u64,
        limit,
        max_time_ms: max_time_of(o, MONGO_MAX_TIME_DEFAULT_MS)?,
    })
}

/// One count. Without a filter (and without `exact`) the browser answers from the collection's
/// metadata — instant on any size — and says the number is an estimate.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoCount {
    pub ns: MongoNs,
    pub filter: Map<String, Value>,
    pub hint: Option<Value>,
    pub exact: bool,
    pub max_time_ms: u64,
}

pub fn mongo_count_of(o: &Value) -> Result<MongoCount, String> {
    Ok(MongoCount {
        ns: mongo_ns_of(o)?,
        filter: doc_opt(o, "filter")?.unwrap_or_default(),
        hint: hint_of(o)?,
        exact: matches!(o.get("exact"), Some(Value::Bool(true))),
        max_time_ms: max_time_of(o, MONGO_COUNT_TIME_MS)?,
    })
}

/// One aggregation. `limit` caps what the reply carries (a `$limit` of limit+1 is appended, so
/// `more` is honest); a pipeline ending in `$out`/`$merge` writes, runs whole, and answers what
/// it wrote instead of documents.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoAggregate {
    pub ns: MongoNs,
    pub pipeline: Vec<Map<String, Value>>,
    pub limit: i64,
    pub collation: Option<Map<String, Value>>,
    pub allow_disk_use: bool,
    pub max_time_ms: u64,
}

/// The stage list of a pipeline: an array of one-key documents whose key is a `$` operator.
/// `{ $match: …, $sort: … }` in one stage is the most common hand-written mistake, and the
/// server's own message for it ("A pipeline stage specification object must contain exactly one
/// field") never says which stage — this one does.
pub fn pipeline_of(v: Option<&Value>) -> Result<Vec<Map<String, Value>>, String> {
    let stages = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err("pipeline must be an array of stages".into()),
    };
    if stages.len() > MONGO_PIPELINE_MAX {
        return Err(format!("pipeline has {} stages; at most {MONGO_PIPELINE_MAX}", stages.len()));
    }
    let mut out = Vec::with_capacity(stages.len());
    for (i, stage) in stages.iter().enumerate() {
        let Value::Object(m) = stage else {
            return Err(format!("stage {} is not a document", i + 1));
        };
        if m.len() != 1 {
            return Err(format!(
                "stage {} must hold exactly one operator (it has {}: {})",
                i + 1,
                m.len(),
                m.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if let Some(key) = m.keys().next() {
            if !key.starts_with('$') {
                return Err(format!("stage {} is \"{key}\", not a $ operator", i + 1));
            }
        }
        out.push(m.clone());
    }
    Ok(out)
}

/// The operator of one stage (`"$match"`).
pub fn stage_op(stage: &Map<String, Value>) -> &str {
    stage.keys().next().map(String::as_str).unwrap_or("")
}

/// True when the pipeline's last stage writes a collection.
pub fn pipeline_writes(pipeline: &[Map<String, Value>]) -> bool {
    pipeline
        .last()
        .is_some_and(|s| matches!(stage_op(s), "$out" | "$merge"))
}

pub fn mongo_aggregate_of(o: &Value) -> Result<MongoAggregate, String> {
    let ns = mongo_ns_of(o)?;
    let mut pipeline = pipeline_of(o.get("pipeline"))?;
    // The stage preview (SPEC §data.mongo-pipeline): `stages: n` runs the first n stages only, so
    // the builder can show what each stage hands the next without the caller slicing JSON.
    if let Some(n) = count_opt(o, "stages")? {
        pipeline.truncate(n as usize);
    }
    // A writing stage is never only "previewed": in the middle of a pipeline it is a server
    // error anyway, and at the end of a truncated preview it would write for real.
    if let Some(i) = pipeline[..pipeline.len().saturating_sub(1)]
        .iter()
        .position(|s| matches!(stage_op(s), "$out" | "$merge"))
    {
        return Err(format!("{} must be the last stage (it is stage {})", stage_op(&pipeline[i]), i + 1));
    }
    let limit = match count_opt(o, "limit")? {
        Some(0) | None => MONGO_PAGE_DEFAULT,
        Some(n) => n.min(MONGO_PAGE_MAX),
    };
    Ok(MongoAggregate {
        ns,
        pipeline,
        limit,
        collation: doc_opt(o, "collation")?.filter(|m| !m.is_empty()),
        allow_disk_use: matches!(o.get("allowDiskUse"), Some(Value::Bool(true))),
        max_time_ms: max_time_of(o, MONGO_MAX_TIME_DEFAULT_MS)?,
    })
}

/// What an explain explains: the documents view's find, or the pipeline builder's aggregation.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoExplainTarget {
    Find(Box<MongoFind>),
    Aggregate(MongoAggregate),
}

/// An explain request. `verbosity` is the server's own word; `executionStats` (the default)
/// runs the winning plan, which is what makes "docs examined vs returned" a real number.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoExplain {
    pub target: MongoExplainTarget,
    pub verbosity: &'static str,
}

pub fn mongo_explain_of(o: &Value) -> Result<MongoExplain, String> {
    let verbosity = match str_field(o, "verbosity").as_str() {
        "" | "executionStats" => "executionStats",
        "queryPlanner" => "queryPlanner",
        "allPlansExecution" => "allPlansExecution",
        other => {
            return Err(format!(
                "verbosity must be queryPlanner, executionStats or allPlansExecution — got \"{other}\""
            ))
        }
    };
    let target = if o.get("pipeline").is_some_and(|p| !p.is_null()) {
        let agg = mongo_aggregate_of(o)?;
        if pipeline_writes(&agg.pipeline) {
            return Err("explain of a pipeline ending in $out/$merge would write; remove the stage to explain the read".into());
        }
        MongoExplainTarget::Aggregate(agg)
    } else {
        MongoExplainTarget::Find(Box::new(mongo_find_of(o, MONGO_PAGE_MAX)?))
    };
    Ok(MongoExplain { target, verbosity })
}

/// The schema request: how many documents to sample, and an optional filter narrowing the
/// population first.
pub fn mongo_sample_of(o: &Value) -> Result<(MongoNs, Map<String, Value>, i64), String> {
    let ns = mongo_ns_of(o)?;
    let size = match count_opt(o, "sample")? {
        Some(0) | None => MONGO_SAMPLE_DEFAULT,
        Some(n) => n.min(MONGO_SAMPLE_MAX),
    };
    Ok((ns, doc_opt(o, "filter")?.unwrap_or_default(), size))
}

// --- edits ---------------------------------------------------------------------------------------

/// One document change (SPEC §data.mongo-edits). Every value is canonical Extended JSON.
///
/// `Replace` and `Delete` carry the document as it was READ: the browser matches on all of it
/// ([`edit_filter`]), so a document someone else changed meanwhile is a 409, never a silent
/// overwrite. The two "many" forms are the bulk actions, which address by filter and say so in
/// their confirm — there is nothing to compare a filter's matches against.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoEdit {
    Insert { doc: Map<String, Value> },
    Replace { original: Map<String, Value>, doc: Map<String, Value> },
    Delete { original: Map<String, Value> },
    DeleteMany { filter: Map<String, Value> },
    UpdateMany { filter: Map<String, Value>, update: Value },
}

impl MongoEdit {
    pub fn op(&self) -> &'static str {
        match self {
            MongoEdit::Insert { .. } => "insert",
            MongoEdit::Replace { .. } => "replace",
            MongoEdit::Delete { .. } => "delete",
            MongoEdit::DeleteMany { .. } => "deleteMany",
            MongoEdit::UpdateMany { .. } => "updateMany",
        }
    }
}

fn need_doc(o: &Value, key: &str, i: usize) -> Result<Map<String, Value>, String> {
    match o.get(key) {
        Some(Value::Object(m)) => Ok(m.clone()),
        _ => Err(format!("edit {}: {key} must be a document", i + 1)),
    }
}

/// The edit list of a commit. Shape only — whether a value is valid Extended JSON is the
/// engine's question, answered with the edit's number before anything is written.
pub fn mongo_edits_of(body: &Value) -> Result<Vec<MongoEdit>, String> {
    let list = match body.get("edits") {
        Some(Value::Array(a)) => a,
        _ => return Err("edits must be an array".into()),
    };
    if list.is_empty() {
        return Err("edits is empty".into());
    }
    if list.len() > MONGO_EDITS_MAX {
        return Err(format!("{} edits in one commit; at most {MONGO_EDITS_MAX}", list.len()));
    }
    let mut out = Vec::with_capacity(list.len());
    for (i, e) in list.iter().enumerate() {
        let op = e.get("op").and_then(Value::as_str).unwrap_or("");
        let edit = match op {
            "insert" => MongoEdit::Insert { doc: need_doc(e, "doc", i)? },
            "replace" => {
                let original = need_doc(e, "original", i)?;
                let doc = need_doc(e, "doc", i)?;
                if !original.contains_key("_id") {
                    return Err(format!("edit {}: the original has no _id to address it by", i + 1));
                }
                MongoEdit::Replace { original, doc }
            }
            "delete" => {
                let original = need_doc(e, "original", i)?;
                if !original.contains_key("_id") {
                    return Err(format!("edit {}: the original has no _id to address it by", i + 1));
                }
                MongoEdit::Delete { original }
            }
            "deleteMany" => MongoEdit::DeleteMany { filter: need_doc(e, "filter", i)? },
            "updateMany" => {
                let filter = need_doc(e, "filter", i)?;
                let update = match e.get("update") {
                    Some(u @ Value::Object(m)) if !m.is_empty() => u.clone(),
                    Some(u @ Value::Array(a)) if !a.is_empty() => u.clone(),
                    _ => {
                        return Err(format!(
                            "edit {}: update must be an update document ($set, $unset, …) or a pipeline",
                            i + 1
                        ))
                    }
                };
                if let Value::Object(m) = &update {
                    if let Some(k) = m.keys().find(|k| !k.starts_with('$')) {
                        return Err(format!(
                            "edit {}: update field \"{k}\" is not an update operator — wrap it in $set",
                            i + 1
                        ));
                    }
                }
                MongoEdit::UpdateMany { filter, update }
            }
            other => {
                return Err(format!(
                    "edit {}: op must be insert, replace, delete, deleteMany or updateMany — got \"{other}\"",
                    i + 1
                ))
            }
        };
        out.push(edit);
    }
    Ok(out)
}

/// The optimistic filter of a replace or a delete (SPEC §data.mongo-edits): the `_id`, which
/// makes it an index lookup, AND the whole stored document equal to the one the panel read.
///
/// `$expr: {$eq: [{$cmp: ["$$ROOT", {$literal: original}]}, 0]}` compares the documents the way
/// BSON compares them — every field, in order, at every depth — so a field another writer added,
/// removed or changed anywhere fails the match. `$literal` keeps a value that happens to look
/// like an expression (a string starting with `$`, a `{ $add: … }` stored as data) from being
/// evaluated. A per-field `$eq` filter would get dotted and `$`-prefixed field names wrong (a
/// filter path `a.b` means nested `b`, not the field named `a.b`), and would miss added fields.
/// What BSON comparison itself cannot see stays unseen: Int32 1 and Double 1.0 compare equal.
///
/// `$cmp` and `$eq` are one comparator; the plain `{$eq: ["$$ROOT", …]}` is not used because
/// 4.4 rewrites a field-path-against-constant `$eq` for the index and asserts on the
/// one-element path `$$ROOT` (code 16409) — every save and delete failed there. `$cmp` is never
/// rewritten, on any version.
pub fn edit_filter(original: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let id = original
        .get("_id")
        .ok_or_else(|| "the document has no _id to address it by".to_string())?;
    let mut f = Map::new();
    f.insert("_id".into(), id.clone());
    f.insert(
        "$expr".into(),
        json!({ "$eq": [{ "$cmp": ["$$ROOT", { "$literal": Value::Object(original.clone()) }] }, 0] }),
    );
    Ok(f)
}

/// The top-level fields whose value differs between two documents, plus the fields only one
/// of them has — what a 409 names, so the panel can mark exactly those.
pub fn changed_fields(a: &Map<String, Value>, b: &Map<String, Value>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, v) in a {
        if b.get(k) != Some(v) {
            out.push(k.clone());
        }
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            out.push(k.clone());
        }
    }
    out
}

/// An index operation of the Indexes tab.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoIndexOp {
    Create { keys: Map<String, Value>, options: Map<String, Value> },
    Drop { name: String },
    Hide { name: String, hidden: bool },
}

/// Index options the create form may set — anything else is refused rather than passed
/// through, so a typo is an error and not an index built without the option it meant.
pub const MONGO_INDEX_OPTIONS: [&str; 9] = [
    "name",
    "unique",
    "sparse",
    "hidden",
    "expireAfterSeconds",
    "partialFilterExpression",
    "collation",
    "wildcardProjection",
    "weights",
];

pub fn mongo_index_op_of(o: &Value) -> Result<MongoIndexOp, String> {
    let name = str_field(o, "name");
    match str_field(o, "op").as_str() {
        "create" => {
            let keys = doc_opt(o, "keys")?.unwrap_or_default();
            if keys.is_empty() {
                return Err("keys must name at least one field".into());
            }
            let options = doc_opt(o, "options")?.unwrap_or_default();
            if let Some(k) = options.keys().find(|k| !MONGO_INDEX_OPTIONS.contains(&k.as_str())) {
                return Err(format!(
                    "unknown index option \"{k}\" (supported: {})",
                    MONGO_INDEX_OPTIONS.join(", ")
                ));
            }
            Ok(MongoIndexOp::Create { keys, options })
        }
        "drop" | "hide" | "unhide" if name.is_empty() => Err("name is required".into()),
        "drop" if name == "_id_" => Err("the _id index cannot be dropped".into()),
        "drop" => Ok(MongoIndexOp::Drop { name }),
        "hide" => Ok(MongoIndexOp::Hide { name, hidden: true }),
        "unhide" => Ok(MongoIndexOp::Hide { name, hidden: false }),
        other => Err(format!("op must be create, drop, hide or unhide — got \"{other}\"")),
    }
}

/// A collection-level operation of the tree.
#[derive(Clone, Debug, PartialEq)]
pub enum MongoCollectionOp {
    Create { name: String, options: Map<String, Value> },
    CreateView { name: String, view_on: String, pipeline: Vec<Map<String, Value>> },
    Rename { name: String, to: String },
    Drop { name: String },
}

/// Options the create-collection sheet may set (`create` command fields).
pub const MONGO_CREATE_OPTIONS: [&str; 8] = [
    "capped",
    "size",
    "max",
    "timeseries",
    "expireAfterSeconds",
    "validator",
    "validationLevel",
    "validationAction",
];

pub fn mongo_collection_op_of(o: &Value) -> Result<MongoCollectionOp, String> {
    let db = str_field(o, "db");
    assert_db_name(&db)?;
    let name = str_field(o, "name");
    assert_coll_name(&db, &name)?;
    match str_field(o, "op").as_str() {
        "create" => {
            let options = doc_opt(o, "options")?.unwrap_or_default();
            if let Some(k) = options.keys().find(|k| !MONGO_CREATE_OPTIONS.contains(&k.as_str())) {
                return Err(format!(
                    "unknown collection option \"{k}\" (supported: {})",
                    MONGO_CREATE_OPTIONS.join(", ")
                ));
            }
            Ok(MongoCollectionOp::Create { name, options })
        }
        "createView" => {
            let view_on = str_field(o, "viewOn");
            assert_coll_name(&db, &view_on)?;
            let pipeline = pipeline_of(o.get("pipeline"))?;
            if pipeline_writes(&pipeline) {
                return Err("a view's pipeline cannot write ($out/$merge)".into());
            }
            Ok(MongoCollectionOp::CreateView { name, view_on, pipeline })
        }
        "rename" => {
            let to = str_field(o, "to");
            assert_coll_name(&db, &to)?;
            if to == name {
                return Err("the new name is the current name".into());
            }
            Ok(MongoCollectionOp::Rename { name, to })
        }
        "drop" => Ok(MongoCollectionOp::Drop { name }),
        other => Err(format!("op must be create, createView, rename or drop — got \"{other}\"")),
    }
}

/// An import: Extended JSON documents (relaxed or canonical — the engine parses both), inserted
/// unordered so one duplicate key does not stop the other nine thousand.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoImport {
    pub ns: MongoNs,
    pub docs: Vec<Map<String, Value>>,
}

pub fn mongo_import_of(o: &Value) -> Result<MongoImport, String> {
    let ns = mongo_ns_of(o)?;
    let list = match o.get("docs") {
        Some(Value::Array(a)) => a,
        _ => return Err("docs must be an array of documents".into()),
    };
    if list.is_empty() {
        return Err("docs is empty".into());
    }
    if list.len() > MONGO_IMPORT_MAX {
        return Err(format!("{} documents in one import; at most {MONGO_IMPORT_MAX}", list.len()));
    }
    let mut docs = Vec::with_capacity(list.len());
    for (i, d) in list.iter().enumerate() {
        match d {
            Value::Object(m) => docs.push(m.clone()),
            _ => return Err(format!("document {} is not a document", i + 1)),
        }
    }
    Ok(MongoImport { ns, docs })
}

/// The streaming export (SPEC §data.mongo): the count up front for the response headers, then
/// one Extended JSON document per piece on a bounded channel, in the relaxed or canonical form
/// the caller asked for. The route formats them; the producer holds one batch, never the result.
pub struct MongoDump {
    pub rows: i64,
    pub capped: bool,
    pub docs: tokio::sync::mpsc::Receiver<Result<Value, String>>,
}

// --- the trait -----------------------------------------------------------------------------------

/// What the mongo engine exposes for the Data view. Constructed on demand like the other two
/// flavours; the heavy thing — the driver's client and its pool — is the engine's own Lazy,
/// shared with the MCP tools, the resources and the health ping.
///
/// Every database on the instance is browsable AND writable through one client: unlike pg there
/// is no per-database connection, so ADR-027's "the others are read-only" has nothing to protect
/// here (SPEC §data.mongo).
#[async_trait]
pub trait MongoBrowser: Send + Sync {
    /// Where this connection points (`shop @ host:port`). Never a credential.
    fn label(&self) -> String;
    /// `{version, topology, setName?, transactions, maxWireVersion}` — the status line, and
    /// whether a multi-edit commit can be all-or-nothing.
    async fn server_info(&self) -> Result<Value, String>;
    /// The database axis in the shape every flavour answers (SPEC §data.databases).
    async fn list_databases(&self) -> Result<Value, String>;
    /// `{db, collections:[{name,type,count?,size?,storageSize?,avgObjSize?,indexes?,
    /// indexSize?,capped?,readOnly?,viewOn?,pipeline?,validator?,timeseries?}], statsOmitted}`.
    async fn list_collections(&self, db: &str) -> Result<Value, String>;
    /// One page: `{db, collection, docs, offset, limit, more}`; docs are canonical EJSON.
    async fn find(&self, q: &MongoFind) -> Result<Value, String>;
    /// `{total, estimated}`; `total` is null with `timedOut` when the count ran out of time.
    async fn count(&self, q: &MongoCount) -> Result<Value, String>;
    /// `{docs, more}` — or, for a writing pipeline, `{docs: [], wrote: "<namespace>"}`.
    async fn aggregate(&self, q: &MongoAggregate) -> Result<Value, String>;
    /// `{explain (relaxed EJSON, as the server said it), summary}` ([`explain_summary`]).
    async fn explain(&self, q: &MongoExplain) -> Result<Value, String>;
    /// Up to `size` documents by `$sample` (after `filter`), canonical EJSON — the input of
    /// [`infer_schema`], which the route runs so the MCP tool and the panel infer alike.
    async fn sample(&self, ns: &MongoNs, filter: &Map<String, Value>, size: i64) -> Result<Vec<Value>, String>;
    /// `{indexes:[{name,key,unique?,sparse?,hidden?,expireAfterSeconds?,
    /// partialFilterExpression?,collation?,size?,ops?,since?}], statsError?}`.
    async fn indexes(&self, ns: &MongoNs) -> Result<Value, String>;
    async fn index_op(&self, ns: &MongoNs, op: &MongoIndexOp) -> Result<Value, String>;
    /// Apply an edit list — in one transaction when the deployment has them (a replica set or
    /// a sharded cluster), else in order, stopping at the first failure; the reply says which.
    async fn apply_edits(&self, ns: &MongoNs, edits: &[MongoEdit]) -> Result<Value, EditError>;
    /// The console: one command document run against `db`, through the engine's command guard
    /// (the same policy the MCP's mongo_command applies). `{reply}` in canonical EJSON.
    async fn run_command(&self, db: &str, command: &Map<String, Value>) -> Result<Value, String>;
    async fn collection_op(&self, db: &str, op: &MongoCollectionOp) -> Result<Value, String>;
    async fn export(&self, q: &MongoFind, relaxed: bool) -> Result<MongoDump, String>;
    /// `{inserted, errors:[{index, message}]}` — at most twenty errors are itemised.
    async fn import(&self, q: &MongoImport) -> Result<Value, String>;
    /// `$currentOp` in the shared activity row shape (SPEC §data.activity).
    async fn activity(&self) -> Result<Value, String>;
    async fn activity_kill(&self, opid: i64) -> Result<Value, String>;
}

// --- reading canonical Extended JSON -------------------------------------------------------------

/// The BSON type name of one canonical (or relaxed) Extended JSON value, in the vocabulary
/// Compass and the shell use. A bare JSON number only arrives from relaxed input: whole numbers
/// read as Int32 when they fit, Int64 when they do not, the rest as Double.
pub fn bson_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Boolean",
        Value::String(_) => "String",
        Value::Array(_) => "Array",
        Value::Number(n) => match n.as_i64() {
            Some(i) if i32::try_from(i).is_ok() => "Int32",
            Some(_) => "Int64",
            None if n.is_u64() => "Int64",
            None => "Double",
        },
        Value::Object(m) => wrapper_type(m).unwrap_or("Document"),
    }
}

/// The type an Extended JSON wrapper object spells, or None for a plain document.
fn wrapper_type(m: &Map<String, Value>) -> Option<&'static str> {
    let mut keys = m.keys();
    let first = keys.next()?;
    let second = keys.next();
    if keys.next().is_some() {
        return None;
    }
    let t = match (first.as_str(), second.map(String::as_str)) {
        ("$oid", None) => "ObjectId",
        ("$numberInt", None) => "Int32",
        ("$numberLong", None) => "Int64",
        ("$numberDouble", None) => "Double",
        ("$numberDecimal", None) => "Decimal128",
        ("$date", None) => "Date",
        ("$binary", None) => match m.get("$binary").and_then(|b| b.get("subType")).and_then(Value::as_str) {
            Some("04") | Some("4") | Some("03") | Some("3") => "UUID",
            _ => "Binary",
        },
        ("$uuid", None) => "UUID",
        ("$regularExpression", None) => "RegExp",
        ("$timestamp", None) => "Timestamp",
        ("$symbol", None) => "Symbol",
        ("$code", None) => "Code",
        ("$code", Some("$scope")) | ("$scope", Some("$code")) => "CodeWithScope",
        ("$minKey", None) => "MinKey",
        ("$maxKey", None) => "MaxKey",
        ("$undefined", None) => "Undefined",
        ("$dbPointer", None) => "DBPointer",
        _ => return None,
    };
    Some(t)
}

/// A numeric Extended JSON value as an f64, for display statistics (min/max) only — an Int64 or
/// Decimal128 beyond 2^53 rounds here and nowhere else.
pub fn ejson_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Object(m) if m.len() == 1 => {
            let (k, inner) = m.iter().next()?;
            match k.as_str() {
                "$numberInt" | "$numberLong" | "$numberDouble" | "$numberDecimal" => {
                    inner.as_str()?.trim().parse::<f64>().ok().filter(|f| f.is_finite())
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// An integral Extended JSON value (Int32, Int64, or a plain JSON integer) exactly, as i64.
fn ejson_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::Object(m) if m.len() == 1 => match m.iter().next() {
            Some((k, Value::String(s))) if k == "$numberInt" || k == "$numberLong" => s.trim().parse().ok(),
            _ => None,
        },
        _ => None,
    }
}

/// The order of two numeric values: exact between integers (two Int64s past 2^53 that round to
/// one double still compare), through f64 otherwise.
fn num_order(a: &Value, b: &Value) -> Ordering {
    match (ejson_int(a), ejson_int(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => match (ejson_number(a), ejson_number(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        },
    }
}

/// A `$date` value as epoch milliseconds — canonical `{"$numberLong": "…"}` or a relaxed number.
/// The relaxed ISO string form is not read here (the browser always answers canonical).
pub fn ejson_date_ms(v: &Value) -> Option<i64> {
    let inner = v.as_object().filter(|m| m.len() == 1)?.get("$date")?;
    match inner {
        Value::Number(n) => n.as_i64(),
        Value::Object(m) => m.get("$numberLong")?.as_str()?.parse::<i64>().ok(),
        _ => None,
    }
}

// --- schema inference (SPEC §data.mongo-schema) --------------------------------------------------

/// Bounds of one inference. A sample of 10 000 heterogeneous documents must still answer in
/// bounded memory and a bounded reply: depth, fields per document level, sample values and
/// distinct-value tracking all stop at a stated number, and the reply says where they stopped.
pub const SCHEMA_MAX_DEPTH: usize = 8;
pub const SCHEMA_MAX_FIELDS: usize = 300;
pub const SCHEMA_SAMPLE_VALUES: usize = 8;
pub const SCHEMA_DISTINCT_MAX: usize = 100;
/// Values longer than this are not tracked for distinctness (a 40 KB blob is never a "top value").
const SCHEMA_DISTINCT_TEXT_MAX: usize = 200;

#[derive(Default)]
struct DocAcc {
    docs: u64,
    fields: Vec<(String, FieldAcc)>,
    index: HashMap<String, usize>,
    capped: bool,
}

#[derive(Default)]
struct FieldAcc {
    count: u64,
    types: Vec<(&'static str, TypeAcc)>,
}

#[derive(Default)]
struct TypeAcc {
    count: u64,
    samples: Vec<Value>,
    distinct: HashMap<String, (Value, u64)>,
    distinct_capped: bool,
    // The extremes as the values themselves, reported as sent: an Int64 past 2^53 reads back
    // digit for digit instead of as its rounded double (ordered by `num_order`).
    num_min: Option<Value>,
    num_max: Option<Value>,
    date_min: Option<i64>,
    date_max: Option<i64>,
    len_min: Option<u64>,
    len_max: Option<u64>,
    len_sum: u64,
    doc: Option<Box<DocAcc>>,
    elems: Option<Box<FieldAcc>>,
}

impl DocAcc {
    fn add(&mut self, m: &Map<String, Value>, depth: usize) {
        self.docs += 1;
        for (k, v) in m {
            let idx = match self.index.get(k) {
                Some(i) => *i,
                None => {
                    if self.fields.len() >= SCHEMA_MAX_FIELDS {
                        self.capped = true;
                        continue;
                    }
                    self.fields.push((k.clone(), FieldAcc::default()));
                    self.index.insert(k.clone(), self.fields.len() - 1);
                    self.fields.len() - 1
                }
            };
            self.fields[idx].1.add(v, depth);
        }
    }

    fn render(&self, parent_path: &str) -> Value {
        let mut fields: Vec<&(String, FieldAcc)> = self.fields.iter().collect();
        // _id first, then by how often the field is present, then by name: the order a reader
        // scans for "what does a document here usually hold".
        fields.sort_by(|a, b| {
            (b.0 == "_id")
                .cmp(&(a.0 == "_id"))
                .then(b.1.count.cmp(&a.1.count))
                .then(a.0.cmp(&b.0))
        });
        Value::Array(
            fields
                .into_iter()
                .map(|(name, f)| {
                    let path = if parent_path.is_empty() { name.clone() } else { format!("{parent_path}.{name}") };
                    f.render(name, &path, self.docs)
                })
                .collect(),
        )
    }
}

impl FieldAcc {
    fn add(&mut self, v: &Value, depth: usize) {
        self.count += 1;
        let t = bson_type(v);
        let idx = match self.types.iter().position(|(n, _)| *n == t) {
            Some(i) => i,
            None => {
                self.types.push((t, TypeAcc::default()));
                self.types.len() - 1
            }
        };
        self.types[idx].1.add(t, v, depth);
    }

    fn render_types(&self, path: &str) -> Value {
        let mut types: Vec<&(&'static str, TypeAcc)> = self.types.iter().collect();
        types.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(b.0)));
        Value::Array(
            types
                .into_iter()
                .map(|(name, t)| t.render(name, path, self.count))
                .collect(),
        )
    }

    fn render(&self, name: &str, path: &str, parent: u64) -> Value {
        json!({
            "name": name,
            "path": path,
            "count": self.count,
            "missing": parent.saturating_sub(self.count),
            "probability": ratio(self.count, parent),
            "types": self.render_types(path),
        })
    }
}

impl TypeAcc {
    fn add(&mut self, t: &'static str, v: &Value, depth: usize) {
        self.count += 1;
        match t {
            "Document" => {
                if depth < SCHEMA_MAX_DEPTH {
                    if let Value::Object(m) = v {
                        self.doc.get_or_insert_with(Box::default).add(m, depth + 1);
                    }
                }
                return;
            }
            "Array" => {
                if let Value::Array(a) = v {
                    let n = a.len() as u64;
                    self.len_min = Some(self.len_min.map_or(n, |x| x.min(n)));
                    self.len_max = Some(self.len_max.map_or(n, |x| x.max(n)));
                    self.len_sum += n;
                    if depth < SCHEMA_MAX_DEPTH {
                        let elems = self.elems.get_or_insert_with(Box::default);
                        for e in a {
                            elems.add(e, depth + 1);
                        }
                    }
                }
                return;
            }
            "String" => {
                if let Value::String(s) = v {
                    let n = s.chars().count() as u64;
                    self.len_min = Some(self.len_min.map_or(n, |x| x.min(n)));
                    self.len_max = Some(self.len_max.map_or(n, |x| x.max(n)));
                }
            }
            "Date" => {
                if let Some(ms) = ejson_date_ms(v) {
                    self.date_min = Some(self.date_min.map_or(ms, |x| x.min(ms)));
                    self.date_max = Some(self.date_max.map_or(ms, |x| x.max(ms)));
                }
            }
            "Int32" | "Int64" | "Double" | "Decimal128" if ejson_number(v).is_some() => {
                if self.num_min.as_ref().is_none_or(|m| num_order(v, m) == Ordering::Less) {
                    self.num_min = Some(v.clone());
                }
                if self.num_max.as_ref().is_none_or(|m| num_order(v, m) == Ordering::Greater) {
                    self.num_max = Some(v.clone());
                }
            }
            _ => {}
        }
        if self.samples.len() < SCHEMA_SAMPLE_VALUES && !self.samples.contains(v) {
            self.samples.push(v.clone());
        }
        let text = v.to_string();
        if text.len() > SCHEMA_DISTINCT_TEXT_MAX {
            self.distinct_capped = true;
            return;
        }
        if let Some(slot) = self.distinct.get_mut(&text) {
            slot.1 += 1;
        } else if self.distinct.len() < SCHEMA_DISTINCT_MAX {
            self.distinct.insert(text, (v.clone(), 1));
        } else {
            self.distinct_capped = true;
        }
    }

    fn render(&self, name: &str, path: &str, of: u64) -> Value {
        let mut out = Map::new();
        out.insert("type".into(), json!(name));
        out.insert("count".into(), json!(self.count));
        out.insert("probability".into(), ratio(self.count, of));
        if !self.samples.is_empty() {
            out.insert("values".into(), Value::Array(self.samples.clone()));
        }
        if !self.distinct.is_empty() {
            let mut top: Vec<&(Value, u64)> = self.distinct.values().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_string().cmp(&b.0.to_string())));
            out.insert(
                "top".into(),
                Value::Array(top.into_iter().take(10).map(|(v, n)| json!({ "value": v, "count": n })).collect()),
            );
            // Distinct counts are only exact while tracking never overflowed; past the cap the
            // number is a floor, and the reply says so instead of passing it off as the truth.
            out.insert("distinct".into(), json!(self.distinct.len()));
            if self.distinct_capped {
                out.insert("distinctCapped".into(), json!(true));
            }
        }
        if let (Some(a), Some(b)) = (&self.num_min, &self.num_max) {
            out.insert("min".into(), a.clone());
            out.insert("max".into(), b.clone());
        }
        if let (Some(a), Some(b)) = (self.date_min, self.date_max) {
            out.insert("min".into(), json!({ "$date": { "$numberLong": a.to_string() } }));
            out.insert("max".into(), json!({ "$date": { "$numberLong": b.to_string() } }));
        }
        if let (Some(a), Some(b)) = (self.len_min, self.len_max) {
            let mut len = json!({ "min": a, "max": b });
            if name == "Array" && self.count > 0 {
                len["avg"] = json!(((self.len_sum as f64 / self.count as f64) * 10.0).round() / 10.0);
            }
            out.insert("lengths".into(), len);
        }
        if let Some(doc) = &self.doc {
            out.insert("fields".into(), doc.render(path));
            if doc.capped {
                out.insert("fieldsCapped".into(), json!(true));
            }
        }
        if let Some(elems) = &self.elems {
            if elems.count > 0 {
                out.insert("elements".into(), json!({ "count": elems.count, "types": elems.render_types(path) }));
            }
        }
        Value::Object(out)
    }
}

fn ratio(n: u64, of: u64) -> Value {
    if of == 0 {
        return json!(0);
    }
    json!(((n as f64 / of as f64) * 10_000.0).round() / 10_000.0)
}

/// Infer a collection's shape from a sample: every field path with how often it is present
/// (`probability`, `missing`), and per field the BSON types it holds — each with its share, a
/// few sample values, the top values with counts, numeric/date min-max, string and array
/// lengths, nested `fields` for documents and `elements` for arrays. Non-document samples are
/// skipped (a `$sample` never yields one; a test stub might).
pub fn infer_schema(docs: &[Value]) -> Value {
    let mut root = DocAcc::default();
    for d in docs {
        if let Value::Object(m) = d {
            root.add(m, 0);
        }
    }
    let mut out = json!({ "sampled": root.docs, "fields": root.render("") });
    if root.capped {
        out["fieldsCapped"] = json!(true);
    }
    out
}

/// The flat field paths of an inferred schema, documents descended into, array element
/// documents included — the query bar's completion list (SPEC §data.mongo-panel).
pub fn schema_paths(schema: &Value) -> Vec<String> {
    fn walk(fields: &Value, out: &mut Vec<String>) {
        let Some(list) = fields.as_array() else { return };
        for f in list {
            if let Some(p) = f.get("path").and_then(Value::as_str) {
                if !out.iter().any(|x| x == p) {
                    out.push(p.to_string());
                }
            }
            for t in f.get("types").and_then(Value::as_array).into_iter().flatten() {
                if let Some(sub) = t.get("fields") {
                    walk(sub, out);
                }
                if let Some(el) = t.get("elements").and_then(|e| e.get("types")).and_then(Value::as_array) {
                    for et in el {
                        if let Some(sub) = et.get("fields") {
                            walk(sub, out);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(f) = schema.get("fields") {
        walk(f, &mut out);
    }
    out
}

// --- explain (SPEC §data.mongo-pipeline) ---------------------------------------------------------

/// Flatten one plan tree (classic `stage`/`inputStage(s)` nodes — the shape both engines use
/// for `queryPlanner.winningPlan`, SBE under `.queryPlan`) into pre-order rows with a depth.
fn plan_rows(node: &Value, depth: usize, out: &mut Vec<Value>) {
    if depth > 64 || out.len() > 256 {
        return;
    }
    let Some(m) = node.as_object() else { return };
    let mut row = Map::new();
    row.insert("depth".into(), json!(depth));
    for key in [
        "stage",
        "indexName",
        "keyPattern",
        "direction",
        "isMultiKey",
        "filter",
        "nReturned",
        "executionTimeMillisEstimate",
        "keysExamined",
        "docsExamined",
        "memLimit",
        "limitAmount",
        "skipAmount",
        "sortPattern",
        "usedDisk",
    ] {
        if let Some(v) = m.get(key) {
            row.insert(key.into(), v.clone());
        }
    }
    out.push(Value::Object(row));
    if let Some(child) = m.get("inputStage") {
        plan_rows(child, depth + 1, out);
    }
    if let Some(Value::Array(children)) = m.get("inputStages") {
        for c in children {
            plan_rows(c, depth + 1, out);
        }
    }
    if let Some(child) = m.get("queryPlan") {
        plan_rows(child, depth + 1, out);
    }
}

fn first_number(v: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|k| v.get(*k).and_then(ejson_number)).map(whole)
}

/// A count as a JSON integer when it is one: `21`, not the `21.0` an f64 prints as.
fn whole(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9.0e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// The cursor-stage half of an explain: queryPlanner + executionStats, wherever the reply put
/// them — at the top (a find, or an aggregation pushed down whole), under the first stage's
/// `$cursor` (an aggregation), or under the first shard (a sharded cluster).
fn cursor_part(explain: &Value) -> Option<&Value> {
    if explain.get("queryPlanner").is_some() {
        return Some(explain);
    }
    if let Some(stages) = explain.get("stages").and_then(Value::as_array) {
        if let Some(c) = stages.first().and_then(|s| s.get("$cursor")) {
            return Some(c);
        }
    }
    if let Some(shards) = explain.get("shards").and_then(Value::as_object) {
        if let Some((_, s)) = shards.iter().next() {
            return cursor_part(s);
        }
    }
    None
}

/// Boil an explain reply down to what a reader acts on: the winning plan as indented stage rows,
/// the totals (returned, keys examined, documents examined, time), the indexes it used, and the
/// two verdicts worth a colour — a collection scan, and a sort done in memory. The raw reply
/// still travels beside this for whoever wants every field.
pub fn explain_summary(explain: &Value) -> Value {
    let mut out = Map::new();
    let cursor = cursor_part(explain);
    let mut rows: Vec<Value> = Vec::new();
    if let Some(c) = cursor {
        let planner = c.get("queryPlanner").unwrap_or(&Value::Null);
        let winning = planner.get("winningPlan").unwrap_or(&Value::Null);
        let tree = winning.get("queryPlan").unwrap_or(winning);
        plan_rows(tree, 0, &mut rows);
        let engine = if winning.get("slotBasedPlan").is_some() || winning.get("queryPlan").is_some() {
            "sbe"
        } else {
            "classic"
        };
        out.insert("engine".into(), json!(engine));
        if let Some(ns) = planner.get("namespace") {
            out.insert("namespace".into(), ns.clone());
        }
        let rejected = planner.get("rejectedPlans").and_then(Value::as_array).map_or(0, Vec::len);
        out.insert("rejectedPlans".into(), json!(rejected));
        if let Some(stats) = c.get("executionStats") {
            for (key, src) in [
                ("nReturned", "nReturned"),
                ("executionTimeMillis", "executionTimeMillis"),
                ("totalKeysExamined", "totalKeysExamined"),
                ("totalDocsExamined", "totalDocsExamined"),
            ] {
                if let Some(n) = first_number(stats, &[src]) {
                    out.insert(key.into(), n);
                }
            }
            // Per-stage counts: the classic execution tree mirrors the winning plan node for
            // node, so the two pre-order walks line up and each plan row gains its numbers.
            // An SBE execution tree is a different vocabulary; it is left to the raw reply.
            let mut exec_rows: Vec<Value> = Vec::new();
            if let Some(es) = stats.get("executionStages") {
                plan_rows(es, 0, &mut exec_rows);
            }
            let same = exec_rows.len() == rows.len()
                && exec_rows.iter().zip(rows.iter()).all(|(a, b)| a.get("stage") == b.get("stage"));
            if same {
                for (row, ex) in rows.iter_mut().zip(exec_rows.iter()) {
                    if let (Some(r), Some(e)) = (row.as_object_mut(), ex.as_object()) {
                        for key in ["nReturned", "executionTimeMillisEstimate", "keysExamined", "docsExamined"] {
                            if let Some(v) = e.get(key) {
                                r.insert(key.into(), v.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    // The aggregation stages past the cursor, with their own counts where the server gave them.
    let mut pipeline_rows: Vec<Value> = Vec::new();
    if let Some(stages) = explain.get("stages").and_then(Value::as_array) {
        for s in stages.iter().skip(1) {
            let Some(m) = s.as_object() else { continue };
            let op = m.keys().find(|k| k.starts_with('$')).cloned().unwrap_or_default();
            let mut row = json!({ "stage": op });
            for key in ["nReturned", "executionTimeMillisEstimate"] {
                if let Some(v) = m.get(key) {
                    row[key] = v.clone();
                }
            }
            pipeline_rows.push(row);
        }
    }
    let names: Vec<String> = rows.iter().filter_map(|r| r.get("stage").and_then(Value::as_str)).map(str::to_string).collect();
    let mut indexes: Vec<Value> = Vec::new();
    for r in &rows {
        if let Some(name) = r.get("indexName") {
            if !indexes.contains(name) {
                indexes.push(name.clone());
            }
        }
    }
    out.insert("collscan".into(), json!(names.iter().any(|n| n == "COLLSCAN")));
    out.insert("inMemorySort".into(), json!(names.iter().any(|n| n == "SORT")));
    out.insert("indexes".into(), Value::Array(indexes));
    out.insert("stages".into(), Value::Array(rows));
    if !pipeline_rows.is_empty() {
        out.insert("pipeline".into(), Value::Array(pipeline_rows));
    }
    if explain.get("shards").is_some() {
        out.insert("sharded".into(), json!(true));
    }
    Value::Object(out)
}

// --- currentOp (SPEC §data.activity) --------------------------------------------------------------

/// Commands the activity text is cut to: a pipeline pasted into an aggregate can be megabytes.
const ACTIVITY_TEXT_MAX: usize = 2_000;

fn clip(s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// `$currentOp` entries that are not anyone's work: the server's own background threads
/// (`op: "none"` — the checkpointer, the TTL monitor, replication workers) and the awaitable
/// `hello` every driver's monitor keeps parked on each server (`maxAwaitTimeMS`), which reads
/// as an "active" operation that never ends. Listing them buries the one query that matters.
pub fn mongo_activity_noise(op: &Value) -> bool {
    if op.get("op").and_then(Value::as_str) == Some("none") {
        return true;
    }
    let Some(cmd) = op.get("command").and_then(Value::as_object) else { return false };
    let first = cmd.keys().next().map(|k| k.to_ascii_lowercase()).unwrap_or_default();
    matches!(first.as_str(), "hello" | "ismaster") && cmd.contains_key("maxAwaitTimeMS")
}

/// One `$currentOp` document (relaxed EJSON) as the shared activity row: `pid` is the opid,
/// `own` marks the `$currentOp` that is asking, `wait` names a lock or flow-control wait,
/// `query` is the command as compact JSON. `ns`, `op`, `client` and `app` ride along for the
/// panel's tooltip. An opid that is not a plain integer (mongos spells `shard:123`) is kept as
/// text and cannot be killed from here — the reply's `killable` says so.
pub fn mongo_activity_row(op: &Value) -> Value {
    let opid = op.get("opid");
    let pid = opid.and_then(ejson_number).filter(|f| f.fract() == 0.0).map(|f| f as i64);
    let command = op.get("command").cloned().unwrap_or(Value::Null);
    let own = command
        .get("pipeline")
        .and_then(Value::as_array)
        .and_then(|p| p.first())
        .is_some_and(|s| s.get("$currentOp").is_some())
        && command.get("aggregate").is_some();
    let user = op
        .get("effectiveUsers")
        .and_then(Value::as_array)
        .and_then(|u| u.first())
        .and_then(|u| u.get("user"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let wait = if op.get("waitingForLock") == Some(&Value::Bool(true)) {
        "lock"
    } else if op.get("waitingForFlowControl") == Some(&Value::Bool(true)) {
        "flowControl"
    } else if op.get("waitingForLatch").is_some() {
        "latch"
    } else {
        ""
    };
    let seconds = op
        .get("microsecs_running")
        .and_then(ejson_number)
        .map(|us| (us / 1_000_000.0).floor() as i64)
        .or_else(|| op.get("secs_running").and_then(ejson_number).map(|s| s as i64))
        .unwrap_or(0);
    let state = match op.get("active") {
        Some(Value::Bool(true)) => "active",
        Some(Value::Bool(false)) => "idle",
        _ => "",
    };
    let query = if command.is_null() { String::new() } else { clip(command.to_string(), ACTIVITY_TEXT_MAX) };
    let mut row = json!({
        "pid": pid.map_or(Value::Null, |p| json!(p)),
        "user": user,
        "state": state,
        "wait": wait,
        "seconds": seconds,
        "query": query,
        "own": own,
        "killable": pid.is_some() && !own,
    });
    for (key, src) in [("ns", "ns"), ("op", "op"), ("client", "client"), ("app", "appName"), ("plan", "planSummary")] {
        if let Some(v) = op.get(src).filter(|v| !v.is_null()) {
            row[key] = v.clone();
        }
    }
    if pid.is_none() {
        if let Some(v) = opid {
            row["opid"] = v.clone();
        }
    }
    row
}

// --- export flattening ---------------------------------------------------------------------------

/// One relaxed Extended JSON value as a CSV cell: scalars as their text, the wrapper types as
/// the value inside them (an ObjectId's hex, a date's ISO string, a decimal's digits), arrays as
/// compact JSON. Documents never reach here — [`flatten_doc`] descends into them.
fn csv_scalar(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Object(m) if m.len() == 1 => {
            let (k, inner) = match m.iter().next() {
                Some(kv) => kv,
                None => return String::new(),
            };
            match (k.as_str(), inner) {
                ("$oid" | "$numberDecimal" | "$numberLong" | "$numberInt" | "$numberDouble" | "$symbol", Value::String(s)) => s.clone(),
                ("$date", Value::String(s)) => s.clone(),
                ("$date", Value::Object(d)) => d.get("$numberLong").and_then(Value::as_str).unwrap_or("").to_string(),
                _ => v.to_string(),
            }
        }
        other => other.to_string(),
    }
}

/// A document flattened to `(dotted path, cell text)` pairs, embedded documents descended into
/// (Compass's CSV export does the same), arrays and wrapper values kept as one cell each.
pub fn flatten_doc(doc: &Map<String, Value>) -> Vec<(String, String)> {
    fn walk(prefix: &str, m: &Map<String, Value>, depth: usize, out: &mut Vec<(String, String)>) {
        for (k, v) in m {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Value::Object(inner) if wrapper_type(inner).is_none() && depth < SCHEMA_MAX_DEPTH => {
                    walk(&path, inner, depth + 1, out)
                }
                other => out.push((path, csv_scalar(other))),
            }
        }
    }
    let mut out = Vec::new();
    walk("", doc, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_refuse_what_the_server_would_refuse_for_shape_alone() {
        assert!(assert_db_name("shop").is_ok());
        assert!(assert_db_name("").unwrap_err().contains("db is required"));
        assert!(assert_db_name("a.b").unwrap_err().contains("'.'"));
        assert!(assert_db_name("a b").is_err());
        assert!(assert_db_name(&"x".repeat(64)).is_err());
        assert!(assert_coll_name("shop", "orders").is_ok());
        assert!(assert_coll_name("shop", "system.profile").is_ok());
        assert!(assert_coll_name("shop", "a$b").is_err());
        assert!(assert_coll_name("shop", "").unwrap_err().contains("collection is required"));
        assert!(assert_coll_name("shop", &"c".repeat(260)).unwrap_err().contains("255"));
        let ns = mongo_ns_of(&json!({ "db": " shop ", "collection": "orders" })).expect("ns");
        assert_eq!(ns, MongoNs { db: "shop".into(), coll: "orders".into() });
    }

    #[test]
    fn a_find_clamps_its_page_and_names_a_bad_option() {
        let q = mongo_find_of(&json!({ "db": "s", "collection": "c" }), MONGO_PAGE_MAX).expect("find");
        assert_eq!(q.limit, MONGO_PAGE_DEFAULT);
        assert_eq!(q.skip, 0);
        assert_eq!(q.max_time_ms, MONGO_MAX_TIME_DEFAULT_MS);
        assert!(q.filter.is_empty() && q.sort.is_none() && q.projection.is_none());
        let q = mongo_find_of(
            &json!({ "db": "s", "collection": "c", "limit": "9999", "skip": "40", "sort": { "a": -1 }, "projection": {} }),
            MONGO_PAGE_MAX,
        )
        .expect("find");
        assert_eq!(q.limit, MONGO_PAGE_MAX);
        assert_eq!(q.skip, 40);
        assert_eq!(q.sort, Some(json!({ "a": -1 }).as_object().cloned().unwrap_or_default()));
        // An empty projection is no projection — it must not reach the server as `{}`.
        assert!(q.projection.is_none());
        let err = mongo_find_of(&json!({ "db": "s", "collection": "c", "filter": "x" }), 10).unwrap_err();
        assert_eq!(err, "filter must be a document");
        let err = mongo_find_of(&json!({ "db": "s", "collection": "c", "skip": -1 }), 10).unwrap_err();
        assert!(err.contains("skip must be a non-negative integer"), "{err}");
        let q = mongo_find_of(&json!({ "db": "s", "collection": "c", "maxTimeMS": 10_000_000 }), 10).expect("find");
        assert_eq!(q.max_time_ms, MONGO_MAX_TIME_MAX_MS);
    }

    #[test]
    fn a_pipeline_names_the_stage_that_is_wrong() {
        assert!(pipeline_of(None).expect("none").is_empty());
        let err = pipeline_of(Some(&json!({ "$match": {} }))).unwrap_err();
        assert_eq!(err, "pipeline must be an array of stages");
        let err = pipeline_of(Some(&json!([{ "$match": {} }, { "$match": {}, "$sort": { "a": 1 } }]))).unwrap_err();
        assert!(err.starts_with("stage 2 must hold exactly one operator (it has 2"), "{err}");
        let err = pipeline_of(Some(&json!([{ "match": {} }]))).unwrap_err();
        assert!(err.contains("not a $ operator"), "{err}");
        let err = pipeline_of(Some(&json!([1]))).unwrap_err();
        assert_eq!(err, "stage 1 is not a document");
    }

    #[test]
    fn a_writing_stage_writes_only_from_the_end() {
        let o = json!({ "db": "s", "collection": "c", "pipeline": [{ "$match": {} }, { "$out": "x" }] });
        let agg = mongo_aggregate_of(&o).expect("agg");
        assert!(pipeline_writes(&agg.pipeline));
        // The stage preview cuts the pipeline before the write: what remains is a read.
        let mut o2 = o.clone();
        o2["stages"] = json!(1);
        let agg = mongo_aggregate_of(&o2).expect("agg");
        assert_eq!(agg.pipeline.len(), 1);
        assert!(!pipeline_writes(&agg.pipeline));
        let o3 = json!({ "db": "s", "collection": "c", "pipeline": [{ "$merge": "x" }, { "$match": {} }] });
        let err = mongo_aggregate_of(&o3).unwrap_err();
        assert_eq!(err, "$merge must be the last stage (it is stage 1)");
        // An explain must never write.
        let err = mongo_explain_of(&o).unwrap_err();
        assert!(err.contains("would write"), "{err}");
        let ex = mongo_explain_of(&json!({ "db": "s", "collection": "c", "filter": { "a": 1 } })).expect("explain");
        assert_eq!(ex.verbosity, "executionStats");
        assert!(matches!(ex.target, MongoExplainTarget::Find(_)));
        assert!(mongo_explain_of(&json!({ "db": "s", "collection": "c", "verbosity": "loud" })).is_err());
    }

    #[test]
    fn edits_need_an_id_and_an_operator_shaped_update() {
        assert_eq!(mongo_edits_of(&json!({})).unwrap_err(), "edits must be an array");
        assert_eq!(mongo_edits_of(&json!({ "edits": [] })).unwrap_err(), "edits is empty");
        let err = mongo_edits_of(&json!({ "edits": [{ "op": "replace", "original": { "a": 1 }, "doc": {} }] })).unwrap_err();
        assert!(err.contains("no _id"), "{err}");
        let err = mongo_edits_of(&json!({ "edits": [{ "op": "updateMany", "filter": {}, "update": { "a": 1 } }] })).unwrap_err();
        assert!(err.contains("wrap it in $set"), "{err}");
        let err = mongo_edits_of(&json!({ "edits": [{ "op": "upsert" }] })).unwrap_err();
        assert!(err.contains("op must be insert, replace, delete"), "{err}");
        let list = mongo_edits_of(&json!({ "edits": [
            { "op": "insert", "doc": { "a": 1 } },
            { "op": "delete", "original": { "_id": { "$oid": "650000000000000000000001" } } },
            { "op": "updateMany", "filter": { "a": 1 }, "update": [{ "$set": { "b": 2 } }] },
        ] }))
        .expect("edits");
        assert_eq!(list.iter().map(MongoEdit::op).collect::<Vec<_>>(), ["insert", "delete", "updateMany"]);
        let too_many: Vec<Value> = (0..=MONGO_EDITS_MAX).map(|_| json!({ "op": "insert", "doc": {} })).collect();
        assert!(mongo_edits_of(&json!({ "edits": too_many })).unwrap_err().contains("at most"));
    }

    #[test]
    fn the_edit_filter_compares_the_whole_document_literally() {
        let original = json!({ "_id": { "$oid": "650000000000000000000001" }, "a.b": 1, "$x": "$notAnExpr" });
        let f = edit_filter(original.as_object().expect("obj")).expect("filter");
        assert_eq!(f["_id"], json!({ "$oid": "650000000000000000000001" }));
        // Dotted and $-prefixed field names ride inside $literal, untouched.
        assert_eq!(f["$expr"]["$eq"][0]["$cmp"][0], json!("$$ROOT"));
        assert_eq!(f["$expr"]["$eq"][0]["$cmp"][1]["$literal"], original);
        assert_eq!(f["$expr"]["$eq"][1], json!(0));
        assert!(edit_filter(json!({ "a": 1 }).as_object().expect("obj")).is_err());
        let a = json!({ "_id": 1, "x": 1, "y": 2 });
        let b = json!({ "_id": 1, "x": 9, "z": 3 });
        assert_eq!(
            changed_fields(a.as_object().expect("a"), b.as_object().expect("b")),
            ["x", "y", "z"]
        );
    }

    #[test]
    fn index_and_collection_ops_refuse_unknown_options() {
        let op = mongo_index_op_of(&json!({ "op": "create", "keys": { "a": 1, "b": -1 }, "options": { "unique": true } })).expect("create");
        assert!(matches!(op, MongoIndexOp::Create { .. }));
        assert!(mongo_index_op_of(&json!({ "op": "create", "keys": {} })).unwrap_err().contains("at least one field"));
        assert!(mongo_index_op_of(&json!({ "op": "create", "keys": { "a": 1 }, "options": { "uniq": true } }))
            .unwrap_err()
            .contains("unknown index option \"uniq\""));
        assert!(mongo_index_op_of(&json!({ "op": "drop", "name": "_id_" })).unwrap_err().contains("_id index"));
        assert_eq!(
            mongo_index_op_of(&json!({ "op": "unhide", "name": "a_1" })).expect("unhide"),
            MongoIndexOp::Hide { name: "a_1".into(), hidden: false }
        );
        let op = mongo_collection_op_of(&json!({ "db": "s", "op": "rename", "name": "a", "to": "b" })).expect("rename");
        assert_eq!(op, MongoCollectionOp::Rename { name: "a".into(), to: "b".into() });
        assert!(mongo_collection_op_of(&json!({ "db": "s", "op": "create", "name": "a", "options": { "autoIndexId": false } })).is_err());
        assert!(mongo_collection_op_of(&json!({ "db": "s", "op": "createView", "name": "v", "viewOn": "a", "pipeline": [{ "$out": "z" }] }))
            .unwrap_err()
            .contains("cannot write"));
    }

    #[test]
    fn bson_types_read_from_canonical_extended_json() {
        let cases = [
            (json!({ "$oid": "650000000000000000000001" }), "ObjectId"),
            (json!({ "$numberInt": "1" }), "Int32"),
            (json!({ "$numberLong": "9007199254740993" }), "Int64"),
            (json!({ "$numberDouble": "1.5" }), "Double"),
            (json!({ "$numberDecimal": "1.10" }), "Decimal128"),
            (json!({ "$date": { "$numberLong": "0" } }), "Date"),
            (json!({ "$binary": { "base64": "AAAA", "subType": "04" } }), "UUID"),
            (json!({ "$binary": { "base64": "AAAA", "subType": "00" } }), "Binary"),
            (json!({ "$regularExpression": { "pattern": "a", "options": "i" } }), "RegExp"),
            (json!({ "$timestamp": { "t": 1, "i": 2 } }), "Timestamp"),
            (json!({ "$code": "x", "$scope": {} }), "CodeWithScope"),
            (json!({ "$minKey": 1 }), "MinKey"),
            (json!({ "$ref": "c", "$id": 1 }), "Document"),
            (json!({ "a": 1 }), "Document"),
            (json!([1]), "Array"),
            (json!(null), "Null"),
            (json!(true), "Boolean"),
            (json!("s"), "String"),
            (json!(3), "Int32"),
            (json!(5_000_000_000_i64), "Int64"),
            (json!(1.5), "Double"),
        ];
        for (v, t) in cases {
            assert_eq!(bson_type(&v), t, "{v}");
        }
        assert_eq!(ejson_number(&json!({ "$numberLong": "42" })), Some(42.0));
        assert_eq!(ejson_number(&json!({ "$numberDouble": "NaN" })), None);
        assert_eq!(ejson_date_ms(&json!({ "$date": { "$numberLong": "86400000" } })), Some(86_400_000));
    }

    #[test]
    fn schema_inference_counts_types_paths_and_presence() {
        let docs = vec![
            json!({ "_id": { "$oid": "650000000000000000000001" }, "name": "ann", "age": { "$numberInt": "30" },
                    "tags": ["a", "b"], "addr": { "city": "x" } }),
            json!({ "_id": { "$oid": "650000000000000000000002" }, "name": "bob", "age": { "$numberLong": "41" },
                    "tags": [], "addr": { "city": "y", "zip": "1" } }),
            json!({ "_id": { "$oid": "650000000000000000000003" }, "name": null, "items": [{ "sku": "s1" }] }),
            json!("not a document"),
        ];
        let s = infer_schema(&docs);
        assert_eq!(s["sampled"], json!(3));
        let fields = s["fields"].as_array().expect("fields");
        assert_eq!(fields[0]["name"], json!("_id"));
        let name = fields.iter().find(|f| f["name"] == json!("name")).expect("name");
        assert_eq!(name["count"], json!(3));
        let types: Vec<&str> = name["types"].as_array().expect("t").iter().filter_map(|t| t["type"].as_str()).collect();
        assert_eq!(types, ["String", "Null"]);
        let age = fields.iter().find(|f| f["name"] == json!("age")).expect("age");
        assert_eq!(age["missing"], json!(1));
        assert_eq!(age["probability"], json!(0.6667));
        let age_types = age["types"].as_array().expect("t");
        assert_eq!(age_types.len(), 2, "Int32 and Int64 are two types");
        let tags = fields.iter().find(|f| f["name"] == json!("tags")).expect("tags");
        assert_eq!(tags["types"][0]["lengths"], json!({ "min": 0, "max": 2, "avg": 1.0 }));
        assert_eq!(tags["types"][0]["elements"]["count"], json!(2));
        let addr = fields.iter().find(|f| f["name"] == json!("addr")).expect("addr");
        let zip = addr["types"][0]["fields"].as_array().expect("sub").iter().find(|f| f["name"] == json!("zip")).expect("zip");
        assert_eq!(zip["path"], json!("addr.zip"));
        assert_eq!(zip["probability"], json!(0.5));
        let paths = schema_paths(&s);
        for p in ["_id", "name", "addr.city", "addr.zip", "items.sku"] {
            assert!(paths.iter().any(|x| x == p), "{p} missing from {paths:?}");
        }
    }

    #[test]
    fn schema_extremes_are_the_values_themselves_not_their_doubles() {
        // 2^53 + 1 and + 3: as f64 both round, and a reported min of 9007199254740992 would be a
        // value the collection does not hold.
        // ...1047 and ...1049 round to one double, so an f64 comparison would keep whichever came
        // first; the order between integers is exact.
        let docs = vec![
            json!({ "n": { "$numberLong": "9007199254741047" } }),
            json!({ "n": { "$numberLong": "9007199254741049" } }),
            json!({ "n": { "$numberLong": "12" } }),
            json!({ "n": { "$numberDouble": "12.5" } }),
        ];
        let s = infer_schema(&docs);
        let n = &s["fields"][0]["types"][0];
        assert_eq!(n["type"], json!("Int64"));
        assert_eq!(n["min"], json!({ "$numberLong": "12" }));
        assert_eq!(n["max"], json!({ "$numberLong": "9007199254741049" }));
    }

    #[test]
    fn schema_inference_stays_bounded() {
        let docs: Vec<Value> = (0..SCHEMA_MAX_FIELDS + 10)
            .map(|i| {
                let mut m = Map::new();
                m.insert("v".into(), json!(format!("value-{i}")));
                m.insert(format!("f{i}"), json!(1));
                Value::Object(m)
            })
            .collect();
        let s = infer_schema(&docs);
        assert_eq!(s["fieldsCapped"], json!(true));
        assert!(s["fields"].as_array().expect("f").len() <= SCHEMA_MAX_FIELDS);
        let v = s["fields"].as_array().expect("f").iter().find(|f| f["name"] == json!("v")).expect("v");
        assert_eq!(v["types"][0]["distinctCapped"], json!(true));
        assert_eq!(v["types"][0]["distinct"], json!(SCHEMA_DISTINCT_MAX));
        assert_eq!(v["types"][0]["values"].as_array().expect("vals").len(), SCHEMA_SAMPLE_VALUES);
    }

    #[test]
    fn explain_summaries_flag_collection_scans_and_memory_sorts() {
        let find = json!({
            "queryPlanner": {
                "namespace": "s.c",
                "winningPlan": { "stage": "SORT", "sortPattern": { "a": 1 },
                    "inputStage": { "stage": "COLLSCAN", "filter": { "b": { "$eq": 1 } } } },
                "rejectedPlans": [],
            },
            "executionStats": {
                "nReturned": 3, "executionTimeMillis": 7, "totalKeysExamined": 0, "totalDocsExamined": 1000,
                "executionStages": { "stage": "SORT", "nReturned": 3,
                    "inputStage": { "stage": "COLLSCAN", "nReturned": 3, "docsExamined": 1000 } },
            },
        });
        let s = explain_summary(&find);
        assert_eq!(s["collscan"], json!(true));
        assert_eq!(s["inMemorySort"], json!(true));
        assert_eq!(s["totalDocsExamined"], json!(1000));
        assert_eq!(s["engine"], json!("classic"));
        assert_eq!(s["stages"][1]["stage"], json!("COLLSCAN"));
        assert_eq!(s["stages"][1]["depth"], json!(1));
        assert_eq!(s["stages"][1]["docsExamined"], json!(1000));
        // An aggregation: the cursor part sits under the first stage, the rest are pipeline rows.
        let agg = json!({
            "stages": [
                { "$cursor": { "queryPlanner": { "winningPlan": { "queryPlan": {
                    "stage": "FETCH", "inputStage": { "stage": "IXSCAN", "indexName": "a_1", "keyPattern": { "a": 1 } } },
                    "slotBasedPlan": {} } } } },
                { "$group": { "_id": "$a" }, "nReturned": 4, "executionTimeMillisEstimate": 2 },
            ],
        });
        let s = explain_summary(&agg);
        assert_eq!(s["collscan"], json!(false));
        assert_eq!(s["indexes"], json!(["a_1"]));
        assert_eq!(s["engine"], json!("sbe"));
        assert_eq!(s["pipeline"][0]["stage"], json!("$group"));
        assert_eq!(s["pipeline"][0]["nReturned"], json!(4));
    }

    #[test]
    fn current_op_rows_speak_the_shared_activity_shape() {
        let row = mongo_activity_row(&json!({
            "opid": 1234, "active": true, "secs_running": 12, "microsecs_running": 12_500_000,
            "op": "query", "ns": "s.c", "waitingForLock": true, "appName": "worker",
            "effectiveUsers": [{ "user": "app", "db": "admin" }],
            "command": { "find": "c", "filter": { "a": 1 } },
        }));
        assert_eq!(row["pid"], json!(1234));
        assert_eq!(row["user"], json!("app"));
        assert_eq!(row["state"], json!("active"));
        assert_eq!(row["wait"], json!("lock"));
        assert_eq!(row["seconds"], json!(12));
        assert_eq!(row["own"], json!(false));
        assert_eq!(row["killable"], json!(true));
        assert_eq!(row["app"], json!("worker"));
        assert!(row["query"].as_str().expect("q").contains("\"find\":\"c\""));
        let own = mongo_activity_row(&json!({ "opid": 9, "command": { "aggregate": 1, "pipeline": [{ "$currentOp": {} }] } }));
        assert_eq!(own["own"], json!(true));
        assert_eq!(own["killable"], json!(false));
        let mongos = mongo_activity_row(&json!({ "opid": "shard01:42", "command": {} }));
        assert_eq!(mongos["pid"], json!(null));
        assert_eq!(mongos["opid"], json!("shard01:42"));
        assert_eq!(mongos["killable"], json!(false));
    }

    #[test]
    fn background_threads_and_parked_monitors_are_not_activity() {
        assert!(mongo_activity_noise(&json!({ "op": "none", "desc": "Checkpointer" })));
        assert!(mongo_activity_noise(&json!({
            "op": "command",
            "command": { "hello": 1, "topologyVersion": {}, "maxAwaitTimeMS": 10000 },
        })));
        // A plain hello, and real work, stay listed.
        assert!(!mongo_activity_noise(&json!({ "op": "command", "command": { "hello": 1 } })));
        assert!(!mongo_activity_noise(&json!({ "op": "query", "command": { "find": "users" } })));
    }

    #[test]
    fn csv_flattening_descends_documents_and_unwraps_scalars() {
        let doc = json!({
            "_id": { "$oid": "650000000000000000000001" },
            "at": { "$date": "2026-01-02T03:04:05Z" },
            "price": { "$numberDecimal": "9.99" },
            "addr": { "city": "x", "geo": { "lat": 1.5 } },
            "tags": ["a", "b"],
            "none": null,
        });
        let cells = flatten_doc(doc.as_object().expect("obj"));
        assert_eq!(
            cells,
            vec![
                ("_id".to_string(), "650000000000000000000001".to_string()),
                ("at".to_string(), "2026-01-02T03:04:05Z".to_string()),
                ("price".to_string(), "9.99".to_string()),
                ("addr.city".to_string(), "x".to_string()),
                ("addr.geo.lat".to_string(), "1.5".to_string()),
                ("tags".to_string(), "[\"a\",\"b\"]".to_string()),
                ("none".to_string(), String::new()),
            ]
        );
    }
}
