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

//! The real seams: the production Probe, Spawner, Killer and Clock the control runs over
//! outside its tests.

use super::*;

/// The production [Probe]: the platform's LISTEN table and per-pid reads.
pub struct SysProbe;

#[async_trait::async_trait]
impl Probe for SysProbe {
    async fn listeners(&self) -> std::io::Result<Vec<PortFact>> {
        Ok(swiss_core::platform::tcp_listeners()?
            .into_iter()
            .map(|(port, pid)| PortFact { port, pid })
            .collect())
    }

    async fn process(&self, pid: u32) -> Option<OwnerFact> {
        Some(OwnerFact {
            pid,
            image: swiss_core::platform::process_name(pid)?,
            created_at: swiss_core::platform::process_created_at(pid)?,
        })
    }
}

/// The production [Spawner]: the detached spawn of §process.detached, plus
/// the exit watch that feeds the plugin's pump (the app outlives the window).
pub struct SysSpawner {
    exits: mpsc::UnboundedSender<ExitLine>,
    clock: Arc<dyn Clock>,
}

impl SysSpawner {
    pub fn new(exits: mpsc::UnboundedSender<ExitLine>, clock: Arc<dyn Clock>) -> Self {
        SysSpawner { exits, clock }
    }
}

#[async_trait::async_trait]
impl Spawner for SysSpawner {
    async fn spawn(&self, spec: &SpawnSpec) -> Result<Spawned, String> {
        let detached = swiss_host::services::detached::spawn_detached(&to_detached_spec(spec))
            .await
            .map_err(|err| err.to_string())?;
        let pid = detached.pid;
        let created_at = detached.created_at;
        let shares_job = detached.shares_job;
        // The window's one-shot: the FIRST wait wins it; the pump always gets
        // the line (suppressed there when a stop did the killing).
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut child = detached.child;
        let app_name = spec
            .capture
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = ExitLine {
            app: app_name,
            pid,
            code: 0,
            at: 0,
        };
        let pump = self.exits.clone();
        let clock = self.clock.clone();
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let _ = tx.send(code);
            let _ = pump.send(ExitLine {
                app: line.app,
                pid: line.pid,
                code,
                at: clock.now_ms(),
            });
        });
        Ok(Spawned {
            pid,
            created_at,
            shares_job,
            exits: Some(rx),
        })
    }
}

fn to_detached_spec(spec: &SpawnSpec) -> swiss_host::services::detached::DetachedSpec {
    swiss_host::services::detached::DetachedSpec {
        program: spec.program.clone(),
        args: spec.args.clone(),
        cwd: spec.cwd.clone(),
        env: spec.env.clone(),
        capture: spec.capture.clone(),
    }
}

/// The production [Killer]: the platform tree kill, survivors and all.
pub struct SysKiller;

#[async_trait::async_trait]
impl Killer for SysKiller {
    async fn kill_tree(&self, pid: u32) -> Vec<u32> {
        swiss_core::platform::tree_kill(pid)
    }
}

/// The production clock: the wall, in Unix milliseconds.
pub struct WallClock;

impl Clock for WallClock {
    fn now_ms(&self) -> u64 {
        swiss_core::util::now_ms() as u64
    }
}
