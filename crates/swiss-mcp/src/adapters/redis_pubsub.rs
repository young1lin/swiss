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

//! Redis pub/sub for the Data view (SPEC §data.pubsub). A channel is not a key - SCAN never sees
//! one, and the console refuses SUBSCRIBE, which would take the shared connection out of
//! service - so the view has two reads of its own: the channels that have a subscriber right now,
//! over the shared connection, and a live feed, each on a connection of its own.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use redis::aio::{PubSubSink, PubSubStream};
use serde_json::{json, Map, Value};
use swiss_host::dbbrowser::{PubSubFeed, PubSubWatch, PUBSUB_LIST_MAX};
use tokio::sync::mpsc;

use super::redis::{redis_cli_repr, value_i64, RedisHandle, CONNECT_TIMEOUT};

/// Live feeds one connection may hold at once, each a socket of its own on the server.
pub const SUBSCRIPTIONS_MAX: usize = 4;
/// A message's payload ships at most this many bytes; `size` still says how long it was.
pub const PAYLOAD_MAX: usize = 16 * 1024;
/// Messages a feed relays per second; the rest are counted, not sent (a `dropped` event).
pub const RATE_MAX: u32 = 200;
/// Events a feed buffers for a reader that has fallen behind before it counts them dropped.
const FEED_CAPACITY: usize = 256;
/// How often a quiet feed pings the server and tells the reader it is still alive.
pub const TICK: Duration = Duration::from_secs(15);
/// How long a SUBSCRIBE, a PING or the closing event may wait - the commandTimeout of the shared
/// connection.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// SPEC §data.pubsub: PUBSUB CHANNELS under `o.pattern` (default `*`), sorted by name and cut at
/// PUBSUB_LIST_MAX, each with its PUBSUB NUMSUB count, plus PUBSUB NUMPAT - the counts in one
/// pipeline. A channel heard only through a pattern subscription has no NUMSUB subscriber and
/// is not listed; NUMPAT is how many such subscriptions the server holds.
pub async fn list_channels(handle: &RedisHandle, o: &Value) -> Result<Value, String> {
    let pattern = o
        .get("pattern")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("*");
    let mut names: Vec<Vec<u8>> = match handle.call_raw("PUBSUB", &["CHANNELS", pattern]).await? {
        redis::Value::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                redis::Value::BulkString(b) => Some(b),
                redis::Value::SimpleString(s) => Some(s.into_bytes()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    names.sort();
    let total = names.len();
    names.truncate(PUBSUB_LIST_MAX);
    let mut numsub: Vec<&[u8]> = Vec::with_capacity(names.len() + 1);
    numsub.push(b"NUMSUB");
    numsub.extend(names.iter().map(Vec::as_slice));
    let numpat: Vec<&[u8]> = vec![b"NUMPAT"];
    let replies = handle
        .pipeline_args(&[("PUBSUB".to_string(), numsub), ("PUBSUB".to_string(), numpat)])
        .await?;
    // NUMSUB answers [name, count, name, count] in the order asked, so a channel's count is
    // at its own position - the names it echoes are the lossy JSON of bytes we already hold.
    let counts = replies.first().and_then(Value::as_array).cloned().unwrap_or_default();
    let channels: Vec<Value> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut row = Map::new();
            match std::str::from_utf8(name) {
                Ok(text) => row.insert("name".into(), json!(text)),
                Err(_) => {
                    row.insert("binary".into(), json!(true));
                    row.insert("name".into(), json!(redis_cli_repr(name)))
                }
            };
            let subscribers = counts.get(2 * i + 1).map(value_i64).unwrap_or(0);
            row.insert("subscribers".into(), json!(subscribers));
            Value::Object(row)
        })
        .collect();
    Ok(json!({
        "pattern": pattern,
        "channels": channels,
        "total": total,
        "truncated": total > PUBSUB_LIST_MAX,
        "patterns": replies.get(1).map(value_i64).unwrap_or(0),
    }))
}

/// One of the connection's SUBSCRIPTIONS_MAX feed places, given back when the feed ends - an
/// early return before its first event included.
struct SubscriptionSlot(Arc<AtomicUsize>);

