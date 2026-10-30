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

//! The data browser's shapes: columns, tables, indexes, edits and their conflicts, filters and
//! built statements.

use serde::Serialize;
use serde_json::{Map, Value};

// --- shapes --------------------------------------------------------------------------------------

/// One column of the Structure view. `defaultValue` and `comment` are always serialized (null
/// when absent) — toBrowseColumns sets both unconditionally, the way the Node object literal did.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseColumn {
    pub name: String,
    #[serde(rename = "dataType")]
    pub data_type: String,
    pub nullable: bool,
    #[serde(rename = "isPrimaryKey")]
    pub is_primary_key: bool,
    #[serde(rename = "defaultValue")]
    pub default_value: Option<String>,
    /// The column's COMMENT text — MySQL information_schema or pg col_description. Null when the
    /// column carries none; the panel surfaces it in the grid header tooltip and Columns tab.
    pub comment: Option<String>,
}

/// One row of the lazy table list.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseTableInfo {
    pub schema: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "approxRows")]
    pub approx_rows: Option<i64>,
    pub size: String,
}

/// One index of the Structure tabs, folded from per-column rows by toBrowseIndexes.
/// MySQL leaves `definition` absent; PG fills it with the CREATE INDEX statement from pg_indexes.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseIndex {
    pub name: String,
    pub unique: bool,
    pub primary: bool,
    pub columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowseForeignKey {
    pub name: String,
    pub column: String,
    #[serde(rename = "refSchema")]
    pub ref_schema: String,
    #[serde(rename = "refTable")]
    pub ref_table: String,
    #[serde(rename = "refColumn")]
    pub ref_column: String,
}

/// Columns, indexes, foreign keys and DDL for one table — the Structure tabs.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseTableDetail {
    pub schema: String,
    pub table: String,
    pub columns: Vec<BrowseColumn>,
    #[serde(rename = "primaryKey")]
    pub primary_key: Vec<String>,
    pub indexes: Vec<BrowseIndex>,
    #[serde(rename = "foreignKeys")]
    pub foreign_keys: Vec<BrowseForeignKey>,
    /// The CREATE TABLE text — verbatim from MySQL SHOW CREATE TABLE, synthesized from the
    /// catalog for Postgres (which has no equivalent command).
    pub ddl: String,
}

/// One applied edit: what it was, and how many rows it touched.
#[derive(Clone, Debug, Serialize)]
pub struct BrowseEditResult {
    pub op: &'static str,
    pub affected: i64,
}

/// A whole-table export folded to text.
#[derive(Clone, Debug, Serialize)]
pub struct ExportResult {
    pub format: String,
    pub columns: Vec<String>,
    pub rows: i64,
    pub capped: bool,
    pub body: String,
}

/// One piece of a streaming export body: UTF-8 bytes ready for the socket, or the error that
/// ended the dump early. Metadata failures happen before any piece exists and answer as a
/// plain Err from export_sql_dump; a failure mid-stream aborts the download, which is the
/// honest behavior of a body whose headers were already sent.
pub type DumpPiece = Result<Vec<u8>, String>;

/// The streaming form of format=sql (SPEC §data.export): response-header facts up front — rows and
/// capped come from a COUNT over the same WHERE the grid, the CSV and the NDJSON exports use —
/// and the body arriving as pieces on a bounded channel. The producer holds one chunk of
/// source rows and one statement under construction, never the table: a dump of any capped
/// size costs O(chunk), where the folded formats cost O(table) for their body string.
pub struct SqlDump {
    pub columns: Vec<String>,
    pub rows: i64,
    pub capped: bool,
    pub body: tokio::sync::mpsc::Receiver<DumpPiece>,
}

/// One redis key as the key list shows it. ttl is seconds; -1 = no expiry.
#[derive(Clone, Debug, Serialize)]
pub struct RedisKeyInfo {
    pub key: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub ttl: i64,
}

