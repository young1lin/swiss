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

//! The bus, through channels. No test here listens on a port (SPEC §bus.link).

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prost::Message;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{self, error::TryRecvError};

use crate::link::{self, Io, IoMsg};
use crate::pb::v1::{
    self, BusSub, ByeReason, ConsolesId, DataSub, Frame, Kind, LinkId, PanelSub, PrefsId, SaveMode, TopicId,
};
use crate::{
    Answer, Bus, Call, Handler, BAD_BODY, HEARTBEAT_MS, HELLO_WAIT, IDLE_LIMIT, LIMIT, MAX_FRAME, MAX_LINKS,
    MAX_QUEUE_BYTES, MAX_QUEUE_FRAMES, MAX_TOPICS, NOT_RELAYABLE, PROTOCOL, PROTOCOL_PING, REGENERATE,
    STAMP_INTERVAL, UNAVAILABLE, UNKNOWN,
};

const PING_HEX: &str = "1001180220012807420708c0ab93af9a34";
const PONG_HEX: &str = "10011802200228093007420e08c0ab93af9a3410c3ab93af9a34";
const PING_SENT: u64 = 1_793_784_600_000;

fn stamp(text: &str) -> crate::Stamp {
    let text = text.to_owned();
    Arc::new(move || text.clone())
}

fn bus() -> Arc<Bus> {
    Bus::new(stamp("stamp-a"))
}

struct End {
    tx: mpsc::Sender<IoMsg>,
    rx: mpsc::Receiver<IoMsg>,
    seq: u32,
}

struct ChanIo {
    inbound: mpsc::Receiver<IoMsg>,
    outbound: mpsc::Sender<IoMsg>,
}

impl Io for ChanIo {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<IoMsg>> + Send + '_>> {
        Box::pin(async move { self.inbound.recv().await })
    }
    fn send(&mut self, msg: IoMsg) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        Box::pin(async move { self.outbound.send(msg).await.is_ok() })
    }
}

fn pair(cap: usize) -> (ChanIo, End) {
    let (to_server, inbound) = mpsc::channel(32);
    let (outbound, from_server) = mpsc::channel(cap);
    (ChanIo { inbound, outbound }, End { tx: to_server, rx: from_server, seq: 1 })
}

macro_rules! frame {
    ($module:expr, $sub:expr, $id:expr, $kind:expr, $seq:expr, $reply_to:expr, $key:expr, $origin:expr, $body:expr, $fault:expr) => {{
        Frame {
            module: $module,
            sub: $sub,
            id: $id,
            kind: $kind as i32,
            seq: $seq,
            reply_to: $reply_to,
            key: $key.to_owned(),
            body: $body,
            fault: $fault,
            origin: $origin.to_owned(),
        }
        .encode_to_vec()
    }};
}

fn hello(page: &str, seq: u32, protocol: u32) -> Vec<u8> {
    let body = v1::Hello { protocol, page_id: page.to_owned(), build: "page".to_owned() }.encode_to_vec();
    frame!(0, BusSub::Link as u32, LinkId::Hello as u32, Kind::Request, seq, 0, "", "", body, None)
}

impl End {
    async fn send_bin(&mut self, bytes: Vec<u8>) {
        self.tx.send(IoMsg::Binary(bytes)).await.expect("the link is still reading");
    }

    async fn request(&mut self, module: u32, sub: u32, id: u32, key: &str, body: Vec<u8>) -> u32 {
        let seq = self.seq;
        self.seq += 1;
        self.send_bin(frame!(module, sub, id, Kind::Request, seq, 0, key, "", body, None)).await;
        seq
    }

    async fn next_bin(&mut self) -> Vec<u8> {
        loop {
            match self.rx.recv().await {
                Some(IoMsg::Binary(bytes)) => return bytes,
                Some(IoMsg::Ping(_)) | Some(IoMsg::Pong(_)) => {}
                other => panic!("expected a frame, got {other:?}"),
            }
        }
    }
}

fn decode(bytes: &[u8]) -> Frame {
    Frame::decode(bytes).expect("a frame")
}

async fn open(bus: &Arc<Bus>, page: &str, cap: usize) -> End {
    let (io, mut end) = pair(cap);
    tokio::spawn(link::run(Arc::clone(bus), io));
    end.send_bin(hello(page, 1, PROTOCOL)).await;
    end.seq = 2;
    let welcome = decode(&end.next_bin().await);
    assert_eq!(welcome.kind, Kind::Response as i32);
    assert_eq!(welcome.seq, 1, "the server's own counter starts at 1");
    assert_eq!(welcome.reply_to, 1);
    let body = v1::Welcome::decode(welcome.body.as_slice()).expect("welcome");
    assert_eq!(body.protocol, PROTOCOL);
    assert_eq!(body.heartbeat_ms, HEARTBEAT_MS);
    assert_eq!(body.max_frame, MAX_FRAME as u32);
    assert_eq!(body.build, bus.stamp());
    end
}

fn echo_handler() -> Handler {
    Arc::new(|call: Call| {
        Box::pin(async move { Answer::ok(call.body.to_vec()) })
    })
}

async fn subscribe(end: &mut End, topics: Vec<v1::Topic>) {
    let body = v1::Topics { topics }.encode_to_vec();
    let seq = end.request(0, BusSub::Topic as u32, TopicId::Subscribe as u32, "", body).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert!(response.fault.is_none(), "{:?}", response.fault);
}

fn topic(module: u32, sub: u32, key: &str) -> v1::Topic {
    v1::Topic { module, sub, key: key.to_owned() }
}

