/*
 * Copyright 2026 The swiss authors
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

//! The two traits every plugin touches: how an instance runs ([`PluginInstance`]) and how
//! instances are built ([`PluginFactory`]). Statically linked, but no less a contract for
//! that — the host knows nothing else about a plugin.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::descriptor::PluginDescriptor;
use super::scope::PluginScope;

/// One running plugin instance. Created by the factory's `create`, started once by the host,
/// stopped at most once per start. The host guarantees:
///
/// - `start` runs exactly once per instance, serialized against every other lifecycle op of
///   the plugin (single-flight — a concurrent second start JOINS the first's outcome by
///   finding the slot filled, never builds a twin);
/// - long-lived work is spawned through the [`PluginScope`] the host hands in, never with a
///   bare `tokio::spawn` — the scope is what makes stop verifiable;
/// - `stop` runs with the instance already out of the route slot, so in-flight requests see a
///   not-serving plugin, and is awaited under a timeout after which the scope is torn down
///   anyway.
#[async_trait]
pub trait PluginInstance: Send + Sync {
    /// Bring the instance up. `Err` fails the start: the host records it on the inventory row
    /// (state `failed` + `lastError`), drops the instance and everything its scope tracks.
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String>;

    /// Release everything the instance owns — connections closed, children reaped, timers
    /// aborted. Idempotence is not required (one stop per start) but panic-freedom is: the
    /// host's teardown must always be able to finish.
    async fn stop(&self) {
        // Nothing beyond the scope to release — the default for plugins whose whole runtime
        // footprint is tasks spawned through the scope.
    }

    /// Apply a new config row IN PLACE, without a restart (docs/11 §8). Default:
    /// [ApplyOutcome::NotApplicable], so a plugin that says nothing keeps today's
    /// restart-or-note behaviour exactly. A plugin whose config is a live table - Jobs -
    /// overrides this so editing one entry never bounces the instance (and with it every
    /// run it owns).
    async fn apply_config(&self, _config: &Value) -> ApplyOutcome {
        ApplyOutcome::NotApplicable
    }
}

/// What [PluginInstance::apply_config] did with the row.
pub enum ApplyOutcome {
    /// The row is live in the running instance; the host notes the new revision.
    Applied,
    /// This plugin has no in-place semantics; restart-or-note decides, as before.
    NotApplicable,
    /// The row is PERSISTED but the running instance could not take it: the instance
    /// stays Active, `lastError` carries the reason, and the config revision does NOT
    /// move - desired and actual stay visibly apart until a later reconcile succeeds
    /// (docs/10 §5). The PUT still answered 200; its body says `applied: false`.
    Failed(String),
}

/// Builds instances of one plugin. One factory per registered plugin; the descriptor is fixed
/// at registration and the factory outlives every instance it builds.
#[async_trait]
pub trait PluginFactory: Send + Sync {
    /// The frozen declaration: id, kind, pages, owned routes, schema.
    fn descriptor(&self) -> PluginDescriptor;

    /// Validate a config the PUT route is about to persist. Runs BEFORE the store write, so a
    /// rejected value never lands on disk (docs/09 §5: validate first, then persist desired,
    /// then reconcile). Returning `Err` is a 400 with the message verbatim.
    fn validate_config(&self, config: &Value) -> Result<(), String> {
        let _ = config;
        Ok(())
    }

    /// Validate a config the START path is about to boot from. Defaults to
    /// [PluginFactory::validate_config], so a plugin that says nothing keeps one strict
    /// validator on both paths. Override it only to tolerate rows an OLDER, laxer
    /// validator of the same plugin may already have persisted (drop-and-warn), never to
    /// weaken what a save accepts: a config refused on PUT but booted anyway must be a
    /// leftover, not a second way for dead config to get in.
    fn validate_config_for_start(&self, config: &Value) -> Result<(), String> {
        self.validate_config(config)
    }

    /// Build (not start) an instance from validated config. Cheap by contract — everything
    /// eager belongs in `start`, so a failed start cannot leak half a construction.
    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String>;

    /// Advisory notes about a config the validator ACCEPTED (docs/11 §3.4): savable rows
    /// the running gateway cannot fully execute yet - an action whose provider is
    /// disabled, say. Carried on the PUT response as `warnings`; empty by default.
    fn config_warnings(&self, _config: &Value) -> Vec<String> {
        Vec::new()
    }
}
