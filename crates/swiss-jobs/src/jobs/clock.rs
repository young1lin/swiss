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

//! The clock the scheduler reads (docs/11 §6.6).
//!
//! DST and suspend-resume tests must not depend on the machine the test run happens to
//! sit in. Everything time-shaped in the scheduler goes through [Clock]: the real one is
//! chrono::Local, tests inject a fake with a synthetic DST rule so the same assertions
//! pass in any machine timezone. No new dependency - no `chrono-tz`, ever (docs/11 §10).

use chrono::TimeZone;

/// Milliseconds since the unix epoch, local wall time, and back. `ms_from_local` is
/// None exactly when the wall-clock time does not exist (the spring-forward gap).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
    fn local_from_ms(&self, ms: i64) -> chrono::NaiveDateTime;
    fn ms_from_local(&self, t: &chrono::NaiveDateTime) -> Option<i64>;
}

/// Truncate a naive time to its minute. Cron works in whole local minutes; walks that
/// start from a second-carrying instant (a lastRunAt at :05) must not drag the seconds
/// into every matching minute they visit.
pub(crate) fn minute_floor(t: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    use chrono::Timelike;
    let secs = chrono::TimeDelta::try_seconds(i64::from(t.second())).unwrap_or_default();
    t - secs
}

/// Local wall time for a unix-ms stamp (None when out of range).
pub(crate) fn local_minute_of_ms(ms: i64) -> Option<chrono::NaiveDateTime> {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.naive_local())
}

/// The production clock: the machine's local time, the same source chrono::Local is.
pub struct LocalClock;

impl Clock for LocalClock {
    fn now_ms(&self) -> i64 {
        swiss_core::util::now_ms() as i64
    }

    fn local_from_ms(&self, ms: i64) -> chrono::NaiveDateTime {
        local_minute_of_ms(ms).unwrap_or_default()
    }

    fn ms_from_local(&self, t: &chrono::NaiveDateTime) -> Option<i64> {
        chrono::Local
            .from_local_datetime(t)
            .single()
            .map(|dt| dt.timestamp_millis())
    }
}
#[cfg(test)]
pub(crate) mod testing {
    use super::Clock;
    use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
    use std::sync::Arc;

    /// The injected clock (docs/11 §6.6). Local time is UTC plus a piecewise offset:
    /// +60 minutes normally, +120 minutes inside [spring_forward_ms, fall_back_ms) - so
    /// on the spring day the local minutes [spring+1h, spring+2h) DO NOT EXIST, and on
    /// the fall day the local minutes [fall+1h, fall+2h) HAPPEN TWICE. The same
    /// assertions then run on any machine timezone, against a zone the test owns.
    ///
    /// Default spring/fall sit far from any test's window (a straight +60 zone); the
    /// DST tests pass their own instants. `local_calls` counts local_from_ms
    /// invocations - the next-due budget test's counter (docs/11 §6.5).
    pub struct FakeClock {
        pub now_ms: AtomicI64,
        pub spring_forward_ms: i64,
        pub fall_back_ms: i64,
        pub local_calls: AtomicU64,
    }

    const HOUR: i64 = 60 * 60 * 1000;

    impl FakeClock {
        /// A straight UTC+60 zone (no transition anywhere near a sane test window).
        pub fn at(now_ms: i64) -> Arc<Self> {
            Arc::new(Self::with_rule(now_ms, i64::MAX / 4, i64::MAX / 4 + HOUR))
        }

        /// A zone with the transition instants given as real (UTC) milliseconds.
        pub fn with_rule(now_ms: i64, spring_forward_ms: i64, fall_back_ms: i64) -> Self {
            Self {
                now_ms: AtomicI64::new(now_ms),
                spring_forward_ms,
                fall_back_ms,
                local_calls: AtomicU64::new(0),
            }
        }

        pub fn set(&self, ms: i64) {
            self.now_ms.store(ms, Ordering::SeqCst);
        }

        fn offset_at(&self, real_ms: i64) -> i64 {
            if real_ms >= self.spring_forward_ms && real_ms < self.fall_back_ms {
                2 * HOUR
            } else {
                HOUR
            }
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> i64 {
            self.now_ms.load(Ordering::SeqCst)
        }

        fn local_from_ms(&self, ms: i64) -> chrono::NaiveDateTime {
            self.local_calls.fetch_add(1, Ordering::SeqCst);
            chrono::DateTime::from_timestamp_millis(ms + self.offset_at(ms))
                .map(|dt| dt.naive_utc())
                .unwrap_or_default()
        }

        fn ms_from_local(&self, t: &chrono::NaiveDateTime) -> Option<i64> {
            let local_ms = t.and_utc().timestamp_millis();
            let (gap_lo, gap_hi) = (
                self.spring_forward_ms + HOUR,
                self.spring_forward_ms + 2 * HOUR,
            );
            if local_ms >= gap_lo && local_ms < gap_hi {
                return None; // the spring-forward gap: this wall time never happens
            }
            if local_ms >= self.spring_forward_ms + 2 * HOUR
                && local_ms < self.fall_back_ms + 2 * HOUR
            {
                // Inside the +120 segment. The fall-back's repeated hour belongs to this
                // segment first (the earlier of the two instants), by checking it before C.
                return Some(local_ms - 2 * HOUR);
            }
            // Before the spring transition, or after the fall one: the +60 segments.
            Some(local_ms - HOUR)
        }
    }
}
