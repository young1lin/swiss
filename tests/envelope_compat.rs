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

//! Cross-implementation proof: this build opens an envelope the NODE build sealed.
//!
//! `tests/fixtures/node-sealed.json` was produced by the reference implementation
//! (`../local-mcp-gateway/dist/secure/envelope.js`) under a fixed master key of 32 bytes of 0x07,
//! the same key this test uses. Regenerate it with:
//!
//! ```text
//! cd ../local-mcp-gateway && node --input-type=module -e '
//!   import { seal } from "./dist/secure/envelope.js";
//!   process.stdout.write(JSON.stringify(seal(Buffer.alloc(32, 7), "env", PAYLOAD), null, 2));'
//! ```
//!
//! This is the test that catches an HKDF argument-order slip, a base64 mistake, or a wrong
//! `HKDF_INFO` — every one of which compiles, passes a Rust-only round trip, and then fails to open
//! a single real file on a user's machine. See SPEC §formats.

use swiss_core::secure::envelope::{is_sealed, seal, unseal, Sealed};

/// The key the fixture was sealed under: `Buffer.alloc(32, 7)`.
const FIXTURE_KEY: [u8; 32] = [7u8; 32];

fn fixture() -> Sealed {
    let raw = include_str!("fixtures/node-sealed.json");
    serde_json::from_str(raw).expect("the fixture is a well-formed envelope")
}

#[test]
fn opens_an_envelope_sealed_by_the_node_build() {
    let opened = unseal(&FIXTURE_KEY, &fixture()).expect("the Node envelope must open here");
    let parsed: serde_json::Value =
        serde_json::from_str(&opened).expect("the payload is the state JSON");

    assert_eq!(parsed["tokenEnv"], "MCP_GATEWAY_TOKEN");
    assert_eq!(parsed["port"], 19999);
    assert_eq!(parsed["servers"]["echo"]["type"], "echo");
}

#[test]
fn the_fixture_is_recognised_as_sealed() {
    let raw: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/node-sealed.json")).expect("well-formed JSON");
    assert!(is_sealed(&raw));
}

#[test]
fn the_fixture_carries_the_frozen_format_fields() {
    let f = fixture();
    // FROZEN wire marker: the envelope key is the format's historical name (SPEC §formats.sealed).
    assert_eq!(f.lmg, 1);
    assert_eq!(f.alg, "aes-256-gcm");
    // keySource is a diagnostic, never trusted for key selection — but it must survive a read.
    assert_eq!(f.key_source, "env");
}

#[test]
fn what_this_build_seals_matches_what_the_node_build_expects() {
    // The other direction: seal here, and assert the envelope has exactly the shape the Node
    // build's `isSealed` accepts and its `unseal` can consume (field names, base64 lengths).
    let sealed = seal(&FIXTURE_KEY, "env", r#"{"round":"trip"}"#);
    let as_json = serde_json::to_value(&sealed).expect("serialisable");

    assert!(is_sealed(&as_json));
    for field in ["lmg", "alg", "keySource", "salt", "iv", "tag", "ct"] {
        assert!(
            as_json.get(field).is_some(),
            "the Node build reads `{field}` — it must be present"
        );
    }
    assert_eq!(
        unseal(&FIXTURE_KEY, &sealed).unwrap(),
        r#"{"round":"trip"}"#
    );
}