fn is_bye(bytes: &[u8], reason: ByeReason) -> bool {
    let frame = decode(bytes);
    frame.id == LinkId::Bye as u32 && v1::Bye::decode(frame.body.as_slice()).ok().is_some_and(|bye| bye.reason == reason as i32)
}

#[test]
fn limits_match_the_spec() {
    assert_eq!(MAX_FRAME, 4 * 1024 * 1024);
    assert_eq!(MAX_LINKS, 32);
    assert_eq!(MAX_TOPICS, 512);
    assert_eq!(MAX_QUEUE_FRAMES, 256);
    assert_eq!(MAX_QUEUE_BYTES, 8 * 1024 * 1024);
    assert_eq!(HELLO_WAIT, Duration::from_secs(10));
    assert_eq!(HEARTBEAT_MS, 20_000);
    assert_eq!(PROTOCOL_PING, Duration::from_secs(30));
    assert_eq!(IDLE_LIMIT, Duration::from_secs(75));
    assert_eq!(crate::REQUEST_TIMEOUT, Duration::from_secs(15));
    assert_eq!(crate::PONG_DEADLINE, Duration::from_secs(10));
    assert_eq!(crate::RECONNECT_MIN, Duration::from_millis(500));
    assert_eq!(crate::RECONNECT_MAX, Duration::from_secs(30));
    assert_eq!(crate::RECONNECT_NOTICE, Duration::from_secs(3));
    assert_eq!(STAMP_INTERVAL, Duration::from_secs(6));
    assert_eq!(crate::NOTICE_GAP, Duration::from_millis(250));
    assert_eq!(crate::LIST_PAGE_MAX, 2 * 1024 * 1024);
}

#[test]
fn codegen_digest_matches_the_proto_sources() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proto");
    let mut sources = Vec::new();
    collect(&root, &root, &mut sources);
    sources.sort();
    let mut hasher = Sha256::new();
    for rel in &sources {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        let bytes = std::fs::read(root.join(rel)).unwrap_or_else(|err| panic!("{rel}: {err}\n{REGENERATE}"));
        hasher.update(bytes);
        hasher.update([0]);
    }
    let digest = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let generated = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pb/swiss.bus.v1.rs"))
        .unwrap_or_else(|err| panic!("{err}\n{REGENERATE}"));
    let header = generated.lines().next().unwrap_or("");
    assert_eq!(header, format!("// proto-sha256: {digest}"), "{REGENERATE}");
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|err| panic!("{err}\n{REGENERATE}"));
    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("{err}\n{REGENERATE}"));
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "proto") {
            let rel = path.strip_prefix(root).expect("a proto under proto/");
            out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
        }
    }
}

#[tokio::test]
async fn hello_is_answered_with_welcome() {
    let _end = open(&bus(), "page-a", 8).await;
}

#[tokio::test]
async fn another_protocol_ends_with_bye_protocol() {
    let (io, mut end) = pair(4);
    tokio::spawn(link::run(bus(), io));
    end.send_bin(hello("page-a", 1, 2)).await;
    let bytes = end.next_bin().await;
    assert!(is_bye(&bytes, ByeReason::ByeProtocol), "{}", hex(&bytes));
}

#[tokio::test(start_paused = true)]
async fn no_hello_within_10s_closes_without_a_frame() {
    let (io, mut end) = pair(4);
    tokio::spawn(link::run(bus(), io));
    // Let the link park on its HELLO deadline before time moves, or the deadline
    // is measured from after the jump and never elapses in this advance.
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(HELLO_WAIT + Duration::from_millis(1)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    match end.rx.try_recv() {
        Err(TryRecvError::Disconnected) => {}
        other => panic!("link stayed open: {other:?}"),
    }
}

#[tokio::test]
async fn ping_is_answered_with_the_same_sent_ms() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    let body = v1::Ping { sent_ms: PING_SENT }.encode_to_vec();
    let seq = end.request(0, BusSub::Link as u32, LinkId::Ping as u32, "", body).await;
    let pong = decode(&end.next_bin().await);
    assert_eq!(pong.reply_to, seq);
    assert_eq!(pong.seq, 2, "server seq is its own counter");
    let body = v1::Pong::decode(pong.body.as_slice()).expect("pong");
    assert_eq!(body.sent_ms, PING_SENT);
    assert!(body.server_ms > 0);
}

#[tokio::test]
async fn a_frame_before_hello_or_a_second_hello_or_a_client_event_is_protocol() {
    let (io, mut end) = pair(4);
    tokio::spawn(link::run(bus(), io));
    let ping = v1::Ping { sent_ms: 1 }.encode_to_vec();
    end.send_bin(frame!(0, BusSub::Link as u32, LinkId::Ping as u32, Kind::Request, 1, 0, "", "", ping, None)).await;
    assert!(is_bye(&end.next_bin().await, ByeReason::ByeProtocol));

    let mut end = open(&bus(), "page-a", 8).await;
    end.send_bin(hello("page-a", 2, PROTOCOL)).await;
    assert!(is_bye(&end.next_bin().await, ByeReason::ByeProtocol));

    let mut end = open(&bus(), "page-b", 8).await;
    end.send_bin(frame!(6, 1, 5, Kind::Event, 2, 0, "c", "", Vec::new(), None)).await;
    assert!(is_bye(&end.next_bin().await, ByeReason::ByeProtocol));

    let mut end = open(&bus(), "page-c", 8).await;
    end.send_bin(frame!(0, 1, 2, Kind::Response, 2, 1, "", "", Vec::new(), None)).await;
    assert!(is_bye(&end.next_bin().await, ByeReason::ByeProtocol));

    let mut end = open(&bus(), "page-d", 8).await;
    end.send_bin(frame!(0, 0, 1, Kind::Request, 2, 0, "", "", Vec::new(), None)).await;
    assert!(is_bye(&end.next_bin().await, ByeReason::ByeProtocol));
}

