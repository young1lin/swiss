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

//! The MongoDB Data-view browser (SPEC §data.mongo): the [`MongoBrowser`] the routes lease, over
//! the engine's own client. Documents leave as canonical Extended JSON and come back the same
//! way, so a value the panel never touched is written back bit for bit.

use std::sync::Arc;

use async_trait::async_trait;
use mongodb::bson::{doc, Bson, Document};
use mongodb::options::UpdateModifications;
use mongodb::ClientSession;
use serde_json::{json, Map, Value};

use swiss_host::dbbrowser::{EditConflict, EditError};
use swiss_host::mongobrowser::{
    changed_fields, edit_filter, explain_summary, mongo_activity_noise, mongo_activity_row, stage_op, MongoAggregate,
    MongoBrowser, MongoCollectionOp, MongoCount, MongoDump, MongoEdit, MongoExplain, MongoFind,
    MongoImport, MongoIndexOp, MongoNs, MONGO_COUNT_TIME_MS,
};

use super::direct::Lazy;
use super::mongo::{
    assert_command_allowed, doc_json, ejson, err_text, find_command, num, to_bson, to_doc, Ejson,
    without_gossip, MongoHandle, ServerFacts,
};

/// Errors one import itemises; the rest are counted.
const IMPORT_ERRORS_SHOWN: usize = 20;
/// Documents an export holds in flight between the cursor and the response body.
const EXPORT_CHANNEL: usize = 64;

pub struct MongoDataBrowser {
    label: String,
    allow_destructive: bool,
    conn: Arc<Lazy<MongoHandle>>,
}

impl MongoDataBrowser {
    pub fn new(label: String, allow_destructive: bool, conn: Arc<Lazy<MongoHandle>>) -> Self {
        Self { label, allow_destructive, conn }
    }

    fn refuse_destroying(&self, what: &str) -> Result<(), String> {
        if self.allow_destructive {
            return Ok(());
        }
        Err(format!(
            "{what} is rejected here: it destroys data. Set \"allowDestructive\": true on this MCP to permit it."
        ))
    }
}

