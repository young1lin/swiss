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

//! Agent-friendly remote execution (docs/32): the swiss-remote plugin.
//!
//! The fourth way to reach a machine: NOT a second SSH client. This crate knows no
//! russh, no tunnels store, no credentials - it holds the agent-facing vocabulary
//! (targets, exec, sync, pull) and leases everything that touches the network through
//! the host's RemoteTransportRegistry, which the Tunnels plugin serves. That seam is
//! the whole design: a target id and a lease, and this crate builds and tests with no
//! transport linked at all (a fake provider stands in, exactly like the shell tests).
//!
//! Runs go through the shared run coordinator like every other capability: the Run ID
//! IS the remote job id (there is no second RemoteJobManager), cancellation is the
//! coordinator's own cancel, and live output is the run output endpoint added with the
//! ActionContext seam.
//!
//! No MCP adapter in this phase (docs/32 §2): the CLI and the /api/remote routes are
//! the surfaces; the MCP adapter is a later, deliberate step.

pub mod actions;
pub mod api;
pub mod project;
pub mod sync;
pub mod target;

use std::sync::{Arc, Mutex};

use swiss_host::services::RuntimeServices;

use target::TargetStore;

/// The shared seat the routes and actions read through: the sealed target table plus
/// the host services (whose remote registry is the transport seam). One instance per
/// process, built at composition time.
pub struct RemoteSystem {
    services: Arc<RuntimeServices>,
    /// The target table. Locked briefly per operation (resolve, list, mutate) - never
    /// across an await: an hours-long run holds NO lock here.
    store: Mutex<TargetStore>,
}

impl RemoteSystem {
    pub fn open(services: Arc<RuntimeServices>, targets_path: std::path::PathBuf) -> Arc<Self> {
        Arc::new(RemoteSystem {
            services,
            store: Mutex::new(TargetStore::open(targets_path)),
        })
    }

    pub fn services(&self) -> &Arc<RuntimeServices> {
        &self.services
    }

    /// Run a closure over the table under its lock. For mutations only; reads on the
    /// run path go through [Self::resolve].
    pub fn with_store<R>(&self, f: impl FnOnce(&mut TargetStore) -> R) -> R {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut store)
    }

    /// Resolve a target id for a run: clone the row out under the lock, then work
    /// lock-free for the rest of the operation.
    pub fn resolve(&self, id: &str) -> Result<target::RemoteTarget, String> {
        let found = {
            let store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            store.get(id).cloned()
        };
        found.ok_or_else(|| {
            format!("target {id} does not exist; 'swiss remote target list' shows what does")
        })
    }
}

#[cfg(test)]
pub(crate) mod testing {
    //! The fake transport the action and route tests lease through: an in-memory
    //! endpoint that runs canned programs and keeps a file map - the same shape the
    //! tunnels tests use, minus every SSH detail.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use swiss_host::services::action::CancelHandle;
    use swiss_host::services::remote::{
        RemoteEndpoint, RemoteError, RemoteExecEvent, RemoteExecRequest, RemoteExecResult,
        RemoteFileStat, RemoteListing, RemoteRead, RemoteTransportProvider, RemoteWrite,
    };
    use swiss_host::services::RuntimeServices;

    /// What the fake answers one exec with. delay_ms spaces the stdout chunks
    /// apart, which is how tests get a deterministic cancel window.
    #[derive(Clone, Default)]
    #[allow(dead_code)] // argv0 documents what a canned program keys on; tests set it for clarity
    pub struct FakeProgram {
        pub argv0: String,
        pub stdout: Vec<u8>,
        pub stderr: Vec<u8>,
        pub exit: i32,
        pub delay_ms: u64,
    }

    pub struct FakeTransport {
        pub programs: Mutex<HashMap<String, FakeProgram>>,
        pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        pub exec_calls: Mutex<Vec<RemoteExecRequest>>,
    }

    impl FakeTransport {
        pub fn new() -> Arc<Self> {
            Arc::new(FakeTransport {
                programs: Mutex::new(HashMap::new()),
                files: Arc::new(Mutex::new(HashMap::new())),
                exec_calls: Mutex::new(Vec::new()),
            })
        }

        pub fn program(&self, argv0: &str, program: FakeProgram) {
            self.programs
                .lock()
                .unwrap()
                .insert(argv0.to_string(), program);
        }
    }

    #[async_trait]
    impl RemoteTransportProvider for FakeTransport {
        fn list(&self) -> Vec<RemoteEndpoint> {
            vec![RemoteEndpoint {
                id: "conn-1".into(),
                label: "Fake".into(),
                state: "connected".into(),
            }]
        }

        fn supports_files(&self) -> bool {
            true
        }

