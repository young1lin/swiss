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

//! The pure status verdict (SPEC §process.apps): one verdict per app from facts
//! gathered once per read. Every row of the spec's table is a unit test here,
//! with no machine access - the caller (the control module) supplies the listener
//! snapshot, the per-owner image and creation time, and the tracked record.

use std::collections::HashMap;

use crate::config::image_matches;

/// One LISTEN row: a port and the pid that owns it (pid 0 = a socket whose
/// owner another user's process holds, which no unprivileged read can name).
#[derive(Clone, Copy, Debug)]
pub struct PortFact {
    pub port: u16,
    pub pid: u32,
}

/// Image and creation time for one distinct owner pid, read once per status
/// pass however many apps share the owner.
#[derive(Clone, Debug)]
pub struct OwnerFact {
    pub pid: u32,
    pub image: String,
    pub created_at: u64,
}

/// The tracked record's entry for one app (SPEC §process.apps): the pid a
/// start left alive, pinned by its creation time and image against reuse.
#[derive(Clone, Debug)]
pub struct TrackedRecord {
    pub pid: u32,
    pub created_at: u64,
    pub image: String,
}

/// One app's verdict. The wire shape (SPEC §process.apps routes) is derived
/// from this; the enum is the truth.
#[derive(Clone, Debug, PartialEq)]
pub enum AppStatus {
    Running,
    /// Some declared ports are up and matching; the rest are listed.
    Partial { down_ports: Vec<u16> },
    Stopped,
    /// A stranger holds a declared port: its pid and image (empty for "owned
    /// by another user", pid 0) and the first such port.
    Conflict { pid: u32, image: String, port: u16 },
    /// The reason is the operator-facing sentence.
    Unknown { reason: String },
}

/// The verdict plus what the caller should do about the tracked record: a
/// reused or gone pid drops it (SPEC §process.apps).
#[derive(Clone, Debug)]
pub struct Verdict {
    pub status: AppStatus,
    /// The running pid when the verdict has one (Running with ports: the owner
    /// of the first declared port; Running tracked: the record's pid).
    pub pid: Option<u32>,
    /// The creation time of that pid (uptime is now minus it, computed by the
    /// caller, which owns the clock).
    pub created_at: Option<u64>,
    pub drop_record: bool,
}

/// The one unknown reason for an app nobody here started (the spec's words).
pub const NEVER_STARTED: &str =
    "not started by this swiss; declare ports to see it from outside";

/// The verdict for one app. `listeners` is None when the table cannot be read
/// (macOS, a failed read): an app with ports is then judged by the rows for an
/// app without them (SPEC §process.apps, the start's rule, applied to reads
/// too so a read and a start can never disagree).
pub fn verdict(
    ports: &[u16],
    image: Option<&str>,
    listeners: Option<&[PortFact]>,
    owners: &HashMap<u32, OwnerFact>,
    tracked: Option<&TrackedRecord>,
    tracked_fact: Option<&OwnerFact>,
) -> Verdict {
    let listeners = match (listeners, !ports.is_empty()) {
        (Some(rows), true) => rows,
        (None, true) => {
            // The table is unreadable: the no-ports rows below decide, exactly
            // as the start window does, so both halves say the same thing.
            return no_ports_verdict(
                tracked,
                tracked_fact,
                Some("the listener table cannot be read; status is tracked-pid only".to_string()),
            );
        }
        _ => return no_ports_verdict(tracked, tracked_fact, None),
    };

    let owned = |port: &u16| listeners.iter().find(|row| row.port == *port);

    // Conflict first: it wins over partial (SPEC §process.apps).
    for port in ports {
        if let Some(row) = owned(port) {
            let stranger = match owners.get(&row.pid) {
                // pid 0: another user's socket, unnameable without privileges.
                None if row.pid == 0 => Some(("owned by another user".to_string(), 0u32)),
                // The owner's facts cannot be read (a protected process, or it
                // died between the table and the facts): it is not provably
                // ours, and calling it "running" would hand Stop a pid we
                // cannot name - the honest verdict is a conflict naming the
                // pid with an unreadable image (found live on the RPC endpoint
                // mapper, port 135: an unreadable svchost read as "running").
                None => Some((String::new(), row.pid)),
                Some(owner) => {
                    let matches = image
                        .map(|declared| image_matches(declared, &owner.image))
                        .unwrap_or(false);
                    (!matches).then(|| (owner.image.clone(), owner.pid))
                }
            };
            if let Some((image, pid)) = stranger {
                return Verdict {
                    status: AppStatus::Conflict { pid, image, port: *port },
                    pid: None,
                    created_at: None,
                    drop_record: false,
                };
            }
        }
    }

    let mut down = Vec::new();
    let mut first_owner = None;
    for port in ports {
        match owned(port) {
            Some(row) => {
                if first_owner.is_none() {
                    first_owner = Some(row.pid);
                }
            }
            None => down.push(*port),
        }
    }
    match (first_owner, down.is_empty()) {
        (Some(pid), true) => Verdict {
            status: AppStatus::Running,
            pid: Some(pid),
            created_at: owners.get(&pid).map(|o| o.created_at),
            drop_record: false,
        },
        (Some(_), false) => Verdict {
            status: AppStatus::Partial { down_ports: down },
            pid: None,
            created_at: None,
            drop_record: false,
        },
        (None, _) => Verdict {
            status: AppStatus::Stopped,
            pid: None,
            created_at: None,
            drop_record: false,
        },
    }
}

