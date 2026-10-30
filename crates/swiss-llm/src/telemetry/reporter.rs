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

//! The exporter: a bounded queue the sessions push into and one task that drains it.
//!
//! A run never waits on the collector - a push is a lock and a `VecDeque` append - and a
//! collector that is down never grows memory without bound: past [QUEUE_MAX] records the
//! oldest go first, counted. The task exports every [FLUSH_EVERY] or as soon as a batch
//! fills; a retryable failure keeps the batch and backs off (1s doubling to a minute), any
//! other failure drops it - it would fail again unchanged. `api_request` records carry a
//! unique `request_id`, so a batch the collector stored before its answer was lost is
//! deduplicated on the retry rather than counted twice.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{watch, Notify};

use super::otlp::LogRecord;
use super::transport::Wire;
use super::{Session, TelemetryConfig};

/// The most records held while the collector is away.
pub const QUEUE_MAX: usize = 4096;
/// The most records one export carries.
pub const BATCH_MAX: usize = 512;
/// How often a quiet queue is flushed (Claude Code's own logs interval).
const FLUSH_EVERY: Duration = Duration::from_secs(5);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// The last flush on shutdown may take this long; the host's grace is longer.
const FINAL_FLUSH: Duration = Duration::from_secs(2);

/// What the panel shows of the exporter.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryStatus {
    pub enabled: bool,
    pub protocol: Option<&'static str>,
    pub endpoint: Option<String>,
    /// Records waiting for the next export.
    pub queued: usize,
    /// Records the collector accepted since the exporter started.
    pub sent: u64,
    /// Records given up on: the queue overflowed, or an export failed for good.
    pub dropped: u64,
    /// Records the collector took delivery of but refused (an OTLP partial success).
    pub rejected: u64,
    pub last_export_ms: Option<u64>,
    pub last_error: Option<String>,
    pub last_error_ms: Option<u64>,
}

#[derive(Default)]
struct Inner {
    /// The enabled block; None while telemetry is off (nothing is queued then).
    config: Option<TelemetryConfig>,
    /// Bumped on every reconfigure, so the task rebuilds its client.
    generation: u64,
    queue: VecDeque<LogRecord>,
    status: TelemetryStatus,
}

/// The shared queue and its exporter.
#[derive(Default)]
pub struct Reporter {
    inner: Mutex<Inner>,
    wake: Notify,
}

/// What one export attempt came to.
#[derive(Debug, PartialEq)]
enum Step {
    /// Nothing to send (or telemetry is off).
    Idle,
    /// Sent; `more` when a full batch is still waiting.
    Sent { more: bool },
    Failed,
}

