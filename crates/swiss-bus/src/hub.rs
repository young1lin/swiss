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

//! Links, topics and handlers (SPEC §bus.topics, SPEC §bus.requests).
//!
//! Publishing never waits. The lock order is the hub lock, then a link's queue
//! account: a publisher holds both only across a `try_send`, and the link task
//! never takes the hub lock while it holds the account.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{mpsc, watch, Notify};

use crate::codec::{self, Outgoing};
use crate::pb::v1::{self, ByeReason, Kind, Topic as ProtoTopic};
use crate::{
    relayable, BAD_BODY, LIMIT, MAX_LINKS, MAX_QUEUE_BYTES, MAX_QUEUE_FRAMES, MAX_TOPICS, NOT_RELAYABLE,
    UNAVAILABLE, UNKNOWN,
};
// Only a debug build restamps (spawn_restamp), so release and quick builds never read it.
#[cfg(debug_assertions)]
use crate::STAMP_INTERVAL;

/// The panel stamp WELCOME and BUILD carry. The composition injects it so this crate
/// does not depend on the panel (SPEC §bus.link).
pub type Stamp = Arc<dyn Fn() -> String + Send + Sync>;

pub type HandlerFut = Pin<Box<dyn Future<Output = Answer> + Send>>;
pub type Handler = Arc<dyn Fn(Call) -> HandlerFut + Send + Sync>;

/// What a handler is handed. `body` is still encoded: the hub does not name a
/// module's types (SPEC §bus.wire).
pub struct Call {
    pub module: u32,
    pub sub: u32,
    pub id: u32,
    pub key: String,
    pub page_id: String,
    pub body: bytes::Bytes,
    pub bus: Arc<Bus>,
}

/// A handler's answer. A fault has an empty body on the wire.
pub struct Answer {
    pub body: Vec<u8>,
    pub code: Option<String>,
    pub message: String,
    pub params: Vec<(String, String)>,
}

impl Answer {
    pub fn ok(body: Vec<u8>) -> Self {
        Self { body, code: None, message: String::new(), params: Vec::new() }
    }

    pub fn fault(code: &str, message: &str) -> Self {
        Self {
            body: Vec::new(),
            code: Some(code.to_owned()),
            message: message.to_owned(),
            params: Vec::new(),
        }
    }

    pub fn fault_with(code: &str, message: &str, params: &[(&str, &str)]) -> Self {
        Self {
            body: Vec::new(),
            code: Some(code.to_owned()),
            message: message.to_owned(),
            params: params.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
        }
    }
}

/// Drops the handler it registered. A later registration of the same sub wins;
/// the older guard then drops nothing (SPEC §bus.requests).
pub struct Registration {
    bus: Arc<Bus>,
    module: u32,
    sub: u32,
    generation: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.bus.unregister(self.module, self.sub, self.generation);
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Topic {
    module: u32,
    sub: u32,
    key: String,
}

struct Acct {
    frames: usize,
    bytes: usize,
    slow: bool,
}

struct Link {
    page_id: String,
    topics: HashSet<Topic>,
    next_seq: u32,
    tx: mpsc::Sender<Vec<u8>>,
    acct: Arc<Mutex<Acct>>,
    welcomed: bool,
}

struct HandlerSlot {
    generation: u64,
    ids: HashSet<u32>,
    handler: Handler,
}

struct Inner {
    links: HashMap<u64, Link>,
    next_link: u64,
    handlers: HashMap<(u32, u32), HandlerSlot>,
    next_generation: u64,
}

pub(crate) struct Opened {
    pub id: u64,
    pub outgoing: mpsc::Receiver<Vec<u8>>,
    pub guard: LinkGuard,
}

pub(crate) struct LinkGuard {
    bus: Arc<Bus>,
    id: u64,
}

impl Drop for LinkGuard {
    fn drop(&mut self) {
        self.bus.close(self.id);
    }
}

/// The hub. Cheap to clone: every handle is an `Arc`.
pub struct Bus {
    inner: Mutex<Inner>,
    stamp: Stamp,
    stop: watch::Sender<bool>,
    drained: Notify,
}

impl Bus {
    pub fn new(stamp: Stamp) -> Arc<Self> {
        let (stop, _) = watch::channel(false);
        let bus = Arc::new(Self {
            inner: Mutex::new(Inner {
                links: HashMap::new(),
                next_link: 1,
                handlers: HashMap::new(),
                next_generation: 1,
            }),
            stamp,
            stop,
            drained: Notify::new(),
        });
        bus.spawn_restamp();
        bus
    }

