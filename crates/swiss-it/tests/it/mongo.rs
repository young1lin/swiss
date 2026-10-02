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

//! L1 + L2, the mongo group (SPEC §testing.it): MongoDataBrowser and the six tools against a
//! real MongoDB replica set (8 in the container; any 4.4+ set through `SWISS_IT_MONGO_URL` -
//! nothing here names a version), over a per-test database seeded by `seed_mongo` (57 orders
//! carrying every BSON type the panel prints, a validated `customers`, two indexes, a view).
//! The browser and the tools are built exactly the way the adapter builds them: ServerDef ->
//! MongoEngine -> Engine::browser() / Engine::call(), carrying the def's own policy flags.
//! The unit tests pin the command builders; what only a server can answer is pinned here -
//! that types survive the wire, that a stale replace loses, that a transaction rolls back,
//! that the guard refuses before the server ever sees the command.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{BrowserFlavor, EditError};
use swiss_host::mongobrowser::{
    infer_schema, mongo_aggregate_of, mongo_collection_op_of, mongo_count_of, mongo_edits_of, mongo_explain_of,
    mongo_find_of, mongo_import_of, mongo_index_op_of, MongoBrowser, MongoNs, MONGO_PAGE_MAX,
};
use swiss_it::engine::Kind;
use swiss_it::seed::{fresh, Fresh, MONGO_SEED_ORDERS};
use swiss_mcp::adapters::mongo::{MongoEngine, MONGO_CHECKS};
use swiss_mcp::adapters::tool_server::Engine;

/// A browser (and the engine behind it) over its own seeded database. `allowDestructive`
/// comes from the def, exactly as the adapter reads it.
async fn browser(tag: &str, allow_destructive: bool) -> (Fresh, MongoEngine, Arc<dyn MongoBrowser>) {
    let f = fresh(Kind::Mongo, tag).await;
    let mut def = f.def.clone();
    def["allowDestructive"] = json!(allow_destructive);
    let def: ServerDef = serde_json::from_value(def).expect("the mongo def parses as a ServerDef");
    let engine = MongoEngine::new(&def, "it-mongo").expect("the mongo def builds an engine");
    match engine.browser() {
        Some(BrowserFlavor::Mongo(b)) => (f, engine, b),
        _ => panic!("the mongo engine must expose a mongo browser"),
    }
}

fn ns(f: &Fresh, coll: &str) -> MongoNs {
    MongoNs { db: f.name.clone(), coll: coll.to_string() }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        other => panic!("expected a document, got {other}"),
    }
}

#[tokio::test]
async fn the_catalog_lists_the_seeded_collections_with_their_statistics() {
    let (f, _e, b) = browser("catalog", false).await;
    let dbs = b.list_databases().await.expect("databases");
    let rows = dbs["databases"].as_array().expect("rows");
    let mine = rows.iter().find(|r| r["name"] == json!(f.name)).expect("the per-test database is listed");
    assert_eq!(mine["primary"], json!(true), "the def's database is the one the panel opens on: {dbs}");
    let colls = b.list_collections(&f.name).await.expect("collections");
    let list = colls["collections"].as_array().expect("list");
    let by = |n: &str| list.iter().find(|c| c["name"] == json!(n)).unwrap_or_else(|| panic!("{n} missing: {colls}"));
    assert_eq!(by("orders")["type"], json!("collection"));
    assert_eq!(by("orders")["count"], json!(MONGO_SEED_ORDERS));
    assert_eq!(by("orders")["indexes"], json!(2), "_id_ and status_1_at_-1");
    assert!(by("customers")["validator"]["$jsonSchema"].is_object(), "the validator rides the catalog row");
    assert_eq!(by("open_orders")["type"], json!("view"));
    assert_eq!(by("open_orders")["viewOn"], json!("orders"));
    let info = b.server_info().await.expect("info");
    assert_eq!(info["topology"], json!("replicaSet"), "{info}");
    assert_eq!(info["transactions"], json!(true), "a replica set has transactions: {info}");
}

