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

//! The panel's only way to start an update: spawn `swiss update --apply` and
//! return. The download stays in that process (ADR-041).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::apply::{self, ApplyEnv, Refusal};

/// An updater this gateway spawned. `done` completes when that process exits.
pub enum Spawned {
    Started { pid: u32, done: Option<tokio::sync::oneshot::Receiver<()>> },
    CannotDetach,
}

/// Whether an updater this process launched is still running, and which notices
/// its exit published. The route shares one of these with the status read.
pub struct InFlight {
    pid: Mutex<Option<u32>>,
    published: Mutex<Vec<(u32, u32)>>,
}

impl InFlight {
    pub fn new() -> Self {
        Self { pid: Mutex::new(None), published: Mutex::new(Vec::new()) }
    }

    pub fn running(&self) -> bool {
        self.pid.lock().unwrap_or_else(|err| err.into_inner()).is_some()
    }

    pub fn start(&self, pid: u32) {
        *self.pid.lock().unwrap_or_else(|err| err.into_inner()) = Some(pid);
    }

    pub fn finish(&self) {
        *self.pid.lock().unwrap_or_else(|err| err.into_inner()) = None;
    }

    pub fn publish(&self, sub: u32, id: u32) {
        self.published.lock().unwrap_or_else(|err| err.into_inner()).push((sub, id));
    }

    pub fn published(&self) -> Vec<(u32, u32)> {
        self.published.lock().unwrap_or_else(|err| err.into_inner()).clone()
    }
}

/// The panel never forces: a dev build, the same version and a downgrade stay refused, and
/// `--force` is the terminal's (ADR-041).
#[async_trait]
pub trait UpdateSpawner: Send + Sync {
    async fn spawn(&self) -> Result<Spawned, Refusal>;
}

pub struct RealUpdate;

#[async_trait]
impl UpdateSpawner for RealUpdate {
    async fn spawn(&self) -> Result<Spawned, Refusal> {
        let exe = std::env::current_exe().map_err(|err| Refusal {
            status: 400,
            code: "update.readOnly",
            message: format!("cannot resolve this executable: {err}"),
        })?;
        let port = crate::daemon::resolve_port();
        let running = crate::pidfile::read_pid_file(port).and_then(|rec| {
            swiss_core::platform::pid_alive(rec.pid).then_some(PathBuf::from(rec.entry))
        });
        let env = ApplyEnv {
            exe: exe.clone(),
            channel: super::channel().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            hash: env!("SWISS_GIT_HASH").to_string(),
            target: env!("SWISS_TARGET").to_string(),
            port,
            data_dir: swiss_core::paths::data_dir(),
            force: false,
            by: "panel".to_string(),
        };
        apply::preflight(&env, running.as_deref())?;
        let capture = env.data_dir.join("update").join("apply.log");
        if let Some(parent) = capture.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let args = vec!["update".to_string(), "--apply".to_string(), "--by-panel".to_string()];
        let spec = swiss_host::services::detached::DetachedSpec {
            program: exe.display().to_string(),
            args,
            cwd: None,
            env: Vec::new(),
            capture,
        };
        let mut spawned = swiss_host::services::detached::spawn_detached(&spec).await.map_err(|err| Refusal {
            status: 400,
            code: "update.readOnly",
            message: err.to_string(),
        })?;
        if spawned.shares_job {
            let _ = spawned.child.start_kill();
            return Ok(Spawned::CannotDetach);
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut child = spawned.child;
        tokio::spawn(async move {
            let _ = child.wait().await;
            let _ = tx.send(());
        });
        Ok(Spawned::Started { pid: spawned.pid, done: Some(rx) })
    }
}

/// The boot installs this. Tests install their own and leave it unset to prove 503.
pub fn real() -> Arc<dyn UpdateSpawner> {
    Arc::new(RealUpdate)
}
