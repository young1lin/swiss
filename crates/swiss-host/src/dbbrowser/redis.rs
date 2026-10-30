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

//! Redis: stream windows and filters (SPEC §data.streams), console word splitting, pipeline
//! bodies and the pub/sub watch set (SPEC §data.pubsub).

use serde_json::Value;

/// The three window shapes one XREVRANGE serves (SPEC §data.streams). `Newest` is the
/// opening page; `Before` pages strictly older than a cursor (“load earlier”);
/// `After` catches up strictly newer than one (the Follow tick). The exclusive
/// bounds need redis >= 6.2 — older servers answer “ERR syntax”, which passes
/// through with the key named; the seed engines are 7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamBound {
    Newest,
    Before(String),
    After(String),
}

/// Default and ceiling of a stream window's `count` (SPEC §data.streams): 100 is a
/// screenful, and 1000 matches `type_aware_read`'s existing cap — 1,000 entries
/// × ~20 fields is roughly 200 KB of JSON, the line where one fetch stops being
/// instant in a browser.
pub const STREAM_WINDOW_DEFAULT: i64 = 100;
pub const STREAM_WINDOW_MAX: i64 = 1000;

/// SPEC §data.streams: clamp of the `count` option. Absent means the default (100);
/// anything above the ceiling clamps DOWN; but zero, negatives and non-numeric
/// values are the caller's mistake and refuse — a silent default would page
/// somewhere the caller never asked. Numeric strings count (the panel sends
/// every query param as a string, exactly like SCAN's count).
pub fn clamp_count(v: Option<&Value>) -> Result<i64, String> {
    let bad = |what: &str| format!("count must be a positive number — got {what}");
    let Some(v) = v else { return Ok(STREAM_WINDOW_DEFAULT) };
    let n = match v {
        Value::Number(n) => n.as_i64().ok_or_else(|| bad(&n.to_string()))?,
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| bad(s.trim()))?,
        other => return Err(bad(&other.to_string())),
    };
    if n <= 0 {
        return Err(bad(&n.to_string()));
    }
    Ok(n.min(STREAM_WINDOW_MAX))
}

/// One term of a stream filter (SPEC §data.streams). `symbol=NVDA` is an exact field
/// match, `data~"last": 1` is a substring of ONE field, and a bare word is a
/// substring of anything the row carries — its id or any of its values. Case is
/// ignored on the value side of all three: a feed writes `NVDA`, an operator
/// types `nvda`, and refusing that is pedantry rather than precision. The FIELD
/// NAME is compared exactly, because it is wire data the stream chose, not copy
/// anyone types twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamTerm {
    Field { name: String, value: String },
    FieldContains { name: String, value: String },
    Text(String),
}

/// A parsed stream filter (SPEC §data.streams): every term must match, so
/// `symbol=NVDA seq~99` narrows twice. AND and not OR because narrowing is what
/// a reader of a fast feed is doing — 90 entries a second is already the OR of
/// everything, and the one move that helps is taking things away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFilter {
    pub terms: Vec<StreamTerm>,
}

/// The value side of one entry field as text: a stream's values are strings on
/// the wire, but a shaped entry can carry a number or null, and a filter that
/// silently skipped those would answer “no match” about a row that matches.
fn stream_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

impl StreamFilter {
    /// True when every term matches one shaped entry — `{ id, ts, fields }`, the
    /// same object the window answers with, so the host filters exactly what the
    /// panel would have seen.
    pub fn matches(&self, entry: &Value) -> bool {
        let fields = entry.get("fields").and_then(Value::as_object);
        let field = |name: &str| -> Option<String> { fields?.get(name).map(stream_text) };
        self.terms.iter().all(|term| match term {
            StreamTerm::Field { name, value } => field(name)
                .map(|v| v.to_lowercase() == *value)
                .unwrap_or(false),
            StreamTerm::FieldContains { name, value } => field(name)
                .map(|v| v.to_lowercase().contains(value.as_str()))
                .unwrap_or(false),
            StreamTerm::Text(needle) => {
                // The id and the timestamp are where a bare word looks first: an
                // operator pastes an id out of a log, or types the minute they are
                // hunting for, and neither is a field of the entry.
                if ["id", "ts"].iter().any(|k| {
                    entry
                        .get(*k)
                        .map(|v| stream_text(v).to_lowercase().contains(needle.as_str()))
                        .unwrap_or(false)
                }) {
                    return true;
                }
                fields
                    .map(|f| {
                        f.values()
                            .any(|v| stream_text(v).to_lowercase().contains(needle.as_str()))
                    })
                    .unwrap_or(false)
            }
        })
    }
}

