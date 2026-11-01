/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */

//! The SQL agent's persisted analysis tool (SPEC §data.analysis). Every entry uses the same
//! ledger and read gate as the HTTP API. The browser receives references, never owns the rows.

use serde_json::{json, Value};
use swiss_host::services::catalog::CatalogRegistry;
use swiss_llm::agent::ToolSpec;
use std::sync::Arc;

use crate::analysis::{AnalysisStore, CatalogRunner};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "analysis".into(),
        description: "Persist and revisit a SQL analysis. Discover schema first; clarify material business ambiguity. run without taskId creates an analysis and executes sql; revise adds an immutable revision (requires baseRevision), then run executes it. list/get recover saved work; read fetches at most 20 rows of a result; validate actually measures unique/nonNull/rowCount rules over saved rows; chart saves a declarative chart; compare compares two result ids; save bookmarks work. Use this for answers users will refine or revisit, run_sql for exploration. Every answer names its result id, truncation and limits. Passing checks covers stored rows only, never certifies business meaning. No todo tool or fixed sequence is required.".into(),
        schema: json!({ "type": "object", "properties": {
            "op": { "type": "string", "enum": ["list", "get", "run", "revise", "read", "validate", "chart", "compare", "save", "cancel"] },
            "taskId": { "type": "string" }, "resultId": { "type": "string" },
            "revisionId": { "type": "string" }, "baseRevision": { "type": "string" },
            "sql": { "type": "string" }, "title": { "type": "string" },
            "conditions": { "type": "object", "description": "Explicit business definitions, time zone, date bounds, grain and unresolved assumptions." },
            "note": { "type": "string" }, "offset": { "type": "integer", "minimum": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 20 },
            "rule": { "type": "object", "properties": {
                "type": { "type": "string", "enum": ["unique", "nonNull", "rowCount"] },
                "columns": { "type": "array", "items": { "type": "string" } }, "expected": { "type": "integer" }
            }, "required": ["type"] },
            "chart": { "type": "object", "properties": {
                "kind": { "type": "string", "enum": ["bar", "line", "area", "scatter"] },
                "x": { "type": "string" }, "y": { "type": "array", "items": { "type": "string" } }, "title": { "type": "string" }
            }, "required": ["kind", "x", "y"] },
            "left": { "type": "string" }, "right": { "type": "string" },
            "keys": { "type": "array", "items": { "type": "string" } },
            "saved": { "type": "boolean" }, "executionId": { "type": "string" }
        }, "required": ["op"] }),
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().filter(|v| !v.trim().is_empty()).ok_or_else(|| format!("{key} is required"))
}

pub async fn call(store: &AnalysisStore, catalog: Arc<CatalogRegistry>, conn: &str, args: &Value) -> Result<Value, String> {
    let op = required(args, "op")?;
    if op == "list" {
        return Ok(json!({ "analyses": store.list(Some(conn)).into_iter().take(50).collect::<Vec<_>>() }));
    }
    let mut task = if op == "run" && args["taskId"].as_str().is_none() {
        store.create(conn, "sql", args["title"].as_str().unwrap_or_default(), required(args, "sql")?,
            args.get("conditions").cloned().unwrap_or_else(|| json!({})), json!({}), args["note"].as_str().unwrap_or_default())?
    } else {
        store.get(required(args, "taskId")?)?
    };
    if task.conn != conn { return Err("the analysis belongs to another connection".into()) }
    let value = match op {
        "get" => {
            let rev = task.revision(&task.current).ok_or("unknown current revision")?;
            json!({ "current": rev, "results": task.results, "executions": task.executions,
                "revisions": task.revisions.iter().rev().take(20).collect::<Vec<_>>() })
        }
        "run" => {
            if args["taskId"].as_str().is_some() && args["sql"].as_str().is_some() {
                return Err("revise SQL first with baseRevision; run never silently changes a revision".into());
            }
            let runner = CatalogRunner::new(catalog, conn);
            store.execute(&runner, &task.id, args["revisionId"].as_str(), None, None).await?
        }
        "revise" => {
            task = store.revise(&task.id, required(args, "baseRevision")?, required(args, "sql")?,
                args.get("conditions").cloned().unwrap_or_else(|| json!({})), json!({}), args["note"].as_str().unwrap_or_default())?;
            json!({ "revisionId": task.current })
        }
        "read" | "validate" | "chart" => {
            let result = args["resultId"].as_str().or_else(|| task.current_result().map(|r| r.id.as_str())).ok_or("no current result; run the revision first")?;
            match op {
                "read" => store.rows(&task.id, result, args["offset"].as_u64().unwrap_or(0).min(20_000) as usize,
                    args["limit"].as_u64().unwrap_or(20).clamp(1, 20) as usize)?,
                "validate" => store.validate(&task.id, result, args.get("rule").ok_or("rule is required")?)?,
                _ => store.set_chart(&task.id, result, args.get("chart").ok_or("chart is required")?)?,
            }
        }
        "compare" => {
            let keys: Vec<String> = args["keys"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
            store.compare(&task.id, required(args, "left")?, required(args, "right")?, &keys)?
        }
        "save" => { task = store.save(&task.id, args["saved"].as_bool().unwrap_or(true))?; json!({ "saved": task.saved }) }
        "cancel" => {
            let run = required(args, "executionId")?;
            if !task.executions.iter().any(|e| e.id == run) { return Err("unknown execution on this analysis".into()) }
            json!({ "stoppedWaiting": store.cancel(run), "databaseCancellationConfirmed": false })
        }
        _ => return Err("unknown analysis operation".into()),
    };
    let latest = store.get(&task.id)?;
    Ok(json!({ "taskId": task.id, "task": latest.head(), "value": value }))
}

/// Validate view references against persisted data before adding them to a user turn. A stale
/// view remains a valid historical reference, but cannot change the task's current revision.
pub fn view_context(store: &AnalysisStore, conn: &str, view: &Value) -> Result<String, String> {
    let task = store.get(required(view, "taskId")?)?;
    if task.conn != conn { return Err("the analysis view belongs to another connection".into()) }
    let result = task.result(required(view, "resultId")?).ok_or("unknown view result")?;
    let row_ids: Vec<&str> = view["rowIds"].as_array().into_iter().flatten().take(20).filter_map(Value::as_str).collect();
    for key in &row_ids {
        let (rid, index) = key.rsplit_once(':').ok_or("invalid result row id")?;
        if rid != result.id || index.parse::<u64>().ok().is_none_or(|n| n >= result.row_count) { return Err("unknown result row id".into()) }
    }
    Ok(json!({ "taskId": task.id, "revisionId": result.revision, "resultId": result.id,
        "rowIds": row_ids, "historical": result.revision != task.current, "truncated": result.truncated }).to_string())
}
