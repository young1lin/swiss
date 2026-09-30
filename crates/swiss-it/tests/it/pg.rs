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

//! L1, the Postgres group (SPEC §testing.it): PgBrowser against a real PostgreSQL 17,
//! over the template-cloned per-test database. The pg-only shapes the seed pins:
//! schemas app/audit with a cross-schema FK, TEXT[] arrays, UUID keys, an enum
//! column type, a partial index, NUMERIC scale, INT8 exactness, TIMESTAMPTZ in
//! its ms-truncated ISO form, JSONB, and the cross-database catalog's honest
//! "needs its own connection" reason.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swiss_host::dbbrowser::{DbBrowser, EditError};
use swiss_it::engine::Kind;
use swiss_it::seed::{fresh, Fresh};
use swiss_mcp::adapters::direct::{BoxFut, Lazy};
use swiss_mcp::adapters::pg::pg_connect_options;
use swiss_mcp::adapters::pg_browser::PgBrowser;

/// A browser over its own template-cloned database, built the adapter's way.
async fn browser(tag: &str) -> (Fresh, PgBrowser) {
    let f = fresh(Kind::Postgres, tag).await;
    let url = f.def["url"].as_str().expect("pg def: url").to_string();
    let label = format!("pg {}", f.name);
    let opts = pg_connect_options(&url).expect("the def url parses as connect options");
    let conn = Arc::new(Lazy::new(move || {
        // Fn, not FnOnce: a failed open retries, so the options clone per invocation.
        let opts = opts.clone();
        Box::pin(async move {
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(opts)
                .await
                .map_err(|e| e.to_string())
        }) as BoxFut<Result<sqlx::postgres::PgPool, String>>
    }));
    (f, PgBrowser::new(label, conn))
}

fn names(reply: &Value) -> Vec<String> {
    reply["tables"]
        .as_array()
        .expect("tables array")
        .iter()
        .map(|t| t["name"].as_str().expect("table name").to_string())
        .collect()
}

#[tokio::test]
async fn list_tables_walks_schemas_with_the_schema_filter() {
    let (_f, b) = browser("p1_list").await;
    let public = b
        .list_tables(&json!({ "schema": "public", "limit": 10 }))
        .await
        .expect("public catalog");
    assert_eq!(public["total"], 6, "five tables and the view");
    let got = names(&public);
    for expected in ["users", "orders", "events", "wide", "big_rows", "active_users"] {
        assert!(got.contains(&expected.to_string()), "missing {expected} in {got:?}");
    }
    let by_name = |n: &str| {
        public["tables"]
            .as_array()
            .expect("tables")
            .iter()
            .find(|t| t["name"] == n)
            .unwrap_or_else(|| panic!("{n} missing"))
            .clone()
    };
    assert_eq!(by_name("users")["type"], "table");
    assert_eq!(by_name("users")["schema"], "public");
    // SPEC §data.browse: the panel's schema picker narrows the walk server-side.
    assert_eq!(by_name("active_users")["type"], "view");

    let app = b
        .list_tables(&json!({ "schema": "app", "limit": 10 }))
        .await
        .expect("app catalog");
    assert_eq!(app["total"], 1);
    assert_eq!(names(&app), vec!["documents"]);

    let audit = b
        .list_tables(&json!({ "schema": "audit", "limit": 10 }))
        .await
        .expect("audit catalog");
    assert_eq!(audit["total"], 1);
    assert_eq!(names(&audit), vec!["log_entries"]);

    // Paging and the more flag, same contract as mysql.
    let page1 = b
        .list_tables(&json!({ "schema": "public", "page": 1, "limit": 4 }))
        .await
        .expect("second page");
    assert_eq!(page1["tables"].as_array().expect("rows").len(), 2);
    assert_eq!(page1["more"], false);
}