/// A console or filter line into its words, the way a shell quotes them - through shlex, less
/// the one shell-ism a data console must not have. shlex 2 reads a `#` that STARTS a word as a
/// comment and drops the rest of the line, so `SET color #ff0000` reached redis as `SET color`
/// and `DEL a #b c` deleted `a` alone and answered 1 (2026-09-29). redis-cli, the panel's own
/// splitter and the operator all read that `#` as a character. `#` means nothing else to shlex,
/// so every `#` rides through the split as 0xFF - a byte UTF-8 never contains, which shlex
/// keeps as an ordinary character - and comes back as itself. None is an unbalanced quote or a
/// trailing backslash, exactly as shlex says it.
pub fn split_words(line: &str) -> Option<Vec<String>> {
    let carried: Vec<u8> = line
        .bytes()
        .map(|b| if b == b'#' { 0xFF } else { b })
        .collect();
    shlex::bytes::split(&carried)?
        .into_iter()
        .map(|w| {
            String::from_utf8(w.into_iter().map(|b| if b == 0xFF { b'#' } else { b }).collect()).ok()
        })
        .collect()
}

/// SPEC §data.streams: the `match` option into a filter. Splitting is shlex's job, not
/// a hand-rolled whitespace split — `data~"last": 12` is one term with a space
/// in it, and the redis console already learned this lesson the hard way
/// (2026-09-28). An unbalanced quote is the caller's mistake and says so; an
/// empty or blank filter is no filter at all, which is how the panel clears it.
pub fn parse_stream_filter(line: &str) -> Result<Option<StreamFilter>, String> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let tokens = split_words(line).ok_or_else(|| {
        "unbalanced quote in the filter — close it, or write a literal quote as \\\" inside the term"
            .to_string()
    })?;
    let mut terms: Vec<StreamTerm> = Vec::new();
    for token in tokens {
        if token.is_empty() {
            continue;
        }
        // The separator is the FIRST = or ~ in the token, and one at position 0
        // names no field: `=42` is a reader typing a value, and reading it as a
        // term over the empty field name would silently match nothing forever.
        let cut = token
            .char_indices()
            .find(|(i, c)| *i > 0 && (*c == '=' || *c == '~'));
        match cut {
            Some((i, sep)) => {
                let name = token[..i].to_string();
                let value = token[i + sep.len_utf8()..].to_lowercase();
                terms.push(if sep == '=' {
                    StreamTerm::Field { name, value }
                } else {
                    StreamTerm::FieldContains { name, value }
                });
            }
            None => terms.push(StreamTerm::Text(token.to_lowercase())),
        }
    }
    if terms.is_empty() {
        return Ok(None);
    }
    Ok(Some(StreamFilter { terms }))
}

/// How far back a filtered window may walk (SPEC §data.streams). Redis has no index
/// over a stream's fields: the only honest answer to “show me NVDA” is to read
/// entries backwards and keep the ones that match, so the walk is bounded and
/// the answer REPORTS its bound — `scanned` entries examined, `scannedTo` the
/// oldest id reached — rather than pretending the stream ends where the budget
/// did. 500 is one XREVRANGE page (the same order as the panel's row cap) and
/// 20,000 entries is ~3 MB of stream, a few hundred milliseconds on loopback.
pub const STREAM_SCAN_PAGE: i64 = 500;
pub const STREAM_SCAN_BUDGET: i64 = 20_000;