#[tokio::test]
async fn topics_match_exact_keys_and_the_empty_key_and_one_unsubscribe_stops_them() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    let consoles = topic(v1::Module::Data as u32, DataSub::Consoles as u32, "alpha");
    subscribe(&mut end, vec![consoles.clone()]).await;
    // The same topic again must not count twice (the server keeps a set, not a refcount).
    subscribe(&mut end, vec![consoles]).await;
    bus.publish(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesChanged as u32, "alpha", "page-a", b"one".to_vec());
    bus.publish(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesChanged as u32, "beta", "page-a", b"two".to_vec());
    let hit = decode(&end.next_bin().await);
    assert_eq!(hit.key, "alpha");
    assert_eq!(hit.body, b"one");
    assert_eq!(hit.origin, "page-a", "the causing page is included");
    tokio::task::yield_now().await;
    assert!(matches!(end.rx.try_recv(), Err(TryRecvError::Empty)));

    subscribe(&mut end, vec![topic(v1::Module::Data as u32, DataSub::Consoles as u32, "")]).await;
    bus.publish(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesChanged as u32, "beta", "", b"all".to_vec());
    let hit = decode(&end.next_bin().await);
    assert_eq!(hit.key, "beta");

    let body = v1::Topics { topics: vec![topic(v1::Module::Data as u32, DataSub::Consoles as u32, "")] }.encode_to_vec();
    end.request(0, BusSub::Topic as u32, TopicId::Unsubscribe as u32, "", body).await;
    let _ = end.next_bin().await;
    bus.publish(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesChanged as u32, "gamma", "", b"nope".to_vec());
    // "alpha" is still subscribed, so a gamma event matches only the empty key, which is gone.
    tokio::task::yield_now().await;
    assert!(matches!(end.rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn a_subscribe_past_512_adds_nothing() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    let topics: Vec<_> = (0..MAX_TOPICS + 1).map(|n| topic(6, 1, &format!("k{n}"))).collect();
    let body = v1::Topics { topics }.encode_to_vec();
    let seq = end.request(0, BusSub::Topic as u32, TopicId::Subscribe as u32, "", body).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    let fault = response.fault.expect("bus.limit");
    assert_eq!(fault.code, LIMIT);
    bus.publish(6, 1, 5, "k0", "", b"x".to_vec());
    tokio::task::yield_now().await;
    assert!(matches!(end.rx.try_recv(), Err(TryRecvError::Empty)), "a refused subscribe adds none");
}

#[tokio::test]
async fn a_relay_reaches_other_subscribers_only_and_a_stray_relay_is_refused() {
    let bus = bus();
    let mut left = open(&bus, "left", 8).await;
    let mut right = open(&bus, "right", 8).await;
    let prefs = topic(v1::Module::Panel as u32, PanelSub::Prefs as u32, "");
    subscribe(&mut left, vec![prefs.clone()]).await;
    subscribe(&mut right, vec![prefs]).await;
    let body = v1::Prefs { lang: Some("zh".to_owned()), theme: None, code_scheme: None }.encode_to_vec();
    let seq = left.seq;
    left.seq += 1;
    left.send_bin(frame!(v1::Module::Panel as u32, PanelSub::Prefs as u32, PrefsId::PrefsChanged as u32, Kind::Relay, seq, 0, "", "", body, None)).await;
    let relay = decode(&right.next_bin().await);
    assert_eq!(relay.kind, Kind::Relay as i32);
    assert_eq!(relay.origin, "left");
    assert_eq!(v1::Prefs::decode(relay.body.as_slice()).expect("prefs").lang.as_deref(), Some("zh"));
    tokio::task::yield_now().await;
    assert!(matches!(left.rx.try_recv(), Err(TryRecvError::Empty)), "a relay is not delivered to its sender");

    let seq = left.seq;
    left.seq += 1;
    left.send_bin(frame!(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesChanged as u32, Kind::Relay, seq, 0, "", "", b"no".to_vec(), None)).await;
    let response = decode(&left.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert_eq!(response.fault.expect("not a relay").code, NOT_RELAYABLE);
}

#[tokio::test]
async fn an_unregistered_sub_is_unavailable_and_dropping_the_guard_unregisters_it() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    let seq = end.request(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesList as u32, "", Vec::new()).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert_eq!(response.fault.expect("unavailable").code, UNAVAILABLE);

    let first = bus.register(v1::Module::Data as u32, DataSub::Consoles as u32, &[ConsolesId::ConsolesList as u32], echo_handler());
    let second = bus.register(v1::Module::Data as u32, DataSub::Consoles as u32, &[ConsolesId::ConsolesList as u32], echo_handler());
    drop(first);
    let body = b"ok".to_vec();
    let seq = end.request(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesList as u32, "c", body.clone()).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert!(response.fault.is_none());
    assert_eq!(response.body, body);
    let seq = end.request(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesPut as u32, "", Vec::new()).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert_eq!(response.fault.expect("unknown id").code, UNKNOWN);

    drop(second);
    let seq = end.request(v1::Module::Data as u32, DataSub::Consoles as u32, ConsolesId::ConsolesList as u32, "", Vec::new()).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert_eq!(response.fault.expect("guard dropped").code, UNAVAILABLE);
}

#[tokio::test]
async fn a_body_that_does_not_decode_is_bad_body() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    let seq = end.request(0, BusSub::Topic as u32, TopicId::Subscribe as u32, "", vec![0xff]).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert_eq!(response.fault.expect("bad body").code, BAD_BODY);

    let seq = end.request(0, BusSub::Link as u32, LinkId::Ping as u32, "", vec![0xff]).await;
    let response = decode(&end.next_bin().await);
    assert_eq!(response.reply_to, seq);
    assert_eq!(response.fault.expect("bad ping").code, BAD_BODY);
}