#[tokio::test]
async fn read_table_pages_sorts_and_filters() {
    let (_f, b) = browser("p0_grid").await;
    let page = b
        .read_table(&json!({ "table": "users", "limit": 3 }))
        .await
        .expect("first page");
    assert_eq!(page["total"], 8);
    assert_eq!(page["rows"].as_array().expect("rows").len(), 3);
    assert_eq!(page["nextPage"], true);
    assert_eq!(page["primaryKey"], json!(["id"]));

    let desc = b
        .read_table(&json!({ "table": "users", "order": "id", "dir": "desc", "limit": 1 }))
        .await
        .expect("sorted page");
    let first = desc["rows"].as_array().expect("rows").first().expect("a row");
    // INT8 cells are exact strings end to end (SPEC §data.browse).
    assert_eq!(first["id"], json!("8"), "{}", first);

    for (filters, expected) in [
        (json!([{ "column": "status", "op": "eq", "value": "active" }]), 4),
        // The grid's LIKE contract: a bare substring, ILIKE on pg.
        (json!([{ "column": "name", "op": "like", "value": "李" }]), 1),
        (json!([{ "column": "email", "op": "isNull" }]), 2),
        // 2^53+1 itself, 2^62 and both i64 tops clear the bar.
        (json!([{ "column": "big", "op": "gt", "value": "9007199254740992" }]), 4),
        (
            json!([
                { "column": "status", "op": "eq", "value": "active" },
                { "column": "balance", "op": "gte", "value": "42" }
            ]),
            4,
        ),
    ] {
        let got = b
            .read_table(&json!({ "table": "users", "filters": filters, "limit": 50 }))
            .await
            .unwrap_or_else(|e| panic!("filters {filters}: {e}"));
        assert_eq!(got["total"], expected, "filters {filters}");
    }
}

#[tokio::test]
async fn read_table_keeps_cell_fidelity() {
    let (_f, b) = browser("p2_cells").await;
    let page = b
        .read_table(&json!({ "table": "users", "order": "id", "filters": [
            { "column": "id", "op": "in", "value": "1, 2" }
        ] }))
        .await
        .expect("the two fidelity rows");
    let rows = page["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    let row = &rows[0];

    // i64::MAX stays exact - never a rounded double.
    assert_eq!(row["big"], json!("9223372036854775807"));
    // NUMERIC keeps its declared scale as text.
    assert_eq!(row["balance"], json!("120.5000"));
    // JSONB arrives parsed.
    assert_eq!(row["flags"]["level"], 3);
    assert_eq!(row["flags"]["beta"], true);
    // TIMESTAMPTZ rides in the ms-truncated ISO form (the Node wire shape).
    assert_eq!(row["born_at"], json!("1990-06-15T08:30:00.123Z"));
    // BYTEA rides as the \x hex wire form.
    assert_eq!(row["avatar"], json!("\\x89504e470d0a1a0a"));
    // The enum column reads as its text value.
    assert_eq!(row["status"], json!("active"));
    assert_eq!(row["is_active"], json!(true));
    assert_eq!(row["name"], json!("张三 🙂"));

    // The epoch row: NULL and negative-scale NUMERIC survive.
    let epoch = &rows[1];
    assert_eq!(epoch["email"], Value::Null);
    assert_eq!(epoch["balance"], json!("-0.0001"));
    assert_eq!(epoch["born_at"], json!("1970-01-01T00:00:00.000Z"));
    assert_eq!(epoch["is_active"], json!(false));
}

#[tokio::test]
async fn read_table_cross_schema_arrays_uuids_and_duplicates() {
    let (_f, b) = browser("p2_cross").await;
    let docs = b
        .read_table(&json!({ "table": "documents", "schema": "app", "limit": 10 }))
        .await
        .expect("app.documents");
    assert_eq!(docs["total"], 4);
    let rows = docs["rows"].as_array().expect("rows");
    let invoice = rows
        .iter()
        .find(|r| r["title"] == "invoice-42")
        .expect("the invoice row");
    // Arrays ride as their pg text literal - node-pg never parsed them either - and
    // UUID as its canonical text.
    assert_eq!(invoice["tags"], json!("{final,accounting}"));
    assert_eq!(
        invoice["id"],
        json!("00000000-0000-4000-8000-000000000002")
    );

    let logs = b
        .read_table(&json!({ "table": "log_entries", "schema": "audit", "limit": 10 }))
        .await
        .expect("audit.log_entries");
    assert_eq!(logs["total"], 5);

    // Keyless duplicates stay byte-identical and both visible, with the note.
    let events = b
        .read_table(&json!({ "table": "events", "limit": 50 }))
        .await
        .expect("events");
    assert_eq!(events["total"], 4);
    let rows = events["rows"].as_array().expect("rows");
    let twins: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "tick").collect();
    assert_eq!(twins.len(), 2);
    assert_eq!(twins[0], twins[1]);
    assert_eq!(events["primaryKey"], json!([]));
    let note = events["editNote"].as_str().expect("editNote");
    assert!(note.contains("ambiguous"), "{note}");

    let wide = b
        .read_table(&json!({ "table": "wide", "limit": 1 }))
        .await
        .expect("wide");
    assert_eq!(wide["columns"].as_array().expect("columns").len(), 60);
    assert_eq!(wide["total"], 3);
}

