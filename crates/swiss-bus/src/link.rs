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

//! One connection's pump (SPEC §bus.link). The socket is an [`Io`] so the tests
//! drive frames through a channel and never open a port.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use prost::Message;

use crate::codec::{self, Outgoing};
use crate::hub::{self, Bus, Call, Opened};
use crate::pb::v1::{self, ByeReason, BusSub, Frame, Kind, LinkId, TopicId};
use crate::{HELLO_WAIT, IDLE_LIMIT, MAX_FRAME, PROTOCOL, PROTOCOL_PING};

/// What the pump exchanges with a socket. Binary is a bus frame; the rest are
/// WebSocket control (SPEC §bus.link).
#[derive(Debug)]
pub enum IoMsg {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Text,
    Close,
}

pub trait Io: Send {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<IoMsg>> + Send + '_>>;
    fn send(&mut self, msg: IoMsg) -> Pin<Box<dyn Future<Output = bool> + Send + '_>>;
}

enum Step {
    Continue,
    Bye(ByeReason),
    /// The frame was over the limit: close, and do not send BYE (SPEC §bus.wire).
    Drop,
}

/// Run one link to completion. Returns when the peer, the protocol or the hub ends it.
pub async fn run<I: Io>(bus: Arc<Bus>, mut io: I) {
    let Some(opened) = bus.try_open() else {
        let _ = io.send(IoMsg::Binary(hub::bye_limit_bytes())).await;
        let _ = io.send(IoMsg::Close).await;
        swiss_core::log::info("bus: refused a link past the cap");
        return;
    };
    let Opened { id, mut outgoing, guard } = opened;
    if !handshake(&bus, id, &mut io).await {
        drop(guard);
        return;
    }
    let mut stop = bus.subscribe_stop();
    let idle = tokio::time::sleep(IDLE_LIMIT);
    tokio::pin!(idle);
    let start = tokio::time::Instant::now() + PROTOCOL_PING;
    let mut ping = tokio::time::interval_at(start, PROTOCOL_PING);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut why = "closed";
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    say_bye(&bus, id, &mut io, ByeReason::ByeShutdown).await;
                    why = "shutdown";
                    break;
                }
            }
            _ = &mut idle => {
                say_bye(&bus, id, &mut io, ByeReason::ByeIdle).await;
                why = "idle";
                break;
            }
            _ = ping.tick() => {
                if !io.send(IoMsg::Ping(Vec::new())).await {
                    why = "closed";
                    break;
                }
            }
            incoming = io.recv() => {
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE_LIMIT);
                match incoming {
                    None | Some(IoMsg::Close) => break,
                    Some(IoMsg::Pong(_)) => {}
                    Some(IoMsg::Ping(payload)) => {
                        if !io.send(IoMsg::Pong(payload)).await {
                            break;
                        }
                    }
                    Some(IoMsg::Text) => {
                        say_bye(&bus, id, &mut io, ByeReason::ByeProtocol).await;
                        why = "protocol";
                        break;
                    }
                    Some(IoMsg::Binary(bytes)) => match on_binary(&bus, id, bytes).await {
                        Step::Continue => {}
                        Step::Drop => {
                            let _ = io.send(IoMsg::Close).await;
                            why = "too large";
                            break;
                        }
                        Step::Bye(reason) => {
                            say_bye(&bus, id, &mut io, reason).await;
                            why = "protocol";
                            break;
                        }
                    },
                }
            }
            frame = outgoing.recv() => {
                let Some(frame) = frame else { break };
                let nbytes = frame.len();
                if !io.send(IoMsg::Binary(frame)).await {
                    break;
                }
                bus.note_sent(id, nbytes);
                if bus.is_slow(id) {
                    say_bye(&bus, id, &mut io, ByeReason::ByeSlow).await;
                    why = "slow";
                    break;
                }
            }
        }
    }
    swiss_core::log::info(&format!("bus: link {id} closed ({why})"));
    drop(guard);
}

