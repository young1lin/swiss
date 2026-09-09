//! The /api/jobs management surface - mounted inside the loopback guard like every other /api
//! tree, same conventions as the tunnel API (camelCase, absent-not-null, admin_error/admin_json).
//! The panel's Jobs tab (Node build's src/admin/js/jobs.js, copied here verbatim with the rest
//! of the panel) is the primary client; curl and AI agents use the same routes.
//!
//! Endpoints:
//! - GET    /api/jobs             - list jobs with lastRunAt/lastOk/running/nextDueAt
//! - PUT    /api/jobs/{name}      - create or update (body: command + everySec|cron,
//!   enabled?, timeoutMs?, cwd?)
//! - DELETE /api/jobs/{name}      - remove the job (its run history stays)
//! - POST   /api/jobs/{name}/run  - run now, wait for the outcome, return the record
//! - GET    /api/jobs/{name}/runs - run history page (newest first; ?limit=&before=)
//!
//! Example - vacuum a database through the MCP every night at 03:30 local time:
//!   curl -X PUT http://127.0.0.1:19999/api/jobs/nightly-vacuum \
//!        -H "Authorization: Bearer $TOKEN" \
//!        -d '{ "command": "psql -d app -c VACUUM ANALYZE", "cron": "30 3 * * *" }'
//! (the command is ARGV, not a shell line - no pipes or redirections unless you wrap it in
//! "cmd /c ..." / "sh -c ..." yourself; ${ENV_VAR} refs expand at run time).

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::Router;
use serde_json::{json, Map, Value};

use super::{run_record, JobDef, JobSystem, RunError};
use crate::app::{admin_error, admin_json};

/// A number from JSON that tolerates the 60.0 spelling of 60 (JS clients) but refuses
/// fractions and NaN - the tunnel store's num() coercion, scoped to what jobs need.
fn whole_number(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let f = v.as_f64()?;
    if f.is_finite() && f >= 1.0 && f.fract() == 0.0 {
        Some(f as u64)
    } else {
        None
    }
}

fn fail(err: &RunError) -> Response {
    match err {
        RunError::Unknown(m) => admin_error(StatusCode::NOT_FOUND, m),
        RunError::Busy(m) => admin_error(StatusCode::CONFLICT, m),
    }
}

/// Build a JobDef from a PUT body; the path's name wins over any name in the body (the panel
/// convention: the URL is the identity). Validation runs inside upsert; this only shapes.
fn def_from_body(name: &str, body: &Value) -> Result<JobDef, String> {
    let command = body
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "command is required".to_string())?;
    let opt_str = |key: &str| -> Option<String> {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let every_sec = match body.get("everySec") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            whole_number(v).ok_or_else(|| "everySec must be a whole number of seconds (>= 1)".to_string())?,
        ),
    };
    let cron = match body.get("cron") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => return Err("cron must be a string".into()),
    };
    Ok(JobDef {
        name: name.to_string(),
        command,
        every_sec,
        cron,
        // Absent keeps the current value on edit and defaults to true on create.
        enabled: body.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        timeout_ms: body
            .get("timeoutMs")
            .and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    whole_number(v)
                }
            })
            .unwrap_or(super::runner::DEFAULT_TIMEOUT_MS),
        cwd: opt_str("cwd"),
        last_run_ms: None,
        last_ok: None,
    })
}