/// The no-ports ladder: a tracked record that still pins its pid, a record
/// whose pid is gone or reused (stopped, and the record drops), or nothing
/// tracked (unknown with the reason).
fn no_ports_verdict(
    tracked: Option<&TrackedRecord>,
    tracked_fact: Option<&OwnerFact>,
    unreadable: Option<String>,
) -> Verdict {
    let Some(record) = tracked else {
        return Verdict {
            status: AppStatus::Unknown {
                reason: unreadable.unwrap_or_else(|| NEVER_STARTED.to_string()),
            },
            pid: None,
            created_at: None,
            drop_record: false,
        };
    };
    if pin_record_matches(Some(record), tracked_fact) {
        Verdict {
            status: AppStatus::Running,
            pid: Some(record.pid),
            created_at: tracked_fact.map(|f| f.created_at),
            drop_record: false,
        }
    } else {
        // Gone, or alive as someone else (the pid was reused): stopped, and
        // the stale record is dropped (SPEC §process.apps).
        Verdict {
            status: AppStatus::Stopped,
            pid: None,
            created_at: None,
            drop_record: true,
        }
    }
}

/// Does the tracked record still pin its pid? The facts for the pid, matched
/// against the record: same pid, same creation time, same image (case and
/// exe-suffix tolerance as the port table, SPEC §process.apps). None when the
/// pid is gone: the record drops either way.
pub fn pin_record_matches(record: Option<&TrackedRecord>, owner: Option<&OwnerFact>) -> bool {
    match (record, owner) {
        (Some(record), Some(owner)) => {
            owner.pid == record.pid
                && owner.created_at == record.created_at
                && image_matches(&record.image, &owner.image)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owners table only carries LIVE pids with pinning facts; the verdict
    /// layer itself never opens a process - this helper builds the map an
    /// app-with-ports read builds.
    fn owners(list: &[(u32, &str, u64)]) -> HashMap<u32, OwnerFact> {
        list.iter()
            .map(|(pid, image, created)| {
                (
                    *pid,
                    OwnerFact {
                        pid: *pid,
                        image: image.to_string(),
                        created_at: *created,
                    },
                )
            })
            .collect()
    }

    fn listeners(list: &[(u16, u32)]) -> Vec<PortFact> {
        list.iter()
            .map(|(port, pid)| PortFact { port: *port, pid: *pid })
            .collect()
    }

    #[test]
    fn an_owner_whose_facts_cannot_be_read_is_a_conflict_naming_the_pid() {
        // Found live on port 135 (the RPC endpoint mapper): a protected
        // svchost whose image cannot be read from an unprivileged process
        // used to count as "ours" - a verdict that would hand Stop a kill
        // of a system process. Unreadable facts are not proof of ownership.
        let rows = listeners(&[(135, 2100)]);
        let v = verdict(
            &[135],
            Some("ping.exe"),
            Some(&rows),
            &owners(&[]),
            None,
            None,
        );
        assert_eq!(
            v.status,
            AppStatus::Conflict { pid: 2100, image: String::new(), port: 135 }
        );
        assert_eq!(v.pid, None);
    }

    #[test]
    fn every_port_owned_by_the_image_is_running() {
        let rows = listeners(&[(4317, 10), (4318, 11), (9999, 50)]);
        let table = owners(&[(10, "cc-otel.exe", 100), (11, "CC-OTEL.EXE", 101), (50, "other.exe", 5)]);
        let v = verdict(
            &[4317, 4318],
            Some("cc-otel.exe"),
            Some(&rows),
            &table,
            None,
            None,
        );
        assert_eq!(v.status, AppStatus::Running);
        // The pid is the owner of the FIRST declared port, and its creation
        // time is the one uptime is computed from.
        assert_eq!(v.pid, Some(10));
        assert_eq!(v.created_at, Some(100));
    }

    #[test]
    fn some_ports_up_is_partial_and_names_the_down_ones() {
        let rows = listeners(&[(4317, 10)]);
        let table = owners(&[(10, "cc-otel.exe", 100)]);
        let v = verdict(
            &[4317, 4318, 4319],
            Some("cc-otel.exe"),
            Some(&rows),
            &table,
            None,
            None,
        );
        assert_eq!(v.status, AppStatus::Partial { down_ports: vec![4318, 4319] });
        assert_eq!(v.pid, None);
    }

    #[test]
    fn no_port_listened_on_is_stopped() {
        let rows = listeners(&[]);
        let v = verdict(&[4317], Some("cc-otel.exe"), Some(&rows), &HashMap::new(), None, None);
        assert_eq!(v.status, AppStatus::Stopped);
    }

    #[test]
    fn a_stranger_on_any_port_is_a_conflict_naming_pid_and_image() {
        let rows = listeners(&[(4317, 10), (4318, 99)]);
        let table = owners(&[(10, "cc-otel.exe", 100), (99, "node.exe", 90)]);
        let v = verdict(
            &[4317, 4318],
            Some("cc-otel.exe"),
            Some(&rows),
            &table,
            None,
            None,
        );
        assert_eq!(
            v.status,
            AppStatus::Conflict { pid: 99, image: "node.exe".to_string(), port: 4318 }
        );
    }

    #[test]
    fn conflict_beats_partial() {
        // One port down AND a stranger on another: the conflict is the verdict,
        // not the partial (SPEC §process.apps).
        let rows = listeners(&[(4317, 99)]);
        let table = owners(&[(99, "stranger.exe", 1)]);
        let v = verdict(&[4317, 4318], Some("cc-otel.exe"), Some(&rows), &table, None, None);
        assert!(matches!(v.status, AppStatus::Conflict { .. }));
    }

    #[test]
    fn a_tracked_pid_with_its_creation_time_is_running() {
        let record = TrackedRecord { pid: 77, created_at: 500, image: "svc.exe".into() };
        let fact = OwnerFact { pid: 77, image: "svc.exe".into(), created_at: 500 };
        let v = verdict(&[], None, Some(&[]), &HashMap::new(), Some(&record), Some(&fact));
        assert_eq!(v.status, AppStatus::Running);
        assert_eq!(v.pid, Some(77));
        assert_eq!(v.created_at, Some(500));
        assert!(!v.drop_record);
    }

    #[test]
    fn a_reused_pid_is_stopped_and_drops_the_record() {
        let record = TrackedRecord { pid: 77, created_at: 500, image: "svc.exe".into() };
        // Alive but as someone else: the creation time moved (the pid was
        // recycled), so the pin fails and the record drops.
        let reused = OwnerFact { pid: 77, image: "svc.exe".into(), created_at: 9000 };
        let v = verdict(&[], None, Some(&[]), &HashMap::new(), Some(&record), Some(&reused));
        assert_eq!(v.status, AppStatus::Stopped);
        assert!(v.drop_record);
        // Gone entirely (no facts for the pid): same answer.
        let v = verdict(&[], None, Some(&[]), &HashMap::new(), Some(&record), None);
        assert_eq!(v.status, AppStatus::Stopped);
        assert!(v.drop_record);
    }

    #[test]
    fn nothing_tracked_without_ports_is_unknown_with_the_reason() {
        let v = verdict(&[], None, Some(&[]), &HashMap::new(), None, None);
        assert_eq!(
            v.status,
            AppStatus::Unknown { reason: NEVER_STARTED.to_string() }
        );
    }

    #[test]
    fn an_unreadable_listener_table_is_unknown_with_the_reason() {
        let record = TrackedRecord { pid: 77, created_at: 500, image: "svc.exe".into() };
        let fact = OwnerFact { pid: 77, image: "svc.exe".into(), created_at: 500 };
        // With a tracked record the tracked ladder still speaks.
        let v = verdict(
            &[4317],
            Some("cc-otel.exe"),
            None,
            &HashMap::new(),
            Some(&record),
            Some(&fact),
        );
        assert_eq!(v.status, AppStatus::Running);
        // Without one, the reason names the unreadable table.
        let v = verdict(&[4317], Some("cc-otel.exe"), None, &HashMap::new(), None, None);
        assert!(matches!(v.status, AppStatus::Unknown { reason } if reason.contains("cannot be read")));
    }
}