impl SubscriptionSlot {
    fn take(count: &Arc<AtomicUsize>) -> Result<Self, String> {
        count
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < SUBSCRIPTIONS_MAX).then_some(n + 1)
            })
            .map(|_| Self(count.clone()))
            .map_err(|_| {
                format!(
                    "too many live subscriptions on this connection (max {SUBSCRIPTIONS_MAX}) - \
                     close a Pub/Sub view open in another window first"
                )
            })
    }
}

impl Drop for SubscriptionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// SPEC §data.pubsub: subscribe to `watch` on a connection of its own and hand back the feed, its
/// first event `subscribed` once the server confirmed every name. Everything before that - the
/// place, the connect, AUTH, a refused SUBSCRIBE - is the Err.
pub async fn open_feed(handle: &RedisHandle, watch: &PubSubWatch) -> Result<PubSubFeed, String> {
    open_feed_ticking(handle, watch, TICK).await
}

/// `open_feed` with its tick interval named, so a test need not wait 15 s for one.
pub(crate) async fn open_feed_ticking(
    handle: &RedisHandle,
    watch: &PubSubWatch,
    tick: Duration,
) -> Result<PubSubFeed, String> {
    let slot = SubscriptionSlot::take(&handle.subscriptions)?;
    let pubsub = tokio::time::timeout(CONNECT_TIMEOUT, handle.client.get_async_pubsub())
        .await
        .map_err(|_| "redis connect timed out after 5s".to_string())?
        .map_err(|e| e.to_string())?;
    let (mut sink, stream) = pubsub.split();
    tokio::time::timeout(REPLY_TIMEOUT, async {
        confirm(&sink, watch).await?;
        verify_subscribed(&mut sink).await
    })
    .await
    .map_err(|_| "SUBSCRIBE timed out after 10s".to_string())??;
    let (tx, rx) = mpsc::channel(FEED_CAPACITY);
    let _ = tx.try_send(json!({
        "event": "subscribed",
        "channels": watch.channels,
        "patterns": watch.patterns,
    }));
    tokio::spawn(pump(sink, stream, tx, tick, slot));
    Ok(rx)
}

/// One SUBSCRIBE or PSUBSCRIBE per name, all sent before the first reply is awaited. One name a
/// command keeps one confirmation a command: redis confirms a many-name SUBSCRIBE once per name,
/// and redis-rs pairs only the first with its request, so a second command sent while the
/// rest were still arriving would be handed one of them as its own answer.
async fn confirm(sink: &PubSubSink, watch: &PubSubWatch) -> Result<(), String> {
    let names = watch
        .channels
        .iter()
        .map(|c| (false, c.clone()))
        .chain(watch.patterns.iter().map(|p| (true, p.clone())));
    let calls = names.map(|(pattern, name)| {
        let mut sink = sink.clone();
        async move {
            if pattern {
                sink.psubscribe(name).await
            } else {
                sink.subscribe(name).await
            }
        }
    });
    futures_util::future::try_join_all(calls)
        .await
        .map(|_| ())
        .map_err(refusal)
}

/// The refusal sentence is the adapter's own (SPEC §data.pubsub). The newer redis-rs hands the
/// server's -ERR answer to SUBSCRIBE to the call itself; the older one read it as done and left
/// the PING check below to say it - whichever shape, the sentence is the same.
fn refusal(e: redis::RedisError) -> String {
    if matches!(e.kind(), redis::ErrorKind::Server(_)) {
        format!("the server refused SUBSCRIBE: {e}")
    } else {
        e.to_string()
    }
}

/// Older redis-rs reads an error reply to SUBSCRIBE as done, the same as the confirmation, so
/// the PING after the last one is the check: a connection subscribed to anything answers ["pong", ""],
/// one subscribed to nothing answers PONG. A name an ACL refuses beside names it allows is not
/// caught - that channel simply stays silent.
async fn verify_subscribed(sink: &mut PubSubSink) -> Result<(), String> {
    match sink.ping::<redis::Value>().await.map_err(|e| e.to_string())? {
        redis::Value::Array(items)
            if matches!(items.first(), Some(redis::Value::BulkString(b)) if b.eq_ignore_ascii_case(b"pong")) =>
        {
            Ok(())
        }
        _ => Err("the server refused SUBSCRIBE: the connection is subscribed to nothing \
                  (a proxy in front of redis without pub/sub, or an ACL denying these channels)"
            .into()),
    }
}

