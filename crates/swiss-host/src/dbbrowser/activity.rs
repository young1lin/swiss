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

//! Activity monitoring (SPEC §data.activity): the session list and the kill statements per
//! dialect.

use serde_json::{json, Map, Value};

use super::DbDialect;

// --- activity monitoring (SPEC §data.activity) -----------------------------------------------------------

/// The sessions query for the Activity page. Postgres reads pg_stat_activity with
/// pgadmin's dashboard columns — the wait_event pair, pg_blocking_pids flattened to a comma
/// string, and the querying session itself marked via pg_backend_pid so the panel can chip
/// it; MySQL reads information_schema.processlist, adminer's source, with COMMAND as the
/// state word and STATE as the wait detail. Both alias to the SAME reply keys: one table,
/// both dialects. The query text is capped server-side (LEFT 2000) so a runaway statement
/// cannot balloon the poll's payload; the panel truncates visually regardless.
/// `seconds` follows pgadmin: elapsed only while the state is active, 0 otherwise.
pub fn activity_sql(dialect: DbDialect) -> String {
    match dialect {
        DbDialect::Pg => "SELECT pid, \
             usename AS \"user\", \
             COALESCE(state, '') AS state, \
             CASE WHEN wait_event IS NULL THEN '' ELSE wait_event_type || ': ' || wait_event END AS wait, \
             CASE WHEN state = 'active' THEN COALESCE(EXTRACT(EPOCH FROM (now() - query_start))::bigint, 0) ELSE 0 END AS seconds, \
             LEFT(query, 2000) AS query, \
             (pid = pg_backend_pid()) AS own, \
             array_to_string(pg_blocking_pids(pid), ',') AS \"blockedBy\" \
             FROM pg_stat_activity ORDER BY pid"
            .to_string(),
        DbDialect::Mysql => "SELECT ID AS pid, \
             USER AS `user`, \
             COALESCE(COMMAND, '') AS state, \
             COALESCE(STATE, '') AS wait, \
             COALESCE(TIME, 0) AS seconds, \
             LEFT(COALESCE(INFO, ''), 2000) AS query, \
             (ID = CONNECTION_ID()) AS own \
             FROM information_schema.processlist ORDER BY ID"
            .to_string(),
        // No sessions to list. The shape stays so a caller that does ask gets no rows
        // rather than another dialect's SQL.
        DbDialect::Sqlite => "SELECT NULL AS pid, '' AS \"user\", '' AS state, '' AS wait, \
             0 AS seconds, '' AS query, 0 AS own WHERE 0"
            .to_string(),
    }
}

/// Cancel vs terminate for one session: Postgres runs pg_cancel_backend /
/// pg_terminate_backend (pgadmin's pair); MySQL runs KILL QUERY / KILL. The pid is an i64 the
/// route already coerced, so the digit string cannot carry anything but a number.
pub fn activity_kill_sql(dialect: DbDialect, pid: i64, terminate: bool) -> Result<String, String> {
    if pid <= 0 {
        return Err("pid must be a session id (positive integer)".into());
    }
    Ok(match (dialect, terminate) {
        (DbDialect::Pg, false) => format!("SELECT pg_cancel_backend({pid})"),
        (DbDialect::Pg, true) => format!("SELECT pg_terminate_backend({pid})"),
        (DbDialect::Mysql, false) => format!("KILL QUERY {pid}"),
        (DbDialect::Mysql, true) => format!("KILL {pid}"),
        (DbDialect::Sqlite, _) => {
            return Err("this connection is query-only and has no sessions to cancel".into());
        }
    })
}

/// One Activity row, typed the way the panel computes on it. Both browsers answer
/// `activity()` through their grid `query()` path, whose cell renderer stringifies every
/// BIGINT on purpose (SPEC §data.browse, `exact_int64`: a JS number cannot hold one). Right for a
/// data grid; wrong for the three columns here that the panel does not merely display:
/// `pid` goes back into the kill route's `as_i64` (a `"88"` is a 400), `seconds` into a
/// duration, and `own` — MySQL's `(ID = CONNECTION_ID())` is an integer 0/1, so it arrived as
/// `"0"`/`"1"` — into an `if`, where `"0"` is truthy and marked EVERY mysql row "this panel"
/// (SPEC §panel.toolchain, 2026-09-20). The three are typed here, once, for both dialects; a cell
/// that is not a number/boolean spelling is left as it came, so an oddity shows rather than
/// vanishes. The display columns pass through untouched.
pub fn activity_row(mut row: Map<String, Value>) -> Map<String, Value> {
    for key in ["pid", "seconds"] {
        if let Some(n) = row.get(key).and_then(wire_i64) {
            row.insert(key.to_string(), json!(n));
        }
    }
    if let Some(b) = row.get("own").and_then(wire_bool) {
        row.insert("own".to_string(), json!(b));
    }
    row
}