    /// The panel stamp WELCOME and BUILD carry. Injected by the composition so this
    /// crate does not depend on the panel (SPEC §bus.link).
    pub fn stamp(&self) -> String {
        (self.stamp)()
    }

    pub fn register(self: &Arc<Self>, module: u32, sub: u32, ids: &[u32], handler: Handler) -> Registration {
        let generation = {
            let mut inner = lock(&self.inner);
            let generation = inner.next_generation;
            inner.next_generation = inner.next_generation.wrapping_add(1);
            // Module 0 is the bus itself. A plugin does not serve it.
            if module != 0 && sub != 0 {
                inner.handlers.insert(
                    (module, sub),
                    HandlerSlot { generation, ids: ids.iter().copied().filter(|id| *id != 0).collect(), handler },
                );
            }
            generation
        };
        Registration { bus: Arc::clone(self), module, sub, generation }
    }

    fn unregister(&self, module: u32, sub: u32, generation: u64) {
        let mut inner = lock(&self.inner);
        let stale = inner.handlers.get(&(module, sub)).is_some_and(|slot| slot.generation != generation);
        if !stale {
            inner.handlers.remove(&(module, sub));
        }
    }

    /// Queue an event for every subscribed link. Never awaits: a link whose queue
    /// cannot take the frame is marked slow and ended by its own task (SPEC §bus.topics).
    pub fn publish(&self, module: u32, sub: u32, id: u32, key: &str, origin: &str, body: Vec<u8>) {
        if module == 0 || sub == 0 || id == 0 {
            return;
        }
        let targets: Vec<u64> = {
            let inner = lock(&self.inner);
            inner
                .links
                .iter()
                .filter(|(_, link)| link.welcomed && wants(link, module, sub, key))
                .map(|(id, _)| *id)
                .collect()
        };
        let out = codec::event(module, sub, id, key, origin, body);
        for id in targets {
            self.push(id, &out);
        }
    }

