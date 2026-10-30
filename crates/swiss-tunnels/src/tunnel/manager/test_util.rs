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

//! The harness the manager's tests share: a store in a scratch directory, a manager over fake
//! clients, connection and rule fixtures, a local echo server and port probes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Duration as StdDuration};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::tunnel::ssh::SshHooks;
use crate::tunnel::store::{ConnInput, RuleInput, TunnelStore};
use crate::tunnel::types::{AuthType, RuleDef, SshConnDef, TunnelError};
use super::client::{ConnFactory, SshLike};
use super::fake::{Built, FakeConn};
use super::links::McpView;
use super::TunnelManager;

/// A store on a fresh temp file. The tunnels file is sealed like every other state file, so
/// the whole test binary is pinned to one deterministic master key rather than reaching for
/// DPAPI or the machine id.
pub(super) fn scratch() -> (PathBuf, Arc<Mutex<TunnelStore>>) {
    swiss_core::secure::key::use_test_master_key();
    let dir =
        std::env::temp_dir().join(format!("swiss-tmgr-{}", swiss_core::util::random_hex(8)));
    std::fs::create_dir_all(&dir).expect("create the scratch dir");
    let store = TunnelStore::new(dir.join("tunnels.json"), 19999);
    (dir, Arc::new(Mutex::new(store)))
}

pub(super) fn manager(store: &Arc<Mutex<TunnelStore>>) -> (Arc<TunnelManager>, Built) {
    manager_with(store, None, None)
}

/// `seed` makes every client this manager builds fail its dials — a host that is simply down.
pub(super) fn manager_with(
    store: &Arc<Mutex<TunnelStore>>,
    mcps: Option<Box<dyn McpView>>,
    seed: Option<TunnelError>,
) -> (Arc<TunnelManager>, Built) {
    let built: Built = Arc::new(Mutex::new(Vec::new()));
    let sink = built.clone();
    let factory: ConnFactory = Arc::new(move |def: &SshConnDef, hooks: SshHooks| {
        let conn = FakeConn::new(def, hooks);
        if let Some(err) = &seed {
            *conn.fail_with.lock().unwrap() = Some(err.clone());
        }
        sink.lock().unwrap().push(conn.clone());
        conn as Arc<dyn SshLike>
    });
    (
        TunnelManager::with_connections(store.clone(), mcps, factory),
        built,
    )
}

pub(super) fn built_len(built: &Built) -> usize {
    built.lock().unwrap().len()
}

pub(super) fn nth(built: &Built, i: usize) -> Arc<FakeConn> {
    built.lock().unwrap()[i].clone()
}

pub(super) fn conn_input() -> ConnInput {
    ConnInput {
        name: "srv".into(),
        host: "10.0.0.1".into(),
        port: 22.0,
        username: "deploy".into(),
        auth_type: AuthType::Key,
        key_path: Some("C:/keys/id_rsa".into()),
        ..Default::default()
    }
}

pub(super) fn rule_input(name: &str, conn_id: &str, local: u16, target: u16) -> RuleInput {
    RuleInput {
        name: name.into(),
        connection_id: conn_id.into(),
        local_port: local as f64,
        target_host: "127.0.0.1".into(),
        target_port: target as f64,
        ..Default::default()
    }
}

pub(super) fn add_conn(store: &Arc<Mutex<TunnelStore>>) -> SshConnDef {
    store
        .lock()
        .unwrap()
        .add_connection(&conn_input())
        .expect("add the connection")
}

pub(super) fn add_rule(store: &Arc<Mutex<TunnelStore>>, input: RuleInput) -> RuleDef {
    store
        .lock()
        .unwrap()
        .add_rule(&input)
        .expect("add the rule")
}

pub(super) fn stored_rule(store: &Arc<Mutex<TunnelStore>>, id: &str) -> RuleDef {
    store.lock().unwrap().rule(id).expect("the stored rule")
}

pub(super) fn row_of(m: &Arc<TunnelManager>, id: &str) -> Value {
    m.rule_row(id).expect("a row for the rule")
}

pub(super) fn state_of(m: &Arc<TunnelManager>, id: &str) -> String {
    row_of(m, id)
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

pub(super) fn field(row: &Value, key: &str) -> Value {
    row.get(key).cloned().unwrap_or(Value::Null)
}

/// A local TCP server that echoes what it receives, upper-cased — the "remote service" the
/// tunnel carries traffic to. It lives until the test binary exits, which is what a service
/// on the far side of a tunnel does anyway.
pub(super) async fn echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
    let port = listener.local_addr().expect("echo addr").port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            let up = buf[..n].to_ascii_uppercase();
                            if sock.write_all(&up).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

/// A port nothing holds right now — bound and released, the way the Node suite picked one.
pub(super) async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind probe");
    listener.local_addr().expect("probe addr").port()
}

/// Hold a port the way a foreign process would, so a start has to fail on it.
pub(super) async fn squat() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind squatter");
    let port = listener.local_addr().expect("squatter addr").port();
    (listener, port)
}

pub(super) async fn round_trip(port: u16, text: &str) -> String {
    let mut c = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the tunnel");
    c.write_all(text.as_bytes()).await.expect("write");
    let mut buf = vec![0u8; text.len()];
    c.read_exact(&mut buf).await.expect("read the echo back");
    String::from_utf8(buf).expect("utf-8 echo")
}

pub(super) async fn port_is_free(port: u16) -> bool {
    crate::tunnel::port::probe_port(port, "127.0.0.1").await
}

/// Poll until `cond` holds. Recovery runs on spawned tasks, so a state change lands a few
/// scheduler turns — and, for a reconnect, one backoff delay — after the event behind it.
pub(super) async fn wait_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(super) fn shells(m: &Arc<TunnelManager>) -> Arc<crate::tunnel::shell::TunnelShells> {
    crate::tunnel::shell::TunnelShells::new(m.clone())
}

/// Wait for something a detached pump settles on its own — a reference handed back by a
/// guard's Drop, for instance. Polling, because there is no handle out here to await.
pub(super) async fn eventually(mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..200 {
        if done() {
            return true;
        }
        tokio::time::sleep(StdDuration::from_millis(5)).await;
    }
    done()
}
