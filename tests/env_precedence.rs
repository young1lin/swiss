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

//! The renamed environment variables and their retired Node-era names.
//!
//! An integration test on purpose: each `tests/*.rs` binary runs in its own process, which is
//! the only place runtime env mutation is sound. The rule under test: only the SWISS_* names
//! are knobs — a Node-era MCP_GATEWAY_* name left over in some old shell is inert, never an
//! override and never an error.

use swiss_core::paths::{data_dir, default_data_dir_in, env_listen_port, env_port_raw, home_dir};
use swiss_core::secure::key::{master_key_candidates, MASTER_KEY_ENV_SWISS};

/// Restore one variable to exactly what it was before this test planted it (None removes it),
/// so the test leaves the runner's environment as it found it.
fn restore(name: &str, before: Option<std::ffi::OsString>) {
    match before {
        Some(value) => unsafe { std::env::set_var(name, value) },
        None => unsafe { std::env::remove_var(name) },
    }
}

/// One test on purpose: it mutates process-global env state, and the file's own process is the
/// isolation boundary. The sections run sequentially: HOME, PORT, MASTER KEY.
#[test]
fn only_the_swiss_names_are_knobs() {
    let tmp = std::env::temp_dir();

    // HOME: SWISS_HOME redirects; a leftover MCP_GATEWAY_HOME is inert.
    let swiss_home = tmp.join("env-precedence-swiss-home");
    let before_home = (
        std::env::var_os("SWISS_HOME"),
        std::env::var_os("MCP_GATEWAY_HOME"),
    );
    // SAFETY: the only test in this binary, so nothing else reads these names while planted.
    unsafe {
        std::env::set_var("MCP_GATEWAY_HOME", tmp.join("env-precedence-legacy-home"));
        std::env::set_var("SWISS_HOME", &swiss_home);
    }
    assert_eq!(data_dir(), swiss_home);
    unsafe { std::env::remove_var("SWISS_HOME") };
    assert_eq!(data_dir(), default_data_dir_in(&home_dir()));
    restore("SWISS_HOME", before_home.0);
    restore("MCP_GATEWAY_HOME", before_home.1);

    // PORT: SWISS_PORT names the port; a leftover MCP_GATEWAY_PORT is inert, and a
    // set-but-unusable SWISS_PORT is None, not an override and not a panic.
    let before_port = (
        std::env::var_os("SWISS_PORT"),
        std::env::var_os("MCP_GATEWAY_PORT"),
    );
    unsafe {
        std::env::set_var("MCP_GATEWAY_PORT", "18093");
    }
    assert_eq!(env_port_raw(), None);
    assert_eq!(env_listen_port(), None);
    unsafe {
        std::env::set_var("SWISS_PORT", "18094");
    }
    assert_eq!(env_port_raw(), Some(("SWISS_PORT", "18094".to_string())));
    assert_eq!(env_listen_port(), Some(18094));
    unsafe {
        std::env::remove_var("MCP_GATEWAY_PORT");
        std::env::set_var("SWISS_PORT", "not-a-port");
    }
    assert_eq!(env_listen_port(), None);
    restore("SWISS_PORT", before_port.0);
    restore("MCP_GATEWAY_PORT", before_port.1);

    // MASTER KEY: SWISS_MASTER_KEY overrides every source; a leftover
    // MCP_GATEWAY_MASTER_KEY is not a key source at all.
    let before_key = std::env::var_os(MASTER_KEY_ENV_SWISS);
    unsafe {
        std::env::set_var("MCP_GATEWAY_MASTER_KEY", "ab".repeat(32));
    }
    let candidates = master_key_candidates().expect("candidates");
    assert!(
        candidates.iter().all(|k| k.id != "env"),
        "the Node-era name is not a key source"
    );
    unsafe {
        std::env::set_var(MASTER_KEY_ENV_SWISS, "cd".repeat(32));
    }
    // "cd" pairs parse to the byte 0xcd, so the expected key reads at a glance.
    let candidates = master_key_candidates().expect("the env key parses");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "env");
    assert_eq!(candidates[0].key, [0xcd; 32]);
    restore(MASTER_KEY_ENV_SWISS, before_key);
    unsafe { std::env::remove_var("MCP_GATEWAY_MASTER_KEY") };
}