#[tokio::test]
async fn describe_table_columns_pk_fk_partial_index_and_ddl() {
    let (_f, b) = browser("p4_structure").await;
    let users = b
        .describe_table(&json!({ "table": "users" }))
        .await
        .expect("users structure");
    assert_eq!(users["primaryKey"], json!(["id"]));
    // The enum surfaces as its own column type.
    let cols = users["columns"].as_array().expect("columns");
    let status = cols
        .iter()
        .find(|c| c["name"] == "status")
        .expect("the status column");
    assert_eq!(status["dataType"], "user_status", "{}", status);
    assert!(users["ddl"].as_str().expect("ddl").contains("CREATE TABLE"));

    let orders = b
        .describe_table(&json!({ "table": "orders" }))
        .await
        .expect("orders structure");
    assert_eq!(orders["primaryKey"], json!(["user_id", "seq"]));
    let fks = orders["foreignKeys"].as_array().expect("foreign keys");
    let fk = fks.first().expect("the users fk");
    assert_eq!(fk["refTable"], "users");
    assert_eq!(fk["column"], "user_id");

    // The cross-schema FK and the partial index are visible where they live.
    let docs = b
        .describe_table(&json!({ "table": "documents", "schema": "app" }))
        .await
        .expect("app.documents structure");
    let fks = docs["foreignKeys"].as_array().expect("foreign keys");
    assert!(
        fks.iter().any(|f| f["refTable"] == "users"),
        "the cross-schema fk: {fks:?}"
    );

    let logs = b
        .describe_table(&json!({ "table": "log_entries", "schema": "audit" }))
        .await
        .expect("audit.log_entries structure");
    let text = serde_json::to_string(&logs).expect("serialize the reply");
    assert!(
        text.contains("log_entries_noted_at"),
        "the partial index is listed, not hidden: {text}"
    );
}

#[tokio::test]
async fn export_table_csv_and_json_counts_match_fetch() {
    let (_f, b) = browser("p4_export").await;
    let fetch = b
        .read_table(&json!({ "table": "users", "limit": 50 }))
        .await
        .expect("fetch");
    assert_eq!(fetch["total"], 8);

    let csv = b
        .export_table(&json!({ "table": "users" }))
        .await
        .expect("csv export");
    assert_eq!(csv["rows"], 8);
    assert_eq!(csv["capped"], false);
    let body = csv["body"].as_str().expect("csv body");
    // RFC 4180: CRLF rows, every cell quoted.
    let lines: Vec<&str> = body.trim_end().split("\r\n").collect();
    assert_eq!(lines.len(), 9, "header plus eight rows: {body}");
    assert!(lines[0].starts_with("\"id\""), "{}", lines[0]);
    assert!(body.contains("张三 🙂"), "utf8 survives the csv");

    let ndjson = b
        .export_table(&json!({ "table": "users", "format": "json" }))
        .await
        .expect("json export");
    assert_eq!(ndjson["rows"], 8);
    let lines: Vec<&str> = ndjson["body"].as_str().expect("body").trim_end().split('\n').collect();
    assert_eq!(lines.len(), 8);

    let filtered = b
        .export_table(&json!({ "table": "users", "filters": [
            { "column": "status", "op": "eq", "value": "banned" }
        ] }))
        .await
        .expect("filtered export");
    assert_eq!(filtered["rows"], 2, "{}", filtered);
}

