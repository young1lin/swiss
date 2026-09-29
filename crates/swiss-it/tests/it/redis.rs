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
//! over a leased db index reloaded from keys.txt (3,020 keys: five types, a
//! three-level tree namespace, one expiring and one persisted TTL key, UTF-8
//! values, 3,000 bulk keys to make SCAN paging earn its keep, and the four
//! generated stream keys of docs/45 §2.7). The browser
//! is built exactly the way the adapter builds it: ServerDef -> RedisEngine ->
//! Engine::browser(), carrying the def's own policy flags.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use swiss_host::config::ServerDef;
use swiss_host::dbbrowser::{BrowserFlavor, RedisBrowser};
use swiss_it::engine::{engine, Kind};
use swiss_it::seed::{fresh, fresh_redis_exclusive, fresh_redis_with_neighbor, Fresh, REDIS_SEED_KEYS};
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
        assert_eq!(page["total"], REDIS_SEED_KEYS, "DBSIZE rides every first page");
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
    // 3,020 distinct keys, no duplication across pages, no loss at the seams.
    assert_eq!(seen.len(), REDIS_SEED_KEYS as usize, "the BTreeSet dedups; len == distinct keys");
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
    // COUNT is a bucket-examination hint, never a row cap: a dense hash tail can hand
    // back MORE than COUNT keys in one page (the per-container hash seed decides, so
    // this flips with a restart). What stays true: 500 examined buckets cannot drain
    // 3,016 keys, so the page is partial.
    assert!(keys.len() < REDIS_SEED_KEYS as usize, "a 500-hint page is partial: {} keys", keys.len());
    assert_eq!(page["total"], REDIS_SEED_KEYS);
    assert_eq!(page["done"], false, "3,020 keys cannot finish one 500 page");
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

    // UTF-8 keys and values cross the console intact.
    b.run_command("SET 名前:鍵 日本語の値").await.expect("utf8 set");
    let utf8 = b.read_key("名前:鍵").await.expect("utf8 read");
    assert_eq!(utf8["value"], json!("日本語の値"));

    // The owner's line, against a real server (2026-09-28): a quoted JSON value with escaped
    // quotes inside it, and the options that follow it. Split on whitespace, this reached redis
    // as five broken words; split as a shell splits, the value is the JSON typed and NX/EX are
    // options — the TTL below proves EX arrived as an option rather than as part of the value.
    let json_set = b
        .run_command(r#"SET it:json "{\"test\": \"123\"}" NX EX 100"#)
        .await
        .expect("quoted json set");
    assert_eq!(json_set, json!("OK"), "NX on a fresh key sets it");
    let stored = b.read_key("it:json").await.expect("read back the json");
    assert_eq!(stored["value"], json!(r#"{"test": "123"}"#));
    let ttl = stored["ttl"].as_i64().expect("a ttl");
    assert!(
        (1..=100).contains(&ttl),
        "EX 100 must be the TTL, got {ttl}"
    );

    // NX means "only if absent": the second run answers nil and leaves the first value alone.
    let again = b
        .run_command(r#"SET it:json "{\"test\": \"changed\"}" NX EX 100"#)
        .await
        .expect("the second NX runs");
    assert_eq!(again, Value::Null, "NX on an existing key sets nothing");
    let same = b.read_key("it:json").await.expect("still the first value");
    assert_eq!(same["value"], json!(r#"{"test": "123"}"#));

    // A value with spaces, and one with an apostrophe inside double quotes.
    b.run_command(r#"SET it:spaced "two words here""#)
        .await
        .expect("spaced set");
    assert_eq!(
        b.read_key("it:spaced").await.expect("spaced read")["value"],
        json!("two words here")
    );

    // Half a quote is refused by name, and nothing is sent.
    let unbalanced = b.run_command(r#"SET it:broken "half"#).await;
    assert!(
        unbalanced.is_err_and(|e| e.contains("unbalanced quote")),
        "an unbalanced quote names itself"
    );
    assert_eq!(
        b.read_key("it:broken").await.expect("nothing was written")["type"],
        "none"
    );

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
    assert_eq!(ours["tables"], REDIS_SEED_KEYS, "the key count is the redis table count");

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
// --- docs/45: stream windows ----------------------------------------------------------------------

/// A raw connection straight to a Fresh def's index — the commandstats reader in
/// the tick test. Its commands (INFO, CONFIG RESETSTAT) never pass through the
/// adapter's guard and never need to: the guard protects the gateway's SHARED
/// connection, not a test's private one.
async fn raw_conn(def: &Value) -> redis::aio::MultiplexedConnection {
    let url = format!(
        "redis://{}:{}/{}",
        def["host"].as_str().expect("host"),
        def["port"].as_u64().expect("port"),
        def["db"].as_u64().expect("db"),
    );
    let client = redis::Client::open(url.as_str()).expect("open the def");
    client
        .get_multiplexed_async_connection()
        .await
        .expect("connect the def")
}

/// The ms half of a stream id, for ordering assertions.
fn id_ms(id: &str) -> i64 {
    id.split('-')
        .next()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(i64::MIN)
}

#[tokio::test]
async fn read_key_on_a_stream_returns_the_newest_window_first() {
    // docs/45 §2.1: opening a stream key must land on the NEWEST 100 entries with
    // the shape the stream view codes against — not type_aware_read's oldest-first
    // page (the panel opens keys without knowing their type).
    let (_f, b) = browser("r45_latest").await;
    let v = b.read_key("stream:ticks").await.expect("read_key");
    assert_eq!(v["key"], "stream:ticks");
    assert_eq!(v["type"], "stream");
    assert_eq!(v["ttl"], json!(-1), "the /key facts still ride along");
    assert_eq!(v["length"], json!(10_000));
    assert_eq!(v["firstId"], json!("1700000000000-0"));
    assert_eq!(v["lastId"], json!("1700000999900-0"));
    let entries = v["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 100, "D1's default window");
    assert_eq!(entries[0]["id"], json!("1700000999900-0"), "newest first");
    assert_eq!(entries[99]["id"], json!("1700000990000-0"), "100 entries back");
    for pair in entries.windows(2) {
        assert!(
            id_ms(pair[0]["id"].as_str().expect("id")) > id_ms(pair[1]["id"].as_str().expect("id")),
            "the window is strictly newest-first"
        );
    }
    assert_eq!(v["columns"], json!(["sym", "px", "qty", "side"]));
    assert_eq!(v["more"], json!(true), "10,000 entries cannot fit one window");
}

#[tokio::test]
async fn read_stream_pages_older_by_cursor_without_loss_or_duplication() {
    // docs/45 §2.1: the exclusive `before` cursor walks the whole stream, oldest
    // page last, with no row repeated at a seam and none dropped.
    let (_f, b) = browser("r45_before").await;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let o = match &cursor {
            Some(id) => json!({ "before": id, "count": 1000 }),
            None => json!({ "count": 1000 }),
        };
        let v = b.read_stream("stream:ticks", &o).await.expect("page");
        let entries = v["entries"].as_array().expect("entries");
        for e in entries {
            assert!(
                seen.insert(e["id"].as_str().expect("id").to_string()),
                "an id repeated at a page seam"
            );
        }
        pages += 1;
        let more = v["more"].as_bool().expect("more");
        cursor = entries.last().map(|e| e["id"].as_str().expect("id").to_string());
        if !more {
            break;
        }
        assert!(pages <= 12, "10,000 entries at count=1000 is ten pages");
    }
    // Eleven pages, not ten: `more` means “the page was full”, so the tenth page
    // (exactly 10,000th entry) still flags more and the walk ends on one empty
    // page — the documented cost of a `more` that never spends a fourth command
    // (docs/45 §2.1); the panel's Load-earlier renders that page as the end.
    assert_eq!(pages, 11);
    assert_eq!(seen.len(), 10_000, "no loss, no duplication");
    assert_eq!(seen.first().map(String::as_str), Some("1700000000000-0"));
    assert_eq!(seen.last().map(String::as_str), Some("1700000999900-0"));
}

#[tokio::test]
async fn read_stream_after_returns_only_newer_entries_newest_first() {
    // docs/45 §2.1/D6: the Follow tick's catch-up — strictly newer than the
    // cursor, newest first, and quiet when caught up.
    let (_f, b) = browser("r45_after").await;
    let cmds: Vec<Vec<String>> = (0..3)
        .map(|i| {
            vec![
                "XADD".into(),
                "stream:ticks".into(),
                format!("{}-0", 1_700_000_000_000_i64 + 1_000_000 + i * 100),
                "sym".into(),
                "AAA".into(),
                "px".into(),
                "1.00".into(),
            ]
        })
        .collect();
    b.run_pipeline(&cmds).await.expect("three arrivals");
    let v = b
        .read_stream("stream:ticks", &json!({ "after": "1700000999900-0" }))
        .await
        .expect("the catch-up page");
    let ids: Vec<&str> = v["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| e["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        vec!["1700001000200-0", "1700001000100-0", "1700001000000-0"],
        "newer than the cursor, newest first"
    );
    assert_eq!(v["more"], json!(false));
    assert_eq!(v["length"], json!(10_003), "XLEN sees the arrivals");
    let quiet = b
        .read_stream("stream:ticks", &json!({ "after": "1700001000200-0" }))
        .await
        .expect("the caught-up page");
    assert_eq!(quiet["entries"].as_array().expect("entries").len(), 0);
    assert_eq!(quiet["more"], json!(false));
}

#[tokio::test]
async fn read_stream_after_caps_and_flags_more() {
    // docs/45 §2.1: a backlog larger than the window must show the NEWEST end of
    // it (catching up means seeing now, docs/45's own words) and flag `more` —
    // never silently fill the window with the oldest of the gap.
    let (_f, b) = browser("r45_backlog").await;
    let cmds: Vec<Vec<String>> = (0..150)
        .map(|i| {
            vec![
                "XADD".into(),
                "stream:ticks".into(),
                format!("{}-0", 1_700_000_000_000_i64 + 2_000_000 + i * 100),
                "sym".into(),
                "AAA".into(),
            ]
        })
        .collect();
    b.run_pipeline(&cmds).await.expect("a 150-entry backlog");
    let v = b
        .read_stream("stream:ticks", &json!({ "after": "1700000999900-0" }))
        .await
        .expect("the after page");
    let entries = v["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 100, "the window caps at the default 100");
    assert_eq!(
        entries[0]["id"],
        json!("1700002014900-0"),
        "the newest of the backlog leads"
    );
    assert!(
        entries
            .iter()
            .all(|e| e["id"].as_str().expect("id") > "1700000999900-0"),
        "every entry is strictly newer than the cursor"
    );
    assert_eq!(v["more"], json!(true), "a capped window says so");
}

#[tokio::test]
async fn follow_polling_costs_three_commands_per_tick() {
    // docs/45 §2.1: one tick = XREVRANGE + XLEN + XINFO STREAM, exactly. INFO
    // commandstats is SERVER-wide, so the measurement needs the whole redis to itself:
    // fresh_redis_exclusive leases all sixteen indexes in ONE atomic acquire (holding
    // nothing while it waits), seeds only the one this browser reads, and holds the
    // other fifteen as unseeded placeholders - every other redis test parks at its
    // next lease, and nobody's commands land in these counters but this test's own.
    let (f, _exclusive) = fresh_redis_exclusive("r45_tick").await;
    let mut def = f.def.clone();
    def["allowDestructive"] = json!(true);
    let server_def: ServerDef =
        serde_json::from_value(def).expect("the redis def parses as a ServerDef");
    let engine = RedisEngine::new(&server_def, "it-redis");
    let b = match engine.browser() {
        Some(BrowserFlavor::Redis(b)) => b,
        _ => panic!("the redis engine must expose a redis browser"),
    };
    let mut conn = raw_conn(&f.def).await;
    redis::cmd("CONFIG")
        .arg("RESETSTAT")
        .query_async::<()>(&mut conn)
        .await
        .expect("reset the counters");
    for tick in 1..=10 {
        let v = b
            .read_stream("stream:ticks", &json!({ "after": "1700000999900-0" }))
            .await
            .expect("tick");
        assert!(
            v["entries"].as_array().expect("entries").is_empty(),
            "a quiet stream is a quiet tick"
        );
        if tick % 5 == 0 {
            b.stream_groups("stream:ticks").await.expect("the groups fold rides every fifth tick");
        }
    }
    let stats: String = redis::cmd("INFO")
        .arg("commandstats")
        .query_async(&mut conn)
        .await
        .expect("the counters");
    let calls = |name: &str| -> i64 {
        stats
            .lines()
            .find(|l| l.starts_with(&format!("{name}:calls=")) || l.starts_with(name))
            .and_then(|l| l.split("calls=").nth(1))
            .and_then(|rest| rest.split(',').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    assert_eq!(calls("cmdstat_xrevrange"), 10, "one window per tick");
    assert_eq!(calls("cmdstat_xlen"), 10, "one length per tick");
    // redis 7 accounts subcommands as their own counters: the window's XINFO STREAM
    // and the fold's XINFO GROUPS land in two buckets — which pins the budget even
    // tighter than one shared xinfo counter would.
    assert_eq!(calls("cmdstat_xinfo|stream"), 10, "one stream info per tick");
    assert_eq!(calls("cmdstat_xinfo|groups"), 2, "the groups fold rides every fifth tick");
    assert_eq!(calls("cmdstat_xread"), 0, "D6: XREAD stays off the shared connection");
    assert_eq!(calls("cmdstat_type"), 0, "the healthy tick never pays for TYPE");
}

#[tokio::test]
async fn read_stream_ts_derives_from_the_id() {
    // docs/45 §2.1: the ms half of the id, as ISO 8601 with milliseconds,
    // derived on the server — the panel never parses an id.
    let (_f, b) = browser("r45_ts").await;
    let one = b.read_stream("stream:one", &json!({})).await.expect("one");
    let e = &one["entries"][0];
    assert_eq!(e["id"], json!("1700000000000-0"));
    assert_eq!(e["ts"], json!("2023-11-14T22:13:20.000Z"));
    let ticks = b.read_stream("stream:ticks", &json!({})).await.expect("ticks");
    assert_eq!(ticks["entries"][0]["ts"], json!("2023-11-14T22:29:59.900Z"));
}

#[tokio::test]
async fn read_stream_ragged_fields_union_columns_in_first_seen_order() {
    // docs/45 D3: the seed's five interleaved entries (a / a,b / b,c / c / a,c,d,
    // oldest to newest) union into first-seen-scanning-newest-first order — a,c,d,b
    // — and each row shows exactly its own fields.
    let (_f, b) = browser("r45_ragged").await;
    let v = b.read_stream("stream:ragged", &json!({})).await.expect("ragged");
    let entries = v["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 5);
    assert_eq!(
        v["columns"],
        json!(["a", "c", "d", "b"]),
        "first-seen scanning newest-first, never alphabetized"
    );
    assert_eq!(entries[0]["id"], json!("1700000000400-0"));
    assert_eq!(entries[0]["fields"].as_object().expect("fields").len(), 3);
    assert_eq!(entries[4]["fields"]["a"], json!("1"));
    assert_eq!(entries[4]["fields"].as_object().expect("fields").len(), 1);
}

#[tokio::test]
async fn read_stream_empty_and_single_entry_streams() {
    // docs/45 §2.5: the empty stream is a key with a type and no entries — a
    // real window, not an error; the single-entry stream is a complete first page.
    let (_f, b) = browser("r45_edges").await;
    let empty = b.read_stream("stream:empty", &json!({})).await.expect("empty");
    assert_eq!(empty["type"], "stream");
    assert_eq!(empty["length"], json!(0));
    assert_eq!(empty["entries"], json!([]));
    assert_eq!(empty["firstId"], Value::Null);
    assert_eq!(empty["lastId"], Value::Null);
    assert_eq!(empty["columns"], json!([]));
    assert_eq!(empty["more"], json!(false));
    let one = b.read_stream("stream:one", &json!({})).await.expect("one");
    assert_eq!(one["length"], json!(1));
    assert_eq!(one["entries"].as_array().expect("entries").len(), 1);
    assert_eq!(one["more"], json!(false), "1 < 100: the stream is fully shown");
}

#[tokio::test]
async fn read_stream_refuses_non_stream_keys_and_both_cursors() {
    // docs/45 §2.1: the refusal names the real type (one follow-up TYPE on the
    // error path only), a missing key is the /key route's { type: none } fact, and
    // the two cursors never run together.
    let (_f, b) = browser("r45_refuse").await;
    let hash = b.read_stream("ns:h:profile", &json!({})).await;
    let err = hash.expect_err("a hash is not a stream");
    assert!(err.contains("is a hash, not a stream"), "{err}");
    let missing = b.read_stream("no:such:key", &json!({})).await.expect("missing");
    assert_eq!(missing["type"], "none");
    assert_eq!(missing["ttl"], json!(-2));
    assert_eq!(missing["value"], Value::Null);
    let both = b
        .read_stream("stream:ticks", &json!({ "before": "1-0", "after": "2-0" }))
        .await;
    assert!(both.expect_err("opposite directions").contains("opposite directions"));
    for bad in [json!({ "count": 0 }), json!({ "count": "abc" })] {
        let refused = b.read_stream("stream:ticks", &bad).await;
        assert!(refused.expect_err("a bad count").contains("positive number"), "{bad:?}");
    }
}

#[tokio::test]
async fn read_stream_filters_through_a_bounded_backward_walk() {
    // docs/49 §2.2: redis indexes nothing inside an entry, so a filter is a walk —
    // read backwards, keep what matches, stop at the count or the budget, and SAY
    // how far you got. The seed's 10,000 ticks cycle five symbols, so `sym=AAA` is
    // every fifth entry: a 500-entry scan page holds exactly the 100 the window
    // wants, and the answer's two cursors are the two directions it can be resumed in.
    let (_f, b) = browser("r49_filter").await;
    let v = b
        .read_stream("stream:ticks", &json!({ "match": "sym=AAA" }))
        .await
        .expect("the filtered window");
    let entries = v["entries"].as_array().expect("entries").clone();
    assert_eq!(
        entries.len(),
        100,
        "the window still caps at the default 100"
    );
    assert!(
        entries.iter().all(|e| e["fields"]["sym"] == json!("AAA")),
        "every kept entry matches the filter"
    );
    assert_eq!(
        entries[0]["id"],
        json!("1700000999500-0"),
        "newest match leads"
    );
    assert_eq!(entries[99]["id"], json!("1700000950000-0"));
    assert_eq!(
        v["scanned"],
        json!(500),
        "one scan page held all 100 matches"
    );
    assert_eq!(
        v["scannedFrom"],
        json!("1700000999900-0"),
        "the newest id examined"
    );
    assert_eq!(v["scannedTo"], json!("1700000950000-0"), "and the oldest");
    assert_eq!(
        v["more"],
        json!(true),
        "the stream did not run out, the count did"
    );
    assert_eq!(
        v["length"],
        json!(10_000),
        "the stream's own length is unfiltered"
    );
    assert_eq!(v["columns"], json!(["sym", "px", "qty", "side"]));

    // Two terms is an AND, and a rarer match makes the walk PAGE: `side=b` halves
    // the matches again, so a hundred of them span a thousand entries — two scan
    // pages, not one, and the budget is nowhere near spent.
    let two = b
        .read_stream("stream:ticks", &json!({ "match": "sym=AAA side=b" }))
        .await
        .expect("the two-term window");
    assert_eq!(two["entries"].as_array().expect("entries").len(), 100);
    assert_eq!(two["entries"][0]["id"], json!("1700000999000-0"));
    assert_eq!(
        two["scanned"],
        json!(1_000),
        "a second page was read for the rest"
    );
    assert_eq!(two["scannedTo"], json!("1700000900000-0"));

    // A bare word looks at every value, `~` is the substring form, and a count rides
    // along unchanged — the filter narrows what is kept, never what was asked for.
    let bare = b
        .read_stream("stream:ticks", &json!({ "match": "EEE", "count": 5 }))
        .await
        .expect("the bare-word window");
    let bare_rows = bare["entries"].as_array().expect("entries").clone();
    assert_eq!(bare_rows.len(), 5);
    assert!(bare_rows.iter().all(|e| e["fields"]["sym"] == json!("EEE")));
    let sub = b
        .read_stream("stream:ticks", &json!({ "match": "side~b", "count": 3 }))
        .await
        .expect("the substring window");
    assert!(sub["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .all(|e| e["fields"]["side"] == json!("b")));

    // The filter rides the follow tick too, and the tick's own low end is kept: the
    // walk pages DOWN from the newest, never back past the cursor it was given.
    let after = b
        .read_stream(
            "stream:ticks",
            &json!({ "after": "1700000999000-0", "match": "sym=AAA" }),
        )
        .await
        .expect("the filtered tick");
    assert_eq!(
        after["entries"].as_array().expect("entries").len(),
        1,
        "one AAA among the nine entries newer than the cursor"
    );
    assert_eq!(after["entries"][0]["id"], json!("1700000999500-0"));
    assert_eq!(
        after["scanned"],
        json!(9),
        "nine examined — the range, not the budget"
    );
    assert_eq!(after["scannedFrom"], json!("1700000999900-0"));
    assert_eq!(after["scannedTo"], json!("1700000999100-0"));
    assert_eq!(
        after["more"],
        json!(false),
        "the range ran out: nothing was skipped"
    );

    // An unfiltered window says nothing about scanning, because none happened —
    // absent, not zero, so the panel can tell "no walk" from "a walk that found none".
    let plain = b
        .read_stream("stream:ticks", &json!({}))
        .await
        .expect("plain");
    assert!(plain.get("scanned").is_none(), "{plain}");
    assert!(plain.get("scannedTo").is_none());
}

#[tokio::test]
async fn a_filter_that_matches_nothing_says_how_far_it_looked() {
    // docs/49 §2.2: the honest empty answer. The walk reads the whole stream (10,000
    // entries, well inside the 20,000 budget), keeps nothing, and reports both the
    // count it examined and the oldest id it reached — so the panel can say "nothing
    // in the newest 10,000" instead of implying the stream holds nothing else.
    // `more` is false here precisely because the RANGE ran out rather than the budget:
    // there is genuinely nothing older to walk to.
    let (_f, b) = browser("r49_nomatch").await;
    let v = b
        .read_stream("stream:ticks", &json!({ "match": "sym=ZZZ" }))
        .await
        .expect("the empty filtered window");
    assert_eq!(v["entries"], json!([]));
    assert_eq!(v["columns"], json!([]), "no rows, no column union");
    assert_eq!(v["scanned"], json!(10_000), "the whole stream was examined");
    assert_eq!(
        v["scannedTo"],
        json!("1700000000000-0"),
        "as far back as there is"
    );
    assert_eq!(
        v["more"],
        json!(false),
        "the stream ran out, not the budget"
    );
    assert_eq!(v["length"], json!(10_000));
    // A filter line that cannot be split is refused before any of that happens.
    let bad = b
        .read_stream("stream:ticks", &json!({ "match": "sym=\"AAA" }))
        .await;
    assert!(
        bad.expect_err("an unbalanced quote")
            .contains("unbalanced quote"),
        "the refusal names what is wrong with the line"
    );
}
#[tokio::test]
async fn the_command_catalog_comes_from_the_server_itself() {
    // docs/50: the console's completion is built from COMMAND DOCS + COMMAND INFO, so it
    // knows the commands THIS server has - its version, its modules - instead of a table
    // kept by hand in the panel. Against a real redis 7 that means summaries, syntax built
    // from redis' own argument spec, the key positions, and the container commands flattened
    // into the names a person types.
    let (_f, b) = browser("r50_catalog").await;
    let v = b.command_catalog().await.expect("the catalog");
    assert_eq!(v["documented"], json!(true), "redis 7 answers COMMAND DOCS");
    let rows = v["commands"].as_array().expect("commands").clone();
    assert!(
        rows.len() > 150,
        "a real redis has hundreds of commands, got {}",
        rows.len()
    );
    let by = |name: &str| -> Value {
        rows.iter()
            .find(|r| r["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name} is missing from the catalog"))
            .clone()
    };

    let set = by("SET");
    assert!(
        set["summary"].as_str().unwrap_or("").to_lowercase().contains("set"),
        "redis' own one-line summary rides along: {}",
        set["summary"]
    );
    let syntax = set["syntax"].as_str().unwrap_or("");
    assert!(syntax.starts_with("key value"), "SET's syntax: {syntax}");
    assert!(syntax.contains("[NX|XX]"), "a one-of of pure tokens: {syntax}");
    assert_eq!(set["firstKey"], json!(1), "COMMAND INFO's key position");
    assert_eq!(set["group"], json!("string"));
    let tokens = set["tokens"].as_array().expect("tokens");
    for t in ["NX", "XX", "EX", "PX"] {
        assert!(tokens.contains(&json!(t)), "SET accepts {t}: {tokens:?}");
    }

    // A key that is not the first argument, and one that runs to the end of the line.
    assert_eq!(by("GET")["firstKey"], json!(1));
    assert_eq!(by("DEL")["lastKey"], json!(-1), "DEL takes keys to the end");
    assert_eq!(by("MSET")["step"], json!(2), "MSET's values are not keys");

    // Containers: the row a person types is "XINFO STREAM", and it inherits XINFO's key
    // position - the console completes a key there because of this line.
    assert_eq!(by("XINFO")["container"], json!(true));
    let xs = by("XINFO STREAM");
    assert_eq!(xs["container"], json!(false));
    assert_eq!(xs["firstKey"], json!(2), "one word later than a plain command's key");
    assert!(xs["syntax"].as_str().unwrap_or("").starts_with("key"), "{}", xs["syntax"]);
    assert!(rows.iter().any(|r| r["name"] == json!("CONFIG GET")), "CONFIG's subcommands too");

    // Nothing here is a write, and nothing here needs allowDestructive: it is introspection.
    let shouty = |r: &Value| -> bool {
        let n = r["name"].as_str().unwrap_or("");
        n == n.to_uppercase()
    };
    assert!(rows.iter().all(shouty), "every name is upper case, the way a console prints it");
}

#[tokio::test]
async fn stream_groups_reports_pending_and_lag() {
    // docs/45 §2.4: the read-only consumer table over the seed's feed group —
    // seven read-never-ACKed entries, one consumer, and the lag that follows; a
    // vanished key is an empty table, not an error.
    let (_f, b) = browser("r45_groups").await;
    let v = b.stream_groups("stream:ticks").await.expect("groups");
    let groups = v["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["name"], json!("feed"));
    assert_eq!(groups[0]["consumers"], json!(1));
    assert_eq!(groups[0]["pending"], json!(7));
    assert_eq!(groups[0]["lag"], json!(9_993), "10,000 minus the seven delivered");
    assert_eq!(groups[0]["last-delivered-id"], json!("1700000000600-0"));
    let gone = b.stream_groups("no:such:key").await.expect("gone");
    assert_eq!(gone["groups"], json!([]));
}


#[tokio::test]
async fn the_catalog_spells_tokened_blocks_and_carries_the_key_specs_of_moving_keys() {
    // 2026-09-29: against a real redis 7 - the token leads its block (ZRANGEBYSCORE's LIMIT,
    // XREAD's STREAMS), a repeated value does not repeat its token (ZINTER's WEIGHTS) unless
    // redis says multiple_token (SORT's GET), and the commands whose keys move carry the key
    // specs the console completes them by.
    let (_f, b) = browser("r50_specs").await;
    let v = b.command_catalog().await.expect("the catalog");
    let rows = v["commands"].as_array().expect("commands").clone();
    let by = |name: &str| -> Value {
        rows.iter()
            .find(|r| r["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name} is missing from the catalog"))
            .clone()
    };
    let syntax = |name: &str| by(name)["syntax"].as_str().unwrap_or("").to_string();
    assert!(syntax("ZRANGEBYSCORE").contains("[LIMIT offset count]"), "{}", syntax("ZRANGEBYSCORE"));
    assert!(syntax("XREAD").contains("STREAMS key [key ...] id [id ...]"), "{}", syntax("XREAD"));
    assert!(syntax("ZINTER").contains("[WEIGHTS weight [weight ...]]"), "{}", syntax("ZINTER"));
    // Redis 7 names SORT's argument get-pattern; the shape is what matters.
    assert!(syntax("SORT").contains("[GET get-pattern [GET get-pattern ...]]"), "{}", syntax("SORT"));

    let xread = by("XREAD");
    let spec = &xread["keySpecs"][0];
    assert_eq!(spec["begin"]["type"], json!("keyword"), "{xread}");
    assert_eq!(spec["begin"]["keyword"], json!("STREAMS"));
    assert_eq!(spec["find"]["lastkey"], json!(-1));
    assert_eq!(spec["find"]["limit"], json!(2), "keys are the first half of what follows");
    let zunion = by("ZUNION");
    assert_eq!(zunion["keySpecs"][0]["find"]["type"], json!("keynum"), "{zunion}");
    let store = by("ZUNIONSTORE");
    assert_eq!(store["keySpecs"].as_array().map(Vec::len), Some(2), "destination + sources: {store}");
    // A subcommand's specs come from its own nested row.
    assert_eq!(by("XINFO STREAM")["keySpecs"][0]["begin"]["index"], json!(2));
    // Every spec the gateway ships is one the console can evaluate.
    for r in &rows {
        for sp in r["keySpecs"].as_array().into_iter().flatten() {
            let bt = sp["begin"]["type"].as_str().unwrap_or("");
            let ft = sp["find"]["type"].as_str().unwrap_or("");
            assert!(["index", "keyword"].contains(&bt), "{}: {sp}", r["name"]);
            assert!(["range", "keynum"].contains(&ft), "{}: {sp}", r["name"]);
        }
    }
}

#[tokio::test]
async fn a_key_named_with_a_space_is_one_argument_end_to_end() {
    // 2026-09-29: the panel's Delete built "DEL " + key and the console route split it like a
    // shell - `DEL my key` removed `my` and `key` and left `my key`. The panel now sends the
    // key through the pipeline as ONE argument; this is that path against a real server, with
    // the two innocent neighbours present to prove they survive.
    let (_f, b) = browser("r50_spaced").await;
    b.run_pipeline(&[
        vec!["SET".into(), "my key".into(), "target".into()],
        vec!["SET".into(), "my".into(), "neighbour 1".into()],
        vec!["SET".into(), "key".into(), "neighbour 2".into()],
        vec!["SET".into(), "taken key".into(), "precious".into()],
        vec!["SET".into(), "".into(), "the empty name".into()],
    ])
    .await
    .expect("seed");

    // Rename onto a name that exists is RENAMENX's 0 - and nothing is overwritten.
    let r = b
        .run_pipeline(&[vec!["RENAMENX".into(), "my key".into(), "taken key".into()]])
        .await
        .expect("renamenx");
    assert_eq!(r["replies"][0], json!(0), "the destination exists");
    assert_eq!(b.read_key("taken key").await.expect("read")["value"], json!("precious"));

    // EXPIRE and DEL, one argument each.
    let r = b
        .run_pipeline(&[vec!["EXPIRE".into(), "my key".into(), "600".into()]])
        .await
        .expect("expire");
    assert_eq!(r["replies"][0], json!(1));
    let r = b
        .run_pipeline(&[vec!["DEL".into(), "my key".into()]])
        .await
        .expect("del");
    assert_eq!(r["replies"][0], json!(1), "exactly one key went");
    assert_eq!(b.read_key("my key").await.expect("read")["type"], json!("none"));
    assert_eq!(b.read_key("my").await.expect("read")["value"], json!("neighbour 1"), "untouched");
    assert_eq!(b.read_key("key").await.expect("read")["value"], json!("neighbour 2"), "untouched");

    // A second DEL of the same key answers 0 - the panel says "already gone" on that.
    let r = b
        .run_pipeline(&[vec!["DEL".into(), "my key".into()]])
        .await
        .expect("del again");
    assert_eq!(r["replies"][0], json!(0));

    // The walk hands every one of these names back exactly - the empty one included - so the
    // tree gets the real bytes to lay out.
    let names = walk_matching(b.as_ref(), None).await;
    for n in ["my", "key", "taken key", ""] {
        assert!(names.contains(n), "{n:?} is walked back as itself: {names:?}");
    }
}

#[tokio::test]
async fn a_namespace_of_thousands_goes_in_chunked_dels_and_the_count_is_redis_own() {
    // 2026-09-29: "很多 key 都是同样的前缀的". 2,500 keys under one prefix, deleted the way the
    // panel sends a band: DEL commands of at most 1,000 keys in one pipeline. The replies add
    // up to what redis removed - and a key deleted by someone else first is not counted. The
    // fixture's own 3,000 bulk:keyNNNNN share the leading `bulk:` and must all survive.
    let (_f, b) = browser("r50_bulk").await;
    let keys: Vec<String> = (0..2500).map(|i| format!("bulk:session:{i}")).collect();
    let mut seed: Vec<Vec<String>> = Vec::new();
    for chunk in keys.chunks(500) {
        let mut cmd = vec!["MSET".to_string()];
        for k in chunk {
            cmd.push(k.clone());
            cmd.push("v".into());
        }
        seed.push(cmd);
    }
    b.run_pipeline(&seed).await.expect("seed 2,500 keys");
    // Someone else deletes one in the meantime.
    b.run_pipeline(&[vec!["DEL".into(), "bulk:session:7".into()]])
        .await
        .expect("the other client");

    let dels: Vec<Vec<String>> = keys
        .chunks(1000)
        .map(|c| std::iter::once("DEL".to_string()).chain(c.iter().cloned()).collect())
        .collect();
    assert_eq!(dels.len(), 3, "1,000 + 1,000 + 500");
    let r = b.run_pipeline(&dels).await.expect("the band's delete");
    let sum: i64 = r["replies"]
        .as_array()
        .expect("replies")
        .iter()
        .map(|x| x.as_i64().unwrap_or(0))
        .sum();
    assert_eq!(sum, 2499, "redis counts what IT removed: the one already gone is not claimed");
    let left = walk_matching(b.as_ref(), Some("bulk:session:*")).await;
    assert!(left.is_empty(), "nothing under the band survives: {} left", left.len());
    let siblings = walk_matching(b.as_ref(), Some("bulk:*")).await;
    assert_eq!(siblings.len(), 3000, "the sibling band under the same prefix is untouched");
    assert!(siblings.iter().all(|k| k.starts_with("bulk:key")), "only the seed's keys remain");
}

/// Every key the walk hands back, all pages, optionally under a MATCH pattern - SCAN's COUNT
/// is a hint, so one page of a 3,000-key fixture is never the whole answer.
async fn walk_matching(b: &dyn RedisBrowser, pattern: Option<&str>) -> BTreeSet<String> {
    let mut cursor = String::from("0");
    let mut seen = BTreeSet::new();
    loop {
        let mut q = json!({ "cursor": cursor, "count": 1000 });
        if let Some(p) = pattern {
            q["pattern"] = json!(p);
        }
        let page = b.list_keys(&q).await.expect("scan page");
        for k in page["keys"].as_array().expect("keys") {
            seen.insert(k["key"].as_str().expect("key text").to_string());
        }
        if page["done"] == json!(true) {
            return seen;
        }
        cursor = page["cursor"].as_str().expect("cursor text").to_string();
    }
}

#[tokio::test]
async fn a_word_leading_hash_reaches_redis_as_itself() {
    // 2026-09-29: the console's shlex read a word-leading `#` as a comment. `SET color #ff0000`
    // reached redis as `SET color` (wrong number of arguments), and `DEL k1 #k2 k3` deleted k1
    // alone and answered 1 - the other two keys silently kept. Against a real server now.
    let (_f, b) = browser("r50_hash").await;
    b.run_command("SET it:color #ff0000").await.expect("a # value");
    assert_eq!(b.read_key("it:color").await.expect("read")["value"], json!("#ff0000"));
    for k in ["k1", "#k2", "k3"] {
        b.run_command(&format!("SET it:{k} v")).await.expect("seed");
    }
    // `#k2` inside a key name is only a character, and so is one leading a word.
    b.run_command("SET #it:lead v").await.expect("a # key");
    let n = b
        .run_command("DEL it:k1 it:#k2 it:k3 #it:lead")
        .await
        .expect("the delete");
    assert_eq!(n, json!(4), "every word was a key");
}

#[tokio::test]
async fn a_key_name_that_is_not_utf8_is_walked_as_its_bytes_and_marked() {
    // 2026-09-29: Spring's JDK serializer writes key names that are not UTF-8. The walk read
    // SCAN's names as text (U+FFFD), asked TYPE of that other name and answered "none", and
    // the panel offered to open and delete a key that does not exist.
    let (f, b) = browser("r50_binary").await;
    let url = format!("{}/{}", engine(Kind::Redis).await.root_url, f.def["db"]);
    let client = redis::Client::open(url.as_str()).expect("open the leased db");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("connect the leased db");
    let name: &[u8] = b"\xac\xed\x00\x05t\x00\x04user";
    redis::cmd("SET")
        .arg(name)
        .arg("v")
        .arg("EX")
        .arg(600)
        .query_async::<()>(&mut conn)
        .await
        .expect("seed a name that is not UTF-8");
    let mut cursor = String::from("0");
    let mut marked = Vec::new();
    loop {
        let page = b
            .list_keys(&json!({ "cursor": cursor, "count": 1000 }))
            .await
            .expect("scan page");
        for k in page["keys"].as_array().expect("keys") {
            if k["binary"] == json!(true) {
                marked.push(k.clone());
            }
        }
        if page["done"] == json!(true) {
            break;
        }
        cursor = page["cursor"].as_str().expect("cursor text").to_string();
    }
    assert_eq!(marked.len(), 1, "exactly the one name is marked: {marked:?}");
    let row = &marked[0];
    assert_eq!(row["key"], json!(r#""\xac\xed\x00\x05t\x00\x04user""#), "printed as redis-cli prints it");
    assert_eq!(row["type"], json!("string"), "TYPE was asked of the real bytes: {row}");
    let ttl = row["ttl"].as_i64().expect("a ttl");
    assert!((1..=600).contains(&ttl), "and so was TTL: {row}");
}
