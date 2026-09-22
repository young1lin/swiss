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

//! L1, the redis group (docs/44 SS2.5): RedisDataBrowser against a real redis 7,
//! over a leased db index reloaded from keys.txt (3,016 keys: five types, a
//! three-level tree namespace, one expiring and one persisted TTL key, UTF-8
//! values, and 3,000 bulk keys to make SCAN paging earn its keep). The browser
//! is built exactly the way the adapter builds it: ServerDef -> RedisEngine ->
//! Engine::browser(), carrying the def's own policy flags.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{BrowserFlavor, RedisBrowser};
use swiss_it::engine::Kind;
use swiss_it::seed::{fresh, fresh_redis_with_neighbor, Fresh};
use swiss_mcp::adapters::redis::RedisEngine;
use swiss_mcp::adapters::tool_server::Engine;

/// A browser over its own leased db index. allowDestructive comes from the def,
/// exactly as the adapter reads it; eval stays off for both browsers here.
async fn browser(tag: &str) -> (Fresh, std::sync::Arc<dyn RedisBrowser>) {
    fresh_with(tag, json!({ "allowDestructive": true })).await
}

async fn fresh_with(tag: &str, extra: Value) -> (Fresh, std::sync::Arc<dyn RedisBrowser>) {
    let f = fresh(Kind::Redis, tag).await;
    let mut def = f.def.clone();
    if let (Some(map), Some(add)) = (def.as_object_mut(), extra.as_object()) {
        for (k, v) in add {
            map.insert(k.clone(), v.clone());
        }
    }
    let def: ServerDef =
        serde_json::from_value(def).expect("the redis def parses as a ServerDef");
    let engine = RedisEngine::new(&def, "it-redis");
    match engine.browser() {
        Some(BrowserFlavor::Redis(b)) => (f, b),
        _ => panic!("the redis engine must expose a redis browser"),
    }
}

/// Walk the whole keyspace page by page the way the panel does.
async fn walk_all(b: &dyn RedisBrowser) -> BTreeSet<String> {
    let mut cursor = String::from("0");
    let mut seen = BTreeSet::new();
    loop {
        let page = b
            .list_keys(&json!({ "cursor": cursor, "count": 1000 }))
            .await
            .expect("scan page");
        for k in page["keys"].as_array().expect("keys") {
            seen.insert(k["key"].as_str().expect("key text").to_string());
        }
        assert_eq!(page["total"], 3016, "DBSIZE rides every first page");
        if page["done"] == json!(true) {
            break;
        }
        cursor = page["cursor"].as_str().expect("cursor text").to_string();
        assert!(cursor != "0", "a non-done page must hand back a cursor");
    }
    seen
}