/// One validated stream request (SPEC §data.streams): which window, how
/// many entries, and — since SPEC §data.streams — what to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamQuery {
    pub bound: StreamBound,
    pub count: i64,
    pub filter: Option<StreamFilter>,
}
/// SPEC §data.streams: one option object into one bound + count. This is the single
/// validation both the route (whose stub tests pin the 400s without a redis) and
/// the real browser impl run — trait callers cannot bypass it and the rule
/// cannot drift between transport and model. `before` and `after` together are
/// refused outright: the two cursors page in OPPOSITE directions, and silently
/// picking one would answer a question the caller did not ask.
pub fn redis_stream_opts(o: &Value) -> Result<StreamQuery, String> {
    let cursor = |name: &str| -> Result<Option<String>, String> {
        match o.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => {
                let t = s.trim();
                if t.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(t.to_string()))
                }
            }
            Some(other) => Err(format!("{name} must be a stream entry id — got {other}")),
        }
    };
    let before = cursor("before")?;
    let after = cursor("after")?;
    if before.is_some() && after.is_some() {
        return Err("pass only one of before / after — they page in opposite directions".into());
    }
    let count = clamp_count(o.get("count"))?;
    let bound = match (before, after) {
        (Some(id), _) => StreamBound::Before(id),
        (None, Some(id)) => StreamBound::After(id),
        (None, None) => StreamBound::Newest,
    };
    let filter = match o.get("match") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => parse_stream_filter(s)?,
        Some(other) => return Err(format!("match must be a filter line — got {other}")),
    };
    Ok(StreamQuery {
        bound,
        count,
        filter,
    })
}
/// Cap on one buffered redis commit (SPEC §data.redis): one pipeline is one bounded round trip,
/// the redis twin of the edit route's MAX_EDITS.
pub const REDIS_PIPELINE_MAX: usize = 1000;

/// The body of POST /api/db/{name}/redis-pipeline — {commands: [[verb, arg...], ...]} — into
/// the typed command list run_pipeline takes. Shape only: WHICH commands may run stays with
/// the adapter's guard, exactly as the console's single command does, so the rule cannot drift
/// between transport and model. Args stay separate strings on purpose — a hash value with
/// spaces is one argument, not five.
pub fn redis_pipeline_commands(body: &Value) -> Result<Vec<Vec<String>>, String> {
    let bad_shape = || "commands must be a non-empty array of redis commands".to_string();
    let list = body
        .get("commands")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(bad_shape)?;
    if list.len() > REDIS_PIPELINE_MAX {
        return Err(format!(
            "too many commands in one pipeline (max {REDIS_PIPELINE_MAX})"
        ));
    }
    list.iter()
        .map(|cmd| {
            let parts = cmd
                .as_array()
                .filter(|a| !a.is_empty())
                .ok_or_else(|| "each command must be an array: [verb, arg...]".to_string())?;
            parts
                .iter()
                .map(|p| match p {
                    Value::String(s) => Ok(s.clone()),
                    // Scores and list indexes cross the wire as JSON numbers — the panel's
                    // typed editors emit the wire form redis itself takes (ZADD's score, LSET's
                    // index). Every redis argument is a bulk string on the socket, so the number
                    // renders as its JSON text: 9.75 stays "9.75", 9 stays "9".
                    Value::Number(n) => Ok(n.to_string()),
                    _ => Err("each command must be an array: [verb, arg...]".to_string()),
                })
                .collect()
        })
        .collect()
}

/// SPEC §data.pubsub: the caps of one subscription's watch set and of one channel listing. 32 names a
/// side keeps the set something a person typed rather than a dump; 512 bytes is longer than any
/// channel name an application uses; 1000 channels is the key list's own ceiling per page.
pub const PUBSUB_NAMES_MAX: usize = 32;
pub const PUBSUB_NAME_BYTES_MAX: usize = 512;
pub const PUBSUB_LIST_MAX: usize = 1000;

/// What one live subscription listens to (SPEC §data.pubsub): exact channels (SUBSCRIBE) and glob
/// patterns (PSUBSCRIBE).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PubSubWatch {
    pub channels: Vec<String>,
    pub patterns: Vec<String>,
}

/// One live subscription's events in arrival order (SPEC §data.pubsub), each a JSON object whose
/// `event` is subscribed, message, dropped, tick or closed. The producer stops as soon as this
/// receiver is dropped: the reader went away, and nobody else is listening.
pub type PubSubFeed = tokio::sync::mpsc::Receiver<Value>;