/// An integer the driver may have shipped as a number or as its decimal text.
fn wire_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// A truth the driver may have shipped as a bool, a 0/1 (MySQL's comparison result), or
/// either of those as text.
fn wire_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|n| n != 0),
        Value::String(s) => match s.trim() {
            "0" | "false" | "f" => Some(false),
            "1" | "true" | "t" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_sql_names_each_dialects_session_source() {
        // SPEC §data.activity: Postgres reads pg_stat_activity with the blocking-pid expansion
        // (pgadmin's dashboard shape); MySQL reads information_schema.processlist (adminer's).
        let pg = activity_sql(DbDialect::Pg);
        assert!(pg.contains("pg_stat_activity"), "{pg}");
        assert!(pg.contains("pg_blocking_pids"), "{pg}");
        // The querying session itself must be visible AND marked, or the chip has no row.
        assert!(pg.contains("pg_backend_pid"), "{pg}");
        let my = activity_sql(DbDialect::Mysql);
        assert!(my.contains("information_schema.processlist"), "{my}");
        assert!(my.contains("CONNECTION_ID()"), "{my}");
        // Both speak the SAME reply keys, or the panel would need two tables.
        for key in ["pid", "user", "state", "wait", "seconds", "query", "own"] {
            assert!(pg.contains(key), "pg misses {key}");
            assert!(my.contains(key), "mysql misses {key}");
        }
    }

    #[test]
    fn activity_kill_sql_picks_cancel_and_terminate_per_dialect() {
        assert_eq!(
            activity_kill_sql(DbDialect::Pg, 42, false).unwrap(),
            "SELECT pg_cancel_backend(42)"
        );
        assert_eq!(
            activity_kill_sql(DbDialect::Pg, 42, true).unwrap(),
            "SELECT pg_terminate_backend(42)"
        );
        assert_eq!(
            activity_kill_sql(DbDialect::Mysql, 42, false).unwrap(),
            "KILL QUERY 42"
        );
        assert_eq!(
            activity_kill_sql(DbDialect::Mysql, 42, true).unwrap(),
            "KILL 42"
        );
        // A pid that is not a session id is the caller's whole mistake — refuse, never run
        // something surprising against pid 0 or a negative.
        for bad in [0i64, -1] {
            assert!(activity_kill_sql(DbDialect::Pg, bad, false).is_err());
            assert!(activity_kill_sql(DbDialect::Mysql, bad, true).is_err());
        }
    }

    fn row_of(v: Value) -> Map<String, Value> {
        v.as_object().cloned().expect("object")
    }

    #[test]
    fn activity_row_types_what_the_panel_computes_on_a_mysql_row() {
        // SPEC §panel.toolchain (2026-09-20): through the grid's BIGINT-as-text path mysql delivered
        // pid "88", seconds "12" and own "0" - and "0" is truthy in the panel, which marked
        // every row "this panel" and would have sent the kill route a pid it refuses.
        let row = activity_row(row_of(json!({
            "pid": "88", "user": "app", "state": "Sleep", "wait": "", "seconds": "12",
            "query": "", "own": "0"
        })));
        assert_eq!(row["pid"], json!(88));
        assert_eq!(row["seconds"], json!(12));
        assert_eq!(row["own"], json!(false));
        // The display columns are not the reply's business.
        assert_eq!(row["user"], json!("app"));
        assert_eq!(row["state"], json!("Sleep"));
        assert_eq!(row["query"], json!(""));
        // "1" is the one true spelling; a driver that types the comparison ships 1/0.
        assert_eq!(activity_row(row_of(json!({ "own": "1" })))["own"], json!(true));
        assert_eq!(activity_row(row_of(json!({ "own": 1 })))["own"], json!(true));
        assert_eq!(activity_row(row_of(json!({ "own": 0 })))["own"], json!(false));
    }

    #[test]
    fn activity_row_leaves_a_typed_postgres_row_alone() {
        // Postgres ships pid (int4) and own (bool) typed already; only its bigint `seconds`
        // rode the text path. Nothing else moves - null user, empty blockedBy included.
        let row = activity_row(row_of(json!({
            "pid": 478202, "user": null, "state": "active", "wait": "", "seconds": "4",
            "query": "SELECT 1", "own": true, "blockedBy": ""
        })));
        assert_eq!(row["pid"], json!(478202));
        assert_eq!(row["seconds"], json!(4));
        assert_eq!(row["own"], json!(true));
        assert_eq!(row["user"], Value::Null);
        assert_eq!(row["blockedBy"], json!(""));
        assert_eq!(row.len(), 8);
    }

    #[test]
    fn activity_row_keeps_a_cell_it_cannot_type() {
        // An unparseable cell stays as it came rather than vanishing: the panel shows the odd
        // text instead of a hole, and the kill route's own pid check still refuses it.
        let row = activity_row(row_of(json!({ "pid": "abc", "seconds": null, "own": "maybe" })));
        assert_eq!(row["pid"], json!("abc"));
        assert_eq!(row["seconds"], Value::Null);
        assert_eq!(row["own"], json!("maybe"));
    }
}