#[tokio::test]
async fn a_full_queue_ends_that_link_and_leaves_the_others() {
    let bus = bus();
    let mut slow = open(&bus, "slow", 1).await;
    let mut healthy = open(&bus, "healthy", 512).await;
    let all = vec![topic(6, 1, "")];
    subscribe(&mut slow, all.clone()).await;
    subscribe(&mut healthy, all).await;
    let flood = async {
        for n in 0..MAX_QUEUE_FRAMES + 40 {
            bus.publish(6, 1, ConsolesId::ConsolesChanged as u32, "c", "", vec![n as u8]);
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(2), flood).await.expect("publish must not wait on a full queue");
    let mut saw_slow = false;
    for _ in 0..MAX_QUEUE_FRAMES + 8 {
        let Ok(Some(msg)) = tokio::time::timeout(Duration::from_secs(2), slow.rx.recv()).await else { break };
        if let IoMsg::Binary(bytes) = msg {
            if is_bye(&bytes, ByeReason::ByeSlow) {
                saw_slow = true;
                break;
            }
        }
    }
    assert!(saw_slow, "a full queue ends the link with BYE SLOW");
    let mut got = 0usize;
    while let Ok(msg) = healthy.rx.try_recv() {
        if let IoMsg::Binary(bytes) = msg {
            let frame = decode(&bytes);
            if frame.kind == Kind::Event as i32 {
                got += 1;
            }
        }
    }
    assert_eq!(got, MAX_QUEUE_FRAMES + 40, "other links still receive every event");
}

#[tokio::test]
async fn the_byte_cap_ends_a_link_before_the_frame_cap() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 1).await;
    subscribe(&mut end, vec![topic(6, 1, "")]).await;
    let blob = vec![7u8; 3_000_000];
    for _ in 0..4 {
        bus.publish(6, 1, 5, "c", "", blob.clone());
    }
    let mut saw = false;
    for _ in 0..8 {
        let Ok(Some(msg)) = tokio::time::timeout(Duration::from_secs(2), end.rx.recv()).await else { break };
        if let IoMsg::Binary(bytes) = msg {
            if is_bye(&bytes, ByeReason::ByeSlow) {
                saw = true;
                break;
            }
            assert!(bytes.len() <= MAX_FRAME);
        }
    }
    assert!(saw);
}

const _: () = assert!(3_000_000 * 2 <= crate::MAX_QUEUE_BYTES && 3_000_000 * 3 > crate::MAX_QUEUE_BYTES);

#[tokio::test]
async fn the_33rd_link_is_refused_with_bye_limit() {
    let bus = bus();
    let mut held = Vec::new();
    for n in 0..MAX_LINKS {
        held.push(open(&bus, &format!("p{n}"), 4).await);
    }
    let (io, mut end) = pair(4);
    tokio::spawn(link::run(Arc::clone(&bus), io));
    end.send_bin(hello("overflow", 1, PROTOCOL)).await;
    assert!(is_bye(&end.next_bin().await, ByeReason::ByeLimit));
    // The ones that were admitted are still there: a ping is answered.
    let body = v1::Ping { sent_ms: 5 }.encode_to_vec();
    held[0].request(0, BusSub::Link as u32, LinkId::Ping as u32, "", body).await;
    let pong = decode(&held[0].next_bin().await);
    assert_eq!(v1::Pong::decode(pong.body.as_slice()).expect("pong").sent_ms, 5);
}

#[tokio::test]
async fn shutdown_tells_every_link() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 4).await;
    let stop = bus.shutdown();
    let read = async { end.next_bin().await };
    let (bytes, ()) = tokio::join!(read, stop);
    assert!(is_bye(&bytes, ByeReason::ByeShutdown));
}

#[tokio::test(start_paused = true)]
async fn silence_for_75s_ends_with_bye_idle_and_a_protocol_ping_goes_at_30s() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(PROTOCOL_PING + Duration::from_millis(1)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    match end.rx.try_recv() {
        Ok(IoMsg::Ping(_)) => {}
        other => panic!("expected a protocol ping, got {other:?}"),
    }
    tokio::time::advance(IDLE_LIMIT - PROTOCOL_PING).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    let mut saw = false;
    while let Ok(msg) = end.rx.try_recv() {
        if let IoMsg::Binary(bytes) = msg {
            if is_bye(&bytes, ByeReason::ByeIdle) {
                saw = true;
            }
        }
    }
    assert!(saw);
}

#[tokio::test(start_paused = true)]
async fn a_debug_build_publishes_build_when_the_stamp_changes() {
    let current = Arc::new(Mutex::new("one".to_owned()));
    let stamp_fn = {
        let current = Arc::clone(&current);
        Arc::new(move || current.lock().unwrap_or_else(|p| p.into_inner()).clone())
    };
    let bus = Bus::new(stamp_fn);
    let mut end = open(&bus, "page-a", 8).await;
    // open() asserts the welcome build is "stamp-a"; this bus's stamp is "one".
    // Re-read is not needed: welcome was already consumed. Change the stamp and wait one interval.
    *current.lock().unwrap_or_else(|p| p.into_inner()) = "two".to_owned();
    tokio::time::advance(STAMP_INTERVAL).await;
    let build = decode(&end.next_bin().await);
    assert_eq!(build.id, LinkId::Build as u32);
    assert_eq!(build.kind, Kind::Event as i32);
    assert_eq!(v1::Build::decode(build.body.as_slice()).expect("build").build, "two");
}