/// Mount the jobs API. State-resolved (Router<()>), ready to merge into the app router.
pub fn mount(jobs: Arc<JobSystem>) -> Router {
    let mut r = Router::<Arc<JobSystem>>::new();

    r = r.route(
        "/api/jobs",
        get(|State(jobs): State<Arc<JobSystem>>| async move {
            admin_json(StatusCode::OK, json!({ "jobs": jobs.all_views() }))
        }),
    );

    r = r.route(
        "/api/jobs/{name}",
        put(
            |State(jobs): State<Arc<JobSystem>>,
             Path(name): Path<String>,
             body: crate::app::NodeBody| async move {
                let def = match def_from_body(&name, &body.0) {
                    Ok(def) => def,
                    Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
                };
                if let Err(err) = jobs.upsert(def) {
                    return admin_error(StatusCode::BAD_REQUEST, &err);
                }
                // Echo the saved row (with its live facts) in the panel's reply style.
                let view = jobs
                    .all_views()
                    .into_iter()
                    .find(|v| v["name"] == json!(name))
                    .unwrap_or(Value::Null);
                admin_json(StatusCode::OK, json!({ "job": view }))
            },
        )
        .delete(
            |State(jobs): State<Arc<JobSystem>>, Path(name): Path<String>| async move {
                if jobs.delete(&name) {
                    admin_json(StatusCode::OK, json!({ "deleted": name }))
                } else {
                    admin_error(StatusCode::NOT_FOUND, &format!("unknown job: {name}"))
                }
            },
        ),
    );

    r = r.route(
        "/api/jobs/{name}/run",
        post(
            |State(jobs): State<Arc<JobSystem>>, Path(name): Path<String>| async move {
                // Waits for the outcome (bounded by the job's own timeoutMs): the caller - an
                // AI agent testing a job, or a human - wants the record, not a 202 to poll.
                match jobs.clone().execute(&name, "manual").await {
                    Ok((seq, out)) => {
                        let mut rec = run_record("manual", &out);
                        if seq > 0 {
                            rec.insert("seq".into(), json!(seq));
                        }
                        admin_json(StatusCode::OK, json!({ "run": Value::Object(rec) }))
                    }
                    Err(err) => fail(&err),
                }
            },
        ),
    );

    r = r.route(
        "/api/jobs/{name}/runs",
        get(
            |Path(name): Path<String>,
             Query(q): Query<HashMap<String, String>>| async move {
                if !super::runlog::valid_name(&name) {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown job: {name}"));
                }
                let limit = q
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(super::runlog::RUNS_PAGE_SIZE);
                let before = q.get("before").and_then(|v| v.parse::<u64>().ok());
                let (runs, next) = super::runlog::read_page(&name, before, limit);
                // nextBefore is absent (not null) when the history is exhausted - the panel's
                // absent-not-null convention.
                let mut body = Map::new();
                body.insert("runs".into(), json!(runs));
                if let Some(next) = next {
                    body.insert("nextBefore".into(), json!(next));
                }
                admin_json(StatusCode::OK, Value::Object(body))
            },
        ),
    );

    r.with_state(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn scratch_system(name: &str) -> Arc<JobSystem> {
        crate::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!("lmg-jobs-api-{}-{}", name, crate::util::random_hex(8)));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let log_dir = dir.join("logs");
        std::fs::create_dir_all(&log_dir).expect("log dir");
        super::super::runlog::set_run_log_dir(log_dir);
        JobSystem::open(dir.join("jobs.json"))
    }

    async fn call(router: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .expect("a request");
        let resp = router.clone().oneshot(req).await.expect("a response");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .expect("a body");
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("json")
        };
        (status, value)
    }

    #[test]
    fn the_panel_tab_is_part_of_the_copied_tree() {
        // The panel's Jobs tab is the primary client of this API (ADR-009: the panel tree is
        // copied verbatim from the Node build). If jobs.js is missing here the tab 404s and hides
        // itself - the routes below would be curl-only with no warning anywhere else.
        let js = include_str!("../admin_assets/js/jobs.js");
        assert!(js.contains("/api/jobs/"), "the tab must call this API");
        // And the probe contract: the tab hides on any non-OK answer, which is what the disabled
        // subsystem stub (subsystems::absent_router) relies on to remove the tab cleanly.
        assert!(js.contains("probeJobs"), "the availability probe must stay");
    }

    #[tokio::test]
    async fn put_list_delete_round_trip() {
        let sys = scratch_system("crud");
        let router = mount(sys.clone());

        // Create: minimal body, defaults applied.
        let (status, body) = call(
            &router,
            "PUT",
            "/api/jobs/nightly",
            Some(json!({ "command": "echo ok", "cron": "30 3 * * *" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["job"]["cron"], json!("30 3 * * *"));
        assert_eq!(body["job"]["enabled"], json!(true), "default on");
        assert_eq!(body["job"]["timeoutMs"], json!(super::super::runner::DEFAULT_TIMEOUT_MS));

        // List shows it.
        let (status, body) = call(&router, "GET", "/api/jobs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["jobs"].as_array().map(Vec::len), Some(1));

        // Edit: disable it.
        let (status, body) = call(
            &router,
            "PUT",
            "/api/jobs/nightly",
            Some(json!({ "command": "echo ok", "cron": "30 3 * * *", "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["job"]["enabled"], json!(false));

        // Delete, then delete again: 404.
        let (status, _) = call(&router, "DELETE", "/api/jobs/nightly", None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(&router, "DELETE", "/api/jobs/nightly", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test]
    async fn invalid_definitions_are_400_with_a_reason() {
        let sys = scratch_system("validate");
        let router = mount(sys.clone());
        for body in [
            json!({ "cron": "30 3 * * *" }),                        // no command
            json!({ "command": "  " , "cron": "30 3 * * *" }),     // blank command
            json!({ "command": "x" }),                              // no schedule
            json!({ "command": "x", "everySec": 60, "cron": "* * * * *" }), // both
            json!({ "command": "x", "everySec": 0 }),               // zero interval
            json!({ "command": "x", "cron": "99 * * * *" }),        // bad cron
        ] {
            let (status, body) = call(&router, "PUT", "/api/jobs/bad", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body["error"].as_str().is_some(), "{body}");
        }
        // A bad NAME is a 400 too, not a silent filesystem hazard.
        let (status, _) = call(
            &router,
            "PUT",
            "/api/jobs/not%20ok",
            Some(json!({ "command": "x", "everySec": 60 })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_manual_run_returns_the_record_and_history_shows_it() {
        let _log = super::super::runlog::RUNLOG_TEST_LOCK.lock().await;
        let sys = scratch_system("run");
        let router = mount(sys.clone());
        call(
            &router,
            "PUT",
            "/api/jobs/echoer",
            Some(json!({ "command": if cfg!(windows) { "cmd /c echo hello-jobs" } else { "sh -c 'echo hello-jobs'" }, "everySec": 3600 })),
        )
        .await;

        let (status, body) = call(&router, "POST", "/api/jobs/echoer/run", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["run"]["ok"], json!(true), "{body}");
        assert_eq!(body["run"]["trigger"], json!("manual"));
        assert!(body["run"]["seq"].as_u64().is_some(), "{body}");
        assert!(body["run"]["output"].as_str().unwrap_or("").contains("hello-jobs"), "{body}");

        let (status, body) = call(&router, "GET", "/api/jobs/echoer/runs?limit=5", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["runs"][0]["trigger"], json!("manual"));
        assert!(body.get("nextBefore").is_none(), "history exhausted");

        // Unknown job: 404.
        let (status, _) = call(&router, "POST", "/api/jobs/ghost/run", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
