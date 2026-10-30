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

//! The hourly maintenance seat (SPEC host.history): a module with an open history
//! database registers ONE hook on start and withdraws it on stop, and the server's
//! retention timer runs every registered hook each hour - the bounded maintenance
//! pass (checkpoint, conditional VACUUM, ledger reconciliation) and the age sweep.
//! The seat outlives plugin instances exactly like the history-query seat: a module
//! that is stopped holds no hook, so its closed database is never touched.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// What a module registers: a closure that starts one maintenance pass. Cheap to
/// clone; it must never open anything - a stopped module simply never registers.
pub type MaintenanceHook =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

pub struct MaintenanceSeat {
    hooks: std::sync::RwLock<BTreeMap<String, MaintenanceHook>>,
}

impl MaintenanceSeat {
    pub fn new() -> Self {
        MaintenanceSeat {
            hooks: std::sync::RwLock::new(BTreeMap::new()),
        }
    }

    /// Register (or replace) one module's hook. Idempotent per id.
    pub fn register(&self, id: &str, hook: MaintenanceHook) {
        self.hooks
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), hook);
    }

    /// Withdraw a module's hook (its stop). Idempotent.
    pub fn unregister(&self, id: &str) {
        self.hooks
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// Run every registered hook once, awaiting each in turn: maintenance is bounded,
    /// not parallel - one owner's VACUUM must not race another's checkpoint.
    /// Returns how many hooks ran.
    pub async fn run_all(&self) -> usize {
        let hooks: Vec<MaintenanceHook> = {
            let map = self.hooks.read().unwrap_or_else(|e| e.into_inner());
            map.values().cloned().collect()
        };
        for hook in &hooks {
            // One panicking hook must not take down the hourly loop (and every
            // other owner's maintenance with it): isolate, log, carry on.
            let ran = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                (hook)(),
            ))
            .await;
            if let Err(panic) = ran {
                let reason = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "an unknown panic".to_string());
                swiss_core::log::error(
                    "a maintenance hook panicked; the pass continues",
                    Some(serde_json::json!({ "reason": reason })),
                );
            }
        }
        hooks.len()
    }
}

impl Default for MaintenanceSeat {
    fn default() -> Self {
        Self::new()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A counting hook: how many times the seat ran it.
    fn counter() -> (MaintenanceHook, Arc<std::sync::atomic::AtomicUsize>) {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits_for_hook = hits.clone();
        let hook: MaintenanceHook = Arc::new(move || {
            let h = hits_for_hook.clone();
            Box::pin(async move {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        (hook, hits)
    }

    #[tokio::test]
    async fn the_seat_runs_every_registered_hook_and_counts_them() {
        let seat = MaintenanceSeat::new();
        let (hook_a, hits_a) = counter();
        let (hook_b, hits_b) = counter();
        seat.register("mcp", hook_a);
        seat.register("remote", hook_b);
        let ran = seat.run_all().await;
        assert_eq!(ran, 2, "both registered hooks ran");
        assert_eq!(hits_a.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(hits_b.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A second pass runs them again - the seat is a timer, not a one-shot.
        assert_eq!(seat.run_all().await, 2);
        assert_eq!(hits_a.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_unregistered_hook_is_never_run_again() {
        let seat = MaintenanceSeat::new();
        let (hook, hits) = counter();
        seat.register("mcp", hook);
        assert_eq!(seat.run_all().await, 1);
        // A stopped module withdraws its hook: the next hourly pass must not touch
        // its closed database.
        seat.unregister("mcp");
        assert_eq!(seat.run_all().await, 0, "the withdrawn module holds no hook");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Unregister is idempotent; registering the same id replaces, not stacks.
        seat.unregister("mcp");
        seat.unregister("never-registered");
        assert_eq!(seat.run_all().await, 0);
    }
}