#[test]
fn golden_frames_decode_and_reencode() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/golden.json");
    if std::env::var("SWISS_WRITE_GOLDEN").is_ok() {
        let text = serde_json::to_string_pretty(&golden_entries()).expect("json") + "\n";
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|err| panic!("{err}"));
        }
        std::fs::write(&path, text.replace("\r\n", "\n")).unwrap_or_else(|err| panic!("{err}"));
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path:?}: {err}\nset SWISS_WRITE_GOLDEN=1 to write it"));
    let entries: Vec<serde_json::Value> = serde_json::from_str(&text).expect("golden.json");
    let built = golden_entries();
    assert_eq!(entries.len(), built.len());
    for (entry, again) in entries.iter().zip(built.iter()) {
        let name = entry["name"].as_str().expect("name");
        let hex_text = entry["hex"].as_str().expect("hex");
        assert_eq!(again["hex"].as_str(), Some(hex_text), "{name} drifted; regenerate with SWISS_WRITE_GOLDEN=1");
        if name == "ping" {
            assert_eq!(hex_text, PING_HEX);
        }
        if name == "pong" {
            assert_eq!(hex_text, PONG_HEX);
        }
        let bytes = unhex(hex_text);
        let frame = decode(&bytes);
        assert_eq!(frame.module, entry["module"].as_u64().expect("module") as u32, "{name}");
        assert_eq!(frame.sub, entry["sub"].as_u64().expect("sub") as u32, "{name}");
        assert_eq!(frame.id, entry["id"].as_u64().expect("id") as u32, "{name}");
        assert_eq!(kind_name(frame.kind), entry["kind"].as_str().unwrap_or(""), "{name}");
        assert_eq!(frame.seq, entry["seq"].as_u64().expect("seq") as u32, "{name}");
        assert_eq!(frame.reply_to, entry["replyTo"].as_u64().unwrap_or(0) as u32, "{name}");
        assert_eq!(frame.key, entry["key"].as_str().unwrap_or(""), "{name}");
        assert_eq!(frame.origin, entry["origin"].as_str().unwrap_or(""), "{name}");
        match entry.get("fault") {
            None | Some(serde_json::Value::Null) => assert!(frame.fault.is_none(), "{name}"),
            Some(fault) => {
                let got = frame.fault.as_ref().expect("fault");
                assert_eq!(got.code, fault["code"].as_str().unwrap_or(""), "{name}");
                assert_eq!(got.message, fault["message"].as_str().unwrap_or(""), "{name}");
                let params = fault["params"].as_object().expect("params");
                assert_eq!(got.params.len(), params.len(), "{name}");
                for (key, value) in params {
                    assert_eq!(got.params.get(key).map(String::as_str), value.as_str(), "{name}");
                }
            }
        }
        assert_body(entry["bodyType"].as_str().unwrap_or(""), &frame.body, &entry["body"], name);
        assert_eq!(frame.encode_to_vec(), bytes, "{name} did not re-encode");
    }
}

fn kind_name(kind: i32) -> &'static str {
    match Kind::try_from(kind).unwrap_or(Kind::Unspecified) {
        Kind::Request => "REQUEST",
        Kind::Response => "RESPONSE",
        Kind::Event => "EVENT",
        Kind::Relay => "RELAY",
        Kind::Unspecified => "UNSPECIFIED",
    }
}

