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

//! Transport loss and recovery: a lost client brings its rules' ports down first; a retryable
//! failure reconnects with a capped, jittered exponential backoff.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::tunnel::types::{is_retryable, RuleDef, RuleState, TunnelError};
use super::TunnelManager;
use swiss_core::log;

/// Cap on reconnect backoff. A sustained outage backs off toward this interval rather than
/// dialing every `reconnectInterval` seconds forever (which, against a host that is truly down,
/// is thrash).
const RECONNECT_CAP_MS: f64 = 5.0 * 60.0 * 1000.0;
/// Clamp the exponent before the cap does, so 2 ** retries can't overflow on a long outage.
const RECONNECT_MAX_EXP: u32 = 8;

impl TunnelManager {
    /// A transport died under one or more running rules.
    ///
    /// Ports come down first, always: a rule reported "reconnecting" while still holding its
    /// port is the exact confusion this feature was built to remove. Synchronous — each rule's
    /// recovery is queued through its own op chain, exactly like the Node build's fire-and-forget
    /// `void this.onConnectionLost(...)`.
    pub(super) fn on_connection_lost(self: &Arc<Self>, conn_id: &str, err: &TunnelError) {
        let rules = self.with_store(|s| s.rules_for_connection(conn_id));
        for rule in rules {
            let rt = self.rt(&rule.id);
            {
                let data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                if data.state != RuleState::Up && data.state != RuleState::Starting {
                    continue;
                }
            }
            let weak = Arc::downgrade(self);
            let err = err.clone();
            let rule = rule.clone();
            tokio::spawn(async move {
                let Some(mgr) = weak.upgrade() else { return };
                let rt = mgr.rt(&rule.id);
                let _guard = rt.op.lock().await;
                // Re-check under the lock: a user stop may have won the race while this op waited.
                let still = {
                    let data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.state == RuleState::Up || data.state == RuleState::Starting
                };
                if !still {
                    return;
                }
                mgr.teardown(&rt).await;
                let retrying = rule.auto_reconnect && is_retryable(err.kind);
                {
                    let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
                    data.lost = true;
                    data.reason = Some(err.message.clone());
                    // An auth or host-key failure must not be retried: hammering the server every
                    // 10s earns a fail2ban ban, and a changed host key needs a human.
                    data.state = if retrying {
                        RuleState::Reconnecting
                    } else {
                        RuleState::Error
                    };
                }
                if retrying {
                    mgr.schedule_retry(&rule);
                }
                log::warn(
                    "tunnel down",
                    Some(json!({
                        "rule": rule.name,
                        "state": rt.data.lock().map(|d| d.state.as_str()).unwrap_or("stopped"),
                        "reason": err.message,
                    })),
                );
            });
        }
    }