/// The feed's producer: relays every message the connection delivers until the reader goes away
/// or the connection ends, and says why in a last `closed` event. It drains redis-rs's
/// unbounded queue continuously - a message over RATE_MAX a second, or one the reader has no
/// room for, is counted and reported as `dropped` before the next one that is sent - and pings
/// the server every `tick`, so a dead tunnel ends the feed instead of leaving it silent.
async fn pump(
    mut sink: PubSubSink,
    mut stream: PubSubStream,
    tx: mpsc::Sender<Value>,
    tick: Duration,
    _slot: SubscriptionSlot,
) {
    let mut rate = RateWindow::new(RATE_MAX, Instant::now());
    let mut dropped: u64 = 0;
    let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + tick, tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let reason = loop {
        tokio::select! {
            // The reader went away: nobody is left to tell anything.
            _ = tx.closed() => return,
            msg = stream.next() => {
                let Some(msg) = msg else {
                    break "the server closed the connection".to_string();
                };
                if !rate.admit(Instant::now()) {
                    dropped += 1;
                    continue;
                }
                flush_dropped(&tx, &mut dropped);
                let channel = msg.get_channel::<Vec<u8>>().unwrap_or_default();
                let pattern = msg.get_pattern::<Option<Vec<u8>>>().unwrap_or_default();
                let event = message_event(
                    &channel,
                    pattern.as_deref(),
                    msg.get_payload_bytes(),
                    swiss_core::util::now_ms(),
                );
                match tx.try_send(event) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => dropped += 1,
                    Err(mpsc::error::TrySendError::Closed(_)) => return,
                }
            }
            _ = ticker.tick() => {
                match tokio::time::timeout(REPLY_TIMEOUT, sink.ping::<redis::Value>()).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => break format!("the connection failed: {e}"),
                    Err(_) => break "the server did not answer PING within 10s".to_string(),
                }
                flush_dropped(&tx, &mut dropped);
                let _ = tx.try_send(json!({ "event": "tick", "at": swiss_core::util::now_ms() }));
            }
        }
    };
    flush_dropped(&tx, &mut dropped);
    // A reader that stopped reading but still holds the feed gets the reason only if it reads
    // within the bound; the slot is given back either way.
    let _ = tokio::time::timeout(
        REPLY_TIMEOUT,
        tx.send(json!({ "event": "closed", "reason": reason })),
    )
    .await;
}

/// Tell the reader how many messages it missed since the last time it was told, if there is
/// room - otherwise the count keeps growing until there is.
fn flush_dropped(tx: &mpsc::Sender<Value>, dropped: &mut u64) {
    if *dropped > 0 && tx.try_send(json!({ "event": "dropped", "count": *dropped })).is_ok() {
        *dropped = 0;
    }
}

/// One delivered message as the event the panel shows: the channel (and the pattern that
/// matched it, for a PSUBSCRIBE), the payload as text, `size` its length in bytes, `at` when the
/// feed received it.
pub(crate) fn message_event(channel: &[u8], pattern: Option<&[u8]>, payload: &[u8], at: u64) -> Value {
    let mut ev = Map::new();
    ev.insert("event".into(), json!("message"));
    let (name, channel_binary, _) = shown(channel, PUBSUB_NAME_SHOWN);
    ev.insert("channel".into(), json!(name));
    if channel_binary {
        ev.insert("channelBinary".into(), json!(true));
    }
    if let Some(pattern) = pattern {
        ev.insert("pattern".into(), json!(String::from_utf8_lossy(pattern)));
    }
    let (text, binary, truncated) = shown(payload, PAYLOAD_MAX);
    ev.insert("payload".into(), json!(text));
    if binary {
        ev.insert("binary".into(), json!(true));
    }
    ev.insert("size".into(), json!(payload.len()));
    if truncated {
        ev.insert("truncated".into(), json!(true));
    }
    ev.insert("at".into(), json!(at));
    Value::Object(ev)
}

/// A channel name ships at most this many bytes - a pattern can match any name a publisher
/// chose, not only one a person typed.
const PUBSUB_NAME_SHOWN: usize = 1024;

