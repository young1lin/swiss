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

//! A throttled notice (SPEC §bus.topics "Throttled notices").
//!
//! The first poke of a key publishes at once. Pokes inside the gap arm one trailing
//! publish at the gap's end, and nothing more: a notice carries no body, so a dropped
//! duplicate loses nothing and the trailing one still announces the last change.
//! Publishing never waits on a link. A key that stays quiet for a whole gap with
//! nothing armed is forgotten, so names do not accumulate.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use prost::Message;

use crate::pb::v1;
use crate::Bus;

struct Slot {
    /// The sleep that is in flight. A stale sleep, from an older gap, sees another gen.
    gen: u64,
    /// A poke arrived after the publish that opened this gap.
    trailing: bool,
}

struct Inner {
    bus: Arc<Bus>,
    module: u32,
    sub: u32,
    id: u32,
    gap: Duration,
    runtime: tokio::runtime::Handle,
    next: AtomicU64,
    keys: Mutex<HashMap<String, Slot>>,
}

/// Publishes empty `Notice`s for one (module, sub, id). Safe to call from any thread:
/// the runtime handle is the one captured at construction, so `poke` never needs to be
/// inside the runtime's context.
pub struct Notifier {
    inner: Arc<Inner>,
}

impl Notifier {
    pub fn new(bus: Arc<Bus>, module: u32, sub: u32, id: u32, gap: Duration) -> Arc<Self> {
        let runtime = tokio::runtime::Handle::try_current()
            .expect("a Notifier is built on the runtime that publishes");
        Arc::new(Self {
            inner: Arc::new(Inner {
                bus,
                module,
                sub,
                id,
                gap,
                runtime,
                next: AtomicU64::new(1),
                keys: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Ask for a notice on `key`. Never blocks.
    pub fn poke(&self, key: &str) {
        Arc::clone(&self.inner).poke(key);
    }

    /// Keys still remembered. A quiet key is dropped once its gap ends with nothing armed.
    #[cfg(test)]
    pub(crate) fn retained_keys(&self) -> usize {
        lock(&self.inner.keys).len()
    }
}

impl Inner {
    fn poke(self: &Arc<Self>, key: &str) {
        let gen = {
            let mut keys = lock(&self.keys);
            if let Some(slot) = keys.get_mut(key) {
                slot.trailing = true;
                None
            } else {
                let gen = self.next.fetch_add(1, Ordering::Relaxed);
                keys.insert(key.to_owned(), Slot { gen, trailing: false });
                Some(gen)
            }
        };
        let Some(gen) = gen else { return };
        self.publish(key);
        self.arm(key, gen);
    }

    fn publish(&self, key: &str) {
        let body = v1::Notice {}.encode_to_vec();
        self.bus.publish(self.module, self.sub, self.id, key, "", body);
    }

    fn arm(self: &Arc<Self>, key: &str, gen: u64) {
        let inner = Arc::clone(self);
        let key = key.to_owned();
        // The handle is cloned so the spawn does not borrow `inner`: the task moves it.
        let runtime = inner.runtime.clone();
        runtime.spawn(async move {
            tokio::time::sleep(inner.gap).await;
            inner.on_gap(&key, gen);
        });
    }

    fn on_gap(self: &Arc<Self>, key: &str, gen: u64) {
        let next = {
            let mut keys = lock(&self.keys);
            let Some(slot) = keys.get_mut(key) else { return };
            if slot.gen != gen {
                return;
            }
            if !slot.trailing {
                keys.remove(key);
                return;
            }
            slot.trailing = false;
            let next = self.next.fetch_add(1, Ordering::Relaxed);
            slot.gen = next;
            next
        };
        // The trailing publish opens the next gap. A poke that lands after the flag
        // was cleared, and before the sleep below is armed, sets `trailing` on this
        // slot, so it rides the gap we start here instead of publishing on its own.
        self.publish(key);
        self.arm(key, next);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}