async fn handshake<I: Io>(bus: &Arc<Bus>, id: u64, io: &mut I) -> bool {
    let deadline = tokio::time::Instant::now() + HELLO_WAIT;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            swiss_core::log::info(&format!("bus: link {id} closed (no hello)"));
            return false;
        }
        match tokio::time::timeout(left, io.recv()).await {
            Err(_) => {
                swiss_core::log::info(&format!("bus: link {id} closed (no hello)"));
                return false;
            }
            Ok(None) | Ok(Some(IoMsg::Close)) => return false,
            Ok(Some(IoMsg::Ping(payload))) => {
                if !io.send(IoMsg::Pong(payload)).await {
                    return false;
                }
            }
            Ok(Some(IoMsg::Pong(_))) => {}
            Ok(Some(IoMsg::Text)) => {
                say_bye(bus, id, io, ByeReason::ByeProtocol).await;
                return false;
            }
            Ok(Some(IoMsg::Binary(bytes))) => {
                if bytes.len() > MAX_FRAME {
                    let _ = io.send(IoMsg::Close).await;
                    return false;
                }
                match hello_of(&bytes) {
                    HelloVerdict::Ok { page_id, seq } => {
                        bus.set_page(id, page_id);
                        let welcome = welcome_of(bus, id, seq);
                        if !bus.welcome(id, &welcome) {
                            return false;
                        }
                        return true;
                    }
                    HelloVerdict::Protocol => {
                        say_bye(bus, id, io, ByeReason::ByeProtocol).await;
                        return false;
                    }
                }
            }
        }
    }
}

enum HelloVerdict {
    Ok { page_id: String, seq: u32 },
    Protocol,
}

fn hello_of(bytes: &[u8]) -> HelloVerdict {
    let Ok(frame) = codec::decode(bytes) else { return HelloVerdict::Protocol };
    if frame.module != 0
        || frame.sub != BusSub::Link as u32
        || frame.id != LinkId::Hello as u32
        || codec::kind_of(&frame) != Kind::Request
        || frame.seq == 0
    {
        return HelloVerdict::Protocol;
    }
    let Ok(hello) = v1::Hello::decode(frame.body.as_slice()) else { return HelloVerdict::Protocol };
    if hello.protocol != PROTOCOL {
        return HelloVerdict::Protocol;
    }
    HelloVerdict::Ok { page_id: hello.page_id, seq: frame.seq }
}

fn welcome_of(bus: &Bus, id: u64, reply_to: u32) -> Outgoing {
    let body = v1::Welcome {
        protocol: PROTOCOL,
        conn_id: id.to_string(),
        build: bus.stamp(),
        heartbeat_ms: crate::HEARTBEAT_MS,
        max_frame: crate::MAX_FRAME as u32,
        server_ms: codec::now_ms(),
    }
    .encode_to_vec();
    Outgoing {
        module: 0,
        sub: BusSub::Link as u32,
        id: LinkId::Hello as u32,
        kind: Kind::Response as i32,
        reply_to,
        key: String::new(),
        body,
        fault: None,
        origin: String::new(),
    }
}

async fn on_binary(bus: &Arc<Bus>, id: u64, bytes: Vec<u8>) -> Step {
    if bytes.len() > MAX_FRAME {
        return Step::Drop;
    }
    let Ok(frame) = codec::decode(&bytes) else { return Step::Bye(ByeReason::ByeProtocol) };
    if frame.sub == 0 || frame.id == 0 || frame.seq == 0 {
        return Step::Bye(ByeReason::ByeProtocol);
    }
    match codec::kind_of(&frame) {
        Kind::Event | Kind::Response | Kind::Unspecified => Step::Bye(ByeReason::ByeProtocol),
        Kind::Relay => on_relay(bus, id, &frame),
        Kind::Request => on_request(bus, id, &frame),
    }
}

fn on_relay(bus: &Bus, id: u64, frame: &Frame) -> Step {
    match bus.relay(id, frame.module, frame.sub, frame.id, &frame.key, &frame.body) {
        Ok(()) => Step::Continue,
        Err(()) => {
            let (code, message) = hub::not_relayable();
            bus.reply_fault(id, frame, code, message);
            Step::Continue
        }
    }
}

