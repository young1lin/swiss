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

//! One request-local fact pass (SPEC §process.apps): the listener table and
//! each relevant owner are read once, then every verdict and held-port column
//! derives from those same facts. Nothing is cached between reads or actions.

use std::collections::{HashMap, HashSet};

use crate::config::AppConfig;
use crate::control::Probe;
use crate::status::{self, OwnerFact, PortFact, TrackedRecord, Verdict};

/// Facts for one read, never a source of targets for a later kill.
pub(crate) struct FactSnapshot {
    listeners: Option<Vec<PortFact>>,
    owners: HashMap<u32, OwnerFact>,
    records: HashMap<String, TrackedRecord>,
}

impl FactSnapshot {
    pub(crate) async fn gather(
        probe: &dyn Probe,
        apps: &[AppConfig],
        records: HashMap<String, TrackedRecord>,
    ) -> Self {
        let listeners = probe.listeners().await.ok();
        let ports: HashSet<u16> = apps.iter().flat_map(|app| app.ports.iter().copied()).collect();
        let mut pids: HashSet<u32> = records.values().map(|record| record.pid).collect();
        if let Some(rows) = &listeners {
            pids.extend(rows.iter().filter(|row| ports.contains(&row.port)).map(|row| row.pid));
        }
        let mut owners = HashMap::new();
        for pid in pids {
            if let Some(fact) = probe.process(pid).await {
                owners.insert(pid, fact);
            }
        }
        FactSnapshot { listeners, owners, records }
    }

    pub(crate) fn verdict(&self, app: &AppConfig) -> Verdict {
        let record = self.records.get(&app.name);
        status::verdict(
            &app.ports,
            app.image.as_deref(),
            self.listeners.as_deref(),
            &self.owners,
            record,
            record.and_then(|record| self.owners.get(&record.pid)),
        )
    }

    pub(crate) fn held_ports(&self, pid: Option<u32>) -> Vec<u16> {
        let Some(pid) = pid else { return Vec::new(); };
        let mut ports: Vec<u16> = self.listeners
            .iter()
            .flatten()
            .filter(|row| row.pid == pid)
            .map(|row| row.port)
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }
}