        async fn exec(
            &self,
            endpoint: &str,
            _holder: &str,
            request: RemoteExecRequest,
            events: tokio::sync::mpsc::Sender<RemoteExecEvent>,
            cancel: CancelHandle,
        ) -> Result<RemoteExecResult, RemoteError> {
            if endpoint == "absent" {
                return Err(RemoteError::Unknown(format!(
                    "endpoint {endpoint} is not known to the transport"
                )));
            }
            self.exec_calls.lock().unwrap().push(request.clone());
            let argv0 = request.argv.first().cloned().unwrap_or_default();
            let program = self
                .programs
                .lock()
                .unwrap()
                .get(&argv0)
                .cloned()
                .unwrap_or(FakeProgram {
                    argv0: argv0.clone(),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit: 127,
                    delay_ms: 0,
                });
            // Stream in small chunks so live output and cancel windows are real.
            for chunk in program.stdout.chunks(3) {
                if program.delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(program.delay_ms)).await;
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        return Err(RemoteError::Canceled("the remote command was canceled".into()));
                    }
                    sent = events.send(RemoteExecEvent::Stdout(chunk.to_vec())) => {
                        if sent.is_err() {
                            return Err(RemoteError::Canceled("the run went away".into()));
                        }
                    }
                }
            }
            for chunk in program.stderr.chunks(3) {
                if events
                    .send(RemoteExecEvent::Stderr(chunk.to_vec()))
                    .await
                    .is_err()
                {
                    return Err(RemoteError::Canceled("the run went away".into()));
                }
            }
            Ok(RemoteExecResult {
                exit_code: program.exit,
            })
        }

        async fn stat(
            &self,
            _endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Option<RemoteFileStat>, RemoteError> {
            let files = self.files.lock().unwrap();
            if let Some(bytes) = files.get(path) {
                return Ok(Some(RemoteFileStat {
                    size: bytes.len() as u64,
                    mtime_ms: Some(1_700_000_000_000),
                    is_dir: false,
                }));
            }
            // The map holds files only; a path that prefixes other keys is a
            // directory, which is what the pull walker's stat dispatch asks.
            let prefix = format!("{}/", path.trim_end_matches('/'));
            if files.keys().any(|k| k.starts_with(&prefix)) {
                return Ok(Some(RemoteFileStat {
                    size: 0,
                    mtime_ms: None,
                    is_dir: true,
                }));
            }
            Ok(None)
        }

        async fn open_read(
            &self,
            _endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Box<dyn RemoteRead>, RemoteError> {
            let bytes = self
                .files
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| RemoteError::Failed(format!("no such file: {path}")))?;
            Ok(Box::new(FakeRead { bytes, at: 0 }))
        }

        async fn create(
            &self,
            _endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Box<dyn RemoteWrite>, RemoteError> {
            Ok(Box::new(FakeWrite {
                files: self.files.clone(),
                path: path.to_string(),
                buf: Vec::new(),
            }))
        }

        async fn mkdir_p(
            &self,
            _endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<(), RemoteError> {
            self.files
                .lock()
                .unwrap()
                .insert(format!("{path}/"), Vec::new());
            Ok(())
        }

        async fn list_dir(
            &self,
            _endpoint: &str,
            _holder: &str,
            path: &str,
        ) -> Result<Vec<RemoteListing>, RemoteError> {
            // Listings are derived from the map keys: the immediate children of
            // the listed path are the first segments of deeper keys, and a child
            // is a directory when it is a prefix of other keys.
            let files = self.files.lock().unwrap();
            let prefix = format!("{}/", path.trim_end_matches('/'));
            let mut names: Vec<String> = Vec::new();
            for key in files.keys() {
                let Some(rest) = key.strip_prefix(prefix.as_str()) else {
                    continue;
                };
                if rest.is_empty() {
                    continue; // the directory's own mkdir marker, not a child
                }
                let name = rest.split('/').next().unwrap_or_default().to_string();
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
            }
            names.sort();
            Ok(names
                .into_iter()
                .map(|name| {
                    let child = format!("{prefix}{name}");
                    RemoteListing {
                        is_dir: files.keys().any(|k| k.starts_with(&format!("{child}/"))),
                        size: files.get(&child).map(|b| b.len() as u64).unwrap_or(0),
                        name,
                    }
                })
                .collect())
        }
    }

    pub struct FakeRead {
        pub bytes: Vec<u8>,
        pub at: usize,
    }

    #[async_trait]
    impl RemoteRead for FakeRead {
        async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, RemoteError> {
            if self.at >= self.bytes.len() {
                return Ok(None);
            }
            let end = (self.at + 64 * 1024).min(self.bytes.len());
            let chunk = self.bytes[self.at..end].to_vec();
            self.at = end;
            Ok(Some(chunk))
        }
    }

    pub struct FakeWrite {
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        path: String,
        buf: Vec<u8>,
    }

    #[async_trait]
    impl RemoteWrite for FakeWrite {
        async fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), RemoteError> {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }

        async fn finish(&mut self) -> Result<(), RemoteError> {
            self.files
                .lock()
                .unwrap()
                .insert(self.path.clone(), std::mem::take(&mut self.buf));
            Ok(())
        }
    }

    /// A transport that serves nothing: the plugin-missing shape every action must
    /// answer honestly.
    #[allow(dead_code)] // the no-provider shape, kept for future tests to name
    pub struct NoTransport;

    #[async_trait]
    impl RemoteTransportProvider for NoTransport {
        fn list(&self) -> Vec<RemoteEndpoint> {
            Vec::new()
        }

        fn supports_files(&self) -> bool {
            false
        }

        async fn exec(
            &self,
            _endpoint: &str,
            _holder: &str,
            _request: RemoteExecRequest,
            _events: tokio::sync::mpsc::Sender<RemoteExecEvent>,
            _cancel: CancelHandle,
        ) -> Result<RemoteExecResult, RemoteError> {
            Err(RemoteError::Unavailable("no transport".into()))
        }

        async fn stat(
            &self,
            _endpoint: &str,
            _holder: &str,
            _path: &str,
        ) -> Result<Option<RemoteFileStat>, RemoteError> {
            Err(RemoteError::Unsupported("no transport".into()))
        }

        async fn open_read(
            &self,
            _endpoint: &str,
            _holder: &str,
            _path: &str,
        ) -> Result<Box<dyn RemoteRead>, RemoteError> {
            Err(RemoteError::Unsupported("no transport".into()))
        }

        async fn create(
            &self,
            _endpoint: &str,
            _holder: &str,
            _path: &str,
        ) -> Result<Box<dyn RemoteWrite>, RemoteError> {
            Err(RemoteError::Unsupported("no transport".into()))
        }

        async fn mkdir_p(
            &self,
            _endpoint: &str,
            _holder: &str,
            _path: &str,
        ) -> Result<(), RemoteError> {
            Err(RemoteError::Unsupported("no transport".into()))
        }

        async fn list_dir(
            &self,
            _endpoint: &str,
            _holder: &str,
            _path: &str,
        ) -> Result<Vec<RemoteListing>, RemoteError> {
            Err(RemoteError::Unsupported("no transport".into()))
        }
    }

    /// Deterministic test wiring: fresh services, the fake registered as the
    /// transport, a scratch target table with one target on the fake endpoint.
    pub fn system_with_fake() -> (Arc<crate::RemoteSystem>, Arc<FakeTransport>) {
        swiss_core::secure::key::use_test_master_key();
        let transport = FakeTransport::new();
        let services = RuntimeServices::new();
        services
            .remote
            .register(transport.clone(), "fake")
            .expect("the fake registers");
        let dir =
            std::env::temp_dir().join(format!("swiss-rsys-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch");
        let system = crate::RemoteSystem::open(services, dir.join("remote.json"));
        system.with_store(|store| {
            store
                .add(crate::target::RemoteTarget {
                    id: "dev".into(),
                    label: "dev".into(),
                    endpoint: "conn-1".into(),
                    workspace_root: "/data/ws/proj".into(),
                    shell: "posix".into(),
                    capabilities: vec!["exec".into(), "files".into(), "sync".into()],
                    default_timeout_ms: None,
                })
                .expect("the dev target adds");
        });
        (system, transport)
    }
}

/// The whole submit-then-read chain (docs/32 SS17): POST /api/runs answers 202 with
/// a runId immediately, the live output endpoint streams what the exec produced while
/// it runs, and the finished row carries the REMOTE exit code. Mounted through the
/// REAL host routes (services::api) plus the real /api/remote tree, driven with
/// tower::ServiceExt::oneshot like every integration test in this repo - no port,
/// no sleep, no server.
#[cfg(test)]
mod chain {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn a_remote_exec_submitted_over_http_streams_and_exits_with_the_remote_code() {
        let (system, fake) = crate::testing::system_with_fake();
        fake.program(
            "make",
            crate::testing::FakeProgram {
                argv0: "make".into(),
                stdout: b"compiling...\nok\n".to_vec(),
                stderr: Vec::new(),
                exit: 0,
                delay_ms: 0,
            },
        );
        crate::actions::register_all(system.clone(), &system.services().actions).unwrap();
        let state = crate::api::RemoteState::new();
        state.install(system.clone());
        let app = swiss_host::services::api::mount(system.services().clone())
            .merge(crate::api::mount(state));

        // Submit: 202, runId, nothing held open.
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/runs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({
                            "action": "remote.exec",
                            "input": { "target": "dev", "argv": ["make", "-j2"] },
                            "timeoutMs": 30000,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let submitted: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let run_id = submitted["runId"].as_u64().unwrap();

        // Read the output through the cursor API until the run is terminal.
        let mut saw_output = false;
        let mut terminal = false;
        for _ in 0..500 {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/runs/{run_id}/output?after=0&max=65536"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                .await
                .unwrap();
            let chunk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            saw_output |= chunk["output"].as_str().unwrap_or("").contains("ok");
            if chunk["terminal"].as_bool().unwrap_or(false) {
                terminal = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert!(terminal, "the run reached a terminal state");
        assert!(saw_output, "the streamed output carried the exec stdout");

        // The finished row carries the remote exit code.
        let response = app
            .oneshot(
                Request::get(format!("/api/runs/{run_id}?output=0"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let run: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(run["state"], "succeeded");
        assert_eq!(run["exitCode"], 0);
    }
}