#[tokio::test]
async fn a_find_pages_and_keeps_every_type_exact() {
    let (f, _e, b) = browser("find_types", false).await;
    let q = mongo_find_of(
        &json!({ "db": f.name, "collection": "orders", "sort": { "n": 1 }, "limit": 20 }),
        MONGO_PAGE_MAX,
    )
    .expect("find");
    let page = b.find(&q).await.expect("page");
    let docs = page["docs"].as_array().expect("docs");
    assert_eq!(docs.len(), 20);
    assert_eq!(page["more"], json!(true));
    let d = &docs[1];
    // Canonical Extended JSON on the browser seam: every width and kind as stored.
    assert_eq!(d["n"], json!({ "$numberInt": "1" }));
    assert_eq!(d["big"], json!({ "$numberLong": "9007199254740994" }), "2^53 + 2, digit for digit");
    assert_eq!(d["price"], json!({ "$numberDecimal": "1.25" }));
    assert_eq!(d["ratio"], json!({ "$numberDouble": "0.25" }));
    assert_eq!(d["at"], json!({ "$date": { "$numberLong": "1704153600000" } }));
    assert_eq!(d["addr"]["zip"], Value::Null);
    let last = mongo_find_of(
        &json!({ "db": f.name, "collection": "orders", "sort": { "n": 1 }, "skip": 40, "limit": 20 }),
        MONGO_PAGE_MAX,
    )
    .expect("find");
    let page = b.find(&last).await.expect("last page");
    assert_eq!(page["docs"].as_array().expect("docs").len(), (MONGO_SEED_ORDERS - 40) as usize);
    assert_eq!(page["more"], json!(false), "the last page says so");
}

#[tokio::test]
async fn count_is_estimated_without_a_filter_and_exact_with_one() {
    let (f, _e, b) = browser("count", false).await;
    let all = b.count(&mongo_count_of(&json!({ "db": f.name, "collection": "orders" })).expect("q")).await.expect("count");
    assert_eq!(all, json!({ "total": MONGO_SEED_ORDERS, "estimated": true }));
    let paid = b
        .count(&mongo_count_of(&json!({ "db": f.name, "collection": "orders", "filter": { "status": "paid" } })).expect("q"))
        .await
        .expect("count");
    assert_eq!(paid, json!({ "total": 15, "estimated": false }));
}

#[tokio::test]
async fn a_replace_is_optimistic_and_a_lost_race_is_a_conflict_naming_the_field() {
    let (f, _e, b) = browser("replace", false).await;
    let q = mongo_find_of(&json!({ "db": f.name, "collection": "orders", "filter": { "n": 3 } }), MONGO_PAGE_MAX).expect("q");
    let original = b.find(&q).await.expect("read")["docs"][0].clone();
    let mut edited = original.clone();
    edited["qty"] = json!({ "$numberInt": "42" });
    let edits = mongo_edits_of(&json!({ "edits": [{ "op": "replace", "original": original, "doc": edited }] })).expect("edits");
    let done = b.apply_edits(&ns(&f, "orders"), &edits).await.unwrap_or_else(|e| panic!("the first replace lands: {e:?}"));
    assert_eq!(done["applied"], json!(1));
    // The same original again: the stored document moved, so the optimistic filter misses.
    let mut again = original.clone();
    again["name"] = json!("Changed");
    let stale = mongo_edits_of(&json!({ "edits": [{ "op": "replace", "original": original, "doc": again }] })).expect("edits");
    match b.apply_edits(&ns(&f, "orders"), &stale).await {
        Err(EditError::Conflict(c)) => {
            assert_eq!(c.columns, vec!["qty".to_string()], "{}", c.message);
            assert!(c.message.contains("Nothing was written"), "{}", c.message);
        }
        Ok(v) => panic!("a stale replace must not land: {v}"),
        Err(EditError::Bad(m)) => panic!("a lost race is a conflict, not a refusal: {m}"),
    }
    let now = b.find(&q).await.expect("reread")["docs"][0].clone();
    assert_eq!(now["name"], original["name"], "the stale write left no trace");
}