/// SPEC §data.pubsub: the subscribe body - { channels?: [name], patterns?: [glob] } - into a watch
/// set, before any connection is opened. A name is taken exactly as sent (a channel may hold a
/// space); an empty one, an oversized one, a non-string or an empty set is the caller's mistake
/// and says which. A repeated name rides once - redis would count it once anyway.
pub fn pubsub_watch(body: &Value) -> Result<PubSubWatch, String> {
    let side = |field: &str| -> Result<Vec<String>, String> {
        let list = match body.get(field) {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(Value::Array(list)) => list,
            Some(other) => return Err(format!("{field} must be an array of names — got {other}")),
        };
        let mut names: Vec<String> = Vec::new();
        for v in list {
            let name = v
                .as_str()
                .ok_or_else(|| format!("{field} must be an array of names — got {v}"))?;
            if name.is_empty() {
                return Err(format!("{field} holds an empty name"));
            }
            if name.len() > PUBSUB_NAME_BYTES_MAX {
                return Err(format!(
                    "a name in {field} is longer than {PUBSUB_NAME_BYTES_MAX} bytes"
                ));
            }
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
        if names.len() > PUBSUB_NAMES_MAX {
            return Err(format!("too many {field} (max {PUBSUB_NAMES_MAX})"));
        }
        Ok(names)
    };
    let watch = PubSubWatch {
        channels: side("channels")?,
        patterns: side("patterns")?,
    };
    if watch.channels.is_empty() && watch.patterns.is_empty() {
        return Err("name at least one channel or pattern to subscribe to".into());
    }
    Ok(watch)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::dbbrowser::paging::{
        browse_offset, browse_page_size, BROWSE_DEFAULT_PAGE, BROWSE_PAGE_SIZES,
    };
    use crate::dbbrowser::RedisBrowser;

    #[test]
    fn stream_count_clamps_and_refuses() {
        // SPEC §data.streams: absent -> the 100-entry screenful; over the ceiling clamps
        // down; zero / negative / non-numeric are the caller's mistake, not a default.
        assert_eq!(clamp_count(None).unwrap(), 100);
        assert_eq!(clamp_count(Some(&json!(1000))).unwrap(), 1000);
        assert_eq!(clamp_count(Some(&json!(1001))).unwrap(), 1000);
        assert_eq!(clamp_count(Some(&json!("500"))).unwrap(), 500);
        assert!(clamp_count(Some(&json!(0))).unwrap_err().contains("positive number"));
        assert!(clamp_count(Some(&json!(-3))).unwrap_err().contains("positive number"));
        assert!(clamp_count(Some(&json!("abc"))).unwrap_err().contains("positive number"));
        assert!(clamp_count(Some(&json!(1.5))).unwrap_err().contains("positive number"));
    }

    #[test]
fn stream_opts_pick_one_direction() {
    // SPEC §data.streams: before and after page in opposite directions — both at
    // once is refused rather than silently resolved; neither is the opening page.
    let q = |v: Value| redis_stream_opts(&v).unwrap();
    assert_eq!(q(json!({})).bound, StreamBound::Newest);
    assert_eq!(q(json!({})).count, 100);
    assert_eq!(q(json!({})).filter, None);
    let after = q(json!({ "after": "5-0", "count": "40" }));
    assert_eq!(after.bound, StreamBound::After("5-0".into()));
    assert_eq!(after.count, 40);
    let before = q(json!({ "before": "5-0" }));
    assert_eq!(before.bound, StreamBound::Before("5-0".into()));
    assert_eq!(before.count, 100);
    let both = redis_stream_opts(&json!({ "before": "1-0", "after": "2-0" })).unwrap_err();
    assert!(both.contains("opposite directions"), "{both}");
    assert!(redis_stream_opts(&json!({ "before": 7 }))
        .unwrap_err()
        .contains("entry id"));
    // SPEC §data.streams: the filter rides the same option object, and a filter that
    // is not a line at all is the caller's mistake — not a silently dropped one.
    assert_eq!(
        q(json!({ "match": "symbol=NVDA" })).filter,
        Some(StreamFilter {
            terms: vec![StreamTerm::Field {
                name: "symbol".into(),
                value: "nvda".into()
            }],
        })
    );
    assert_eq!(q(json!({ "match": "   " })).filter, None);
    let bad = redis_stream_opts(&json!({ "match": 7 })).unwrap_err();
    assert!(bad.contains("filter line"), "{bad}");
}

#[test]
fn a_filter_line_splits_into_terms_the_way_a_shell_would() {
    // SPEC §data.streams: three forms — exact field, field substring, and a bare word
    // that looks everywhere. Quoting is shlex's, so a term may carry spaces, and
    // an unbalanced quote is refused instead of being split down the middle.
    let f = parse_stream_filter("symbol=NVDA").unwrap().unwrap();
    assert_eq!(
        f.terms,
        vec![StreamTerm::Field {
            name: "symbol".into(),
            value: "nvda".into()
        }]
    );
    let two = parse_stream_filter("symbol=NVDA seq~99").unwrap().unwrap();
    assert_eq!(
        two.terms,
        vec![
            StreamTerm::Field {
                name: "symbol".into(),
                value: "nvda".into()
            },
            StreamTerm::FieldContains {
                name: "seq".into(),
                value: "99".into()
            },
        ]
    );
    // Quoting is what makes a term with a space in it ONE term — the whole term
    // is quoted, exactly as a shell would take it, and the quotes come off.
    let quoted = parse_stream_filter("data~'\"last\": 12'").unwrap().unwrap();
    assert_eq!(
        quoted.terms,
        vec![StreamTerm::FieldContains {
            name: "data".into(),
            value: "\"last\": 12".into(),
        }]
    );
    let bare = parse_stream_filter("\"600519.SH\"").unwrap().unwrap();
    assert_eq!(bare.terms, vec![StreamTerm::Text("600519.sh".into())]);
    // A separator at position 0 names no field: it is a value someone typed, and
    // a term over the empty field name would match nothing for ever, silently.
    assert_eq!(
        parse_stream_filter("=42").unwrap().unwrap().terms,
        vec![StreamTerm::Text("=42".into())]
    );
    assert_eq!(parse_stream_filter("").unwrap(), None);
    assert_eq!(parse_stream_filter("  \t ").unwrap(), None);
    let unbalanced = parse_stream_filter("symbol=\"NVDA").unwrap_err();
    assert!(unbalanced.contains("unbalanced quote"), "{unbalanced}");
}
    #[test]
    fn a_filter_keeps_the_rows_that_match_every_term() {
        // SPEC §data.streams: AND across terms, case ignored on the value side, and a row
        // missing the field never matches a term about it (rather than matching the
        // empty string). The bare word looks at the id too — that is how an operator
        // pastes an entry id from a log back into the filter box.
        let row = json!({
            "id": "1759100000000-0",
            "ts": "2026-09-29T00:40:00.000Z",
            "fields": { "symbol": "NVDA", "price": "121.90", "seq": 4499 },
        });
        let m = |line: &str| parse_stream_filter(line).unwrap().unwrap().matches(&row);
        assert!(m("symbol=NVDA"));
        assert!(m("symbol=nvda"), "case is ignored on the value side");
        assert!(!m("symbol=NVD"), "= is exact, not a prefix");
        assert!(m("symbol~vd"), "~ is the substring form");
        assert!(m("nvda"), "a bare word looks at every value");
        assert!(m("4499"), "a numeric value is text to the filter");
        assert!(m("1759100000000"), "and the id is one of the places it looks");
        assert!(m("00:40"), "so is the timestamp — the minute someone is hunting for");
        assert!(m("symbol=NVDA price~121"), "every term must match");
        assert!(!m("symbol=NVDA price~999"));
        assert!(!m("side=buy"), "a field this row has not got matches nothing");
        assert!(!m("ETH"));
    }

    #[tokio::test]
    async fn stream_trait_defaults_refuse_honestly() {
        // SPEC §data.databases's list_databases precedent, held for SPEC §data.streams: a flavor that
        // never heard of streams must keep compiling and answer “not supported”,
        // never a panic and never a silent empty window.
        struct Bare;
        #[async_trait::async_trait]
        impl RedisBrowser for Bare {
            fn label(&self) -> String {
                "bare".into()
            }
            async fn list_keys(&self, _o: &Value) -> Result<Value, String> {
                Ok(json!(null))
            }
            async fn read_key(&self, _key: &str) -> Result<Value, String> {
                Ok(json!(null))
            }
            async fn run_command(&self, _line: &str) -> Result<Value, String> {
                Ok(json!(null))
            }
            async fn run_pipeline(&self, _commands: &[Vec<String>]) -> Result<Value, String> {
                Ok(json!(null))
            }
        }
        let b = Bare;
        assert_eq!(
            b.read_stream("k", &json!({})).await.unwrap_err(),
            "stream windows are not supported by this connection"
        );
        assert_eq!(
            b.stream_groups("k").await.unwrap_err(),
            "stream groups are not supported by this connection"
        );
        // SPEC §data.pubsub: pub/sub is defaulted the same way.
        assert_eq!(
            b.pubsub_channels(&json!({})).await.unwrap_err(),
            "pub/sub is not supported by this connection"
        );
        let watch = PubSubWatch { channels: vec!["c".into()], patterns: Vec::new() };
        assert_eq!(
            b.subscribe(&watch).await.unwrap_err(),
            "pub/sub is not supported by this connection"
        );
    }

    #[test]
    fn a_watch_set_is_names_on_two_sides_checked_before_any_socket() {
        // SPEC §data.pubsub: channels and patterns, each optional, together never empty; a name rides
        // exactly as sent (spaces and all) and a repeat rides once.
        let w = pubsub_watch(&json!({ "channels": ["news", "a b", "news"], "patterns": ["q.*"] })).unwrap();
        assert_eq!(w.channels, vec!["news".to_string(), "a b".to_string()]);
        assert_eq!(w.patterns, vec!["q.*".to_string()]);
        let only = pubsub_watch(&json!({ "patterns": ["*"] })).unwrap();
        assert!(only.channels.is_empty());
        assert_eq!(only.patterns, vec!["*".to_string()]);
        for (bad, says) in [
            (json!({}), "at least one"),
            (json!({ "channels": [], "patterns": null }), "at least one"),
            (json!({ "channels": "news" }), "must be an array"),
            (json!({ "channels": [7] }), "must be an array"),
            (json!({ "patterns": [""] }), "empty name"),
            (json!({ "channels": ["x".repeat(PUBSUB_NAME_BYTES_MAX + 1)] }), "longer than"),
        ] {
            let err = pubsub_watch(&bad).unwrap_err();
            assert!(err.contains(says), "{bad} -> {err}");
        }
        let many: Vec<String> = (0..=PUBSUB_NAMES_MAX).map(|i| format!("c{i}")).collect();
        let err = pubsub_watch(&json!({ "channels": many })).unwrap_err();
        assert!(err.contains("too many channels"), "{err}");
        let at_cap: Vec<String> = (0..PUBSUB_NAMES_MAX).map(|i| format!("p{i}*")).collect();
        assert_eq!(pubsub_watch(&json!({ "patterns": at_cap })).unwrap().patterns.len(), PUBSUB_NAMES_MAX);
    }
    #[test]
    fn browse_paging_clamps() {
        assert_eq!(browse_page_size(None, BROWSE_DEFAULT_PAGE), 50);
        assert_eq!(
            browse_page_size(Some(&json!("nonsense")), BROWSE_DEFAULT_PAGE),
            50
        );
        assert_eq!(browse_page_size(Some(&json!(0)), BROWSE_DEFAULT_PAGE), 50);
        assert_eq!(browse_page_size(Some(&json!(-5)), BROWSE_DEFAULT_PAGE), 50);
        assert_eq!(
            browse_page_size(Some(&json!(100_000)), BROWSE_DEFAULT_PAGE),
            500
        );
        assert_eq!(browse_page_size(Some(&json!(10)), BROWSE_DEFAULT_PAGE), 10);
        assert_eq!(BROWSE_PAGE_SIZES, [10, 20, 50, 100, 200, 500]);
        assert_eq!(browse_offset(None), 0);
        assert_eq!(browse_offset(Some(&json!(-3))), 0);
        assert_eq!(browse_offset(Some(&json!("40"))), 40);
    }

    #[test]
    fn a_redis_pipeline_body_parses_into_separate_string_args() {
        // SPEC §data.redis: the panel's typed editors post [verb, arg...] arrays — a hash value
        // with spaces is ONE argument, never re-split, because the pipeline binds args as-is.
        let cmds = redis_pipeline_commands(&json!({
            "commands": [["HSET", "h:1", "a field", "two words"], ["ZADD", "z", 9.75, "m"], ["LSET", "l", 0, "v"]]
        }))
        .expect("valid body");
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0], vec!["HSET", "h:1", "a field", "two words"]);
        // A score and a list index arrive as JSON numbers and render as their exact text.
        assert_eq!(cmds[1], vec!["ZADD", "z", "9.75", "m"]);
        assert_eq!(cmds[2], vec!["LSET", "l", "0", "v"]);
    }

    #[test]
    fn a_redis_pipeline_body_with_a_bad_shape_is_refused_by_name() {
        // Absent, empty, non-array, an empty inner command, an argument that is neither
        // string nor number: each is the caller's whole mistake and answers the message the
        // panel can act on. A bare number IS an argument (a score, an index) and stays valid.
        for bad in [
            json!({}),
            json!({ "commands": [] }),
            json!({ "commands": "HSET k v" }),
            json!({ "commands": [[]] }),
            json!({ "commands": [["HSET", true]] }),
            json!({ "commands": [["HSET", null]] }),
            json!({ "commands": ["HSET"] }),
        ] {
            let err = redis_pipeline_commands(&bad).expect_err("refused");
            assert!(
                err.starts_with("commands must be") || err.starts_with("each command must be"),
                "{bad} -> {err}"
            );
        }
    }

    #[test]
    fn a_redis_pipeline_is_capped_like_an_edit_batch() {
        let one = json!(["HSET", "k", "f", "v"]);
        let cmds = vec![one.clone(); REDIS_PIPELINE_MAX];
        assert!(redis_pipeline_commands(&json!({ "commands": cmds })).is_ok());
        let cmds = vec![one; REDIS_PIPELINE_MAX + 1];
        let err = redis_pipeline_commands(&json!({ "commands": cmds })).expect_err("refused");
        assert!(err.contains("too many commands"), "{err}");
    }

    #[test]
    fn a_word_leading_hash_is_a_character_not_a_comment() {
        // 2026-09-29: shlex 2 read `#k2` as a comment, so `DEL k1 #k2 k3` deleted k1 alone and
        // `SET color #ff0000` reached redis one argument short.
        assert_eq!(
            split_words("DEL k1 #k2 k3"),
            Some(vec!["DEL".into(), "k1".into(), "#k2".into(), "k3".into()])
        );
        assert_eq!(shlex::split("DEL k1 #k2 k3"), Some(vec!["DEL".into(), "k1".into()]), "the crate's own rule");
        // The stream filter splits the same way: a term may start with '#'.
        let f = parse_stream_filter("#tag=1 x").unwrap().unwrap();
        assert_eq!(f.terms.len(), 2, "{f:?}");
    }

    #[test]
    fn the_console_splits_as_the_shared_corpus_says() {
        // tests/fixtures/console-split.json is read by the panel's suite too: the words the
        // server hands redis, and the names the console's completion types for the operator.
        let corpus: Value =
            serde_json::from_str(include_str!("../../../../tests/fixtures/console-split.json")).expect("corpus");
        let lines = corpus["lines"].as_array().expect("lines");
        assert!(lines.len() >= 30, "the corpus is the edge list, not a sample");
        for case in lines {
            let line = case["line"].as_str().expect("line");
            let want: Option<Vec<String>> = serde_json::from_value(case["words"].clone()).expect("words");
            assert_eq!(split_words(line), want, "{line:?}");
        }
        for case in corpus["keys"].as_array().expect("keys") {
            let key = case["key"].as_str().expect("key");
            let typed = case["typed"].as_str().expect("typed");
            assert_eq!(
                split_words(&format!("GET {typed}")),
                Some(vec!["GET".to_string(), key.to_string()]),
                "{typed} must reach redis as {key:?}"
            );
        }
    }
}