/// One buffered change, as the panel posts it. pk maps primary-key column -> value; a null or
/// string value is passed through to the driver as a bound parameter (both drivers cast).
#[derive(Clone, Debug)]
pub enum BrowseEdit {
    Update {
        pk: Map<String, Value>,
        changes: Map<String, Value>,
        /// SPEC §data.edits: the row's original values, against which every CHANGED column is
        /// compared in the WHERE (the optimistic lock — cloudbeaver sends the same shape in
        /// ResultSetEditAction.ts:112-127). The panel instead packs the whole row into pk;
        /// either map feeds the lock, whichever carries the column.
        source: Option<Map<String, Value>>,
    },
    Insert {
        values: Map<String, Value>,
    },
    Delete {
        pk: Map<String, Value>,
    },
}

impl BrowseEdit {
    pub fn op(&self) -> &'static str {
        match self {
            BrowseEdit::Update { .. } => "update",
            BrowseEdit::Insert { .. } => "insert",
            BrowseEdit::Delete { .. } => "delete",
        }
    }
}

/// SPEC §data.edits: the CHANGED columns an update compares against the buffered originals —
/// the optimistic lock's column list, which is also the 409 body's answer. Empty when the
/// edit carries no originals to compare with (a pk-only update keeps today's semantics:
/// zero affected rows stays a quiet result, not a conflict — the request never claimed to
/// know the row). A keyless table's lock is its every-column address, so every changed
/// column rides it by construction.
pub fn optimistic_lock_columns(primary_key: &[String], edit: &BrowseEdit) -> Vec<String> {
    match edit {
        BrowseEdit::Update {
            pk,
            changes,
            source,
        } => {
            let mut cols: Vec<String> = if primary_key.is_empty() {
                changes.keys().cloned().collect()
            } else {
                let origin = source.as_ref().unwrap_or(pk);
                changes
                    .keys()
                    .filter(|c| origin.contains_key(*c) && !primary_key.contains(*c))
                    .cloned()
                    .collect()
            };
            cols.sort();
            cols
        }
        _ => Vec::new(),
    }
}

/// A lost optimistic-lock race (SPEC §data.edits): another writer moved the row between the
/// read and the commit. The columns are the ones whose buffered originals no longer match,
/// so the panel can paint exactly those cells red; an empty list means the mismatch sat in
/// the addressing itself (the row exists but nothing we meant to change differs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditConflict {
    pub message: String,
    pub columns: Vec<String>,
    /// The buffered row the race was over, as the caller addressed it (the pk map the
    /// panel posted) — echoed so the grid can find the exact buffered entry among many,
    /// instead of guessing by column names alone.
    pub row: Option<Map<String, Value>>,
}

/// apply_edits' failure vocabulary: Bad is the caller's fault (a 400 — an unaddressable
/// row, an ambiguous twin), Conflict is another writer's (a 409 — the optimistic lock
/// lost, and the whole batch rolled back).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    Bad(String),
    Conflict(EditConflict),
}

impl From<String> for EditError {
    fn from(m: String) -> Self {
        EditError::Bad(m)
    }
}

impl From<&str> for EditError {
    fn from(m: &str) -> Self {
        EditError::Bad(m.to_string())
    }
}

/// Why a write of the Data agent's kept nothing (SPEC §data.agent Writes): Held is not the
/// agent's to run on its own - the auto gate refused its shape, or it changed more rows than
/// the cap allows and was rolled back - so the user reviews and runs it; Failed is the server
/// refusing or failing it, as a console run of it would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteRefusal {
    Held(String),
    Failed(String),
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::Bad(m) => f.write_str(m),
            EditError::Conflict(c) => f.write_str(&c.message),
        }
    }
}

/// Diff the buffered originals against the row the database shows right now: every changed
/// column whose current value differs from its original names the conflict. A missing probe
/// row means deleted-or-rewritten — a keyless address cannot tell the two apart (there is
/// no key to re-find it by) — and the changed columns are still the honest answer.
pub fn conflict_of(
    changed: &[String],
    source: &Map<String, Value>,
    current: Option<&Map<String, Value>>,
) -> EditConflict {
    match current {
        None => EditConflict {
            message: format!(
                "row was changed or deleted by another writer — columns: {} (re-check the grid and resubmit)",
                changed.join(", ")
            ),
            columns: changed.to_vec(),
            row: None,
        },
        Some(row) => {
            let moved: Vec<&String> = changed
                .iter()
                .filter(|c| row.get(*c) != source.get(*c))
                .collect();
            let columns: Vec<String> = moved.iter().map(|c| (*c).clone()).collect();
            EditConflict {
                message: format!(
                    "row was changed by another writer{} — re-check the grid and resubmit",
                    if columns.is_empty() {
                        String::new()
                    } else {
                        format!(" — columns: {}", columns.join(", "))
                    }
                ),
                columns,
                row: None,
            }
        }
    }
}

