//! Subsystem composition — the plugin-ization seam, ported from the reference harness's
//! "everything is a plugin" design into a statically-linked single binary.
//!
//! What survives the translation (and what this module IS):
//! - Rows: a subsystem is addressed by its key in gateway.config.json. Its config block, its
//!   enable/disable switch, and future knobs are all the SAME row — "turn off jobs" is
//!   {"jobs":{"disabled":true}}, no second mechanism, no separate flag file. Deleting the row
//!   re-enables: a subsystem absent from config is ON (the default-on rule).
//! - disable is entry metadata, never a field inside the sealed state file: tunnels.json and
//!   jobs.json are never touched by a toggle, so re-enabling restores exactly what was there.
//! - Explicit absence: a disabled subsystem's API answers a structured 503 naming the row that
//!   turned it off (see absent_router) — never a silent empty 200, never an anonymous 404.
//!   Consumers (the panel, an AI client) can tell "not built" from "switched off".
//! - One composition point: adding a subsystem is one module + one compose line + one state
//!   file + one test — the checklist RH enforces with verify scripts. Since the plugin host
//!   (host/) landed, server.rs composes subsystems as PLUGINS: their start/stop is the host's
//!   lifecycle, their rows are read through the ConfigStore (v2 plugins.<id> rows take
//!   precedence over these root rows), and a not-serving plugin's 503 comes from the host's
//!   live route boundary (host::api::plugin_boundary) rather than a swapped-in stub router.
//!
//! These helpers remain the row-reading and explicit-absence vocabulary of that design; the
//! tests below pin the row semantics the host now inherits.
//!
//! What is deliberately NOT ported: dynamic loading (no dylib; a Rust "plugin" is a module
//! compiled straight into the binary),
//! and realms (single-tenant local process). Runtime start/stop DID arrive — with the host —
//! and the MCP hosting core is a plugin now too (row "mcp"), while per-MCP disable remains
//! the panel's per-entry toggle in managed.json.

use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use serde_json::Value;

use crate::reply::admin_error;

/// Whether a subsystem's config row disables it. Strict boolean: anything but literal
/// `true` (absent row, absent flag, false, "yes", 1) reads as ENABLED — the fail-direction
/// is "still on", matching the default-on rule; a typo cannot silently switch a subsystem
/// off the way a truthy cast would.
pub fn disabled(raw: &Value, name: &str) -> bool {
    raw.get(name)
        .and_then(|row| row.get("disabled"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// The structured 503 a disabled subsystem answers on EVERY path it would have owned.
/// Shape is the admin API's {error} (admin_error), so the panel's toast shows it verbatim.
fn absent(subsystem: &'static str) -> Response {
    admin_error(
        StatusCode::SERVICE_UNAVAILABLE,
        &format!(
            "{subsystem} subsystem is disabled by gateway.config.json (row {{\"{subsystem}\":{{\"disabled\":true}}}}). \
             Remove the row or set disabled:false to re-enable; its saved state is untouched.",
        ),
    )
}

/// The router swapped in for a subsystem's whole API tree when its row is disabled: the
/// prefix itself plus every deeper path, every method, one answer.
pub fn absent_router(subsystem: &'static str, prefix: &str) -> Router {
    let deep = format!("{prefix}/{{*rest}}");
    Router::new()
        .route(prefix, any(move || async move { absent(subsystem) }))
        .route(&deep, any(move || async move { absent(subsystem) }))
}

/// The composition-point bookkeeping: what a disabled subsystem's boot logs, in one shape so
/// an operator grepping "subsystem" finds every toggle in the boot log.
pub fn log_disabled(subsystem: &str) {
    swiss_core::log::log(
        "info",
        "subsystem disabled",
        Some(serde_json::json!({
            "subsystem": subsystem,
            "row": format!("{{\"{subsystem}\":{{\"disabled\":true}}}}"),
            "note": "state files untouched; remove the row to re-enable",
        })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::util::ServiceExt;

    fn row(v: Value) -> Value {
        v
    }

    #[test]
    fn only_a_literal_true_disables() {
        let cfg = serde_json::json!({
            "jobs": { "disabled": true },
            "tunnels": { "disabled": false },
            "traffic": {},
            "weird": { "disabled": "yes" },
            "num": { "disabled": 1 },
        });
        assert!(disabled(&cfg, "jobs"));
        assert!(!disabled(&cfg, "tunnels"), "explicit false is enabled");
        assert!(
            !disabled(&cfg, "traffic"),
            "row without the flag is enabled"
        );
        assert!(!disabled(&cfg, "weird"), "a string is not a boolean");
        assert!(!disabled(&cfg, "num"), "a number is not a boolean");
        assert!(!disabled(&cfg, "absent"), "no row at all is enabled");
        // An empty config (first-run seed) disables nothing.
        assert!(!disabled(&row(serde_json::json!({})), "jobs"));
    }

    #[tokio::test]
    async fn a_disabled_subsystem_answers_a_structured_503_on_every_path() {
        let router = absent_router("jobs", "/api/jobs");
        for (method, uri) in [
            ("GET", "/api/jobs"),
            ("PUT", "/api/jobs/nightly"),
            ("POST", "/api/jobs/nightly/run"),
            ("GET", "/api/jobs/nightly/runs?limit=5"),
            ("DELETE", "/api/jobs/nightly"),
        ] {
            let req = axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .expect("a request");
            let resp = router.clone().oneshot(req).await.expect("a response");
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {uri}"
            );
            let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .expect("a body");
            let body: Value = serde_json::from_slice(&bytes).expect("the admin {error} shape");
            let msg = body["error"].as_str().unwrap_or_default();
            assert!(msg.contains("jobs"), "names the subsystem: {msg}");
            assert!(msg.contains("disabled"), "says what happened: {msg}");
            assert!(
                msg.contains("gateway.config.json"),
                "names the row that did it: {msg}"
            );
        }
    }

    #[tokio::test]
    async fn an_enabled_subsystem_is_not_shadowed_by_the_stub() {
        // The stub is only merged when the row disables the subsystem; this pins the wire-up
        // contract from the other side by asserting the stub's router NEVER answers 200 — the
        // real API it replaces does (jobs::api tests cover that half).
        let router = absent_router("tunnels", "/api/tunnels");
        let req = axum::http::Request::builder()
            .uri("/api/tunnels")
            .body(Body::empty())
            .expect("a request");
        let resp = router.oneshot(req).await.expect("a response");
        assert_ne!(resp.status(), StatusCode::OK);
        assert_ne!(resp.status(), StatusCode::NOT_FOUND, "not an anonymous 404");
    }

    #[test]
    fn the_raw_root_survives_a_config_load() {
        // The toggle reads GatewayConfig::raw, so the load must retain unknown rows verbatim —
        // a parser that dropped what it did not understand would silently re-enable every
        // subsystem on the next boot.
        swiss_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!("swiss-subs-{}", swiss_core::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        unsafe { std::env::set_var("SWISS_SUBS_TOKEN", "tok") };
        let path = dir.join("gateway.config.json");
        std::fs::write(
            &path,
            r##"{"tokenEnv":"SWISS_SUBS_TOKEN","servers":{},"jobs":{"disabled":true},"tunnels":{}}"##,
        )
        .expect("write a legacy-plaintext config (accepted, then re-sealed)");
        let cfg = swiss_host::config::load_config(&path).expect("loads");
        assert!(disabled(&cfg.raw, "jobs"), "the row survived the load");
        assert!(!disabled(&cfg.raw, "tunnels"));
    }
}