impl Reporter {
    pub fn new() -> Arc<Reporter> {
        Arc::new(Reporter::default())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Take a new block. Off (None, or `enabled: false`) drops whatever is queued: those
    /// records were meant for a collector the operator just turned away from.
    pub fn configure(&self, config: Option<&TelemetryConfig>) {
        let next = config.filter(|c| c.enabled).cloned();
        let mut inner = self.lock();
        if inner.config == next {
            return;
        }
        inner.generation += 1;
        inner.status.last_error = None;
        inner.status.last_error_ms = None;
        if next.is_none() {
            inner.queue.clear();
        }
        inner.status.enabled = next.is_some();
        inner.status.protocol = next.as_ref().map(|c| c.protocol.as_str());
        inner.status.endpoint = next.as_ref().map(|c| c.endpoint.clone());
        inner.config = next;
        drop(inner);
        self.wake.notify_one();
    }

    /// A session for one run: reporting while telemetry is on, disabled otherwise.
    pub fn session(self: &Arc<Self>, agent: &str, consumer: &str) -> Session {
        let log_prompts = match &self.lock().config {
            Some(config) => config.log_prompts,
            None => return Session::disabled(),
        };
        Session::new(self.clone(), agent, consumer, log_prompts)
    }

    pub(crate) fn push(&self, record: LogRecord) {
        let mut inner = self.lock();
        if inner.config.is_none() {
            return;
        }
        if inner.queue.len() >= QUEUE_MAX {
            inner.queue.pop_front();
            inner.status.dropped += 1;
        }
        inner.queue.push_back(record);
        let full = inner.queue.len() >= BATCH_MAX;
        drop(inner);
        if full {
            self.wake.notify_one();
        }
    }

    pub fn status(&self) -> TelemetryStatus {
        let inner = self.lock();
        TelemetryStatus { queued: inner.queue.len(), ..inner.status.clone() }
    }

    /// The export task: runs until the scope's cancel signal, then flushes once.
    pub async fn run(self: Arc<Self>, mut cancel: watch::Receiver<bool>) {
        let mut wire: Option<(u64, Wire)> = None;
        let mut backoff = Duration::ZERO;
        loop {
            let step = self.export_once(&mut wire).await;
            let wait = match step {
                Step::Sent { more: true } => {
                    backoff = Duration::ZERO;
                    Duration::ZERO
                }
                Step::Sent { more: false } | Step::Idle => {
                    backoff = Duration::ZERO;
                    FLUSH_EVERY
                }
                Step::Failed => {
                    backoff = (backoff * 2).clamp(BACKOFF_MIN, BACKOFF_MAX);
                    backoff
                }
            };
            let failing = step == Step::Failed;
            tokio::select! {
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        if !failing {
                            let _ = tokio::time::timeout(FINAL_FLUSH, self.export_once(&mut wire)).await;
                        }
                        return;
                    }
                }
                // A full batch wakes the task early - unless it is backing off.
                _ = self.wake.notified(), if !failing => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    async fn export_once(&self, wire: &mut Option<(u64, Wire)>) -> Step {
        let (generation, config, batch) = {
            let mut inner = self.lock();
            let Some(config) = inner.config.clone() else { return Step::Idle };
            if inner.queue.is_empty() {
                return Step::Idle;
            }
            let n = inner.queue.len().min(BATCH_MAX);
            let batch: Vec<LogRecord> = inner.queue.drain(..n).collect();
            (inner.generation, config, batch)
        };
        if wire.as_ref().map(|(g, _)| *g) != Some(generation) {
            match Wire::build(&config) {
                Ok(built) => *wire = Some((generation, built)),
                Err(message) => {
                    // A header reference the vault does not hold yet: keep the records, it
                    // may gain the name.
                    self.settle_failure(generation, batch, message, true);
                    return Step::Failed;
                }
            }
        }
        let Some((_, client)) = wire.as_ref() else { return Step::Failed };
        let count = batch.len();
        let mut request = config.request(batch);
        match client.export(&request).await {
            Ok(accepted) => {
                let mut inner = self.lock();
                let rejected = accepted.rejected.clamp(0, count as i64) as u64;
                inner.status.sent += count as u64 - rejected;
                inner.status.rejected += rejected;
                inner.status.last_export_ms = Some(swiss_core::util::now_ms());
                if rejected > 0 {
                    inner.status.last_error = Some(format!("the collector refused {rejected} of {count} records: {}", accepted.message));
                    inner.status.last_error_ms = inner.status.last_export_ms;
                }
                Step::Sent { more: inner.queue.len() >= BATCH_MAX }
            }
            Err(err) => {
                let records = std::mem::take(&mut request.resource_logs[0].scope_logs[0].log_records);
                self.settle_failure(generation, records, err.message, err.retryable);
                Step::Failed
            }
        }
    }

    /// File a failed batch: a retryable one goes back to the front of the queue (oldest
    /// dropped past the cap), any other is counted as dropped.
    fn settle_failure(&self, generation: u64, records: Vec<LogRecord>, message: String, retryable: bool) {
        let mut inner = self.lock();
        if inner.generation == generation {
            inner.status.last_error = Some(message);
            inner.status.last_error_ms = Some(swiss_core::util::now_ms());
        }
        if retryable && inner.config.is_some() {
            for record in records.into_iter().rev() {
                inner.queue.push_front(record);
            }
            while inner.queue.len() > QUEUE_MAX {
                inner.queue.pop_front();
                inner.status.dropped += 1;
            }
        } else {
            inner.status.dropped += records.len() as u64;
        }
    }

    #[cfg(test)]
    pub(crate) fn drain_for_test(&self) -> Vec<LogRecord> {
        self.lock().queue.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::parse;
    use serde_json::json;

    fn record() -> LogRecord {
        LogRecord { event_name: "user_prompt".into(), ..LogRecord::default() }
    }

    fn configured(endpoint: &str, protocol: &str) -> Arc<Reporter> {
        let reporter = Reporter::new();
        let cfg = parse(Some(&json!({ "protocol": protocol, "endpoint": endpoint }))).expect("parse");
        reporter.configure(cfg.as_ref());
        reporter
    }

    #[test]
    fn the_queue_is_bounded_and_off_drops_it() {
        let reporter = configured("http://127.0.0.1:9", "grpc");
        for _ in 0..QUEUE_MAX + 3 {
            reporter.push(record());
        }
        let status = reporter.status();
        assert_eq!((status.queued, status.dropped, status.enabled), (QUEUE_MAX, 3, true));
        reporter.configure(None);
        let status = reporter.status();
        assert_eq!((status.queued, status.enabled), (0, false));
        reporter.push(record());
        assert_eq!(reporter.status().queued, 0, "off queues nothing");
        assert!(!reporter.session("a", "c").reporting());
    }

    #[tokio::test]
    async fn an_export_sends_the_batch_and_counts_it() {
        let (url, server) = crate::provider::tests::fake_server(200, "{}".into()).await;
        let reporter = configured(&url, "http/json");
        let session = reporter.session("distiller", "memory.sync");
        session.prompt("go");
        let mut wire = None;
        assert_eq!(reporter.export_once(&mut wire).await, Step::Sent { more: false });
        let status = reporter.status();
        assert_eq!((status.sent, status.queued, status.last_error.as_deref()), (1, 0, None));
        let (_, body) = server.await.expect("server");
        let sent: serde_json::Value = serde_json::from_str(&body).expect("json");
        let resource = &sent["resourceLogs"][0]["resource"]["attributes"];
        assert_eq!(resource[0]["value"]["stringValue"], "swiss");
        let record = &sent["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(record["body"]["stringValue"], "claude_code.user_prompt");
        assert_eq!(reporter.export_once(&mut wire).await, Step::Idle);
    }

    #[tokio::test]
    async fn a_retryable_failure_keeps_the_batch_and_a_final_one_drops_it() {
        let reporter = configured("http://127.0.0.1:9", "http/protobuf");
        reporter.push(record());
        reporter.push(record());
        let mut wire = None;
        assert_eq!(reporter.export_once(&mut wire).await, Step::Failed);
        let status = reporter.status();
        assert_eq!((status.queued, status.dropped), (2, 0));
        assert!(status.last_error.is_some());

        let (url, _server) = crate::provider::tests::fake_server(400, r#"{"message":"bad"}"#.into()).await;
        let reporter = configured(&url, "http/protobuf");
        reporter.push(record());
        let mut wire = None;
        assert_eq!(reporter.export_once(&mut wire).await, Step::Failed);
        let status = reporter.status();
        assert_eq!((status.queued, status.dropped), (0, 1));
        assert!(status.last_error.expect("error").contains("400"));
    }

    #[tokio::test]
    async fn the_task_flushes_on_cancel() {
        let (url, server) = crate::provider::tests::fake_server(200, "{}".into()).await;
        let reporter = configured(&url, "http/json");
        let (stop, cancel) = watch::channel(false);
        let task = tokio::spawn(reporter.clone().run(cancel));
        // The first pass finds nothing; the record lands while the task sleeps.
        tokio::time::sleep(Duration::from_millis(50)).await;
        reporter.push(record());
        stop.send(true).expect("cancel");
        task.await.expect("task");
        assert_eq!(reporter.status().sent, 1);
        let (_, body) = server.await.expect("server");
        assert!(body.contains("logRecords"), "{body}");
    }
}
