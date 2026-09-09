//! Per-plugin supervised resources — the PluginScope of docs/09 §4. A plugin may own any
//! number of tokio tasks, but never a DETACHED one: every task spawned through the scope is
//! tracked, hears the cancel signal, and is joined (then aborted) on shutdown. A stopped
//! plugin provably leaves nothing running, which is what "disable must really release" means
//! for background work.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

/// How long shutdown waits for each tracked task before aborting it. A task that misses this
/// window ignored the cancel signal — the abort is the host's promise that stop terminates,
/// not a suggestion.
const TASK_GRACE: Duration = Duration::from_secs(5);

pub struct PluginScope {
    plugin_id: String,
    /// Set to `true` when the host stops the plugin. Tasks `select!` on `changed()` of a
    /// `cancel_signal()` receiver and exit; no second cancellation mechanism is invented.
    cancel: tokio::sync::watch::Sender<bool>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl PluginScope {
    pub fn new(plugin_id: &str) -> Self {
        let (cancel, _) = tokio::sync::watch::channel(false);
        Self {
            plugin_id: plugin_id.to_string(),
            cancel,
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// A cloneable receiver for task loops: `select! { _ = rx.changed() => break, ... }`.
    pub fn cancel_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.cancel.subscribe()
    }

    /// Whether shutdown has been requested — for checks between await points.
    pub fn cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    /// Track one background task. It runs to completion (or until cancelled); shutdown joins
    /// it and aborts it if it overstays the grace. A panicked task is just reaped — a plugin
    /// task must not take the host down (the one-failure-isolates rule).
    pub fn spawn(&self, fut: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(fut);
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.push(handle);
        }
    }

    /// Cancel, wait out the grace, and abort whatever is left. Consumes the scope: after
    /// this, nothing the plugin spawned through it can still be polled.
    pub async fn shutdown(self) {
        let _ = self.cancel.send(true);
        let handles = self
            .tasks
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default();
        for mut handle in handles {
            // JoinHandle is Unpin, so a `&mut` await works; the handle survives the timeout
            // and can still abort the task that ignored the signal.
            if tokio::time::timeout(TASK_GRACE, &mut handle).await.is_err() {
                handle.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn a_tracked_task_stops_when_the_scope_shuts_down() {
        let scope = PluginScope::new("test");
        let rx = scope.cancel_signal();
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        scope.spawn(async move {
            let mut rx = rx;
            loop {
                tokio::select! {
                    changed = rx.changed() => {
                        if changed.is_err() || *rx.borrow() {
                            flag.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
            }
        });
        assert!(!scope.cancelled());
        scope.shutdown().await;
        assert!(done.load(Ordering::SeqCst), "the task observed the cancel signal");
    }

    #[tokio::test]
    async fn a_panicked_task_does_not_break_shutdown() {
        // One plugin's task panicking must not take the shutdown (or the host) down: the
        // join result is deliberately ignored, and shutdown still terminates.
        let scope = PluginScope::new("panicky");
        scope.spawn(async { panic!("a plugin task died"); });
        scope.spawn(async { /* finishes immediately */ });
        tokio::time::sleep(Duration::from_millis(20)).await; // let the panic land
        scope.shutdown().await;
    }
}