#[tokio::test]
async fn several_edits_commit_in_one_transaction_or_not_at_all() {
    let (f, _e, b) = browser("txn", false).await;
    // The second insert breaks the validator (no name): on a replica set the first rolls back.
    let edits = mongo_edits_of(&json!({ "edits": [
        { "op": "insert", "doc": { "name": "Ken" } },
        { "op": "insert", "doc": { "email": "nobody@example.com" } },
    ] }))
    .expect("edits");
    match b.apply_edits(&ns(&f, "customers"), &edits).await {
        Err(EditError::Bad(m)) => assert!(m.to_lowercase().contains("nothing was written"), "{m}"),
        Err(EditError::Conflict(c)) => panic!("a validation failure is not a race: {}", c.message),
        Ok(v) => panic!("the invalid insert must fail the batch: {v}"),
    }
    let n = b
        .count(&mongo_count_of(&json!({ "db": f.name, "collection": "customers", "filter": {} , "exact": true })).expect("q"))
        .await
        .expect("count");
    assert_eq!(n["total"], json!(2), "the valid insert rolled back with the invalid one");
    let ok = mongo_edits_of(&json!({ "edits": [
        { "op": "insert", "doc": { "name": "Ken" } },
        { "op": "updateMany", "filter": { "name": "Ada" }, "update": { "$set": { "vip": true } } },
    ] }))
    .expect("edits");
    let done = b.apply_edits(&ns(&f, "customers"), &ok).await.unwrap_or_else(|_| panic!("a valid batch commits"));
    assert_eq!(done["transaction"], json!(true), "{done}");
    assert_eq!(done["applied"], json!(2));
}

#[tokio::test]
async fn the_guard_refuses_server_commands_and_destructive_ones_without_the_flag() {
    let (f, _e, b) = browser("guard", false).await;
    let err = b.run_command(&f.name, &obj(json!({ "shutdown": 1 }))).await.expect_err("shutdown is refused");
    assert!(err.contains("stops the MongoDB server"), "{err}");
    let err = b.run_command(&f.name, &obj(json!({ "drop": "orders" }))).await.expect_err("drop needs the flag");
    assert!(err.contains("allowDestructive"), "{err}");
    let err = b
        .run_command(&f.name, &obj(json!({ "find": "orders", "lsid": { "id": 1 } })))
        .await
        .expect_err("a session field is refused");
    assert!(err.contains("lsid"), "{err}");
    let pong = b.run_command(&f.name, &obj(json!({ "ping": 1 }))).await.expect("ping runs");
    assert_eq!(pong["reply"]["ok"], json!({ "$numberDouble": "1.0" }), "{pong}");
    assert!(pong["reply"].get("$clusterTime").is_none(), "the gossip fields are stripped: {pong}");
    // The collection still exists: the refusal happened before the server saw the command.
    let colls = b.list_collections(&f.name).await.expect("collections");
    assert!(colls["collections"].as_array().expect("list").iter().any(|c| c["name"] == json!("orders")));
}

#[tokio::test]
async fn with_the_flag_a_drop_runs() {
    let (f, _e, b) = browser("drop", true).await;
    b.run_command(&f.name, &obj(json!({ "drop": "customers" }))).await.expect("drop runs with allowDestructive");
    let colls = b.list_collections(&f.name).await.expect("collections");
    assert!(!colls["collections"].as_array().expect("list").iter().any(|c| c["name"] == json!("customers")));
}