/// One grid filter as the panel posts it ({column, op, value}). Structural validation happened
/// at the route; unknown columns/operators are refused by build_filter_where, which keeps the
/// rule next to the SQL it guards.
#[derive(Clone, Debug)]
pub struct BrowseFilter {
    pub column: String,
    pub op: String,
    pub value: Option<Value>,
}

/// A statement plus its bound parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltStatement {
    pub sql: String,
    pub params: Vec<Value>,
}

/// A WHERE fragment plus the parameters it binds.
#[derive(Clone, Debug)]
pub struct BuiltWhere {
    pub frag: String,
    pub params: Vec<Value>,
}

/// The column list build_filter_where guards: a bare name (the mysql callers pass no type info,
/// so a pg-style CAST can never be built) or a full BrowseColumn — Node's `(string | BrowseColumn)[]`.
#[derive(Clone, Debug)]
pub enum FilterColumn {
    Name(String),
    Column(BrowseColumn),
}

impl FilterColumn {
    pub fn name(&self) -> &str {
        match self {
            FilterColumn::Name(n) => n,
            FilterColumn::Column(c) => &c.name,
        }
    }

    /// The column's information_schema data_type, when the caller knew it.
    pub fn data_type(&self) -> Option<&str> {
        match self {
            FilterColumn::Name(_) => None,
            FilterColumn::Column(c) => Some(&c.data_type),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::dbbrowser::test_util::edit;

    #[test]
    fn optimistic_lock_columns_follow_the_carried_originals() {
        // SPEC §data.edits: what rides the optimistic lock. A keyed update that carries the
        // row compares every changed non-key column; the bare pk-only payload an API
        // caller might post compares nothing (old semantics, byte-identical SQL); a
        // keyless update compares all changed columns through its every-column address.
        let pk = vec!["id".to_string()];
        let full = edit(json!({
            "op": "update",
            "pk": { "id": 7, "name": "old", "city": "la" },
            "changes": { "name": "new", "city": "sf" },
        }));
        assert_eq!(
            optimistic_lock_columns(&pk, &full),
            vec!["city".to_string(), "name".to_string()]
        );
        let bare = edit(json!({
            "op": "update",
            "pk": { "id": 7 },
            "changes": { "name": "new" },
        }));
        assert!(optimistic_lock_columns(&pk, &bare).is_empty());
        let keyless = edit(json!({
            "op": "update",
            "pk": { "name": "old", "city": "la" },
            "changes": { "name": "new" },
        }));
        assert_eq!(
            optimistic_lock_columns(&[], &keyless),
            vec!["name".to_string()]
        );
    }

    #[test]
    fn conflict_of_names_the_moved_columns() {
        // The probe row the adapter reads inside the doomed transaction decides the body:
        // a differing column is named, a missing row names the changed set (deleted or
        // rewritten — a keyless address cannot tell), and a matching row means only the
        // addressing moved, so the column list is empty and the message says so.
        let changed = vec!["name".to_string(), "city".to_string()];
        let jmap = |v: Value| -> Map<String, Value> { v.as_object().cloned().unwrap_or_default() };
        let source = jmap(json!({ "name": "old", "city": "la" }));
        let now = jmap(json!({ "name": "theirs", "city": "la" }));
        let c = conflict_of(&changed, &source, Some(&now));
        assert_eq!(c.columns, vec!["name".to_string()]);
        assert!(c.message.contains("name"), "{}", c.message);

        let gone = conflict_of(&changed, &source, None);
        assert_eq!(gone.columns, changed.clone());
        assert!(gone.message.contains("deleted"), "{}", gone.message);

        let same = jmap(json!({ "name": "old", "city": "la" }));
        let c = conflict_of(&changed, &source, Some(&same));
        assert!(c.columns.is_empty());
        assert!(c.message.contains("another writer"), "{}", c.message);
    }
}