#[tokio::test]
async fn export_sql_dump_streams_head_rows_and_foot() {
    let (_f, b) = browser("p4_dump").await;
    let mut dump = b
        .export_sql_dump(&json!({ "table": "users" }))
        .await
        .expect("sql dump");
    assert_eq!(dump.rows, 8);
    assert!(!dump.capped);
    assert_eq!(dump.columns.len(), 10);
    let mut bytes = Vec::new();
    while let Some(piece) = dump.body.recv().await {
        bytes.extend_from_slice(&piece.expect("dump piece"));
    }
    let text = String::from_utf8(bytes).expect("the dump is utf8");
    assert!(text.starts_with("-- swiss SQL dump"), "{text}");
    assert!(text.contains("CREATE TABLE"), "the DDL head: {text}");
    assert!(text.contains("INSERT INTO"), "the row body: {text}");
}

#[tokio::test]
async fn import_table_inserts_mapped_rows() {
    let (_f, b) = browser("p4_import").await;
    let reply = b
        .import_table(&json!({
            "table": "users",
            "header": ["name", "balance", "big", "born_at"],
            "mapping": ["name", "balance", "big", "born_at"],
            "lines": ["imported-one,0,7,2026-01-01 00:00:00"],
            "mode": "insert"
        }))
        .await
        .expect("import");
    assert_eq!(reply["inserted"], 1, "{}", reply);
    let after = b
        .read_table(&json!({ "table": "users", "filters": [
            { "column": "name", "op": "eq", "value": "imported-one" }
        ] }))
        .await
        .expect("read back");
    assert_eq!(after["total"], 1);
    let row = &after["rows"].as_array().expect("rows")[0];
    assert_eq!(row["big"], json!("7"));
}

#[tokio::test]
async fn apply_edits_update_insert_delete_with_readback() {
    let (_f, b) = browser("p42_edits").await;
    let reply = b
        .apply_edits(&json!({
            "table": "users",
            "edits": [
                { "op": "update", "pk": { "id": "1" }, "changes": { "name": "renamed" } },
                { "op": "insert", "values": {
                    "name": "inserted", "balance": 0, "big": 9,
                    "born_at": "2026-01-01 00:00:00"
                } },
                { "op": "delete", "pk": { "id": "8" } }
            ]
        }))
        .await
        .expect("the buffered edit batch");
    let results = reply["results"].as_array().expect("results");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["op"], "update");
    assert_eq!(results[0]["affected"], 1);
    assert_eq!(results[0]["row"]["name"], "renamed");
    assert_eq!(results[1]["op"], "insert");
    // RETURNING readback: the identity-assigned id arrives exact.
    assert_eq!(results[1]["row"]["id"], json!("9"), "{}", results[1]);
    assert_eq!(results[2]["op"], "delete");
    assert_eq!(results[2]["affected"], 1);

    let after = b
        .read_table(&json!({ "table": "users", "limit": 50 }))
        .await
        .expect("count after");
    assert_eq!(after["total"], 8, "8 - 1 deleted + 1 inserted");
}