/// Bytes as the text the panel shows, at most `cap` bytes of them: UTF-8 as itself, cut at a
/// character boundary; anything else as redis-cli prints it, from a quarter as many bytes,
/// since each may print as four characters. Returns the text, whether it is that printed form,
/// and whether it was cut.
fn shown(bytes: &[u8], cap: usize) -> (String, bool, bool) {
    let head = &bytes[..bytes.len().min(cap)];
    match std::str::from_utf8(head) {
        Ok(text) => (text.to_string(), false, bytes.len() > cap),
        // The cut fell inside a character: what comes before it is still text.
        Err(e) if bytes.len() > cap && e.error_len().is_none() => {
            let text = String::from_utf8_lossy(&head[..e.valid_up_to()]).into_owned();
            (text, false, true)
        }
        Err(_) => {
            let repr_cap = cap / 4;
            let printed = redis_cli_repr(&bytes[..bytes.len().min(repr_cap)]);
            (printed, true, bytes.len() > repr_cap)
        }
    }
}

/// At most `max` admissions per window of one second; the next window opens at the first call
/// a second or more after the last one opened.
pub(crate) struct RateWindow {
    max: u32,
    start: Instant,
    used: u32,
}

impl RateWindow {
    pub(crate) fn new(max: u32, now: Instant) -> Self {
        Self { max, start: now, used: 0 }
    }