/// The default name the server would give an index: `field_dir` pairs joined by `_`
/// (`email_1`, `a_1_b_-1`, `body_text`). Named here because servers before 4.2 require one.
pub fn default_index_name(keys: &Map<String, Value>) -> String {
    keys.iter()
        .map(|(k, v)| {
            let dir = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Object(o) => o.values().next().and_then(Value::as_str).unwrap_or("1").to_string(),
                other => other.to_string(),
            };
            format!("{k}_{dir}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// One edit, converted to BSON before anything is written — a value that is not valid
/// Extended JSON is a 400 naming its edit, never a half-applied batch.
enum Prepared {
    Insert(Document),
    Replace { filter: Document, doc: Document, id: Value, original: Map<String, Value> },
    Delete { filter: Document, id: Value, original: Map<String, Value> },
    DeleteMany(Document),
    UpdateMany(Document, UpdateModifications),
}

fn prepare(i: usize, e: &MongoEdit) -> Result<Prepared, String> {
    let at = |what: &str| format!("edit {} ({}): {what}", i + 1, e.op());
    Ok(match e {
        MongoEdit::Insert { doc } => Prepared::Insert(to_doc(doc, &at("doc"))?),
        MongoEdit::Replace { original, doc } => {
            let id = original.get("_id").cloned().unwrap_or(Value::Null);
            // _id is immutable: say so here rather than relay the server's code 66. Compared as
            // BSON — `{"$numberInt": "1"}` and a relaxed `1` are the same Int32.
            if let Some(new_id) = doc.get("_id") {
                if to_bson(new_id, &at("doc._id"))? != to_bson(&id, &at("original._id"))? {
                    return Err(at("_id cannot change — insert a copy with the new _id and delete this one instead"));
                }
            }
            Prepared::Replace {
                filter: to_doc(&edit_filter(original)?, &at("original"))?,
                doc: to_doc(doc, &at("doc"))?,
                id,
                original: original.clone(),
            }
        }
        MongoEdit::Delete { original } => Prepared::Delete {
            filter: to_doc(&edit_filter(original)?, &at("original"))?,
            id: original.get("_id").cloned().unwrap_or(Value::Null),
            original: original.clone(),
        },
        MongoEdit::DeleteMany { filter } => Prepared::DeleteMany(to_doc(filter, &at("filter"))?),
        MongoEdit::UpdateMany { filter, update } => {
            let filter = to_doc(filter, &at("filter"))?;
            let update = match update {
                Value::Array(stages) => {
                    let mut out = Vec::with_capacity(stages.len());
                    for (j, s) in stages.iter().enumerate() {
                        match s {
                            Value::Object(m) => out.push(to_doc(m, &at(&format!("update stage {}", j + 1)))?),
                            _ => return Err(at(&format!("update stage {} is not a document", j + 1))),
                        }
                    }
                    UpdateModifications::Pipeline(out)
                }
                Value::Object(m) => UpdateModifications::Document(to_doc(m, &at("update"))?),
                _ => return Err(at("update must be a document or a pipeline")),
            };
            Prepared::UpdateMany(filter, update)
        }
    })
}

/// What one applied edit did. `None` from a replace/delete means its optimistic filter matched
/// nothing: another writer got there first.
async fn run_one(
    coll: &mongodb::Collection<Document>,
    p: &Prepared,
    session: Option<&mut ClientSession>,
) -> Result<Option<Value>, mongodb::error::Error> {
    macro_rules! go {
        ($action:expr) => {
            match session {
                Some(s) => $action.session(s).await,
                None => $action.await,
            }
        };
    }
    Ok(match p {
        Prepared::Insert(d) => {
            let r = go!(coll.insert_one(d.clone()))?;
            Some(json!({ "op": "insert", "insertedId": ejson(r.inserted_id, Ejson::Canonical) }))
        }
        Prepared::Replace { filter, doc, .. } => {
            let r = go!(coll.replace_one(filter.clone(), doc.clone()))?;
            (r.matched_count > 0).then(|| json!({ "op": "replace", "matched": r.matched_count, "modified": r.modified_count }))
        }
        Prepared::Delete { filter, .. } => {
            let r = go!(coll.delete_one(filter.clone()))?;
            (r.deleted_count > 0).then(|| json!({ "op": "delete", "deleted": r.deleted_count }))
        }
        Prepared::DeleteMany(filter) => {
            let r = go!(coll.delete_many(filter.clone()))?;
            Some(json!({ "op": "deleteMany", "deleted": r.deleted_count }))
        }
        Prepared::UpdateMany(filter, update) => {
            let r = go!(coll.update_many(filter.clone(), update.clone()))?;
            Some(json!({ "op": "updateMany", "matched": r.matched_count, "modified": r.modified_count }))
        }
    })
}

/// The 409 of a lost race: which fields moved (or that the document is gone), the document
/// addressed by its `_id` so the panel finds the exact edit among many.
async fn conflict_of(
    coll: &mongodb::Collection<Document>,
    i: usize,
    id: &Value,
    original: &Map<String, Value>,
    tail: &str,
) -> EditConflict {
    let current = match to_bson(id, "_id") {
        Ok(b) => coll.find_one(doc! { "_id": b }).await.ok().flatten(),
        Err(_) => None,
    };
    let row = Some(Map::from_iter([("_id".to_string(), id.clone())]));
    match current {
        None => EditConflict {
            message: format!("edit {}: the document was deleted by another writer{tail}", i + 1),
            columns: Vec::new(),
            row,
        },
        Some(d) => {
            let now = match doc_json(d, Ejson::Canonical) {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let columns = changed_fields(original, &now);
            EditConflict {
                message: format!(
                    "edit {}: the document was changed by another writer{}{tail}",
                    i + 1,
                    if columns.is_empty() { String::new() } else { format!(" — fields: {}", columns.join(", ")) }
                ),
                columns,
                row,
            }
        }
    }
}

fn kept(applied: usize, facts: &ServerFacts) -> String {
    let why = format!("a {} has no multi-document transactions", facts.topology);
    match applied {
        0 => " Nothing was written.".to_string(),
        1 => format!(" Edit 1 was applied and stays applied: {why}."),
        n => format!(" Edits 1–{n} were applied and stay applied: {why}."),
    }
}

#[async_trait]
impl MongoBrowser for MongoDataBrowser {
    fn label(&self) -> String {
        self.label.clone()
    }

    async fn server_info(&self) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let f = handle.server_facts().await?;
        Ok(json!({
            "version": f.version,
            "topology": f.topology,
            "setName": f.set_name,
            "maxWireVersion": f.max_wire,
            "transactions": f.transactions(),
            "allowDestructive": self.allow_destructive,
            "defaultDb": handle.default_db(),
        }))
    }

    async fn list_databases(&self) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let mut dbs = handle.databases().await?;
        let system = |n: &str| matches!(n, "admin" | "local" | "config");
        dbs.sort_by(|a, b| system(&a.0).cmp(&system(&b.0)).then(a.0.cmp(&b.0)));
        let primary = handle.default_db().map(str::to_string);
        let current = primary.clone().or_else(|| dbs.iter().find(|d| !system(&d.0)).map(|d| d.0.clone()));
        let rows: Vec<Value> = dbs
            .into_iter()
            .map(|(name, size, empty)| {
                // The row the panel opens on: the URL's database, else the first user database
                // (a URL that names none still browses somewhere, never an empty sidebar).
                let mut row = json!({
                    "name": name,
                    "primary": current.as_deref() == Some(name.as_str()),
                    "browsable": true,
                    "system": system(&name),
                });
                if let Some(s) = size {
                    row["size"] = json!(s);
                }
                if empty {
                    row["empty"] = json!(true);
                }
                row
            })
            .collect();
        Ok(json!({ "primary": primary, "current": current, "databases": rows }))
    }

    async fn list_collections(&self, db: &str) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let (rows, omitted) = handle.collections(db, true, Ejson::Canonical).await?;
        Ok(json!({ "db": db, "collections": rows, "statsOmitted": omitted }))
    }

    async fn find(&self, q: &MongoFind) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let (docs, more) = handle.find_page(q).await?;
        let docs: Vec<Value> = docs.into_iter().map(|d| doc_json(d, Ejson::Canonical)).collect();
        Ok(json!({
            "db": q.ns.db,
            "collection": q.ns.coll,
            "docs": docs,
            "offset": q.skip,
            "limit": q.limit,
            "more": more,
        }))
    }

    async fn count(&self, q: &MongoCount) -> Result<Value, String> {
        self.conn.get().await?.count(q).await
    }

    async fn aggregate(&self, q: &MongoAggregate) -> Result<Value, String> {
        if q.pipeline.iter().any(|s| stage_op(s) == "$out") {
            self.refuse_destroying("$out")?;
        }
        let handle = self.conn.get().await?;
        let (docs, more, wrote) = handle.aggregate_docs(q).await?;
        if let Some(ns) = wrote {
            return Ok(json!({ "docs": [], "more": false, "wrote": ns }));
        }
        let docs: Vec<Value> = docs.into_iter().map(|d| doc_json(d, Ejson::Canonical)).collect();
        Ok(json!({ "docs": docs, "more": more }))
    }

    async fn explain(&self, q: &MongoExplain) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let reply = doc_json(handle.explain(q).await?, Ejson::Relaxed);
        let summary = explain_summary(&reply);
        Ok(json!({ "explain": reply, "summary": summary }))
    }

    async fn sample(&self, ns: &MongoNs, filter: &Map<String, Value>, size: i64) -> Result<Vec<Value>, String> {
        let handle = self.conn.get().await?;
        Ok(handle.sample(ns, filter, size).await?.into_iter().map(|d| doc_json(d, Ejson::Canonical)).collect())
    }

    async fn indexes(&self, ns: &MongoNs) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let (list, stats_error) = handle.indexes(ns, Ejson::Canonical).await?;
        let mut out = json!({ "indexes": list });
        if let Some(e) = stats_error {
            out["statsError"] = json!(e);
        }
        Ok(out)
    }

    async fn index_op(&self, ns: &MongoNs, op: &MongoIndexOp) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        match op {
            MongoIndexOp::Create { keys, options } => {
                let name = options
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| default_index_name(keys));
                let mut spec = doc! { "key": to_doc(keys, "keys")?, "name": &name };
                for (k, v) in options {
                    if k != "name" {
                        spec.insert(k.clone(), to_bson(v, k)?);
                    }
                }
                handle.command(&ns.db, doc! { "createIndexes": &ns.coll, "indexes": [spec] }).await?;
                Ok(json!({ "ok": true, "name": name }))
            }
            MongoIndexOp::Drop { name } => {
                handle.command(&ns.db, doc! { "dropIndexes": &ns.coll, "index": name }).await?;
                Ok(json!({ "ok": true }))
            }
            MongoIndexOp::Hide { name, hidden } => {
                handle
                    .command(&ns.db, doc! { "collMod": &ns.coll, "index": { "name": name, "hidden": *hidden } })
                    .await?;
                Ok(json!({ "ok": true }))
            }
        }
    }

    /// SPEC §data.mongo-edits. Every edit is converted first; then, with more than one edit on
    /// a deployment that has transactions, all of them commit or none does. Without
    /// transactions they apply in order and the first failure stops the rest — and the reply
    /// says exactly which edits stayed applied, never "rolled back" when nothing could be.
    async fn apply_edits(&self, ns: &MongoNs, edits: &[MongoEdit]) -> Result<Value, EditError> {
        let prepared: Vec<Prepared> = edits.iter().enumerate().map(|(i, e)| prepare(i, e)).collect::<Result<_, _>>()?;
        let handle = self.conn.get().await?;
        let facts = handle.server_facts().await?;
        let coll = handle.client().database(&ns.db).collection::<Document>(&ns.coll);
        let atomic = prepared.len() > 1 && facts.transactions();
        let mut results: Vec<Value> = Vec::with_capacity(prepared.len());
        if atomic {
            let mut session = handle.client().start_session().await.map_err(|e| err_text(&e))?;
            session.start_transaction().await.map_err(|e| err_text(&e))?;
            for (i, p) in prepared.iter().enumerate() {
                match run_one(&coll, p, Some(&mut session)).await {
                    Ok(Some(r)) => results.push(r),
                    Ok(None) => {
                        let _ = session.abort_transaction().await;
                        let (id, original) = match p {
                            Prepared::Replace { id, original, .. } | Prepared::Delete { id, original, .. } => (id, original),
                            _ => return Err(EditError::Bad(format!("edit {} matched nothing", i + 1))),
                        };
                        let tail = " — the transaction was rolled back; nothing was written. Reload and resubmit.";
                        return Err(EditError::Conflict(conflict_of(&coll, i, id, original, tail).await));
                    }
                    Err(e) => {
                        let _ = session.abort_transaction().await;
                        return Err(EditError::Bad(format!(
                            "edit {} ({}) failed: {} — the transaction was rolled back; nothing was written.",
                            i + 1,
                            edits[i].op(),
                            err_text(&e)
                        )));
                    }
                }
            }
            session.commit_transaction().await.map_err(|e| {
                EditError::Bad(format!("the commit failed: {} — nothing was written.", err_text(&e)))
            })?;
        } else {
            for (i, p) in prepared.iter().enumerate() {
                match run_one(&coll, p, None).await {
                    Ok(Some(r)) => results.push(r),
                    Ok(None) => {
                        let (id, original) = match p {
                            Prepared::Replace { id, original, .. } | Prepared::Delete { id, original, .. } => (id, original),
                            _ => return Err(EditError::Bad(format!("edit {} matched nothing", i + 1))),
                        };
                        let tail = format!(" — reload and resubmit.{}", kept(i, &facts));
                        return Err(EditError::Conflict(conflict_of(&coll, i, id, original, &tail).await));
                    }
                    Err(e) => {
                        return Err(EditError::Bad(format!(
                            "edit {} ({}) failed: {}.{}",
                            i + 1,
                            edits[i].op(),
                            err_text(&e),
                            kept(i, &facts)
                        )));
                    }
                }
            }
        }
        Ok(json!({ "ok": true, "applied": results.len(), "transaction": atomic, "results": results }))
    }

    async fn run_command(&self, db: &str, command: &Map<String, Value>) -> Result<Value, String> {
        assert_command_allowed(command, self.allow_destructive)?;
        let cmd = to_doc(command, "command")?;
        let handle = self.conn.get().await?;
        let reply = handle.command(db, cmd).await?;
        Ok(json!({ "reply": doc_json(without_gossip(reply), Ejson::Canonical) }))
    }

    async fn collection_op(&self, db: &str, op: &MongoCollectionOp) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        match op {
            MongoCollectionOp::Create { name, options } => {
                let mut cmd = doc! { "create": name };
                for (k, v) in options {
                    cmd.insert(k.clone(), to_bson(v, k)?);
                }
                handle.command(db, cmd).await?;
            }
            MongoCollectionOp::CreateView { name, view_on, pipeline } => {
                let mut stages: Vec<Bson> = Vec::with_capacity(pipeline.len());
                for (i, s) in pipeline.iter().enumerate() {
                    stages.push(Bson::Document(to_doc(s, &format!("stage {}", i + 1))?));
                }
                handle.command(db, doc! { "create": name, "viewOn": view_on, "pipeline": stages }).await?;
            }
            MongoCollectionOp::Rename { name, to } => {
                // renameCollection runs against admin with full namespaces; dropTarget stays off,
                // so a rename onto an existing collection is the server's refusal, not a drop.
                handle
                    .command("admin", doc! { "renameCollection": format!("{db}.{name}"), "to": format!("{db}.{to}") })
                    .await?;
            }
            MongoCollectionOp::Drop { name } => {
                self.refuse_destroying("drop")?;
                handle.command(db, doc! { "drop": name }).await?;
            }
        }
        Ok(json!({ "ok": true }))
    }

    async fn export(&self, q: &MongoFind, relaxed: bool) -> Result<MongoDump, String> {
        let handle = self.conn.get().await?;
        // The count first, for the response headers; a count that runs out of its short budget
        // leaves the size unknown (-1) rather than holding the download back.
        let count = handle
            .count(&MongoCount {
                ns: q.ns.clone(),
                filter: q.filter.clone(),
                hint: q.hint.clone(),
                exact: !q.filter.is_empty(),
                max_time_ms: MONGO_COUNT_TIME_MS,
            })
            .await
            .ok()
            .and_then(|c| c.get("total").and_then(Value::as_i64));
        let total = count.map(|n| (n - q.skip as i64).max(0));
        let rows = total.map_or(-1, |n| n.min(q.limit));
        let capped = total.is_some_and(|n| n > q.limit);
        let cmd = find_command(q, false)?;
        let mode = if relaxed { Ejson::Relaxed } else { Ejson::Canonical };
        let client = handle.client().clone();
        let db = q.ns.db.clone();
        let (tx, rx) = tokio::sync::mpsc::channel(EXPORT_CHANNEL);
        tokio::spawn(async move {
            let mut cursor = match client.database(&db).run_cursor_command(cmd).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(err_text(&e))).await;
                    return;
                }
            };
            loop {
                let item = match cursor.advance().await {
                    Ok(true) => cursor.deserialize_current().map(|d| doc_json(d, mode)).map_err(|e| err_text(&e)),
                    Ok(false) => break,
                    Err(e) => Err(err_text(&e)),
                };
                let stop = item.is_err();
                // A closed receiver is a download the client gave up on: stop reading.
                if tx.send(item).await.is_err() || stop {
                    break;
                }
            }
        });
        Ok(MongoDump { rows, capped, docs: rx })
    }

    async fn import(&self, q: &MongoImport) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let mut docs: Vec<Document> = Vec::with_capacity(q.docs.len());
        let mut source: Vec<usize> = Vec::with_capacity(q.docs.len());
        let mut errors: Vec<(usize, String)> = Vec::new();
        for (i, m) in q.docs.iter().enumerate() {
            match to_doc(m, "the document") {
                Ok(d) => {
                    docs.push(d);
                    source.push(i);
                }
                Err(e) => errors.push((i, e)),
            }
        }
        let mut inserted = 0usize;
        if !docs.is_empty() {
            let total = docs.len();
            let coll = handle.client().database(&q.ns.db).collection::<Document>(&q.ns.coll);
            match coll.insert_many(docs).ordered(false).await {
                Ok(r) => inserted = r.inserted_ids.len(),
                Err(e) => match e.kind.as_ref() {
                    mongodb::error::ErrorKind::InsertMany(m) => {
                        let failed = m.write_errors.as_deref().unwrap_or(&[]);
                        inserted = total.saturating_sub(failed.len());
                        for w in failed {
                            let at = source.get(w.index).copied().unwrap_or(w.index);
                            // A validation failure explains itself in errInfo — which rule, which field.
                            let details = w.details.clone().map(|d| format!(" — {}", doc_json(d, Ejson::Relaxed))).unwrap_or_default();
                            errors.push((at, format!("{} (code {}){details}", w.message, w.code)));
                        }
                        if let Some(wc) = &m.write_concern_error {
                            errors.push((usize::MAX, format!("write concern failed: {} (code {})", wc.message, wc.code)));
                        }
                    }
                    _ => return Err(err_text(&e)),
                },
            }
        }
        errors.sort_by_key(|(i, _)| *i);
        let error_count = errors.len();
        let shown: Vec<Value> = errors
            .into_iter()
            .take(IMPORT_ERRORS_SHOWN)
            .map(|(i, message)| if i == usize::MAX { json!({ "message": message }) } else { json!({ "index": i, "message": message }) })
            .collect();
        Ok(json!({ "inserted": inserted, "errorCount": error_count, "errors": shown }))
    }

    async fn activity(&self) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let rows: Vec<Value> = handle
            .current_ops(true)
            .await?
            .into_iter()
            .map(|d| doc_json(d, Ejson::Relaxed))
            .filter(|op| !mongo_activity_noise(op))
            .map(|op| mongo_activity_row(&op))
            .collect();
        Ok(json!({ "rows": rows }))
    }

    async fn activity_kill(&self, opid: i64) -> Result<Value, String> {
        let handle = self.conn.get().await?;
        let op = match i32::try_from(opid) {
            Ok(n) => Bson::Int32(n),
            Err(_) => Bson::Int64(opid),
        };
        let reply = handle.command("admin", doc! { "killOp": 1, "op": op }).await?;
        Ok(json!({ "ok": num(reply.get("ok")).unwrap_or(0) == 1 }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            other => panic!("expected a document, got {other}"),
        }
    }

    #[test]
    fn index_names_follow_the_servers_own_rule() {
        assert_eq!(default_index_name(&map(json!({ "email": 1 }))), "email_1");
        assert_eq!(default_index_name(&map(json!({ "a": 1, "b": -1 }))), "a_1_b_-1");
        assert_eq!(default_index_name(&map(json!({ "body": "text" }))), "body_text");
        assert_eq!(default_index_name(&map(json!({ "n": { "$numberInt": "-1" } }))), "n_-1");
    }

    #[test]
    fn an_edit_with_bad_extended_json_is_refused_before_anything_is_written() {
        let e = MongoEdit::Insert { doc: map(json!({ "_id": { "$oid": "zz" } })) };
        let err = prepare(2, &e).err().expect("refused");
        assert!(err.starts_with("edit 3 (insert): doc is not valid Extended JSON"), "{err}");
    }

    #[test]
    fn a_replace_may_not_change_the_id() {
        let e = MongoEdit::Replace {
            original: map(json!({ "_id": { "$numberInt": "1" }, "a": "x" })),
            doc: map(json!({ "_id": { "$numberInt": "2" }, "a": "y" })),
        };
        let err = prepare(0, &e).err().expect("refused");
        assert!(err.contains("_id cannot change"), "{err}");
    }

    #[test]
    fn a_replace_matches_on_the_whole_document_it_read() {
        let original = map(json!({ "_id": { "$oid": "65f0c0ffee00000000000001" }, "n": { "$numberLong": "9007199254740993" } }));
        let e = MongoEdit::Replace { original: original.clone(), doc: map(json!({ "n": { "$numberLong": "1" } })) };
        let Ok(Prepared::Replace { filter, .. }) = prepare(0, &e) else { panic!("a replace") };
        assert!(matches!(filter.get("_id"), Some(Bson::ObjectId(_))));
        // The comparison carries the Int64 exactly, not a rounded double.
        let literal = filter
            .get_document("$expr")
            .unwrap()
            .get_array("$eq")
            .unwrap()[1]
            .as_document()
            .unwrap()
            .get_document("$literal")
            .unwrap()
            .clone();
        assert_eq!(literal.get("n"), Some(&Bson::Int64(9_007_199_254_740_993)));
    }

    #[test]
    fn the_id_compares_as_bson_not_as_text() {
        let e = MongoEdit::Replace {
            original: map(json!({ "_id": { "$numberInt": "1" }, "a": "x" })),
            doc: map(json!({ "_id": 1, "a": "y" })),
        };
        assert!(prepare(0, &e).is_ok());
    }

    #[test]
    fn an_update_pipeline_is_kept_a_pipeline() {
        let e = MongoEdit::UpdateMany { filter: Map::new(), update: json!([{ "$set": { "a": 1 } }]) };
        assert!(matches!(prepare(0, &e), Ok(Prepared::UpdateMany(_, UpdateModifications::Pipeline(p))) if p.len() == 1));
        let e = MongoEdit::UpdateMany { filter: Map::new(), update: json!({ "$inc": { "a": 1 } }) };
        assert!(matches!(prepare(0, &e), Ok(Prepared::UpdateMany(_, UpdateModifications::Document(_)))));
    }

    #[test]
    fn a_partial_apply_says_what_stayed_applied() {
        let standalone = ServerFacts { version: String::new(), topology: "standalone", set_name: None, max_wire: 21 };
        assert_eq!(kept(0, &standalone), " Nothing was written.");
        assert!(kept(1, &standalone).contains("Edit 1 was applied"));
        assert!(kept(3, &standalone).contains("Edits 1–3 were applied and stay applied: a standalone"));
    }
}