#[tokio::test]
async fn apply_edits_composite_pk_keyless_and_jsonb_address() {
    let (_f, b) = browser("p42_composite").await;
    let reply = b
        .apply_edits(&json!({
            "table": "orders",
            "edits": [
                { "op": "update", "pk": { "user_id": "1", "seq": "1" },
                  "changes": { "note": "edited" } }
            ]
        }))
        .await
        .expect("composite pk update");
    assert_eq!(reply["results"][0]["affected"], 1);
    assert_eq!(reply["results"][0]["row"]["note"], "edited");

    // An empty keyless address is refused, not guessed.
    let refused = b
        .apply_edits(&json!({
            "table": "events",
            "edits": [ { "op": "update", "pk": {}, "changes": { "kind": "x" } } ]
        }))
        .await;
    assert!(
        matches!(refused, Err(EditError::Bad(_))),
        "expected a Bad refusal, got {refused:?}"
    );

    // The JSONB column addresses by its JSON value; LIMIT semantics are the
    // adapter's - one twin goes, its byte-identical sibling stays.
    // Postgres has no DELETE ... LIMIT, so an every-column address that matches
    // two byte-identical rows refuses the whole batch (SPEC §data.edits) - the honest
    // answer, where mysql clips with LIMIT 1 and reports one.
    let one_twin = b
        .apply_edits(&json!({
            "table": "events",
            "edits": [ { "op": "delete", "pk": {
                "at": "2026-06-01T12:00:00.000Z", "kind": "tick", "detail": { "n": 1 }
            } } ]
        }))
        .await;
    match &one_twin {
        Err(EditError::Bad(m)) => assert!(m.contains("ambiguous"), "{m}"),
        other => panic!("pg must refuse the ambiguous twin delete: {other:?}"),
    }
    let after = b
        .read_table(&json!({ "table": "events", "limit": 50 }))
        .await
        .expect("events after");
    let ticks = after["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter(|r| r["kind"] == "tick")
        .count();
    assert_eq!(ticks, 2, "the refused batch rolled back - both twins remain");
}

#[tokio::test]
async fn run_query_reads_writes_and_reports_limits() {
    let (_f, b) = browser("p3_console").await;
    let read = b
        .run_query("SELECT COUNT(*) AS c FROM users", None)
        .await
        .expect("console read");
    assert_eq!(read["rowCount"], 1);
    assert_eq!(read["columns"], json!(["c"]));
    // COUNT(*) is INT8: an exact string, never a double.
    assert_eq!(read["rows"][0]["c"], json!("8"));

    let write = b
        .run_query("UPDATE users SET balance = 42.50 WHERE id = 1", None)
        .await
        .expect("console write");
    assert_eq!(write["rowCount"], 0, "an execute carries no rows");
    let back = b
        .read_table(&json!({ "table": "users", "filters": [
            { "column": "id", "op": "eq", "value": "1" }
        ] }))
        .await
        .expect("read back the write");
    // NUMERIC keeps its column scale on the way back out.
    assert_eq!(back["rows"][0]["balance"], json!("42.5000"));

    let capped = b
        .run_query("SELECT n FROM big_rows", None)
        .await
        .expect("capped read");
    assert_eq!(capped["rowCount"], 50);
    assert_eq!(capped["limitApplied"], 50, "{}", capped);
}

#[tokio::test]
async fn ddl_op_creates_renames_and_drops() {
    let (_f, b) = browser("p46_ddl").await;
    let created = b
        .ddl_op(&json!({
            "op": "create_table",
            "table": "scratch",
            "columns": [
                { "name": "sid", "type": "INT", "nullable": false },
                { "name": "note", "type": "TEXT", "nullable": true }
            ],
            "primary": ["sid"]
        }))
        .await
        .expect("create_table");
    assert!(created["ran"].as_str().expect("ran").contains("CREATE TABLE"));
    let after_create = b
        .list_tables(&json!({ "schema": "public", "limit": 10 }))
        .await
        .expect("list");
    assert_eq!(after_create["total"], 7);

    let renamed = b
        .ddl_op(&json!({ "op": "rename", "table": "scratch", "to": "scratch2" }))
        .await
        .expect("rename");
    assert!(renamed["ran"].as_str().expect("ran").contains("RENAME"));
    let after_rename = b
        .list_tables(&json!({ "schema": "public", "limit": 10 }))
        .await
        .expect("list");
    assert!(names(&after_rename).contains(&"scratch2".to_string()));

    b.ddl_op(&json!({ "op": "drop", "table": "scratch2" }))
        .await
        .expect("drop");
    let after_drop = b
        .list_tables(&json!({ "schema": "public", "limit": 10 }))
        .await
        .expect("list");
    assert_eq!(after_drop["total"], 6);
    assert!(!names(&after_drop).contains(&"scratch2".to_string()));
}

#[tokio::test]
async fn activity_lists_this_connection() {
    let (_f, b) = browser("p32_activity").await;
    b.list_tables(&json!({ "limit": 1 })).await.expect("warm the pool");
    let reply = b.activity().await.expect("activity");
    let rows = reply["rows"].as_array().expect("rows");
    assert!(!rows.is_empty());
    for row in rows {
        assert!(row["pid"].is_number(), "{row}");
        assert!(row["own"].is_boolean(), "{row}");
    }
    assert!(
        rows.iter().any(|r| r["user"] == "it"),
        "the def account's own sessions appear: {rows:?}"
    );
}

#[tokio::test]
async fn activity_kill_cancels_a_sleeping_query() {
    let (f, b) = browser("p32_kill").await;
    use sqlx::Connection;
    let url = f.def["url"].as_str().expect("url").to_string();
    let mut sleeper = sqlx::PgConnection::connect(&url)
        .await
        .expect("the sleeper connects");
    // pg_backend_pid() is INT4.
    let pid = sqlx::query_scalar::<sqlx::Postgres, i32>("SELECT pg_backend_pid()")
        .fetch_one(&mut sleeper)
        .await
        .expect("the sleeper's pid") as i64;
    let sleeping = tokio::spawn(async move {
        sqlx::query("SELECT pg_sleep(30)").fetch_all(&mut sleeper).await
    });

    let mut found = false;
    for _ in 0..50 {
        let reply = b.activity().await.expect("activity");
        if let Some(rows) = reply["rows"].as_array() {
            if rows.iter().any(|r| r["pid"].as_i64() == Some(pid)) {
                found = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "the sleeping query never appeared in the activity list");

    b.activity_kill(pid, false).await.expect("pg_cancel_backend");
    // pg_cancel_backend errors the cancelled statement (57014), and either way
    // the 30-second sleep resolves within seconds, not after 30 s.
    let outcome = tokio::time::timeout(Duration::from_secs(5), sleeping)
        .await
        .expect("the cancelled query terminates instead of sleeping 30 s");
    assert!(
        outcome.map(|r| r.is_err()).unwrap_or(false),
        "pg_cancel_backend turns pg_sleep into query_canceled"
    );
}

#[tokio::test]
async fn completion_serves_tables_and_from_columns() {
    let (_f, b) = browser("p31_completion").await;
    // The pg table list is schema-qualified (matching the panel's catalog), so the
    // prefix matches the schema, not the bare table name.
    let tables = b
        .completion("SELECT * FROM pu", "SELECT * FROM pu".len())
        .await
        .expect("table completion");
    let items = tables["items"].as_array().expect("items");
    assert!(
        items.iter().any(|i| i["label"] == "public.users" && i["kind"] == "table"),
        "public.users among the candidates: {items:?}"
    );

    let columns = b
        .completion(
            "SELECT * FROM users WHERE na",
            "SELECT * FROM users WHERE na".len(),
        )
        .await
        .expect("column completion");
    let items = columns["items"].as_array().expect("items");
    assert!(
        items.iter().any(|i| i["label"] == "name" && i["kind"] == "column"),
        "the FROM-nearest table's columns: {items:?}"
    );
}

#[tokio::test]
async fn list_databases_primary_system_and_the_foreign_reason() {
    let (f, b) = browser("p3_databases").await;
    let reply = b.list_databases().await.expect("the catalog");
    assert_eq!(reply["primary"], json!(f.name));
    assert_eq!(reply["current"], json!(f.name));
    let databases = reply["databases"].as_array().expect("databases");
    let ours = databases
        .iter()
        .find(|d| d["name"] == f.name.as_str())
        .unwrap_or_else(|| panic!("our database is listed: {databases:?}"));
    assert_eq!(ours["primary"], true);
    assert_eq!(ours["browsable"], true);
    assert_eq!(ours["reason"], Value::Null);
    // The postgres database is flagged system but still listed (allow-conn).
    let sys = databases
        .iter()
        .find(|d| d["name"] == "postgres")
        .expect("the postgres database");
    assert_eq!(sys["system"], true);
    // SPEC §data.databases / ADR-027 option b: every other database is listed but not
    // browsable, with the reason the panel shows - a PgPool is bound to one db.
    assert_eq!(sys["browsable"], false);
    let reason = sys["reason"].as_str().expect("the reason text");
    assert!(reason.contains("bound to one database"), "{reason}");
    // Template siblings are filtered server-side.
    assert!(databases.iter().all(|d| d["name"] != "template0"));
    assert!(databases.iter().all(|d| d["name"] != "template1"));
}
