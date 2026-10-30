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

//! The one list of the gateway's own children (SPEC §process.children).
//!
//! Runs, proc MCPs and local terminals each already knew how to stop their own child —
//! cancel the run, stop the MCP, close the session. What none of them had was a shared
//! place where an operator can SEE every child and stop one through its owner's path:
//! killing a pid underneath its owner leaves the owner's state lying (a run that never
//! reaches its terminal state, an MCP that still reads as started). The registry is that
//! place. Managed apps are NOT children — they outlive the gateway on purpose and are
//! never registered here (§process.detached).
//!
//! Registration returns a guard, not an unregister call: a panic between the two would
//! leave a stale row pointing at a pid the OS may have handed to someone else, so the
//! row's lifetime is tied to the child handle's owner by Drop, the way the supervisor's
//! old PidGuard worked — now for every kind of child, not just runs.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use swiss_core::platform::{process_created_at, process_name};

/// Which subsystem owns a child row; the panel groups by this (SPEC §process.panel).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChildKind {
    Run,
    Mcp,
    Terminal,
    /// An agent runtime the gateway spawned as a tool (the memory sync runner):
    /// a real child that ends with the gateway, not a managed app.
    Agent,
}

impl ChildKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChildKind::Run => "run",
            ChildKind::Mcp => "mcp",
            ChildKind::Terminal => "terminal",
            ChildKind::Agent => "agent",
        }
    }
}

/// The owner's own teardown path for one child (SPEC §process.children): the run's
/// coordinator cancel, the MCP lifecycle stop, the terminal session close. Returning
/// Err names why the stop failed; the answer route shows that sentence.
pub trait ChildStop: Send + Sync {
    fn stop(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

/// A ChildStop from a closure — the shape every owner that already HAS a stop function
/// needs, without a named struct per owner.
struct FnChildStop<F>(F);

impl<F, Fut> ChildStop for FnChildStop<F>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    fn stop(&self) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> {
        Box::pin((self.0)())
    }
}

/// Wrap a stop closure as a hook. `Fut` must be `'static` — clone what the future needs
/// inside the closure, the way the run hook clones its weak coordinator handle.
pub fn child_stop_fn<F, Fut>(f: F) -> Arc<dyn ChildStop>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    Arc::new(FnChildStop(f))
}

/// What a spawn carries into the registry (SPEC §process.children): the owner's identity,
/// the row's label, and the hook that reaches the owner's own stop. Built by the run
/// coordinator for runs; a direct supervisor caller has none and registers as the host.
#[derive(Clone)]
pub struct ChildTag {
    pub owner: String,
    pub label: String,
    pub stop: Option<Arc<dyn ChildStop>>,
}

impl ChildTag {
    /// The untagged shape (SPEC §process.children): a supervisor spawn by a direct caller.
    pub fn host(program: &str) -> Self {
        ChildTag {
            owner: "host".to_string(),
            label: program.to_string(),
            stop: None,
        }
    }
}

/// Debug without the hook: the hook is a closure chain whose identity says nothing.
impl std::fmt::Debug for ChildTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildTag")
            .field("owner", &self.owner)
            .field("label", &self.label)
            .field("has_stop", &self.stop.is_some())
            .finish()
    }
}

/// One registration.
pub struct ChildSpec {
    pub pid: u32,
    pub owner: String,
    pub kind: ChildKind,
    pub label: String,
    pub stop: Option<Arc<dyn ChildStop>>,
}

/// The stored row. The creation time and image are read ONCE at registration — they are
/// the row's identity for its whole life, so a later pid reuse cannot silently repaint a
/// row into someone else's process.
pub struct ChildEntry {
    pub pid: u32,
    pub created_at: Option<u64>,
    pub image: Option<String>,
    pub owner: String,
    pub kind: ChildKind,
    pub label: String,
    pub registered_at: u64,
    stop: Option<Arc<dyn ChildStop>>,
}

/// What the stop route needs to know about one pid (SPEC §process.children): the 404, the
/// 409 and the hook, decided by the registry so no route re-derives them.
pub enum StopLookup {
    Unknown,
    NoHook,
    Hook(Arc<dyn ChildStop>),
}

impl std::fmt::Debug for StopLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StopLookup::Unknown => f.write_str("Unknown"),
            StopLookup::NoHook => f.write_str("NoHook"),
            StopLookup::Hook(_) => f.write_str("Hook(..)"),
        }
    }
}

/// The public answer of `GET /api/process/children` minus the memory column, which the
/// reader adds (a row without a reading shows none, never 0).
pub struct ChildRow {
    pub pid: u32,
    pub owner: String,
    pub kind: ChildKind,
    pub label: String,
    pub uptime_sec: u64,
    pub stoppable: bool,
}

/// The registry RuntimeServices holds (SPEC §process.children). Cheap: one mutex over a
/// map that holds at most a few dozen rows.
pub struct ChildRegistry {
    entries: Mutex<HashMap<u32, ChildEntry>>,
}

fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ChildRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(ChildRegistry {
            entries: Mutex::new(HashMap::new()),
        })
    }

    /// Register one child and take the guard that removes the row. `None` for a pid of 0
    /// (a spawn that handed back no pid) — there is no process to speak of.
    pub fn register(self: &Arc<Self>, spec: ChildSpec) -> Option<ChildGuard> {
        if spec.pid == 0 {
            return None;
        }
        let entry = ChildEntry {
            pid: spec.pid,
            created_at: process_created_at(spec.pid),
            image: process_name(spec.pid),
            owner: spec.owner,
            kind: spec.kind,
            label: spec.label,
            registered_at: wall_now_ms(),
            stop: spec.stop,
        };
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec.pid, entry);
        Some(ChildGuard {
            pid: spec.pid,
            registry: Some(Arc::clone(self)),
        })
    }

    /// An owner's rename reaches its already-registered row (SPEC §process.children):
    /// the label follows the name the owner carries now, so a row never answers to
    /// a name its owner no longer has. A pid without a row is a no-op - the child
    /// was not live at the rename, nothing to relabel.
    pub fn relabel(&self, pid: u32, label: &str) {
        if let Some(entry) = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&pid)
        {
            entry.label = label.to_string();
        }
    }

    /// The memory reading's roots (SPEC §host.memory): one pid per subtree to measure.
    /// Order is unspecified; the reader sorts.
    pub fn roots(&self) -> Vec<u32> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .copied()
            .collect()
    }

    /// Every row, ordered by owner then label (SPEC §process.children) — the order the
    /// route answers in and the panel groups by.
    pub fn rows(&self) -> Vec<ChildRow> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now_ms = wall_now_ms();
        let mut rows: Vec<ChildRow> = entries
            .values()
            .map(|entry| ChildRow {
                pid: entry.pid,
                owner: entry.owner.clone(),
                kind: entry.kind,
                label: entry.label.clone(),
                uptime_sec: now_ms.saturating_sub(entry.registered_at) / 1000,
                stoppable: entry.stop.is_some(),
            })
            .collect();
        rows.sort_by(|a, b| a.owner.cmp(&b.owner).then_with(|| a.label.cmp(&b.label)));
        rows
    }

    /// The stop lookup for one pid (SPEC §process.children): Unknown → 404, NoHook → 409.
    pub fn stop(&self, pid: u32) -> StopLookup {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get(&pid) {
            None => StopLookup::Unknown,
            Some(entry) => match entry.stop.clone() {
                Some(hook) => StopLookup::Hook(hook),
                None => StopLookup::NoHook,
            },
        }
    }

    /// How many rows — the poll and the panel's badge.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn remove(&self, pid: u32) {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
    }
}

/// Removes the row on drop — the panic-safe unregister (SPEC §process.children). Held by
/// whoever owns the child: the supervisor's run future, the proc adapter's LiveChild, the
/// PTY reader thread.
pub struct ChildGuard {
    pid: u32,
    registry: Option<Arc<ChildRegistry>>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            registry.remove(self.pid);
        }
    }
}

#[cfg(test)]
mod tests {
    //! The registry's own contracts (SPEC §process.children): the guard's drop-unregister
    //! (including through a panic) and stop-through-the-owner's-hook. Real children are
    //! exercised end to end by swiss-it's detached group; here the rows are fake pids,
    //! which is exactly what the registry itself sees.
    use super::*;
    use std::sync::atomic::Ordering;

    fn spec(pid: u32, owner: &str, label: &str) -> ChildSpec {
        ChildSpec {
            pid,
            owner: owner.to_string(),
            kind: ChildKind::Run,
            label: label.to_string(),
            stop: None,
        }
    }

    #[test]
    fn a_dropped_guard_unregisters_even_on_panic() {
        let registry = ChildRegistry::new();
        {
            let _guard = registry.register(spec(4242, "jobs", "sleep")).unwrap();
            assert_eq!(registry.len(), 1);
            assert_eq!(registry.roots(), vec![4242]);
            // A panic in the code between register and the natural end of the scope must
            // not leave the row: the guard unwinds with the stack.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _held = registry.register(spec(4243, "panel", "x")).unwrap();
                panic!("the owner's code went wrong");
            }));
            assert!(result.is_err());
            assert_eq!(registry.len(), 1, "only the still-held row survives");
        }
        assert!(registry.is_empty(), "the scope end unregisters the held row");
        assert!(registry.roots().is_empty());
    }

    #[tokio::test]
    async fn stop_goes_through_the_owners_hook() {
        let registry = ChildRegistry::new();
        let flipped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&flipped);
        let mut with_hook = spec(8100, "mcp", "a-proc-server");
        with_hook.kind = ChildKind::Mcp;
        with_hook.stop = Some(child_stop_fn(move || {
            let flag = Arc::clone(&flag);
            async move {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }
        }));
        let _guard = registry.register(with_hook).unwrap();
        let _no_hook = registry.register(spec(8101, "host", "a direct caller")).unwrap();

        match registry.stop(8100) {
            StopLookup::Hook(hook) => {
                hook.stop().await.expect("the owner's stop ran");
                assert!(flipped.load(Ordering::SeqCst), "the hook was the owner's own");
            }
            other => panic!("the tagged row must carry its hook, got {other:?}"),
        }
        assert!(matches!(registry.stop(8101), StopLookup::NoHook));
        assert!(matches!(registry.stop(9_999), StopLookup::Unknown));
    }
}
