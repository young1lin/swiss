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

//! The secret vault (SPEC §host.vault): names never values, writes by revision, the name grammar,
//! and vault references in MCP definitions.

use axum::http::StatusCode;
use serde_json::json;

use super::{def, setup};
use swiss_mcp::adapters::make_adapter;

// --- the secret vault (SPEC §host.vault) ------------------------------------------------------------------
// The real router, the real sealed file under the sandbox home: names only on GET, write-only
// PUT, rev-checked mutations, and the value observable through no response. One tokio mutex
// serializes these five — the vault is process-global state, and two tests planning mutations
// against the same rev would race each other into honest 409s.
pub(super) static VAULT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn the_vault_lists_names_never_values() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/panel-list",
        json!({ "value": "sk_live_never_in_a_response", "rev": rev }),
    )
    .await;

    let (status, body) = h.get("/api/secrets").await;
    assert_eq!(status, StatusCode::OK);
    let names = body["secrets"].as_array().expect("a names array");
    assert!(
        names.iter().any(|n| n == "panel-list"),
        "the name is listed: {names:?}"
    );
    assert!(
        !body.to_string().contains("sk_live_never_in_a_response"),
        "GET never carries a value"
    );
}

#[tokio::test]
async fn the_vault_stores_overwrites_and_deletes_by_rev() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");

    let (status, body) = h
        .put(
            "/api/secrets/panel-roundtrip",
            json!({ "value": "first", "rev": rev }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "store: {body}");
    let rev2 = body["rev"].as_u64().expect("the bumped rev");

    let (status, body) = h
        .put(
            "/api/secrets/panel-roundtrip",
            json!({ "value": "second", "rev": rev2 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "overwrite: {body}");

    // The stale rev the panel was planning on is a 409 carrying both numbers.
    let (status, body) = h
        .put(
            "/api/secrets/panel-roundtrip",
            json!({ "value": "third", "rev": rev }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale rev: {body}");
    assert_eq!(body["have"], rev2 + 1);
    assert_eq!(body["saw"], rev);

    let resolved =
        swiss_core::secure::refs::resolve("${secret://panel-roundtrip}").expect("stored");
    assert_eq!(resolved, "second", "the overwrite won");

    let (status, _) = h.delete("/api/secrets/panel-roundtrip").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a rev-less delete is refused"
    );

    let rev_now = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    let (status, _) = h
        .delete(&format!("/api/secrets/panel-roundtrip?rev={rev_now}"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        swiss_core::secure::secretstore::vault_lookup("panel-roundtrip").is_none(),
        "gone from the vault"
    );

    // Deleting again with the OLD rev answers the rev mismatch (the rev gate comes before
    // the existence check — a stale caller learns its rev is stale, not what is stored).
    let (status, body) = h
        .delete(&format!("/api/secrets/panel-roundtrip?rev={rev_now}"))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "stale rev delete: {body}");

    // With the fresh rev, the absent name is an honest 404.
    let rev_after = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    let (status, _) = h
        .delete(&format!("/api/secrets/panel-roundtrip?rev={rev_after}"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "already gone, honestly");
}

#[tokio::test]
async fn the_vault_refuses_names_outside_the_grammar() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    for bad in ["BadName", "under_score", "1digit", "-dash"] {
        let (status, body) = h
            .put(
                &format!("/api/secrets/{bad}"),
                json!({ "value": "v", "rev": 0 }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains("lowercase kebab"),
            "{bad} quotes the grammar: {msg}"
        );
    }
}

#[tokio::test]
async fn an_mcp_builder_refuses_a_missing_vault_reference() {
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    // The strict contract's first wiring (SPEC §host.refs): make_adapter resolves vault refs and
    // REFUSES the build when one names a secret this machine does not hold — the error names
    // the MCP, the JSON path, and the reference, never a value.
    let d = def(json!({
        "type": "http",
        "url": "https://example.test/mcp",
        "headers": { "Authorization": "Bearer ${secret://context7}" }
    }));
    let err = match make_adapter(&d, "context7", &h.calls, None) {
        Err(e) => e,
        Ok(_) => panic!("a missing vault reference must refuse the build"),
    };
    assert!(
        err.contains("context7")
            && err.contains("headers.Authorization")
            && err.contains("secret://context7")
            && err.contains("not in the vault"),
        "names the surface and the reference: {err}"
    );

    // With the secret stored, the same definition builds and the header carries the value.
    let rev = h.get("/api/secrets").await.1["rev"]
        .as_u64()
        .expect("a rev");
    h.put(
        "/api/secrets/context7",
        json!({ "value": "tok-context7", "rev": rev }),
    )
    .await;
    let adapter = make_adapter(&d, "context7", &h.calls, None).expect("builds once stored");
    assert_eq!(adapter.kind().to_string(), "http");
}

#[tokio::test]
async fn the_rest_connection_test_fails_honestly_on_a_missing_vault_reference() {
    // The rest branch of /api/mcpdefs/test resolves the WHOLE def before its one plain GET
    // (SPEC §host.refs): a missing vault reference answers ok:false naming the JSON path — no
    // request is made, no reference text ships anywhere.
    let _guard = VAULT_LOCK.lock().await;
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcpdefs/test",
            json!({
                "type": "rest",
                "baseUrl": "https://example.test/api",
                "tools": [{ "name": "ping", "template": { "method": "GET", "path": "/x" } }],
                "headers": { "Authorization": "Bearer ${secret://rest-test-gone}" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "the test endpoint answers 200");
    assert_eq!(body["ok"], false, "and reports the failure: {body}");
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("headers.Authorization") && err.contains("secret://rest-test-gone"),
        "names the surface and the reference: {err}"
    );
}

#[tokio::test]
async fn the_connection_test_accepts_mariadb_like_mysql() {
    // The panel's Test button offers mariadb (fields.ts TESTABLE_TYPES) while the gate
    // here refused it with a 400 — the adapter itself was always able to answer. A dead
    // port proves the pass-through: ok:false with the driver's error, in-band, never
    // "no connection test for type".
    let h = setup();
    let (status, body) = h
        .post(
            "/api/mcpdefs/test",
            json!({
                "type": "mariadb",
                "host": "127.0.0.1",
                "port": 1,
                "user": "root",
                "password": "nonsense"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "mariadb passes the gate: {status}");
    assert_eq!(body["ok"], false, "the dead port answers in-band: {body}");
    let err = body["error"].as_str().unwrap_or_default();
    assert!(
        !err.contains("no connection test"),
        "the gate must not refuse mariadb: {err}"
    );
}

#[tokio::test]
async fn a_vault_reference_masks_like_an_env_ref() {
    // is_env_ref is what the whole masking stack keys off; a vault reference must read as a
    // reference so the panel edit forms show it as authored, never hold the value. Both the
    // envelope form (SPEC §host.refs) and the legacy whole-value bare form (still on disks the
    // loaders have not re-saved, SPEC §host.refs) pass; anything mixed or malformed must not.
    use swiss_host::config::is_env_ref;
    assert!(is_env_ref(&json!("secret://stripe-key")));
    assert!(is_env_ref(&json!("${secret://stripe-key}")));
    assert!(is_env_ref(&json!("${STRIPE_KEY}")));
    assert!(!is_env_ref(&json!("secret://")));
    assert!(!is_env_ref(&json!("secret://BadName")));
    assert!(!is_env_ref(&json!("${secret://}")));
    assert!(!is_env_ref(&json!("${secret://BadName}")));
    assert!(!is_env_ref(&json!("Bearer ${secret://stripe-key}")));
    assert!(!is_env_ref(&json!("plain text")));
}
