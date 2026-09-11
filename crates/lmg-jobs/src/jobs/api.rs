//! The /api/jobs management surface - mounted inside the loopback guard like every other /api
//! tree, same conventions as the tunnel API (camelCase, absent-not-null, admin_error/admin_json).
//! The panel's Jobs tab (Node build's src/admin/js/jobs.js, copied here verbatim with the rest
//! of the panel) is the primary client; curl and AI agents use the same routes.
//!
//! Endpoints:
//! - GET    /api/jobs             - list jobs with lastRunAt/lastOk/running/nextDueAt
//! - PUT    /api/jobs/{name}      - create or update (body: command + everySec|cron,
//!   enabled?, timeoutMs?, cwd?, env? — an object of per-run environment variables)
//! - DELETE /api/jobs/{name}      - remove the job (its run history stays)
//! - POST   /api/jobs/{name}/run  - run now, wait for the outcome, return the record;
//!   body {"async": true} -> 202 {"runId": N}, polled via GET /api/runs/{id}
//! - GET    /api/jobs/{name}/runs - run history page (newest first; ?limit=&cursor=,
//!   `before` kept as the old spelling of `cursor`)
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

use super::{ran_record, JobDef, JobSystem, RunError, WriteError};
use lmg_host::reply::{admin_error, admin_json};

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