#[tokio::test]
async fn aggregate_explain_and_schema_answer_from_the_server() {
    let (f, _e, b) = browser("analysis", false).await;
    let agg = mongo_aggregate_of(&json!({
        "db": f.name, "collection": "orders",
        "pipeline": [ { "$group": { "_id": "$status", "n": { "$sum": 1 } } }, { "$sort": { "_id": 1 } } ],
    }))
    .expect("agg");
    let out = b.aggregate(&agg).await.expect("aggregate");
    assert_eq!(out["docs"], json!([
        { "_id": "open", "n": { "$numberInt": "42" } },
        { "_id": "paid", "n": { "$numberInt": "15" } },
    ]));
    let indexed = mongo_explain_of(&json!({ "db": f.name, "collection": "orders", "filter": { "status": "paid" } })).expect("q");
    let s = b.explain(&indexed).await.expect("explain")["summary"].clone();
    assert_eq!(s["collscan"], json!(false), "{s}");
    assert_eq!(s["indexes"], json!(["status_1_at_-1"]), "{s}");
    let scan = mongo_explain_of(&json!({ "db": f.name, "collection": "orders", "filter": { "name": "Ada" } })).expect("q");
    let s = b.explain(&scan).await.expect("explain")["summary"].clone();
    assert_eq!(s["collscan"], json!(true), "{s}");
    assert_eq!(s["totalDocsExamined"], json!(MONGO_SEED_ORDERS), "{s}");
    // A pipeline's plan sits where the release puts it - under a `$cursor` stage (4.4-6.0), or
    // at the top with a slot-based tree once `$group` is pushed down (7.0+) - and reads the same.
    let group = json!({ "$group": { "_id": "$name", "n": { "$sum": 1 } } });
    let indexed = mongo_explain_of(&json!({
        "db": f.name, "collection": "orders", "pipeline": [ { "$match": { "status": "paid" } }, group ],
    }))
    .expect("q");
    let s = b.explain(&indexed).await.expect("explain")["summary"].clone();
    assert_eq!((s["collscan"].clone(), s["indexes"].clone()), (json!(false), json!(["status_1_at_-1"])), "{s}");
    let scan = mongo_explain_of(&json!({
        "db": f.name, "collection": "orders", "pipeline": [ { "$match": { "name": "Ada" } }, group ],
    }))
    .expect("q");
    let s = b.explain(&scan).await.expect("explain")["summary"].clone();
    assert_eq!((s["collscan"].clone(), s["totalDocsExamined"].clone()), (json!(true), json!(MONGO_SEED_ORDERS)), "{s}");
    let sample =b.sample(&ns(&f, "orders"), &Map::new(), 1000).await.expect("sample");
    assert_eq!(sample.len(), MONGO_SEED_ORDERS as usize);
    let schema = infer_schema(&sample);
    let big = schema["fields"].as_array().expect("fields").iter().find(|x| x["name"] == json!("big")).expect("big");
    assert_eq!(big["types"][0]["type"], json!("Int64"));
    assert_eq!(big["types"][0]["min"], json!({ "$numberLong": "9007199254740993" }), "the extremes are exact: {big}");
    assert_eq!(big["types"][0]["max"], json!({ "$numberLong": "9007199254741049" }));
}

#[tokio::test]
async fn indexes_report_use_and_change_on_request() {
    let (f, _e, b) = browser("indexes", false).await;
    let made = mongo_index_op_of(&json!({ "op": "create", "keys": { "name": 1 } })).expect("op");
    let r = b.index_op(&ns(&f, "orders"), &made).await.expect("create");
    assert_eq!(r["name"], json!("name_1"), "the server's default name: {r}");
    let hide = mongo_index_op_of(&json!({ "op": "hide", "name": "name_1" })).expect("op");
    b.index_op(&ns(&f, "orders"), &hide).await.expect("hide");
    let list = b.indexes(&ns(&f, "orders")).await.expect("indexes");
    let ix = list["indexes"].as_array().expect("list").iter().find(|i| i["name"] == json!("name_1")).expect("name_1");
    assert_eq!(ix["hidden"], json!(true), "{list}");
    assert!(ix["size"].is_number(), "sizes ride the list: {list}");
    assert!(ix.get("ops").is_some(), "$indexStats use counts ride the list: {list}");
}

#[tokio::test]
async fn import_inserts_what_it_can_and_names_what_it_could_not() {
    let (f, _e, b) = browser("import", false).await;
    let q = mongo_import_of(&json!({
        "db": f.name, "collection": "customers",
        "docs": [ { "name": "Ken" }, { "email": "no-name@example.com" }, { "name": "Dennis" } ],
    }))
    .expect("import");
    let r = b.import(&q).await.expect("import");
    assert_eq!(r["inserted"], json!(2), "{r}");
    assert_eq!(r["errorCount"], json!(1));
    assert_eq!(r["errors"][0]["index"], json!(1), "the rejected document is named by its place: {r}");
    assert!(r["errors"][0]["message"].as_str().expect("message").contains("validation"), "{r}");
}