#[tokio::test]
async fn scan_walks_the_whole_keyspace_without_loss_or_duplication() {
    let (_f, b) = browser("r1_walk").await;
    let seen = walk_all(b.as_ref()).await;
    // 3,016 distinct keys, no duplication across pages, no loss at the seams.
    assert_eq!(seen.len(), 3016, "the BTreeSet dedups; len == distinct keys");
    for expected in [
        "users:1",
        "ns:h:profile",
        "ns:list:queue",
        "ns:set:tags",
        "ns:zset:ladder",
        "tree:l1:l2:leaf1",
        "ns:ttl:ephemeral",
        "ns:ttl:persisted",
        "bulk:key00001",
        "bulk:key03000",
    ] {
        assert!(seen.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn scan_pages_carry_type_ttl_and_total() {
    let (_f, b) = browser("r1_page").await;
    let page = b
        .list_keys(&json!({ "count": 500 }))
        .await
        .expect("first page");
    let keys = page["keys"].as_array().expect("keys");
    assert!(!keys.is_empty());
    assert!(keys.len() <= 500, "COUNT is a hint, never a contract: {keys:?}");
    assert_eq!(page["total"], 3016);
    assert_eq!(page["done"], false, "3,016 keys cannot finish one 500 page");
    for k in keys {
        assert!(k["type"].is_string(), "each key carries its TYPE: {k}");
        assert!(k["ttl"].is_number(), "each key carries its TTL: {k}");
    }
}

#[tokio::test]
async fn scan_match_and_type_narrow_the_walk() {
    let (_f, b) = browser("r1_narrow").await;
    // The three-level tree namespace: only the l2 subtree matches.
    let mut cursor = String::from("0");
    let mut matched = BTreeSet::new();
    loop {
        let page = b
            .list_keys(&json!({ "cursor": cursor, "count": 500, "pattern": "tree:l1:l2:*" }))
            .await
            .expect("matched page");
        for k in page["keys"].as_array().expect("keys") {
            matched.insert(k["key"].as_str().expect("key text").to_string());
        }
        if page["done"] == json!(true) {
            break;
        }
        cursor = page["cursor"].as_str().expect("cursor").to_string();
    }
    assert_eq!(
        matched,
        BTreeSet::from([
            "tree:l1:l2:leaf1".to_string(),
            "tree:l1:l2:leaf2".to_string(),
            "tree:l1:l2:leaf5:extra".to_string(),
        ])
    );

    // Server-side TYPE: exactly the one hash among 3,016 keys. SCAN is incremental -
    // a filtered walk still pages by cursor until done, like the panel does.
    let mut cursor = String::from("0");
    let mut typed = Vec::new();
    loop {
        let page = b
            .list_keys(&json!({
                "cursor": cursor, "count": 1000, "pattern": "ns:*", "type": "hash"
            }))
            .await
            .expect("typed page");
        for k in page["keys"].as_array().expect("keys") {
            typed.push(k.clone());
        }
        if page["done"] == json!(true) {
            break;
        }
        cursor = page["cursor"].as_str().expect("cursor").to_string();
    }
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(typed[0]["key"], "ns:h:profile");
    assert_eq!(typed[0]["type"], "hash");
}

#[tokio::test]
async fn read_key_covers_every_type_and_both_ttl_kinds() {
    let (_f, b) = browser("r2_read").await;
    let string = b.read_key("users:1").await.expect("string read");
    assert_eq!(string["type"], "string");
    assert_eq!(string["value"], json!("张三 🙂"));
    assert_eq!(string["ttl"], -1, "a plain key has no expiry");

    let hash = b.read_key("ns:h:profile").await.expect("hash read");
    assert_eq!(hash["type"], "hash");
    assert_eq!(hash["length"], 50);
    let fields = hash["value"].as_object().expect("fields");
    assert_eq!(fields.len(), 50);
    assert_eq!(fields["f01"], "v01");
    assert_eq!(fields["f50"], "v50");
    assert_eq!(hash["truncated"], Value::Null, "50 fields fit the cap");

    let list = b.read_key("ns:list:queue").await.expect("list read");
    assert_eq!(list["type"], "list");
    assert_eq!(list["length"], 100);
    let items = list["value"].as_array().expect("items");
    assert_eq!(items.len(), 100);
    assert_eq!(items[0], "item-001");
    assert_eq!(items[99], "item-100");

    let set = b.read_key("ns:set:tags").await.expect("set read");
    assert_eq!(set["type"], "set");
    assert_eq!(set["length"], 10);
    let members = set["value"].as_array().expect("members");
    assert_eq!(members.len(), 10);
    // SSCAN order is server-internal: the read dedups and sorts for stability.
    assert_eq!(members[0], "alpha");

    let zset = b.read_key("ns:zset:ladder").await.expect("zset read");
    assert_eq!(zset["type"], "zset");
    assert_eq!(zset["length"], 10);
    let scores = zset["value"].as_object().expect("member -> score");
    // Scores ride as doubles - the wire form redis answers ZRANGE WITHSCORES in.
    let score = |m: &str| scores[m].as_f64().expect("a number");
    assert_eq!(score("alpha"), 100.0);
    assert_eq!(score("beta"), 85.5);
    assert_eq!(score("delta"), 55.25);

    // The two TTL kinds the seed pins: one ephemeral, one persisted twin.
    let ephemeral = b.read_key("ns:ttl:ephemeral").await.expect("ttl read");
    let ttl = ephemeral["ttl"].as_i64().expect("a number");
    assert!(ttl > 0 && ttl <= 3600, "the SETEX window: {ttl}");
    let persisted = b.read_key("ns:ttl:persisted").await.expect("persisted read");
    assert_eq!(persisted["ttl"], -1, "PERSIST removed the twin's expiry");

    // Redis' own answer for a missing key is a fact, not an error.
    let missing = b.read_key("no:such:key").await.expect("missing read");
    assert_eq!(missing["type"], "none");
    assert_eq!(missing["value"], Value::Null);
}

#[tokio::test]
async fn run_command_reads_writes_and_refuses_the_dangerous() {
    let (_f, b) = browser("r3_console").await;
    let got = b.run_command("GET users:2").await.expect("console read");
    assert_eq!(got, json!("Ada Lovelace"));

    let set = b.run_command("SET it:console:key console-value").await;
    assert!(set.is_ok(), "{set:?}");
    let back = b.read_key("it:console:key").await.expect("read back");
    assert_eq!(back["value"], json!("console-value"));

    // UTF-8 keys and values cross the whitespace-split console intact.
    b.run_command("SET 名前:鍵 日本語の値").await.expect("utf8 set");
    let utf8 = b.read_key("名前:鍵").await.expect("utf8 read");
    assert_eq!(utf8["value"], json!("日本語の値"));

    // KEYS would block the server: refused before the socket, always.
    let refused = b.run_command("KEYS *").await;
    assert!(refused.is_err(), "KEYS must never run");

    // EVAL stays off: the def never opted in.
    let eval = b.run_command("EVAL return 1 0").await;
    assert!(eval.is_err(), "EVAL must stay refused without allowEval");
}

#[tokio::test]
async fn run_pipeline_applies_structured_edits_with_readback() {
    let (_f, b) = browser("r3_pipeline").await;
    let reply = b
        .run_pipeline(&[
            vec!["SET".into(), "it:pipe:a".into(), "one".into()],
            // An existing field reports 0 (updated), a new one reports 1 (added).
            vec!["HSET".into(), "ns:h:profile".into(), "f01".into(), "edited".into()],
            vec!["HSET".into(), "ns:h:profile".into(), "f99".into(), "brand-new".into()],
            vec!["RPUSH".into(), "ns:list:queue".into(), "queued".into()],
            vec!["DEL".into(), "users:2".into()],
        ])
        .await
        .expect("the edit batch");
    let replies = reply["replies"].as_array().expect("replies");
    assert_eq!(replies.len(), 5, "one reply per command, in order");
    assert_eq!(replies[1], 0, "HSET over an existing field updates, adds nothing");
    assert_eq!(replies[2], 1, "HSET of a new field adds exactly one");
    assert_eq!(replies[4], 1, "DEL reports one key gone");

    let field = b.read_key("ns:h:profile").await.expect("read back");
    assert_eq!(field["value"]["f01"], json!("edited"));
    assert_eq!(field["value"]["f99"], json!("brand-new"));
    assert_eq!(field["length"], 51);
    let list = b.read_key("ns:list:queue").await.expect("read back");
    assert_eq!(list["length"], 101);
    let gone = b.read_key("users:2").await.expect("read back");
    assert_eq!(gone["type"], "none");
    let made = b.read_key("it:pipe:a").await.expect("read back");
    assert_eq!(made["value"], json!("one"));
}

#[tokio::test]
async fn a_refused_command_rejects_the_whole_pipeline() {
    // docs/22 W3.3: the guard vets the whole batch before the socket - a
    // buffered edit never half-applies, so the write in front of the refused
    // EVAL must not land either.
    let (_f, b) = fresh_with("r3_refuse", json!({ "allowDestructive": true })).await;
    let refused = b
        .run_pipeline(&[
            vec!["SET".into(), "it:refused:canary".into(), "should-not-exist".into()],
            vec!["EVAL".into(), "return 1".into(), "0".into()],
        ])
        .await;
    let err = refused.expect_err("the batch must be refused");
    assert!(err.contains("EVAL"), "the refusal names the command: {err}");
    let canary = b.read_key("it:refused:canary").await.expect("read back");
    assert_eq!(canary["type"], "none", "nothing half-applied");
}

#[tokio::test]
async fn a_strict_def_refuses_server_destructive_edits() {
    // No allowDestructive on the def: the adapter's own policy flags ride the
    // browser. Key-level edits (SET/DEL) pass with no opt-in by design; the
    // server-level wipes do not.
    let (_f, b) = fresh_with("r3_strict", json!({})).await;
    let refused = b
        .run_pipeline(&[vec!["FLUSHDB".into()]])
        .await;
    assert!(refused.is_err(), "FLUSHDB must stay refused: {refused:?}");
    let alive = b.read_key("users:1").await.expect("read back");
    assert_eq!(alive["value"], json!("张三 🙂"), "nothing ran");
}

#[tokio::test]
async fn list_databases_names_each_keyspace_with_the_foreign_reason() {
    let (f, neighbor) = fresh_redis_with_neighbor("r4_catalog").await;
    let def: ServerDef =
        serde_json::from_value(f.def.clone()).expect("the redis def parses");
    let engine = RedisEngine::new(&def, "it-redis");
    let b = match engine.browser() {
        Some(BrowserFlavor::Redis(b)) => b,
        _ => panic!("the redis engine must expose a redis browser"),
    };
    let reply = b.list_databases().await.expect("the keyspace catalog");
    let mine = f.def["db"].to_string();
    assert_eq!(reply["primary"], json!(mine));
    assert_eq!(reply["current"], json!(mine));
    let databases = reply["databases"].as_array().expect("databases");
    let ours = databases
        .iter()
        .find(|d| d["name"].as_str().map(|n| n == mine.trim_matches('"')) == Some(true))
        .unwrap_or_else(|| panic!("our db is listed: {databases:?}"));
    assert_eq!(ours["primary"], true);
    assert_eq!(ours["browsable"], true);
    assert_eq!(ours["reason"], Value::Null);
    assert_eq!(ours["tables"], 3016, "the key count is the redis table count");

    // The neighbor db exists in the keyspace but is not browsable from this
    // connection: SELECT would re-mode the shared handle.
    let theirs = neighbor.def["db"].to_string();
    let theirs = theirs.trim_matches('"');
    let other = databases
        .iter()
        .find(|d| d["name"] == theirs)
        .expect("the neighbor db is listed");
    assert_eq!(other["browsable"], false);
    let reason = other["reason"].as_str().expect("the reason text");
    assert!(reason.contains("its own connection"), "{reason}");
}