    /// Tell every open link the gateway is stopping, then wait until each has left.
    /// The wait is the caller's; a link that never reads its socket stays until the
    /// caller drops this future (SPEC §bus.link).
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        loop {
            let notified = self.drained.notified();
            if self.link_count() == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn subscribe_stop(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }

    pub(crate) fn try_open(self: &Arc<Self>) -> Option<Opened> {
        let mut inner = lock(&self.inner);
        if inner.links.len() >= MAX_LINKS {
            return None;
        }
        // Wider than the frame cap so the cap, not the channel, is what refuses.
        let (tx, outgoing) = mpsc::channel(MAX_QUEUE_FRAMES.saturating_mul(2).max(1));
        let id = inner.next_link;
        inner.next_link = inner.next_link.wrapping_add(1).max(1);
        inner.links.insert(
            id,
            Link {
                page_id: String::new(),
                topics: HashSet::new(),
                next_seq: 1,
                tx,
                acct: Arc::new(Mutex::new(Acct { frames: 0, bytes: 0, slow: false })),
                welcomed: false,
            },
        );
        Some(Opened { id, outgoing, guard: LinkGuard { bus: Arc::clone(self), id } })
    }

    fn close(&self, id: u64) {
        let removed = lock(&self.inner).links.remove(&id).is_some();
        if removed {
            self.drained.notify_waiters();
        }
    }

    fn link_count(&self) -> usize {
        lock(&self.inner).links.len()
    }

    pub(crate) fn set_page(&self, id: u64, page_id: String) {
        if let Some(link) = lock(&self.inner).links.get_mut(&id) {
            link.page_id = page_id;
        }
    }

    pub(crate) fn page_id(&self, id: u64) -> String {
        lock(&self.inner).links.get(&id).map(|link| link.page_id.clone()).unwrap_or_default()
    }

    /// WELCOME is the first frame queued, and nothing can pass it: the flag and the
    /// push share one hold of the lock.
    pub(crate) fn welcome(&self, id: u64, out: &Outgoing) -> bool {
        let mut inner = lock(&self.inner);
        {
            let Some(link) = inner.links.get_mut(&id) else { return false };
            link.welcomed = true;
        }
        push_held(&mut inner, id, out)
    }

    pub(crate) fn push(&self, id: u64, out: &Outgoing) -> bool {
        push_held(&mut lock(&self.inner), id, out)
    }

    /// A frame that must leave even when the queue is full (BYE). It takes the next
    /// seq and does not enter the queue.
    pub(crate) fn alloc(&self, id: u64, out: &Outgoing) -> Option<Vec<u8>> {
        let mut inner = lock(&self.inner);
        let link = inner.links.get_mut(&id)?;
        let seq = link.next_seq;
        link.next_seq = seq.saturating_add(1).max(seq);
        Some(codec::encode(seq, out))
    }

    pub(crate) fn is_slow(&self, id: u64) -> bool {
        let inner = lock(&self.inner);
        let Some(link) = inner.links.get(&id) else { return true };
        let slow = lock(&link.acct).slow;
        slow
    }

    pub(crate) fn note_sent(&self, id: u64, nbytes: usize) {
        let inner = lock(&self.inner);
        let Some(link) = inner.links.get(&id) else { return };
        let mut acct = lock(&link.acct);
        acct.frames = acct.frames.saturating_sub(1);
        acct.bytes = acct.bytes.saturating_sub(nbytes);
    }

    /// Apply a whole SUBSCRIBE or refuse it. Nothing is added on the refusal
    /// (SPEC §bus.wire).
    pub(crate) fn subscribe(&self, id: u64, topics: &[ProtoTopic]) -> Result<(), ()> {
        let mut inner = lock(&self.inner);
        let Some(link) = inner.links.get_mut(&id) else { return Err(()) };
        let mut next = link.topics.clone();
        for topic in topics {
            if topic.module == 0 || topic.sub == 0 {
                continue;
            }
            next.insert(Topic { module: topic.module, sub: topic.sub, key: topic.key.clone() });
        }
        if next.len() > MAX_TOPICS {
            return Err(());
        }
        link.topics = next;
        Ok(())
    }

    pub(crate) fn unsubscribe(&self, id: u64, topics: &[ProtoTopic]) {
        let mut inner = lock(&self.inner);
        let Some(link) = inner.links.get_mut(&id) else { return };
        for topic in topics {
            link.topics.remove(&Topic { module: topic.module, sub: topic.sub, key: topic.key.clone() });
        }
    }

    /// Forward a relay unread. `Err` means it is not on the relay list; the caller
    /// answers `bus.notRelayable`. The sender does not get a copy (SPEC §bus.wire).
    pub(crate) fn relay(&self, from: u64, module: u32, sub: u32, id: u32, key: &str, body: &[u8]) -> Result<(), ()> {
        if !relayable(module, sub) {
            return Err(());
        }
        let origin = self.page_id(from);
        let targets: Vec<u64> = {
            let inner = lock(&self.inner);
            inner
                .links
                .iter()
                .filter(|(id, link)| **id != from && link.welcomed && wants(link, module, sub, key))
                .map(|(id, _)| *id)
                .collect()
        };
        let out = Outgoing {
            module,
            sub,
            id,
            kind: Kind::Relay as i32,
            reply_to: 0,
            key: key.to_owned(),
            body: body.to_vec(),
            fault: None,
            origin,
        };
        for id in targets {
            self.push(id, &out);
        }
        Ok(())
    }

    pub(crate) fn lookup(&self, module: u32, sub: u32) -> Option<(HashSet<u32>, Handler)> {
        let inner = lock(&self.inner);
        inner.handlers.get(&(module, sub)).map(|slot| (slot.ids.clone(), Arc::clone(&slot.handler)))
    }

    pub(crate) fn reply(&self, id: u64, module: u32, sub: u32, msg: u32, reply_to: u32, answer: Answer) {
        let fault = answer.code.map(|code| {
            let mut fault = codec::fault(&code, &answer.message);
            for (key, value) in answer.params {
                fault.params.insert(key, value);
            }
            fault
        });
        self.push(id, &codec::response(module, sub, msg, reply_to, answer.body, fault));
    }

    pub(crate) fn reply_fault(&self, id: u64, frame: &v1::Frame, code: &str, message: &str) {
        self.reply(id, frame.module, frame.sub, frame.id, frame.seq, Answer::fault(code, message));
    }

    #[cfg(debug_assertions)]
    fn announce_build(&self, build: &str) {
        use prost::Message as _;
        let body = v1::Build { build: build.to_owned() }.encode_to_vec();
        let targets: Vec<u64> = {
            let inner = lock(&self.inner);
            inner.links.iter().filter(|(_, link)| link.welcomed).map(|(id, _)| *id).collect()
        };
        let out = codec::event(0, v1::BusSub::Link as u32, v1::LinkId::Build as u32, "", "", body);
        for id in targets {
            self.push(id, &out);
        }
    }

    fn spawn_restamp(self: &Arc<Self>) {
        // Release builds have a fixed stamp: a new build is a new process (SPEC §bus.link).
        #[cfg(debug_assertions)]
        {
            let Ok(handle) = tokio::runtime::Handle::try_current() else { return };
            let bus = Arc::clone(self);
            handle.spawn(async move {
                let mut last = bus.stamp();
                let mut stop = bus.subscribe_stop();
                loop {
                    tokio::select! {
                        _ = stop.changed() => break,
                        _ = tokio::time::sleep(STAMP_INTERVAL) => {
                            let now = bus.stamp();
                            if now != last {
                                last = now.clone();
                                bus.announce_build(&now);
                            }
                        }
                    }
                }
            });
        }
    }
}

fn push_held(inner: &mut Inner, id: u64, out: &Outgoing) -> bool {
    let (seq, tx, acct) = {
        let Some(link) = inner.links.get(&id) else { return false };
        (link.next_seq, link.tx.clone(), Arc::clone(&link.acct))
    };
    let bytes = codec::encode(seq, out);
    let nbytes = bytes.len();
    {
        let mut acct = lock(&acct);
        if acct.slow {
            return false;
        }
        // One frame that does not fit ends the link. It is not queued, and the
        // link task sends BYE: publishing itself never waits (SPEC §bus.topics).
        if acct.frames >= MAX_QUEUE_FRAMES || acct.bytes.saturating_add(nbytes) > MAX_QUEUE_BYTES {
            acct.slow = true;
            return false;
        }
        if tx.try_send(bytes).is_err() {
            acct.slow = true;
            return false;
        }
        acct.frames += 1;
        acct.bytes += nbytes;
    }
    if let Some(link) = inner.links.get_mut(&id) {
        link.next_seq = seq.saturating_add(1);
    }
    true
}

fn wants(link: &Link, module: u32, sub: u32, key: &str) -> bool {
    link.topics.iter().any(|topic| topic.module == module && topic.sub == sub && (topic.key.is_empty() || topic.key == key))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(crate) fn unavailable() -> (&'static str, &'static str) {
    (UNAVAILABLE, "the module that serves this request is not running")
}

pub(crate) fn unknown() -> (&'static str, &'static str) {
    (UNKNOWN, "no such message")
}

pub(crate) fn bad_body() -> (&'static str, &'static str) {
    (BAD_BODY, "the request body did not decode")
}

pub(crate) fn not_relayable() -> (&'static str, &'static str) {
    (NOT_RELAYABLE, "this message is not a relay")
}

pub(crate) fn limited() -> (&'static str, &'static str) {
    (LIMIT, "the limit for this connection was reached")
}

pub(crate) fn bye_out(reason: ByeReason) -> Outgoing {
    codec::event(0, v1::BusSub::Link as u32, v1::LinkId::Bye as u32, "", "", codec::bye_body(reason))
}

/// A frame the refuser can send without a slot: the newest link past the cap
/// (SPEC §bus.wire).
pub(crate) fn bye_limit_bytes() -> Vec<u8> {
    codec::encode(1, &bye_out(ByeReason::ByeLimit))
}
