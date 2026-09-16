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

//! Precedence of the renamed environment variables over their Node-era names.
//!
//! An integration test on purpose: each `tests/*.rs` binary runs in its own process, which is
//! the only place runtime env mutation is sound. The rule under test: the new name (SWISS_*)
//! wins when both eras are set, and the legacy name (MCP_GATEWAY_*) keeps working alone, so no
//! existing shell, script, CI job or sealed state breaks.

use swiss_core::paths::{data_dir, env_listen_port, env_port_raw};
use swiss_core::secure::key::{master_key_candidates, MASTER_KEY_ENV, MASTER_KEY_ENV_SWISS};

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
fn new_env_names_win_and_legacy_still_works() {
    let tmp = std::env::temp_dir();

    // HOME: the new name wins when both are set; the legacy name still works alone.
    let swiss_home = tmp.join("env-precedence-swiss-home");
    let legacy_home = tmp.join("env-precedence-legacy-home");
    let before_home = (
        std::env::var_os("SWISS_HOME"),
        std::env::var_os("MCP_GATEWAY_HOME"),
    );
    // SAFETY: the only test in this binary, so nothing else reads these names while planted.
    unsafe {
        std::env::set_var("MCP_GATEWAY_HOME", &legacy_home);
        std::env::set_var("SWISS_HOME", &swiss_home);
    }
    assert_eq!(data_dir(), swiss_home);
    unsafe { std::env::remove_var("SWISS_HOME") };
    assert_eq!(data_dir(), legacy_home);
    restore("SWISS_HOME", before_home.0);
    restore("MCP_GATEWAY_HOME", before_home.1);

    // PORT: same precedence; a set-but-unusable value is None, not an override and not a panic.
    let before_port = (
        std::env::var_os("SWISS_PORT"),
        std::env::var_os("MCP_GATEWAY_PORT"),
    );
    unsafe {
        std::env::set_var("MCP_GATEWAY_PORT", "18093");
        std::env::set_var("SWISS_PORT", "18094");
    }
    assert_eq!(env_port_raw(), Some(("SWISS_PORT", "18094".to_string())));
    assert_eq!(env_listen_port(), Some(18094));
    unsafe { std::env::remove_var("SWISS_PORT") };
    assert_eq!(env_listen_port(), Some(18093));
    unsafe {
        std::env::remove_var("MCP_GATEWAY_PORT");
        std::env::set_var("SWISS_PORT", "not-a-port");
    }
    assert_eq!(env_listen_port(), None);
    restore("SWISS_PORT", before_port.0);
    restore("MCP_GATEWAY_PORT", before_port.1);

    // MASTER KEY: exactly one candidate, from whichever name is set. At least one of the two
    // stays planted for every call, so the OS key sources (which would write a master.key into
    // whatever data_dir() points at) are never reached. "cd" pairs parse to the byte 0xcd and
    // "ab" to 0xab, so the expected keys read at a glance.
    let before_key = (
        std::env::var_os(MASTER_KEY_ENV_SWISS),
        std::env::var_os(MASTER_KEY_ENV),
    );
    unsafe {
        std::env::set_var(MASTER_KEY_ENV, "ab".repeat(32));
        std::env::set_var(MASTER_KEY_ENV_SWISS, "cd".repeat(32));
    }
    let candidates = master_key_candidates().expect("the env key parses");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "env");
    assert_eq!(candidates[0].key, [0xcd; 32]);
    unsafe { std::env::remove_var(MASTER_KEY_ENV_SWISS) };
    let candidates = master_key_candidates().expect("the legacy env key parses");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].key, [0xab; 32]);
    restore(MASTER_KEY_ENV_SWISS, before_key.0);
    restore(MASTER_KEY_ENV, before_key.1);
}