fn on_request(bus: &Arc<Bus>, id: u64, frame: &Frame) -> Step {
    if frame.module == 0 {
        return on_bus(bus, id, frame);
    }
    match bus.lookup(frame.module, frame.sub) {
        None => {
            let (code, message) = hub::unavailable();
            bus.reply_fault(id, frame, code, message);
            Step::Continue
        }
        Some((ids, _)) if !ids.contains(&frame.id) => {
            let (code, message) = hub::unknown();
            bus.reply_fault(id, frame, code, message);
            Step::Continue
        }
        Some((_, handler)) => {
            let call = Call {
                module: frame.module,
                sub: frame.sub,
                id: frame.id,
                key: frame.key.clone(),
                page_id: bus.page_id(id),
                body: bytes::Bytes::from(frame.body.clone()),
                bus: Arc::clone(bus),
            };
            let link = id;
            let module = frame.module;
            let sub = frame.sub;
            let msg = frame.id;
            let reply_to = frame.seq;
            let bus = Arc::clone(bus);
            tokio::spawn(async move {
                let answer = handler(call).await;
                bus.reply(link, module, sub, msg, reply_to, answer);
            });
            Step::Continue
        }
    }
}

fn on_bus(bus: &Bus, id: u64, frame: &Frame) -> Step {
    if frame.sub == BusSub::Link as u32 && frame.id == LinkId::Hello as u32 {
        return Step::Bye(ByeReason::ByeProtocol);
    }
    if frame.sub == BusSub::Link as u32 && frame.id == LinkId::Ping as u32 {
        return on_ping(bus, id, frame);
    }
    if frame.sub == BusSub::Topic as u32 && frame.id == TopicId::Subscribe as u32 {
        return on_subscribe(bus, id, frame, true);
    }
    if frame.sub == BusSub::Topic as u32 && frame.id == TopicId::Unsubscribe as u32 {
        return on_subscribe(bus, id, frame, false);
    }
    if frame.sub == BusSub::Link as u32 || frame.sub == BusSub::Topic as u32 {
        let (code, message) = hub::unknown();
        bus.reply_fault(id, frame, code, message);
        return Step::Continue;
    }
    let (code, message) = hub::unavailable();
    bus.reply_fault(id, frame, code, message);
    Step::Continue
}

fn on_ping(bus: &Bus, id: u64, frame: &Frame) -> Step {
    let Ok(ping) = v1::Ping::decode(frame.body.as_slice()) else {
        let (code, message) = hub::bad_body();
        bus.reply_fault(id, frame, code, message);
        return Step::Continue;
    };
    let body = v1::Pong { sent_ms: ping.sent_ms, server_ms: codec::now_ms() }.encode_to_vec();
    bus.push(id, &codec::response(0, frame.sub, frame.id, frame.seq, body, None));
    Step::Continue
}

fn on_subscribe(bus: &Bus, id: u64, frame: &Frame, subscribe: bool) -> Step {
    let Ok(topics) = v1::Topics::decode(frame.body.as_slice()) else {
        let (code, message) = hub::bad_body();
        bus.reply_fault(id, frame, code, message);
        return Step::Continue;
    };
    if subscribe && bus.subscribe(id, &topics.topics).is_err() {
        let (code, message) = hub::limited();
        bus.reply_fault(id, frame, code, message);
        return Step::Continue;
    }
    if !subscribe {
        bus.unsubscribe(id, &topics.topics);
    }
    bus.push(id, &codec::response(0, frame.sub, frame.id, frame.seq, Vec::new(), None));
    Step::Continue
}

async fn say_bye<I: Io>(bus: &Bus, id: u64, io: &mut I, reason: ByeReason) {
    if let Some(bytes) = bus.alloc(id, &hub::bye_out(reason)) {
        let _ = io.send(IoMsg::Binary(bytes)).await;
    }
    let _ = io.send(IoMsg::Close).await;
}

