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

//! L1, the MySQL group (SPEC §testing.it): MysqlBrowser against a real MySQL 8.4, one
//! test per trait method plus the shapes the seed exists to pin. The browser is
//! constructed exactly the way the adapter constructs it - same ServerDef, same
//! connect options, same Lazy pool - so the SQL-to-database half is what runs, with
//! no HTTP in between (that contract is dbbrowser_api.rs's, guarded by stubs).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{DbBrowser, EditError};
use swiss_it::engine::Kind;
use swiss_it::seed::{fresh, Fresh};
use swiss_mcp::adapters::direct::{BoxFut, Lazy};
use swiss_mcp::adapters::mysql::mysql_connect_options;
use swiss_mcp::adapters::mysql_browser::MysqlBrowser;

/// A browser over its own seeded database. The pool is the adapter's own recipe
/// (max 4, 5 s acquire) so pool-shaped behavior is the product's, not a test double's.
async fn browser(tag: &str) -> (Fresh, MysqlBrowser) {
    let f = fresh(Kind::Mysql, tag).await;
    let def: ServerDef =
        serde_json::from_value(f.def.clone()).expect("the def parses as a ServerDef");
    let database = def
        .get_str("database")
        .expect("mysql def: database")
        .to_string();
    let label = format!(
        "{} @ {}:{}",
        database,
        def.get_str("host").unwrap_or("?"),
        def.get_number("port").unwrap_or(0.0)
    );
    let opts = mysql_connect_options(&def);
    let conn = Arc::new(Lazy::new(move || {
        // Fn, not FnOnce: a failed open retries, so the options clone per invocation.
        let opts = opts.clone();
        Box::pin(async move {
            sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(4)
                .acquire_timeout(Duration::from_secs(5))
                .connect_with(opts)
                .await
                .map_err(|e| e.to_string())
        }) as BoxFut<Result<sqlx::mysql::MySqlPool, String>>
    }));
    (f, MysqlBrowser::new(database, label, conn))
}

fn names(reply: &Value) -> Vec<&str> {
    reply["tables"]
        .as_array()
        .expect("tables array")
        .iter()
        .map(|t| t["name"].as_str().expect("table name"))
        .collect()
}