    pub(super) fn schedule_retry(self: &Arc<Self>, rule: &RuleDef) {
        let rt = self.rt(&rule.id);
        self.clear_retry(&rt);
        let retries = {
            let mut data = rt.data.lock().unwrap_or_else(|e| e.into_inner());
            data.retries += 1;
            data.retries
        };
        let base = (rule.reconnect_interval.max(1) as f64) * 1000.0;
        // Exponential backoff capped at RECONNECT_CAP_MS: a sustained outage dials less and less
        // often instead of every `interval` seconds forever. ±20% jitter keeps rules on one host
        // from retrying in lockstep. (Auth/host-key never reach here; port failures no longer do
        // either — see is_retryable. Per-connection coalescing of sibling retries is a future
        // improvement; the SSH single-flight already collapses near-simultaneous dials.)
        let exp =
            (base * 2f64.powi((retries - 1).min(RECONNECT_MAX_EXP) as i32)).min(RECONNECT_CAP_MS);
        let delay = (exp * (0.8 + rand::random::<f64>() * 0.4)).round() as u64;
        let weak = Arc::downgrade(self);
        let id = rule.id.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let Some(mgr) = weak.upgrade() else { return };
            let rt = mgr.rt(&id);
            if let Ok(mut slot) = rt.retry.lock() {
                *slot = None; // the timer fired; a later clear must not abort the start itself
            }
            // State and reason are already recorded — the result of the retry lands there too.
            let _ = mgr.start_rule(&id).await;
        });
        let retry_slot = rt.retry.lock();
        if let Ok(mut slot) = retry_slot {
            *slot = Some(task);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::tunnel::manager::fake::Built;
    use crate::tunnel::manager::test_util::{
        add_conn, add_rule, built_len, echo_server, field, free_port, manager, manager_with, nth,
        port_is_free, round_trip, row_of, rule_input, scratch, squat, state_of, wait_until,
    };
    use crate::tunnel::store::RuleInput;
    use crate::tunnel::types::FailureKind;

    /// Every dial this manager made, across however many clients it built.
    fn total_dials(built: &Built) -> u32 {
        built.lock().unwrap().iter().map(|c| c.dials()).sum()
    }

    // --- transport loss ---------------------------------------------------------------------

    #[tokio::test]
    async fn releases_the_port_and_schedules_a_reconnect_when_auto_reconnect_is_on() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id)
                == "reconnecting")
            .await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        // The port must be free while the tunnel is down — this is the whole point.
        assert!(port_is_free(port).await);

        assert!(
            wait_until(Duration::from_secs(10), || state_of(&m, &r.id) == "up").await,
            "the retry never brought it back up"
        );
        assert!(!port_is_free(port).await);
        assert!(field(&row_of(&m, &r.id), "reconnectedAt").is_string());
        assert_eq!(round_trip(port, "back").await, "BACK");
        // The dead client was ended and released; the retry dialed a fresh one.
        assert_eq!(nth(&built, 0).ended(), 1);
        assert_eq!(built_len(&built), 2);
        m.close_all().await;
    }

    #[tokio::test]
    async fn does_not_schedule_a_reconnect_for_an_auth_failure() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Auth);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id) == "error").await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(port_is_free(port).await);
        // Past the interval: hammering an auth failure every 10s earns a fail2ban ban, so
        // nothing may retry — not even once.
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert_eq!(state_of(&m, &r.id), "error");
        assert_eq!(built_len(&built), 1, "a retry would have built a client");
        assert_eq!(nth(&built, 0).dials(), 1);
    }

    #[tokio::test]
    async fn leaves_a_rule_in_error_port_released_when_auto_reconnect_is_off() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(&store, rule_input("pg", &c.id, port, echo));

        m.start_rule(&r.id).await.expect("start");
        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || state_of(&m, &r.id) == "error").await,
            "state stayed {}",
            state_of(&m, &r.id)
        );
        assert!(port_is_free(port).await);
        let reason = field(&row_of(&m, &r.id), "reason")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(reason.contains("lost"), "{reason}");
    }

    #[tokio::test]
    async fn brings_every_rule_on_a_shared_connection_down_together() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let mut ports = Vec::new();
        let mut ids = Vec::new();
        for i in 0..2 {
            let port = free_port().await;
            ids.push(add_rule(&store, rule_input(&format!("r{i}"), &c.id, port, echo)).id);
            ports.push(port);
        }
        m.start_all().await;

        nth(&built, 0).die(FailureKind::Network);
        assert!(
            wait_until(Duration::from_secs(5), || ids
                .iter()
                .all(|id| state_of(&m, id) == "error"))
            .await,
            "one rule stayed up under a dead transport"
        );
        for port in ports {
            assert!(port_is_free(port).await);
        }
    }

    // --- reconnect policy -------------------------------------------------------------------

    #[tokio::test]
    async fn does_not_retry_a_port_failure() {
        let (_dir, store) = scratch();
        let (m, built) = manager(&store);
        let echo = echo_server().await;
        let c = add_conn(&store);
        let (squatter, port) = squat().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, echo)
            },
        );

        m.start_rule(&r.id).await.expect_err("the port is taken");
        assert_eq!(state_of(&m, &r.id), "error", "not 'reconnecting'");
        // Whatever the start itself spent (on Windows it reclaims what looks like its own stale
        // listener and tries once more), nothing may happen after the failure is reported: a
        // port held by someone else will not free itself, so retrying only thrashes — and walks
        // the TCP owner table each attempt. Wait well past the interval.
        let (clients, dials) = (built_len(&built), total_dials(&built));
        tokio::time::sleep(Duration::from_millis(1400)).await;
        assert_eq!(state_of(&m, &r.id), "error");
        assert_eq!(
            built_len(&built),
            clients,
            "a retry would have built a client"
        );
        assert_eq!(total_dials(&built), dials, "the failed start, nothing more");
        drop(squatter);
    }

    #[tokio::test(start_paused = true)]
    async fn spaces_network_reconnects_out_with_backoff_and_never_gives_up() {
        // On a paused clock: the retries below are scheduled against tokio's timer, so the
        // suite pays microseconds for what is a quarter of an hour of simulated outage.
        let (_dir, store) = scratch();
        let (m, built) = manager_with(
            &store,
            None,
            Some(TunnelError::new("host unreachable", FailureKind::Network)),
        );
        let c = add_conn(&store);
        let port = free_port().await;
        let r = add_rule(
            &store,
            RuleInput {
                auto_reconnect: true,
                reconnect_interval: 1.0,
                ..rule_input("pg", &c.id, port, 9)
            },
        );
        let _ = port;

        let t0 = tokio::time::Instant::now();
        m.start_rule(&r.id).await.expect_err("every dial fails");
        assert_eq!(state_of(&m, &r.id), "reconnecting");
        // A failed dial leaves the client in the map with no references, so every retry lands on
        // the same one: its dial count IS the number of attempts.
        assert!(
            wait_until(Duration::from_secs(20), || nth(&built, 0).dials() >= 5).await,
            "only {} attempts",
            nth(&built, 0).dials()
        );
        let elapsed = t0.elapsed();
        // 1s, 2s, 4s, 8s (±20% jitter) — at least 12s of simulated time for four retries. A flat
        // `reconnectInterval` loop would have spent 4s and dialed a host that is down 15 times.
        assert!(
            elapsed >= Duration::from_secs(11),
            "four retries took only {elapsed:?} — that is a storm, not backoff"
        );
        assert!(elapsed <= Duration::from_secs(25), "{elapsed:?}");
        assert_eq!(
            state_of(&m, &r.id),
            "reconnecting",
            "a network outage is retried forever"
        );
        m.close_all().await;
    }
}