/// The status of a definition write (docs/11 §7.1): a bad definition is the caller's
/// (400), a job the v1 shape cannot spell points at the config editor (409), and a
/// failed persist is this gateway's (500).
fn write_status(err: &WriteError) -> StatusCode {
    match err {
        WriteError::Invalid(_) => StatusCode::BAD_REQUEST,
        WriteError::NotEditableV1(_) => StatusCode::CONFLICT,
        WriteError::Persist(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn fail(err: &RunError) -> Response {
    match err {
        RunError::Unknown(m) => admin_error(StatusCode::NOT_FOUND, m),
        RunError::Busy(m) => admin_error(StatusCode::CONFLICT, m),
        // The shared pool is full: the same visible capacity refusal /api/runs gives, so a
        // caller cannot mistake "not started" for "started and still going".
        RunError::Capacity(m) => admin_error(StatusCode::TOO_MANY_REQUESTS, m),
        // The process capability is not registered (its plugin is disabled) — a missing
        // dependency, reported as one rather than skipped in silence.
        RunError::Unavailable(m) => admin_error(StatusCode::SERVICE_UNAVAILABLE, m),
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
            whole_number(v)
                .ok_or_else(|| "everySec must be a whole number of seconds (>= 1)".to_string())?,
        ),
    };
    let cron = match body.get("cron") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(_) => return Err("cron must be a string".into()),
    };
    // Per-job env vars (the panel's "Environment variables" box). Same rules the
    // capability's own input parser enforces, refused HERE so a bad row never lands.
    let env = match body.get("env") {
        None | Some(Value::Null) => None,
        Some(Value::Object(map)) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map {
                let Some(s) = v.as_str() else {
                    return Err(format!("env.{k}: must be a string"));
                };
                if k.is_empty() || k.contains('=') || k.contains('\0') {
                    return Err(format!("env.{k}: not a usable variable name"));
                }
                out.insert(k.clone(), s.to_string());
            }
            if out.is_empty() { None } else { Some(out) }
        }
        Some(_) => return Err("env: must be an object of string values".into()),
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
            .and_then(|v| if v.is_null() { None } else { whole_number(v) })
            .unwrap_or(super::runner::DEFAULT_TIMEOUT_MS),
        cwd: opt_str("cwd"),
        env,
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
             body: lmg_host::reply::NodeBody| async move {
                let def = match def_from_body(&name, &body.0) {
                    Ok(def) => def,
                    Err(err) => return admin_error(StatusCode::BAD_REQUEST, &err),
                };
                if let Err(err) = jobs.upsert_v1(def) {
                    return admin_error(write_status(&err), &err.to_string());
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
                match jobs.delete(&name) {
                    Ok(true) => admin_json(StatusCode::OK, json!({ "deleted": name })),
                    Ok(false) => {
                        admin_error(StatusCode::NOT_FOUND, &format!("unknown job: {name}"))
                    }
                    // Removed from the table but not from the config row: the next boot
                    // brings it back, so this is not a deletion and must not report one.
                    Err(err) => admin_error(write_status(&err), &err.to_string()),
                }
            },
        ),
    );

    r = r.route(
        "/api/jobs/{name}/run",
        post(
            |State(jobs): State<Arc<JobSystem>>,
             Path(name): Path<String>,
             body: lmg_host::reply::NodeBody| async move {
                // {"async": true} (docs/11 §7.3): 202 + the runId, the work outlives the
                // request - the panel's Run now polls GET /api/runs/{id} so closing the
                // page never cancels a job. The SAME coordinator run as the sync door;
                // the record and lastOk settle identically, one tick later.
                if body.0.get("async").and_then(Value::as_bool) == Some(true) {
                    return match jobs.clone().execute_async(&name).await {
                        Ok(run_id) => admin_json(StatusCode::ACCEPTED, json!({ "runId": run_id })),
                        Err(err) => fail(&err),
                    };
                }
                // Default: waits for the outcome (bounded by the job's own timeoutMs) - the
                // caller, an AI agent testing a job or a human, wants the record, not a
                // 202 to poll.
                match jobs.clone().execute(&name, "manual").await {
                    Ok((seq, out)) => {
                        let mut rec = ran_record("manual", None, 1, 1, None, &out);
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
            |State(jobs): State<Arc<JobSystem>>,
             Path(name): Path<String>,
             Query(q): Query<HashMap<String, String>>| async move {
                if !super::runlog::valid_name(&name) {
                    return admin_error(StatusCode::NOT_FOUND, &format!("unknown job: {name}"));
                }
                let limit = q
                    .get("limit")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(super::runlog::RUNS_PAGE_SIZE);
                // `cursor` is the spelling docs/11 §7.3 gives the page walk; `before` stays
                // as its alias so existing clients (and the shared tests) see no change.
                let before = q
                    .get("cursor")
                    .or_else(|| q.get("before"))
                    .and_then(|v| v.parse::<u64>().ok());
                let (runs, next) = jobs.read_runs(&name, before, limit);
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

    /// A job system plus its config store on one private tree (the S3 shape): jobs.json,
    /// jobs-state.json, logs/jobs and gateway.config.json all derive from one scratch
    /// directory, so no two tests share anything.
    async fn scratch_system(
        name: &str,
    ) -> (Arc<JobSystem>, Arc<lmg_host::config_store::ConfigStore>) {
        lmg_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "lmg-jobs-api-{}-{}",
            name,
            lmg_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let store = lmg_host::config_store::ConfigStore::from_loaded(
            dir.join("gateway.config.json"),
            json!({}),
        );
        (
            JobSystem::open(
                dir.join("jobs.json"),
                super::super::test_services(),
                store.clone(),
            ),
            store,
        )
    }

    /// The same tree, but the CONFIG file sits under a directory that does not exist, so
    /// every definition persist fails while everything else stays real.
    async fn unwritable_system(name: &str) -> Arc<JobSystem> {
        lmg_core::secure::key::use_test_master_key();
        let dir = std::env::temp_dir().join(format!(
            "lmg-jobs-api-{}-{}",
            name,
            lmg_core::util::random_hex(8)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let store = lmg_host::config_store::ConfigStore::from_loaded(
            dir.join("gone").join("gateway.config.json"),
            json!({}),
        );
        JobSystem::open(dir.join("jobs.json"), super::super::test_services(), store)
    }

    async fn call(
        router: &Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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
        let js = include_str!("../../../lmg-panel/src/admin_assets/js/jobs.js");
        assert!(js.contains("/api/jobs/"), "the tab must call this API");
        // And the probe contract: the tab hides on any non-OK answer, which is what the disabled
        // subsystem stub (subsystems::absent_router) relies on to remove the tab cleanly.
        assert!(js.contains("probeJobs"), "the availability probe must stay");
    }

    #[tokio::test]
    async fn put_list_delete_round_trip() {
        let (sys, _store) = scratch_system("crud").await;
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
        assert_eq!(
            body["job"]["timeoutMs"],
            json!(super::super::runner::DEFAULT_TIMEOUT_MS)
        );

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

    /// docs/11 §7.1, the frozen half: every v1 field the panel reads is present with the
    /// type it always had, and the v2 fields ride along. One test per field so a silent
    /// shape change fails with the offender named.
    #[tokio::test]
    async fn the_listing_carries_the_frozen_v1_shape_and_the_v2_fields() {
        let (sys, store) = scratch_system("shape").await;
        let router = mount(sys.clone());
        call(
            &router,
            "PUT",
            "/api/jobs/shaped",
            Some(json!({ "command": "echo ok", "cron": "30 3 * * *", "cwd": "C:\\tmp", "timeoutMs": 12000, "env": { "DEPLOY_ENV": "staging", "EXTRA_FLAG": "1" } })),
        )
        .await;
        let (status, body) = call(&router, "GET", "/api/jobs", None).await;
        assert_eq!(status, StatusCode::OK);
        let job = &body["jobs"][0];
        assert_eq!(job["name"], json!("shaped"));
        assert_eq!(job["command"], json!("echo ok"));
        assert_eq!(job["cron"], json!("30 3 * * *"));
        assert!(job.get("everySec").is_none(), "absent-not-null, v1 rule");
        assert_eq!(job["enabled"], json!(true));
        assert_eq!(job["timeoutMs"], json!(12000));
        assert_eq!(job["cwd"], json!("C:\\tmp"));
        // The per-job env box round-trips: what the panel saved is what a run gets.
        assert_eq!(
            job["env"],
            json!({ "DEPLOY_ENV": "staging", "EXTRA_FLAG": "1" })
        );
        assert_eq!(job["running"], json!(false));
        assert!(
            job.get("lastRunAt").is_none(),
            "absent-not-null for a never-run job"
        );
        assert!(job.get("lastOk").is_none(), "absent-not-null");
        assert!(job["nextDueAt"].as_str().is_some(), "{job}");
        // The v2 additions (docs/11 §7.1).
        assert_eq!(job["id"], json!("shaped"));
        assert_eq!(job["title"], json!("shaped"));
        assert_eq!(job["labels"], json!([]));
        assert_eq!(job["trigger"]["kind"], json!("cron"));
        assert_eq!(job["action"]["type"], json!("process.legacy-command"));
        assert_eq!(job["overlap"], json!("skip"));
        assert_eq!(job["misfire"], json!("skip"));
        assert_eq!(job["source"], json!("config"));
        assert_eq!(job["actionAvailable"], json!(true));
        assert_eq!(job["editableInV1"], json!(true));
        assert!(job["configRevision"].as_u64().is_some(), "{job}");
        // The row the listing projects is the one in the store - one source of truth.
        assert_eq!(
            store.plugin_config("jobs")["definitions"]["shaped"]["trigger"]["expression"],
            json!("30 3 * * *"),
            "the v1 write landed in the config row"
        );
    }

    /// docs/11 §7.1: PUT on a job the v1 shape cannot spell is a 409 that names the
    /// config editor, and the definition is left exactly as it was.
    #[tokio::test]
    async fn v1_put_on_a_v2_only_job_is_a_409_pointing_at_the_config_editor() {
        let (sys, store) = scratch_system("v2only").await;
        let row = json!({
            "definitions": {
                "proud": {
                    "title": "A titled job",
                    "trigger": { "kind": "interval", "everyMs": 60000 },
                    "action": { "type": "process.legacy-command", "input": { "command": "echo hi" } }
                }
            }
        });
        let rev = store.snapshot().revision;
        store
            .update_plugin("jobs", rev, row.clone())
            .expect("the row installs");
        sys.apply_config(&row).expect("the row applies");
        let router = mount(sys.clone());

        let (status, body) = call(
            &router,
            "PUT",
            "/api/jobs/proud",
            Some(json!({ "command": "echo x", "everySec": 60 })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("/api/plugins/jobs/config"),
            "{body}"
        );
        // Refused, not applied: the row is untouched.
        assert_eq!(
            store.plugin_config("jobs")["definitions"]["proud"]["title"],
            json!("A titled job")
        );
        // And a definition whose provider is missing still lists, flagged.
        let (status, body) = call(&router, "GET", "/api/jobs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["jobs"][0]["editableInV1"], json!(false));
    }

    #[tokio::test]
    async fn invalid_definitions_are_400_with_a_reason() {
        let (sys, _store) = scratch_system("validate").await;
        let router = mount(sys.clone());
        for body in [
            json!({ "cron": "30 3 * * *" }),                   // no command
            json!({ "command": "  " , "cron": "30 3 * * *" }), // blank command
            json!({ "command": "x" }),                         // no schedule
            json!({ "command": "x", "everySec": 60, "cron": "* * * * *" }), // both
            json!({ "command": "x", "everySec": 0 }),          // zero interval
            json!({ "command": "x", "cron": "99 * * * *" }),   // bad cron
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

    /// A rejected DEFINITION is the caller's fault (400); a failed WRITE is this gateway's
    /// (500). One status for both would tell a user to fix a job that is already correct, and
    /// a 200 for the second would lose their edit without saying so.
    #[tokio::test]
    async fn a_save_that_never_reached_disk_is_a_500_not_a_success() {
        let sys = unwritable_system("persist").await;
        let router = mount(sys.clone());

        let (status, body) = call(
            &router,
            "PUT",
            "/api/jobs/doomed",
            Some(json!({ "command": "cmd /c echo x", "everySec": 60 })),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("persist"),
            "{body}"
        );

        // An invalid definition is still a 400 on that very same route.
        let (status, _) = call(
            &router,
            "PUT",
            "/api/jobs/bad",
            Some(json!({ "command": "x" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // The definitions row never landed, so there is nothing to delete: a 404,
        // not a write failure. The v1 edit path goes through the config store, and a
        // store whose file cannot be written refuses without half-applying anywhere.
        let (status, body) = call(&router, "DELETE", "/api/jobs/doomed", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let (status, _) = call(&router, "DELETE", "/api/jobs/never-existed", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // And the listing stayed empty: no half-applied definition anywhere.
        let (status, body) = call(&router, "GET", "/api/jobs", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["jobs"].as_array().map(Vec::len), Some(0));
    }

    #[tokio::test]
    async fn a_manual_run_returns_the_record_and_history_shows_it() {
        let (sys, _store) = scratch_system("run").await;
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
        assert!(
            body["run"]["output"]
                .as_str()
                .unwrap_or("")
                .contains("hello-jobs"),
            "{body}"
        );

        let (status, body) = call(&router, "GET", "/api/jobs/echoer/runs?limit=5", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["runs"][0]["trigger"], json!("manual"));
        assert!(body.get("nextBefore").is_none(), "history exhausted");

        // Unknown job: 404.
        let (status, _) = call(&router, "POST", "/api/jobs/ghost/run", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// docs/11 §7.3, the S6 half: POST /run with {"async": true} is a 202 carrying the
    /// runId. The run outlives the request - the response carries NO record - but the
    /// SAME coordinator run settles into the history with the same shape the sync door
    /// writes (outcome, runId, attempt fields), and lastOk lands with it.
    #[tokio::test]
    async fn an_async_manual_run_is_a_202_whose_record_settles_later() {
        let (sys, _store) = scratch_system("async-run").await;
        let router = mount(sys.clone());
        call(
            &router,
            "PUT",
            "/api/jobs/slow-echoer",
            Some(json!({ "command": if cfg!(windows) { "cmd /c echo async-hello" } else { "sh -c 'echo async-hello'" }, "everySec": 3600 })),
        )
        .await;

        let (status, body) = call(
            &router,
            "POST",
            "/api/jobs/slow-echoer/run",
            Some(json!({ "async": true })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let run_id = body["runId"].as_u64().expect("the runId to poll");
        assert!(
            body.get("run").is_none(),
            "no record yet - the work is still going"
        );

        // The coordinator knows the run (what GET /api/runs/{id} reads): same run id,
        // live right after the 202.
        let listed = sys.services.runs.list();
        assert!(
            listed.iter().any(|v| v.run_id == run_id),
            "run {run_id} is in the coordinator index"
        );

        // The settled record: poll the history like the panel does, bounded.
        let mut record = None;
        for _ in 0..300 {
            let (_, body) = call(&router, "GET", "/api/jobs/slow-echoer/runs?limit=5", None).await;
            if let Some(rec) = body["runs"].as_array().and_then(|a| a.first()) {
                if rec["runId"].as_u64() == Some(run_id) {
                    record = Some(rec.clone());
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let rec = record.expect("the async run's record settled");
        assert_eq!(rec["outcome"], json!("ran"), "{rec}");
        assert_eq!(rec["trigger"], json!("manual"));
        assert_eq!(rec["attempt"], json!(1), "a manual run is one attempt");
        assert!(
            rec.get("occurrenceKey").is_none(),
            "manual has no occurrence"
        );
        assert!(
            rec["output"].as_str().unwrap_or("").contains("async-hello"),
            "{rec}"
        );
        // And the run facts settled with it, exactly as the sync door writes them.
        assert_eq!(sys.run_state("slow-echoer").last_ok, Some(true));
    }

    /// docs/11 §7.3: `cursor` is the pagination parameter's name now, `before` its
    /// alias - both spellings walk the same pages.
    #[tokio::test]
    async fn the_history_cursor_parameter_walks_the_same_pages_as_before() {
        let (sys, _store) = scratch_system("cursor").await;
        // A history of three records, written straight into the log this system reads.
        for seq in [1u64, 2, 3] {
            sys.append_test_run("pager", seq);
        }
        let router = mount(sys.clone());

        let (status, body) = call(&router, "GET", "/api/jobs/pager/runs?limit=2", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["runs"].as_array().map(Vec::len), Some(2));
        let next = body["nextBefore"].as_u64().expect("a page remains");

        // The new spelling and the alias return the same page.
        let (status, by_cursor) = call(
            &router,
            "GET",
            &format!("/api/jobs/pager/runs?limit=2&cursor={next}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{by_cursor}");
        let (status, by_before) = call(
            &router,
            "GET",
            &format!("/api/jobs/pager/runs?limit=2&before={next}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{by_before}");
        assert_eq!(
            by_cursor["runs"], by_before["runs"],
            "cursor and before are one parameter"
        );
        assert_eq!(
            by_cursor["runs"].as_array().map(Vec::len),
            Some(1),
            "the oldest record"
        );
    }
}