#[tokio::test]
async fn the_tools_answer_in_relaxed_extended_json() {
    let (f, e, b) = browser("tools", false).await;
    let found = e
        .call("mongo_find", &json!({ "db": f.name, "collection": "orders", "filter": { "n": 0 } }))
        .await
        .expect("mongo_find");
    let doc = &found["docs"][0];
    assert_eq!(doc["qty"], json!(0), "relaxed: an Int32 is a plain number in a tool reply: {found}");
    // Relaxed prints an Int64 as a number too - with its exact digits; the model reads text.
    assert_eq!(doc["big"].to_string(), "9007199254740993", "{found}");
    let described = e
        .call("mongo_describe_collection", &json!({ "db": f.name, "collection": "customers" }))
        .await
        .expect("mongo_describe_collection");
    assert!(described["validator"]["$jsonSchema"].is_object(), "{described}");
    let listed = e.call("mongo_list_collections", &json!({ "db": f.name })).await.expect("mongo_list_collections");
    assert!(listed.to_string().contains("open_orders"), "{listed}");
    let refused = e.call("mongo_command", &json!({ "db": f.name, "command": { "dropDatabase": 1 } })).await;
    assert!(refused.expect_err("dropDatabase needs the flag").contains("allowDestructive"));
    let server = e.call("mongo_inspect", &json!({ "check": "server" })).await.expect("mongo_inspect server");
    let info = b.server_info().await.expect("info");
    assert_eq!(server["summary"]["version"], info["version"], "the server check names the version: {server}");
}

#[tokio::test]
async fn every_inspect_check_answers() {
    // The checks lean on server commands whose shape moved across releases (top, $indexStats,
    // the profiler, replSetGetStatus); each must answer on whatever server the harness holds.
    let (f, e, b) = browser("inspect", false).await;
    let version = b.server_info().await.expect("info")["version"].clone();
    for check in MONGO_CHECKS {
        let r = e
            .call("mongo_inspect", &json!({ "check": check.name, "db": f.name }))
            .await
            .unwrap_or_else(|err| panic!("mongo_inspect {} on {version}: {err}", check.name));
        assert_eq!(r["check"], json!(check.name), "{r}");
    }
    let unused = e
        .call("mongo_inspect", &json!({ "check": "unused_indexes", "db": f.name, "limit": 50 }))
        .await
        .expect("unused_indexes");
    assert!(unused.to_string().contains("status_1_at_-1"), "nothing has queried the seeded index yet: {unused}");
    let repl = e.call("mongo_inspect", &json!({ "check": "replication" })).await.expect("replication");
    assert!(repl.to_string().contains("PRIMARY"), "the one member is the primary: {repl}");
}

#[tokio::test]
async fn collections_are_created_renamed_and_dropped_and_an_export_streams_canonical() {
    let (f, _e, b) = browser("collops", true).await;
    let run = |v: Value| {
        let op = mongo_collection_op_of(&v).expect("collection op");
        let b = b.clone();
        let db = f.name.clone();
        async move { b.collection_op(&db, &op).await }
    };
    run(json!({ "db": f.name, "op": "create", "name": "log", "options": { "capped": true, "size": 65536, "max": 100 } }))
        .await
        .expect("create a capped collection");
    run(json!({ "db": f.name, "op": "createView", "name": "paid", "viewOn": "orders", "pipeline": [ { "$match": { "status": "paid" } } ] }))
        .await
        .expect("create a view");
    run(json!({ "db": f.name, "op": "rename", "name": "log", "to": "journal" })).await.expect("rename");
    let colls = b.list_collections(&f.name).await.expect("collections");
    let names: Vec<&str> = colls["collections"].as_array().expect("list").iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(names.contains(&"journal") && names.contains(&"paid") && !names.contains(&"log"), "{colls}");
    run(json!({ "db": f.name, "op": "drop", "name": "journal" })).await.expect("drop, with the flag");
    let q = mongo_find_of(
        &json!({ "db": f.name, "collection": "paid", "sort": { "n": 1 }, "limit": 50 }),
        MONGO_PAGE_MAX,
    )
    .expect("find");
    let mut dump = b.export(&q, false).await.expect("export");
    assert_eq!((dump.rows, dump.capped), (15, false), "the count comes first, through the view");
    let mut docs = Vec::new();
    while let Some(d) = dump.docs.recv().await {
        docs.push(d.expect("an exported document"));
    }
    assert_eq!(docs.len(), 15);
    assert_eq!(docs[1]["big"], json!({ "$numberLong": "9007199254740997" }), "canonical: n = 4, 2^53 + 5");
    let ops = b.activity().await.expect("activity");
    assert!(ops["rows"].is_array(), "{ops}");
    // killOp of an operation that is not running is not an error on any version.
    assert_eq!(b.activity_kill(i64::from(i32::MAX)).await.expect("killOp"), json!({ "ok": true }));
}