#[tokio::test]
async fn list_tables_pages_greps_and_marks_the_view() {
    let (_f, b) = browser("w1_list").await;
    let all = b.list_tables(&json!({ "limit": 10 })).await.expect("list_tables");
    assert_eq!(all["total"], 6, "six objects: five tables and the view");
    let names = names(&all);
    for expected in ["users", "orders", "events", "wide", "big_rows", "active_users"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let by_name = |n: &str| {
        all["tables"]
            .as_array()
            .expect("tables")
            .iter()
            .find(|t| t["name"] == n)
            .unwrap_or_else(|| panic!("{n} missing"))
            .clone()
    };
    // Node-style types: "table"/"view" - the panel's edit affordances key off them.
    assert_eq!(by_name("users")["type"], "table");
    assert_eq!(by_name("active_users")["type"], "view");

    // Paging: page math and the more flag.
    let page0 = b.list_tables(&json!({ "page": 0, "limit": 2 })).await.expect("page 0");
    assert_eq!(page0["tables"].as_array().expect("rows").len(), 2);
    assert_eq!(page0["more"], true, "four more objects after the first page");
    let page2 = b.list_tables(&json!({ "page": 2, "limit": 2 })).await.expect("page 2");
    assert_eq!(page2["tables"].as_array().expect("rows").len(), 2);
    assert_eq!(page2["more"], false, "the last page knows it is the last");

    // SPEC §data.browse: grep narrows the catalog (users and active_users both match).
    let grepped = b.list_tables(&json!({ "grep": "user" })).await.expect("grep");
    assert_eq!(grepped["total"], 2, "{grepped}");
}

#[tokio::test]
async fn read_table_pages_sorts_and_filters() {
    let (_f, b) = browser("w0_grid").await;
    let page = b
        .read_table(&json!({ "table": "users", "limit": 3 }))
        .await
        .expect("first page");
    assert_eq!(page["total"], 8);
    assert_eq!(page["rows"].as_array().expect("rows").len(), 3);
    assert_eq!(page["nextPage"], true, "five rows still follow");
    assert_eq!(page["primaryKey"], json!(["id"]));

    let tail = b
        .read_table(&json!({ "table": "users", "offset": 6, "limit": 3 }))
        .await
        .expect("tail page");
    assert_eq!(tail["rows"].as_array().expect("rows").len(), 2);
    assert_eq!(tail["nextPage"], false);

    let desc = b
        .read_table(&json!({ "table": "users", "order": "id", "dir": "desc", "limit": 1 }))
        .await
        .expect("sorted page");
    let first = desc["rows"].as_array().expect("rows").first().expect("a row");
    // BIGINT cells are exact strings end to end (SPEC §data.browse) - even small ones.
    assert_eq!(first["id"], json!("8"), "{first}");

    for (filters, expected) in [
        (json!([{ "column": "status", "op": "eq", "value": "active" }]), 4),
        // The grid's LIKE contract (SPEC §data.browse): a bare substring, not % patterns -
        // a user's own % is escaped literal.
        (json!([{ "column": "name", "op": "like", "value": "李" }]), 1),
        (json!([{ "column": "email", "op": "isNull" }]), 2),
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
    let (_f, b) = browser("w2_cells").await;
    let page = b
        .read_table(&json!({ "table": "users", "order": "id", "filters": [
            { "column": "id", "op": "in", "value": "1, 3" }
        ] }))
        .await
        .expect("the two fidelity rows");
    let rows = page["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    let row = &rows[0];

    // 2^64-1 must arrive as the exact string, never a rounded double.
    assert_eq!(row["big"], json!("18446744073709551615"));
    // DECIMAL keeps its scale as text.
    assert_eq!(row["balance"], json!("120.5000"));
    // JSON arrives parsed.
    assert_eq!(row["flags"]["level"], 3);
    assert_eq!(row["flags"]["beta"], true);
    // Microseconds survive the DATETIME(6) wire form.
    assert_eq!(row["born_at"], json!("1990-06-15 08:30:00.123456"));
    // Non-UTF-8 bytes ride as the \x hex wire form (the PNG magic bytes).
    assert_eq!(row["avatar"], json!("\\x89504e470d0a1a0a"));
    // ENUM, boolean and the utf8mb4 name round trip.
    assert_eq!(row["status"], json!("active"));
    assert_eq!(row["is_active"], json!(1));
    assert_eq!(row["name"], json!("张三 🙂"));

    let second = &rows[1];
    assert_eq!(second["balance"], json!("99999999.9999"));
    // NULL stays an explicit null (id 2 is the NULL-email row).
    let ada = b
        .read_table(&json!({ "table": "users", "filters": [
            { "column": "id", "op": "eq", "value": "2" }
        ] }))
        .await
        .expect("the null-email row");
    assert_eq!(ada["rows"][0]["email"], Value::Null);
}

#[tokio::test]
async fn read_table_duplicates_keyless_note_and_wide_columns() {
    let (_f, b) = browser("w1_keyless").await;
    let page = b
        .read_table(&json!({ "table": "events", "limit": 50 }))
        .await
        .expect("events page");
    assert_eq!(page["total"], 4);
    let rows = page["rows"].as_array().expect("rows");
    // The two byte-identical rows are BOTH there - no silent dedup.
    let twins: Vec<&Value> = rows.iter().filter(|r| r["kind"] == "tick").collect();
    assert_eq!(twins.len(), 2);
    assert_eq!(twins[0], twins[1], "the duplicates stay byte-identical");
    // SPEC §data.edits: keyless tables stay editable, with the addressing note.
    assert_eq!(page["editable"], true);
    let note = page["editNote"].as_str().expect("editNote");
    assert!(note.contains("ambiguous"), "{note}");
    assert_eq!(page["primaryKey"], json!([]));

    let wide = b
        .read_table(&json!({ "table": "wide", "limit": 1 }))
        .await
        .expect("wide page");
    assert_eq!(wide["columns"].as_array().expect("columns").len(), 60);
    assert_eq!(wide["total"], 3);
}

#[tokio::test]
async fn describe_table_columns_pk_fk_indexes_and_ddl() {
    let (_f, b) = browser("w4_structure").await;
    let users = b
        .describe_table(&json!({ "table": "users" }))
        .await
        .expect("users structure");
    assert_eq!(users["primaryKey"], json!(["id"]));
    let ddl = users["ddl"].as_str().expect("ddl");
    assert!(ddl.contains("CREATE TABLE"), "{ddl}");
    assert!(ddl.contains("utf8mb4"), "the charset shows up in the DDL: {ddl}");

    let orders = b
        .describe_table(&json!({ "table": "orders" }))
        .await
        .expect("orders structure");
    // The composite primary key, in ordinal order.
    assert_eq!(orders["primaryKey"], json!(["user_id", "seq"]));
    // The foreign key to users, with its column pair.
    let fks = orders["foreignKeys"].as_array().expect("foreign keys");
    let fk = fks.first().expect("the fk_orders_user foreign key");
    assert_eq!(fk["refTable"], "users");
    assert_eq!(fk["column"], "user_id");
    // The PRIMARY index appears among the indexes.
    let idx = orders["indexes"].as_array().expect("indexes");
    assert!(
        idx.iter().any(|i| i["primary"] == json!(true)),
        "PRIMARY index missing: {idx:?}"
    );
}

#[tokio::test]
async fn export_table_csv_and_json_counts_match_fetch() {
    let (_f, b) = browser("w4_export").await;
    let fetch = b
        .read_table(&json!({ "table": "users", "limit": 50 }))
        .await
        .expect("fetch");
    assert_eq!(fetch["total"], 8);

    let csv = b
        .export_table(&json!({ "table": "users" }))
        .await
        .expect("csv export");
    assert_eq!(csv["format"], "csv");
    assert_eq!(csv["rows"], 8, "the export agrees with the fetch count");
    assert_eq!(csv["capped"], false);
    let body = csv["body"].as_str().expect("csv body");
    // RFC 4180: CRLF rows, and a quoted cell may legally span lines - split on CRLF.
    let lines: Vec<&str> = body.trim_end().split("\r\n").collect();
    assert_eq!(lines.len(), 9, "header plus eight rows: {body}");
    // Every cell rides quoted (RFC 4180, quotes doubled inside).
    assert!(lines[0].starts_with("\"id\""), "{}", lines[0]);
    assert!(body.contains("张三 🙂"), "utf8mb4 survives the csv");

    let ndjson = b
        .export_table(&json!({ "table": "users", "format": "json" }))
        .await
        .expect("json export");
    assert_eq!(ndjson["rows"], 8);
    let lines: Vec<&str> = ndjson["body"].as_str().expect("body").trim_end().split('\n').collect();
    assert_eq!(lines.len(), 8, "no header in the json form");

    // A filtered export exports the filtered set (SPEC §data.export).
    let filtered = b
        .export_table(&json!({ "table": "users", "filters": [
            { "column": "status", "op": "eq", "value": "banned" }
        ] }))
        .await
        .expect("filtered export");
    assert_eq!(filtered["rows"], 2, "{filtered}");
}

#[tokio::test]
async fn export_sql_dump_streams_head_rows_and_foot() {
    let (_f, b) = browser("w4_dump").await;
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
    // The replay foot restores the FK switch the head turned off.
    assert!(text.contains("SET FOREIGN_KEY_CHECKS=1"), "{text}");
}

#[tokio::test]
async fn import_table_inserts_mapped_rows() {
    let (_f, b) = browser("w4_import").await;
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
    assert_eq!(reply["inserted"], 1, "{reply}");
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
    let (_f, b) = browser("w42_edits").await;
    let reply = b
        .apply_edits(&json!({
            "table": "users",
            "edits": [
                { "op": "update", "pk": { "id": "1" }, "changes": { "name": "renamed" } },
                { "op": "insert", "values": {
                    "name": "inserted", "balance": 0, "big": 9,
                    "born_at": "2026-01-01 00:00:00"
                } },
                // id 8 has no orders referencing it - the FK must not block the batch.
                { "op": "delete", "pk": { "id": "8" } }
            ]
        }))
        .await
        .expect("the buffered edit batch");
    let results = reply["results"].as_array().expect("results");
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["op"], "update");
    assert_eq!(results[0]["affected"], 1);
    assert_eq!(results[0]["row"]["name"], "renamed", "the update reads back");
    assert_eq!(results[1]["op"], "insert");
    // LAST_INSERT_ID readback on the same connection (SPEC §data.edits).
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
async fn apply_edits_composite_pk_and_keyless_refusal() {
    let (_f, b) = browser("w42_composite").await;
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

    // A keyless update that cannot name its row is refused, not guessed.
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

    // A keyless delete addressed by every column clips to one row (LIMIT 1) - the
    // twins are individually deletable even though they are indistinguishable.
    let one_twin = b
        .apply_edits(&json!({
            "table": "events",
            // The JSON column addresses by its JSON value, not a string spelling.
            "edits": [ { "op": "delete", "pk": {
                "at": "2026-06-01 12:00:00.000000", "kind": "tick", "detail": { "n": 1 }
            } } ]
        }))
        .await
        .expect("twin delete");
    assert_eq!(one_twin["results"][0]["affected"], 1);
    let after = b
        .read_table(&json!({ "table": "events", "limit": 50 }))
        .await
        .expect("events after");
    assert_eq!(after["total"], 3);
}

#[tokio::test]
async fn run_query_reads_writes_and_reports_limits() {
    let (_f, b) = browser("w3_console").await;
    let read = b
        .run_query("SELECT COUNT(*) AS c FROM users", None)
        .await
        .expect("console read");
    assert_eq!(read["rowCount"], 1);
    assert_eq!(read["columns"], json!(["c"]));
    // COUNT(*) is BIGINT: an exact string (SPEC §data.browse), never a double.
    assert_eq!(read["rows"][0]["c"], json!("8"));

    let write = b
        .run_query("UPDATE users SET balance = 1 WHERE id = 1", None)
        .await
        .expect("console write");
    assert_eq!(write["rowCount"], 0, "an OK packet carries no rows");
    let back = b
        .read_table(&json!({ "table": "users", "filters": [
            { "column": "id", "op": "eq", "value": "1" }
        ] }))
        .await
        .expect("read back the write");
    assert_eq!(back["rows"][0]["balance"], json!("1.0000"));

    // The LIMIT-less SELECT is capped at 50 and the report says so - a silent
    // truncation at 50 rows is a lie the console must not tell.
    let capped = b
        .run_query("SELECT n FROM big_rows", None)
        .await
        .expect("capped read");
    assert_eq!(capped["rowCount"], 50);
    assert_eq!(capped["limitApplied"], 50, "{capped}");
}

#[tokio::test]
async fn ddl_op_creates_renames_and_drops() {
    let (_f, b) = browser("w46_ddl").await;
    let created = b
        .ddl_op(&json!({
            "op": "create_table",
            "table": "scratch",
            "columns": [
                { "name": "sid", "type": "INT", "nullable": false },
                { "name": "note", "type": "VARCHAR(40)", "nullable": true }
            ],
            "primary": ["sid"]
        }))
        .await
        .expect("create_table");
    assert!(created["ran"].as_str().expect("ran").contains("CREATE TABLE"));
    let after_create = b.list_tables(&json!({ "limit": 10 })).await.expect("list");
    assert_eq!(after_create["total"], 7);

    let renamed = b
        .ddl_op(&json!({ "op": "rename", "table": "scratch", "to": "scratch2" }))
        .await
        .expect("rename");
    assert!(renamed["ran"].as_str().expect("ran").contains("RENAME"));
    let after_rename = b.list_tables(&json!({ "limit": 10 })).await.expect("list");
    assert!(names(&after_rename).contains(&"scratch2"));

    b.ddl_op(&json!({ "op": "drop", "table": "scratch2" }))
        .await
        .expect("drop");
    let after_drop = b.list_tables(&json!({ "limit": 10 })).await.expect("list");
    assert_eq!(after_drop["total"], 6);
    assert!(!names(&after_drop).contains(&"scratch2"));
}

#[tokio::test]
async fn activity_lists_this_connection() {
    let (_f, b) = browser("w32_activity").await;
    // Touch the pool first so this connection exists.
    b.list_tables(&json!({ "limit": 1 })).await.expect("warm the pool");
    let reply = b.activity().await.expect("activity");
    let rows = reply["rows"].as_array().expect("rows");
    assert!(!rows.is_empty());
    // The shared shape (SPEC §data.activity), typed - not the grid's stringly BIGINTs.
    for row in rows {
        assert!(row["pid"].is_number(), "{row}");
        assert!(row["user"].is_string(), "{row}");
        assert!(row["own"].is_boolean(), "{row}");
    }
    assert!(
        rows.iter().any(|r| r["user"] == "it"),
        "the def account's own sessions appear: {rows:?}"
    );
}

#[tokio::test]
async fn activity_kill_terminates_a_sleeping_query() {
    let (f, b) = browser("w32_kill").await;
    use sqlx::Connection;
    // A second connection running something slow, on this test's own database.
    // Value's Display is its JSON spelling (quotes included) - extract as typed.
    let url = format!(
        "mysql://{}:{}@{}:{}/{}",
        f.def["user"].as_str().expect("user"),
        f.def["password"].as_str().expect("password"),
        f.def["host"].as_str().expect("host"),
        f.def["port"].as_u64().expect("port"),
        f.def["database"].as_str().expect("database")
    );
    let mut sleeper = sqlx::MySqlConnection::connect(&url)
        .await
        .expect("the sleeper connects");
    // CONNECTION_ID() is BIGINT UNSIGNED - decode as u64, carry as i64.
    let pid = sqlx::query_scalar::<sqlx::MySql, u64>("SELECT CONNECTION_ID()")
        .fetch_one(&mut sleeper)
        .await
        .expect("the sleeper's pid") as i64;
    let sleeping = tokio::spawn(async move {
        let res = sqlx::query("SELECT SLEEP(30)").fetch_all(&mut sleeper).await;
        res.is_err()
    });

    // The sleeping query shows up in the activity list, then dies when killed.
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

    b.activity_kill(pid, false).await.expect("KILL QUERY");
    // Proof of the kill is TIMING, not the result shape: the 30-second SLEEP resolves
    // within seconds whether MySQL reports it as error 1317 or as an early Ok.
    let started = std::time::Instant::now();
    let _ = tokio::time::timeout(Duration::from_secs(5), sleeping)
        .await
        .expect("the killed query terminates instead of sleeping 30 s");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn completion_serves_tables_and_from_columns() {
    let (_f, b) = browser("w31_completion").await;
    let tables = b
        .completion("SELECT * FROM us", "SELECT * FROM us".len())
        .await
        .expect("table completion");
    let items = tables["items"].as_array().expect("items");
    assert!(
        items.iter().any(|i| i["label"] == "users" && i["kind"] == "table"),
        "users among the candidates: {items:?}"
    );

    let columns = b
        .completion("SELECT * FROM users WHERE na", "SELECT * FROM users WHERE na".len())
        .await
        .expect("column completion");
    let items = columns["items"].as_array().expect("items");
    assert!(
        items.iter().any(|i| i["label"] == "name" && i["kind"] == "column"),
        "the FROM-nearest table's columns: {items:?}"
    );
}

#[tokio::test]
async fn list_databases_marks_primary_and_system_and_refuses_unknown() {
    let (f, b) = browser("m3_databases").await;
    let reply = b.list_databases().await.expect("the catalog");
    let mine = f.def["database"].as_str().expect("our database name");
    assert_eq!(reply["primary"], json!(mine));
    assert_eq!(reply["current"], json!(mine));
    let databases = reply["databases"].as_array().expect("databases");
    let ours = databases
        .iter()
        .find(|d| d["name"] == mine)
        .unwrap_or_else(|| panic!("our database is listed: {databases:?}"));
    assert_eq!(ours["primary"], true);
    assert_eq!(ours["system"], false);
    assert_eq!(ours["browsable"], true);
    assert_eq!(ours["tables"], 6);
    // The catalog is privilege-honest: `it` holds grants only on its own databases,
    // so information_schema (visible to every account) is listed and flagged system,
    // while mysql/performance_schema/sys are simply not this account's to see.
    let info = databases
        .iter()
        .find(|d| d["name"] == "information_schema")
        .expect("information_schema is visible to every account");
    assert_eq!(info["system"], true);
    assert_eq!(info["browsable"], true);
    assert!(
        databases.iter().all(|d| d["name"] != "mysql"),
        "a database the def account cannot read is not listed: {databases:?}"
    );

    // SPEC §data.databases: an unknown database name is refused before any SQL is built.
    let refused = b
        .read_table(&json!({ "table": "users", "schema": "no_such_database" }))
        .await;
    assert!(
        refused.unwrap_err().contains("not a database on this connection"),
        "the whitelist refusal"
    );
}
