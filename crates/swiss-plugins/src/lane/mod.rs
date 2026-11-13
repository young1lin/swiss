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

//! The lane plugin (SPEC §lane): one machine-wide queue for heavy commands.
//!
//! Several agents share one desktop. Two `cargo build`s, or a build beside a vitest run, starve
//! each other until tests fail at their own deadlines and rustc runs the paging file dry. A
//! command run through `swiss lane run` holds a slot here for as long as it runs; whoever
//! comes next waits its turn. The lane never runs anything itself: the CLI spawns the command
//! in the caller's own terminal, and the gateway only counts. That is why every refusal is
//! open - an unreachable gateway or a disabled plugin means the command runs at once.
//!
//! The instance owns a [Lane] (the state machine in [queue]) and installs it in the
//! [LaneSlot] the routes in [api] read.

pub mod api;
pub mod queue;

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{json, Value};

use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::services::RuntimeServices;

pub use queue::{Lane, LaneConfig, Seat, Ticket};

/// The plugin id, also the config row key and the routes' owner.
pub const PLUGIN_ID: &str = "lane";

/// The seat the running instance's [Lane] sits in. Built once at boot, handed to the factory
/// (which fills it) and to [api::mount] (which reads it).
pub struct LaneSlot {
    lane: RwLock<Option<Arc<Lane>>>,
}

impl LaneSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(LaneSlot { lane: RwLock::new(None) })
    }

    fn install(&self, lane: Arc<Lane>) {
        *self.lane.write().unwrap_or_else(|e| e.into_inner()) = Some(lane);
    }

    fn withdraw(&self) -> Option<Arc<Lane>> {
        self.lane.write().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// The live lane, or `None` while the plugin is not serving.
    pub fn current(&self) -> Option<Arc<Lane>> {
        self.lane.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub struct LanePlugin {
    services: Arc<RuntimeServices>,
    slot: Arc<LaneSlot>,
}

impl LanePlugin {
    pub fn new(services: Arc<RuntimeServices>, slot: Arc<LaneSlot>) -> Self {
        LanePlugin { services, slot }
    }
}

#[async_trait]
impl PluginFactory for LanePlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PLUGIN_ID.into(),
            kind: "lane".into(),
            label: "Lane".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            // Mirrors LaneConfig::parse key for key: the panel's form comes from this, and the
            // parser is the authority the numbers must match.
            config_schema: json!({
                "type": "object",
                "properties": {
                    "capacity": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": queue::CAPACITY_MAX,
                        "default": 1,
                        "description": "How many heavy commands may run at once. 1 means one at a time, which is the point of the lane."
                    },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "Variables a command gets when its caller does not set them. Absent = CARGO_BUILD_JOBS=2 and VITEST_MAX_WORKERS = half the cores (2 to 4); {} = none."
                    }
                },
                "additionalProperties": false
            }),
            // Beside Process (55): both are about what runs on this machine.
            pages: vec![PageDescriptor {
                id: "lane".into(),
                plugin_id: PLUGIN_ID.into(),
                label: "Lane".into(),
                order: 56,
                path: "#lane".into(),
                entry: "/admin/js/views/lane.js".into(),
                sidebar: false,
                layout: "page",
            }],
            routes: vec!["/api/lane".into()],
            // Capacity and caps apply in place: a restart would close every lease, and each
            // holder would run on unqueued until it reconnects.
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        LaneConfig::parse(config).map(|_| ())
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        Ok(Arc::new(LaneInstance {
            services: self.services.clone(),
            slot: self.slot.clone(),
            config: LaneConfig::parse(config)?,
        }))
    }
}

struct LaneInstance {
    services: Arc<RuntimeServices>,
    slot: Arc<LaneSlot>,
    config: LaneConfig,
}

#[async_trait]
impl PluginInstance for LaneInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        // LANE.QUEUE: one notice per change the page shows (SPEC §lane.panel).
        let bus = self.services.bus.clone();
        let notice: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            swiss_host::notice::publish_at(
                &bus,
                swiss_bus::pb::v1::Module::Lane as u32,
                swiss_bus::pb::v1::LaneSub::Queue as u32,
                swiss_bus::pb::v1::LaneQueueId::LaneQueueChanged as u32,
            );
        });
        let lane = Arc::new(Lane::new(self.config.clone(), Some(notice)));
        // The server drains its connections BEFORE plugins stop, and a lease never ends on its
        // own: close the lane the moment the gateway starts stopping, so every lease ends with
        // its `closed` line instead of holding the drain to its timeout.
        let mut stopping = self.services.bus.stopping();
        let closing = lane.clone();
        scope.spawn(async move {
            if stopping.wait_for(|stop| *stop).await.is_ok() {
                closing.close();
            }
        });
        self.slot.install(lane);
        Ok(())
    }

    async fn stop(&self) {
        // Withdraw first so no request finds a closing lane, then tell every holder: each
        // CLI hears `closed` and lets its command finish outside the lane.
        if let Some(lane) = self.slot.withdraw() {
            lane.close();
        }
    }

    async fn apply_config(&self, config: &Value) -> ApplyOutcome {
        match LaneConfig::parse(config) {
            Ok(parsed) => {
                if let Some(lane) = self.slot.current() {
                    lane.apply(parsed);
                }
                ApplyOutcome::Applied
            }
            Err(err) => ApplyOutcome::Failed(err),
        }
    }
}

#[cfg(test)]
mod tests;