    pub(crate) fn admit(&mut self, now: Instant) -> bool {
        if now.duration_since(self.start) >= Duration::from_secs(1) {
            self.start = now;
            self.used = 0;
        }
        if self.used < self.max {
            self.used += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::redis::tests::{bulk, fake_redis, handle_at, take_command};

    fn array(items: &[&[u8]]) -> Vec<u8> {
        let mut out = format!("*{}\r\n", items.len()).into_bytes();
        for item in items {
            out.extend_from_slice(format!("${}\r\n", item.len()).as_bytes());
            out.extend_from_slice(item);
            out.extend_from_slice(b"\r\n");
        }
        out
    }

    /// The confirmation redis sends for one SUBSCRIBE or PSUBSCRIBE name.
    fn confirmation(kind: &str, name: &str, count: i64) -> Vec<u8> {
        let mut out = b"*3\r\n".to_vec();
        out.extend_from_slice(&bulk(kind));
        out.extend_from_slice(&bulk(name));
        out.extend_from_slice(format!(":{count}\r\n").as_bytes());
        out
    }

    const PONG: &[u8] = b"*2\r\n$4\r\npong\r\n$0\r\n\r\n";

    async fn next_event(rx: &mut PubSubFeed) -> Value {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("an event within 5s")
            .expect("the feed is still open")
    }

    #[tokio::test]
    async fn the_channel_listing_pairs_each_name_with_its_numsub_count() {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = asked.clone();
        let port = fake_redis(Duration::ZERO, move |verb, args| match (verb, args.first().map(String::as_str)) {
            ("PUBSUB", Some("CHANNELS")) => {
                seen.lock().unwrap().push(args[1].clone());
                array(&[b"orders", b"\xac\xed\x00\x05", b"alerts"])
            }
            ("PUBSUB", Some("NUMSUB")) => {
                // Answered in the order asked; a name's count is fixed by its position here.
                let mut out = format!("*{}\r\n", 2 * (args.len() - 1)).into_bytes();
                for (i, name) in args[1..].iter().enumerate() {
                    out.extend_from_slice(&bulk(name));
                    out.extend_from_slice(format!(":{}\r\n", i + 1).as_bytes());
                }
                out
            }
            ("PUBSUB", Some("NUMPAT")) => b":28\r\n".to_vec(),
            _ => b"+OK\r\n".to_vec(),
        })
        .await;
        let handle = handle_at(port).await;
        let out = list_channels(&handle, &json!({})).await.unwrap();
        assert_eq!(
            out,
            json!({
                "pattern": "*",
                "channels": [
                    { "name": "alerts", "subscribers": 1 },
                    { "name": "orders", "subscribers": 2 },
                    { "binary": true, "name": "\"\\xac\\xed\\x00\\x05\"", "subscribers": 3 },
                ],
                "total": 3,
                "truncated": false,
                "patterns": 28,
            })
        );
        list_channels(&handle, &json!({ "pattern": " ord* " })).await.unwrap();
        assert_eq!(*asked.lock().unwrap(), ["*", "ord*"]);
    }

    #[tokio::test]
    async fn a_feed_relays_what_the_server_pushes_then_ticks() {
        let port = fake_redis(Duration::ZERO, |verb, args| match verb {
            "SUBSCRIBE" => {
                let mut out = confirmation("subscribe", &args[0], 1);
                out.extend(array(&[b"message", b"orders", b"{\"id\":1}"]));
                out.extend(array(&[b"message", b"orders", b"\xac\xed\x00\x05"]));
                out.extend(array(&[b"message", b"orders", "x".repeat(PAYLOAD_MAX + 10).as_bytes()]));
                out
            }
            "PSUBSCRIBE" => {
                let mut out = confirmation("psubscribe", &args[0], 2);
                out.extend(array(&[b"pmessage", b"order*", b"orders.eu", b"hi"]));
                out
            }
            "PING" => PONG.to_vec(),
            _ => b"+OK\r\n".to_vec(),
        })
        .await;
        let handle = handle_at(port).await;
        let watch = PubSubWatch { channels: vec!["orders".into()], patterns: vec!["order*".into()] };
        let mut rx = open_feed_ticking(&handle, &watch, Duration::from_millis(100)).await.unwrap();
        assert_eq!(
            next_event(&mut rx).await,
            json!({ "event": "subscribed", "channels": ["orders"], "patterns": ["order*"] })
        );
        let mut messages = Vec::new();
        for _ in 0..4 {
            let mut ev = next_event(&mut rx).await;
            assert!(ev["at"].as_u64().unwrap() > 0, "{ev}");
            ev.as_object_mut().unwrap().remove("at");
            messages.push(ev);
        }
        assert_eq!(messages[0], json!({ "event": "message", "channel": "orders", "payload": "{\"id\":1}", "size": 8 }));
        assert_eq!(
            messages[1],
            json!({ "event": "message", "channel": "orders", "payload": "\"\\xac\\xed\\x00\\x05\"", "binary": true, "size": 4 })
        );
        assert_eq!(messages[2]["payload"].as_str().unwrap().len(), PAYLOAD_MAX);
        assert_eq!(messages[2]["size"], json!(PAYLOAD_MAX + 10));
        assert_eq!(messages[2]["truncated"], json!(true));
        assert_eq!(
            messages[3],
            json!({ "event": "message", "channel": "orders.eu", "pattern": "order*", "payload": "hi", "size": 2 })
        );
        assert_eq!(next_event(&mut rx).await["event"], json!("tick"));
        assert_eq!(handle.subscriptions.load(Ordering::SeqCst), 1);
        drop(rx);
        let released = async {
            while handle.subscriptions.load(Ordering::SeqCst) != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(2), released)
            .await
            .expect("a dropped feed gives its place back");
    }

    #[tokio::test]
    async fn a_burst_past_the_rate_is_counted_not_relayed() {
        let port = fake_redis(Duration::ZERO, |verb, args| match verb {
            "SUBSCRIBE" => {
                let mut out = confirmation("subscribe", &args[0], 1);
                for i in 0..RATE_MAX + 50 {
                    out.extend(array(&[b"message", b"burst", i.to_string().as_bytes()]));
                }
                out
            }
            "PING" => PONG.to_vec(),
            _ => b"+OK\r\n".to_vec(),
        })
        .await;
        let handle = handle_at(port).await;
        let watch = PubSubWatch { channels: vec!["burst".into()], patterns: Vec::new() };
        let mut rx = open_feed_ticking(&handle, &watch, Duration::from_millis(300)).await.unwrap();
        assert_eq!(next_event(&mut rx).await["event"], json!("subscribed"));
        for i in 0..RATE_MAX {
            assert_eq!(next_event(&mut rx).await["payload"], json!(i.to_string()));
        }
        // The overflow is said at the next tick, before the tick itself.
        assert_eq!(next_event(&mut rx).await, json!({ "event": "dropped", "count": 50 }));
        assert_eq!(next_event(&mut rx).await["event"], json!("tick"));
    }

    #[tokio::test]
    async fn the_feed_ends_with_closed_when_the_server_hangs_up() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while let Ok(n) = sock.read(&mut chunk).await {
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some((verb, args, used)) = take_command(&buf) {
                            buf.drain(..used);
                            match verb.as_str() {
                                "SUBSCRIBE" => {
                                    let _ = sock.write_all(&confirmation("subscribe", &args[0], 1)).await;
                                }
                                "PING" => {
                                    let _ = sock.write_all(PONG).await;
                                    // Hang up once subscribed: the feed must end, not go quiet.
                                    return;
                                }
                                _ => {
                                    let _ = sock.write_all(b"+OK\r\n").await;
                                }
                            }
                        }
                    }
                });
            }
        });
        let handle = handle_at(port).await;
        let watch = PubSubWatch { channels: vec!["c".into()], patterns: Vec::new() };
        let mut rx = open_feed(&handle, &watch).await.unwrap();
        assert_eq!(next_event(&mut rx).await["event"], json!("subscribed"));
        assert_eq!(
            next_event(&mut rx).await,
            json!({ "event": "closed", "reason": "the server closed the connection" })
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn a_refused_subscribe_is_the_error_and_holds_no_place() {
        // A server that refuses SUBSCRIBE stays out of subscribed mode: its PING says PONG.
        let port = fake_redis(Duration::ZERO, |verb, _| match verb {
            "SUBSCRIBE" => b"-ERR unknown command 'SUBSCRIBE'\r\n".to_vec(),
            "PING" => b"+PONG\r\n".to_vec(),
            _ => b"+OK\r\n".to_vec(),
        })
        .await;
        let handle = handle_at(port).await;
        let watch = PubSubWatch { channels: vec!["c".into()], patterns: Vec::new() };
        let err = open_feed(&handle, &watch).await.unwrap_err();
        assert!(err.starts_with("the server refused SUBSCRIBE"), "{err}");
        assert_eq!(handle.subscriptions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_connection_holds_at_most_four_feeds() {
        let port = fake_redis(Duration::ZERO, |verb, args| match verb {
            "SUBSCRIBE" => confirmation("subscribe", &args[0], 1),
            "PING" => PONG.to_vec(),
            _ => b"+OK\r\n".to_vec(),
        })
        .await;
        let handle = handle_at(port).await;
        let watch = PubSubWatch { channels: vec!["c".into()], patterns: Vec::new() };
        let mut feeds = Vec::new();
        for _ in 0..SUBSCRIPTIONS_MAX {
            feeds.push(open_feed(&handle, &watch).await.unwrap());
        }
        let err = open_feed(&handle, &watch).await.unwrap_err();
        assert!(err.contains("too many live subscriptions on this connection (max 4)"), "{err}");
        drop(feeds.pop());
        let reopened = async {
            loop {
                if let Ok(feed) = open_feed(&handle, &watch).await {
                    return feed;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(2), reopened)
            .await
            .expect("a closed feed's place is free again");
    }

    #[test]
    fn a_payload_is_shown_as_text_cut_at_a_character_or_as_redis_cli_prints_it() {
        assert_eq!(shown(b"hello", 16), ("hello".into(), false, false));
        // "é" is two bytes: a cut at 3 falls inside the second one and keeps the first.
        assert_eq!(shown("éé".as_bytes(), 3), ("é".into(), false, true));
        assert_eq!(shown(b"\xff\x00ab", 16), ("\"\\xff\\x00ab\"".into(), true, false));
        assert_eq!(shown(b"\xff\x00abcdef", 16), ("\"\\xff\\x00ab\"".into(), true, true));
        let ev = message_event(b"\xfe", None, b"", 7);
        assert_eq!(ev, json!({ "event": "message", "channel": "\"\\xfe\"", "channelBinary": true, "payload": "", "size": 0, "at": 7 }));
    }

    #[test]
    fn the_rate_window_admits_its_max_then_waits_for_the_next_second() {
        let t0 = Instant::now();
        let mut rate = RateWindow::new(3, t0);
        assert!((0..3).all(|_| rate.admit(t0)));
        assert!(!rate.admit(t0 + Duration::from_millis(999)));
        assert!(rate.admit(t0 + Duration::from_secs(1)));
        assert!(rate.admit(t0 + Duration::from_millis(1500)));
    }
}