fn assert_body(ty: &str, bytes: &[u8], fields: &serde_json::Value, name: &str) {
    match ty {
        "Ping" => {
            let ping = v1::Ping::decode(bytes).expect(name);
            assert_eq!(ping.sent_ms, fields["sentMs"].as_u64().expect("sentMs"), "{name}");
        }
        "Pong" => {
            let pong = v1::Pong::decode(bytes).expect(name);
            assert_eq!(pong.sent_ms, fields["sentMs"].as_u64().expect("sentMs"), "{name}");
            assert_eq!(pong.server_ms, fields["serverMs"].as_u64().expect("serverMs"), "{name}");
        }
        "Hello" => {
            let hello = v1::Hello::decode(bytes).expect(name);
            assert_eq!(hello.protocol, fields["protocol"].as_u64().expect("protocol") as u32, "{name}");
            assert_eq!(hello.page_id, fields["pageId"].as_str().unwrap_or(""), "{name}");
            assert_eq!(hello.build, fields["build"].as_str().unwrap_or(""), "{name}");
        }
        "Welcome" => {
            let welcome = v1::Welcome::decode(bytes).expect(name);
            assert_eq!(welcome.protocol, fields["protocol"].as_u64().unwrap_or(0) as u32, "{name}");
            assert_eq!(welcome.conn_id, fields["connId"].as_str().unwrap_or(""), "{name}");
            assert_eq!(welcome.build, fields["build"].as_str().unwrap_or(""), "{name}");
            assert_eq!(welcome.heartbeat_ms, fields["heartbeatMs"].as_u64().unwrap_or(0), "{name}");
            assert_eq!(welcome.max_frame, fields["maxFrame"].as_u64().unwrap_or(0) as u32, "{name}");
            assert_eq!(welcome.server_ms, fields["serverMs"].as_u64().unwrap_or(0), "{name}");
        }
        "Bye" => {
            let bye = v1::Bye::decode(bytes).expect(name);
            assert_eq!(bye.reason, ByeReason::ByeSlow as i32, "{name}");
        }
        "Topics" => {
            let topics = v1::Topics::decode(bytes).expect(name);
            assert_eq!(topics.topics.len(), 1, "{name}");
            assert_eq!(topics.topics[0].module, 6, "{name}");
            assert_eq!(topics.topics[0].key, "alpha", "{name}");
        }
        "ConsolePut" => {
            let put = v1::ConsolePut::decode(bytes).expect(name);
            assert_eq!(put.conn, "alpha", "{name}");
            assert_eq!(put.mode, SaveMode::Update as i32, "{name}");
            let console = put.console.expect("console");
            assert_eq!(console.agent_tab.as_deref(), Some("agent-1"), "{name}");
            assert_eq!(console.closed_at, Some(1_700_000_000_000), "{name}");
            assert_eq!(console.revision, 3, "{name}");
            assert_eq!(console.sql, "select 1", "{name}");
        }
        "ConsoleList" => {
            let list = v1::ConsoleList::decode(bytes).expect(name);
            assert_eq!(list.next, "c1", "{name}");
            assert_eq!(list.consoles.len(), 1, "{name}");
            assert_eq!(list.consoles[0].id, "c1", "{name}");
        }
        "ConsoleSaved" => {
            let saved = v1::ConsoleSaved::decode(bytes).expect(name);
            assert_eq!(saved.console.expect("console").id, "c1", "{name}");
        }
        "Prefs" => {
            let prefs = v1::Prefs::decode(bytes).expect(name);
            assert_eq!(prefs.lang.as_deref(), Some("zh"), "{name}");
            assert!(prefs.theme.is_none(), "{name}");
            assert!(prefs.code_scheme.is_none(), "{name}");
        }
        "Notice" => {
            assert_eq!(v1::Notice::decode(bytes).expect(name), v1::Notice {}, "{name}");
            assert!(bytes.is_empty(), "{name}: a notice carries no body");
        }
        "RunChanged" => {
            let run = v1::RunChanged::decode(bytes).expect(name);
            assert_eq!(run.run_id, fields["runId"].as_u64().expect("runId"), "{name}");
            assert_eq!(run.revision, fields["revision"].as_u64().expect("revision") as u32, "{name}");
            assert_eq!(run.state, v1::RunState::Failed as i32, "{name}");
            assert_eq!(run.action, "remote.exec", "{name}");
            assert_eq!(run.owner, "manual", "{name}");
            assert_eq!(run.actor, "cli:user@host", "{name}");
            assert_eq!(run.lane, "build", "{name}");
            assert_eq!(run.queued_at_ms, PING_SENT, "{name}");
            assert_eq!(run.started_at_ms, Some(PING_SENT + 5), "{name}");
            assert_eq!(run.ended_at_ms, Some(PING_SENT + 8015), "{name}");
            assert_eq!(run.exit_code, Some(2), "{name}");
        }
        "" => assert!(bytes.is_empty(), "{name}"),
        other => panic!("{name}: unknown body {other}"),
    }
}

fn golden_entries() -> Vec<serde_json::Value> {
    let console = v1::ConsoleRecord {
        id: "c1".to_owned(),
        tab: "scratch".to_owned(),
        agent: true,
        agent_tab: Some("agent-1".to_owned()),
        open: true,
        sql: "select 1".to_owned(),
        revision: 3,
        created_at: 1_700_000_000_000,
        updated_at: 1_700_000_000_100,
        closed_at: Some(1_700_000_000_000),
    };
    let mut fault = crate::codec::fault("console.changed", "conflict: console changed; reload the server record before saving");
    fault.params.insert("id".to_owned(), "c1".to_owned());
    let specs = [
        ("ping", frame!(0, 1, 2, Kind::Request, 7, 0, "", "", v1::Ping { sent_ms: PING_SENT }.encode_to_vec(), None), meta_ping()),
        ("pong", frame!(0, 1, 2, Kind::Response, 9, 7, "", "", v1::Pong { sent_ms: PING_SENT, server_ms: PING_SENT + 3 }.encode_to_vec(), None), meta_pong()),
        (
            "hello",
            frame!(0, 1, 1, Kind::Request, 1, 0, "", "", v1::Hello { protocol: 1, page_id: "page-a".to_owned(), build: "stamp-a".to_owned() }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Hello","body":{"protocol":1,"pageId":"page-a","build":"stamp-a"}}),
        ),
        (
            "welcome",
            frame!(0, 1, 1, Kind::Response, 1, 1, "", "", v1::Welcome { protocol: 1, conn_id: "7".to_owned(), build: "stamp-a".to_owned(), heartbeat_ms: HEARTBEAT_MS, max_frame: MAX_FRAME as u32, server_ms: PING_SENT }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Welcome","body":{"protocol":1,"connId":"7","build":"stamp-a","heartbeatMs":20000,"maxFrame":4194304,"serverMs":1793784600000u64}}),
        ),
        (
            "bye-slow",
            frame!(0, 1, 3, Kind::Event, 4, 0, "", "", v1::Bye { reason: ByeReason::ByeSlow as i32 }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Bye","body":{"reason":"SLOW"}}),
        ),
        (
            "subscribe",
            frame!(0, 2, 1, Kind::Request, 3, 0, "", "", v1::Topics { topics: vec![topic(6, 1, "alpha")] }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Topics","body":{"topics":[{"module":6,"sub":1,"key":"alpha"}]}}),
        ),
        (
            "consoles-put",
            frame!(6, 1, 2, Kind::Request, 4, 0, "", "", v1::ConsolePut { conn: "alpha".to_owned(), console: Some(console.clone()), mode: SaveMode::Update as i32 }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"ConsolePut","body":{"conn":"alpha"}}),
        ),
        (
            "consoles-changed-fault",
            frame!(6, 1, 2, Kind::Response, 8, 4, "", "", Vec::new(), Some(fault)),
            serde_json::json!({"bodyType":"","body":{},"fault":{"code":"console.changed","message":"conflict: console changed; reload the server record before saving","params":{"id":"c1"}}}),
        ),
        (
            "consoles-changed",
            frame!(6, 1, 5, Kind::Event, 9, 0, "alpha", "page-a", v1::ConsoleSaved { console: Some(console.clone()) }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"ConsoleSaved","body":{"id":"c1"},"origin":"page-a","key":"alpha"}),
        ),
        (
            "consoles-list",
            frame!(6, 1, 1, Kind::Response, 6, 5, "", "", v1::ConsoleList { consoles: vec![console], next: "c1".to_owned() }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"ConsoleList","body":{"next":"c1"}}),
        ),
        (
            "seat-notice",
            frame!(4, 2, 1, Kind::Event, 11, 0, "", "page-b", v1::Notice {}.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Notice","body":{},"origin":"page-b"}),
        ),
        (
            "targets-notice",
            frame!(10, 1, 1, Kind::Event, 12, 0, "", "page-a", v1::Notice {}.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Notice","body":{},"origin":"page-a"}),
        ),
        (
            "history-notice",
            frame!(10, 2, 1, Kind::Event, 13, 0, "", "page-a", v1::Notice {}.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Notice","body":{},"origin":"page-a"}),
        ),
        (
            "runs-changed",
            frame!(4, 1, 1, Kind::Event, 10, 0, "remote", "page-a", v1::RunChanged {
                run_id: 12,
                revision: 3,
                state: v1::RunState::Failed as i32,
                action: "remote.exec".to_owned(),
                owner: "manual".to_owned(),
                actor: "cli:user@host".to_owned(),
                lane: "build".to_owned(),
                queued_at_ms: PING_SENT,
                started_at_ms: Some(PING_SENT + 5),
                ended_at_ms: Some(PING_SENT + 8015),
                exit_code: Some(2),
            }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"RunChanged","body":{"runId":12,"revision":3,"state":"FAILED","action":"remote.exec","owner":"manual","actor":"cli:user@host","lane":"build","queuedAtMs":1793784600000u64,"startedAtMs":1793784600005u64,"endedAtMs":1793784608015u64,"exitCode":2},"origin":"page-a","key":"remote"}),
        ),
        (
            "plugins-notice",
            frame!(4, 3, 1, Kind::Event, 14, 0, "alpha", "page-a", v1::Notice {}.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Notice","body":{},"origin":"page-a","key":"alpha"}),
        ),
        (
            "prefs-relay",
            frame!(14, 1, 1, Kind::Relay, 2, 0, "", "left", v1::Prefs { lang: Some("zh".to_owned()), theme: None, code_scheme: None }.encode_to_vec(), None),
            serde_json::json!({"bodyType":"Prefs","body":{"lang":"zh"},"origin":"left"}),
        ),
    ];
    specs
        .into_iter()
        .map(|(name, bytes, mut extra)| {
            let frame = decode(&bytes);
            extra["name"] = serde_json::Value::String(name.to_owned());
            extra["hex"] = serde_json::Value::String(hex(&bytes));
            extra["module"] = serde_json::json!(frame.module);
            extra["sub"] = serde_json::json!(frame.sub);
            extra["id"] = serde_json::json!(frame.id);
            extra["kind"] = serde_json::Value::String(kind_name(frame.kind).to_owned());
            extra["seq"] = serde_json::json!(frame.seq);
            extra["replyTo"] = serde_json::json!(frame.reply_to);
            if extra.get("key").is_none() {
                extra["key"] = serde_json::Value::String(frame.key);
            }
            if extra.get("origin").is_none() {
                extra["origin"] = serde_json::Value::String(frame.origin);
            }
            extra
        })
        .collect()
}

fn meta_ping() -> serde_json::Value {
    serde_json::json!({"bodyType":"Ping","body":{"sentMs":1793784600000u64}})
}

fn meta_pong() -> serde_json::Value {
    serde_json::json!({"bodyType":"Pong","body":{"sentMs":1793784600000u64,"serverMs":1793784600003u64}})
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex"))
        .collect()
}

/// WELCOME without the link pump, so a test can subscribe a link that is not yet
/// welcomed. The frame is only there so `welcome` has something to queue.
fn welcome_link(bus: &Bus, id: u64) {
    let out = crate::codec::Outgoing {
        module: 0,
        sub: 1,
        id: 1,
        kind: Kind::Response as i32,
        reply_to: 1,
        key: String::new(),
        body: Vec::new(),
        fault: None,
        origin: String::new(),
    };
    assert!(bus.welcome(id, &out), "the link is open");
}

// SPEC §bus.topics: demand rises with the first welcomed subscriber and falls when the last one leaves.
#[test]
fn demand_rises_on_the_first_subscriber_and_falls_on_the_last() {
    let bus = bus();
    let mut demand = bus.demand(7, 1);
    assert!(!*demand.borrow_and_update(), "nobody is subscribed");

    let left = bus.try_open().expect("a link");
    welcome_link(&bus, left.id);
    bus.subscribe(left.id, &[topic(7, 1, "left")]).expect("under the cap");
    assert!(demand.has_changed().expect("the watch stays open"));
    assert!(*demand.borrow_and_update());

    let right = bus.try_open().expect("a link");
    welcome_link(&bus, right.id);
    bus.subscribe(right.id, &[topic(7, 1, "right")]).expect("under the cap");
    assert!(!demand.has_changed().expect("the watch stays open"), "a second link keeps the flag true");
    assert!(*demand.borrow());

    bus.unsubscribe(left.id, &[topic(7, 1, "left")]);
    assert!(!demand.has_changed().expect("the watch stays open"));
    assert!(*demand.borrow(), "one link still subscribes");

    drop(right);
    assert!(demand.has_changed().expect("the watch stays open"));
    assert!(!*demand.borrow(), "the last link's close drops demand");
}

// SPEC §bus.topics: a link that has not been welcomed does not count toward demand.
#[test]
fn demand_ignores_a_link_that_is_not_welcomed() {
    let bus = bus();
    let opened = bus.try_open().expect("a link");
    bus.subscribe(opened.id, &[topic(5, 1, "srv")]).expect("under the cap");
    let mut demand = bus.demand(5, 1);
    assert!(!*demand.borrow_and_update());
    // WELCOME is what starts the count: the topic was already on the link.
    welcome_link(&bus, opened.id);
    assert!(demand.has_changed().expect("the watch stays open"));
    assert!(*demand.borrow());
}

// SPEC §bus.topics: a receiver taken while demand already holds starts true.
#[test]
fn a_demand_receiver_made_while_demand_holds_starts_true() {
    let bus = bus();
    let opened = bus.try_open().expect("a link");
    welcome_link(&bus, opened.id);
    bus.subscribe(opened.id, &[topic(4, 6, "")]).expect("under the cap");
    let first = bus.demand(4, 6);
    assert!(*first.borrow());
    let second = bus.demand(4, 6);
    assert!(*second.borrow());
}

/// Let a spawned sleep park, and a woken one finish, before the assertion reads.
/// `advance` wakes a deadline; the task runs on a later poll, the same way the
/// idle-limit test waits out its protocol ping.
async fn idle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

fn drain_bins(end: &mut End) -> Vec<Frame> {
    let mut out = Vec::new();
    loop {
        match end.rx.try_recv() {
            Ok(IoMsg::Binary(bytes)) => out.push(decode(&bytes)),
            Ok(IoMsg::Ping(_)) | Ok(IoMsg::Pong(_)) => {}
            Ok(other) => panic!("expected a frame, got {other:?}"),
            Err(TryRecvError::Empty) => return out,
            Err(TryRecvError::Disconnected) => panic!("the link closed"),
        }
    }
}

// SPEC §bus.topics: a throttled notice publishes the first poke at once and one trailing poke at the gap's end.
#[tokio::test(start_paused = true)]
async fn notifier_publishes_the_first_poke_at_once_and_one_trailing_for_a_burst() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    subscribe(&mut end, vec![topic(4, 3, "")]).await;
    let notifier = crate::Notifier::new(Arc::clone(&bus), 4, 3, 1, crate::NOTICE_GAP);
    for _ in 0..10 {
        notifier.poke("mcp");
    }
    // Park the gap's sleep at t = 0, and let the link forward the first notice, before time moves.
    tokio::time::advance(Duration::from_millis(0)).await;
    idle().await;
    let frames = drain_bins(&mut end);
    assert_eq!(frames.len(), 1, "the first poke publishes at once");
    assert_eq!(frames[0].key, "mcp");
    assert_eq!(frames[0].kind, Kind::Event as i32);
    assert!(frames[0].body.is_empty(), "a notice carries nothing");

    tokio::time::advance(crate::NOTICE_GAP - Duration::from_millis(1)).await;
    idle().await;
    assert!(drain_bins(&mut end).is_empty(), "the trailing notice waits out the gap");

    tokio::time::advance(Duration::from_millis(1)).await;
    idle().await;
    let frames = drain_bins(&mut end);
    assert_eq!(frames.len(), 1, "ten pokes inside the gap are two frames");
    assert_eq!(frames[0].key, "mcp");
}

// SPEC §bus.topics: one key's gap does not hold another key's notice.
#[tokio::test(start_paused = true)]
async fn notifier_keys_are_independent() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    subscribe(&mut end, vec![topic(4, 3, "")]).await;
    let notifier = crate::Notifier::new(Arc::clone(&bus), 4, 3, 1, crate::NOTICE_GAP);
    notifier.poke("a");
    notifier.poke("b");
    notifier.poke("a");
    tokio::time::advance(Duration::from_millis(0)).await;
    idle().await;
    let frames = drain_bins(&mut end);
    assert_eq!(frames.iter().map(|frame| frame.key.as_str()).collect::<Vec<_>>(), ["a", "b"]);

    tokio::time::advance(crate::NOTICE_GAP).await;
    idle().await;
    let frames = drain_bins(&mut end);
    assert_eq!(frames.len(), 1, "only the key that was poked again trails");
    assert_eq!(frames[0].key, "a");
}

// SPEC §bus.topics: a key that stays quiet for a whole gap is forgotten, so the next poke publishes at once.
#[tokio::test(start_paused = true)]
async fn notifier_forgets_a_quiet_key() {
    let bus = bus();
    let mut end = open(&bus, "page-a", 8).await;
    subscribe(&mut end, vec![topic(4, 3, "")]).await;
    let notifier = crate::Notifier::new(Arc::clone(&bus), 4, 3, 1, crate::NOTICE_GAP);
    notifier.poke("mcp");
    tokio::time::advance(Duration::from_millis(0)).await;
    idle().await;
    assert_eq!(drain_bins(&mut end).len(), 1);
    assert_eq!(notifier.retained_keys(), 1, "the gap is still open");

    tokio::time::advance(crate::NOTICE_GAP).await;
    idle().await;
    assert!(drain_bins(&mut end).is_empty(), "a quiet gap publishes nothing more");
    assert_eq!(notifier.retained_keys(), 0, "a quiet key is dropped");

    notifier.poke("mcp");
    tokio::time::advance(Duration::from_millis(0)).await;
    idle().await;
    let frames = drain_bins(&mut end);
    assert_eq!(frames.len(), 1, "the next poke is a first poke again");
    assert_eq!(frames[0].key, "mcp");
}
