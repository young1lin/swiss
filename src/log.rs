//! JSON-line logging — port of `log.ts` (3 lines there; the timestamp work is what grew).
//! Hand-rolled on purpose: no `tracing`, no subscriber machinery (ADR-007).

use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// `new Date().toISOString()` — RFC3339 with millisecond precision, UTC, `Z`-suffixed.
pub fn iso_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

pub fn log(level: &str, msg: &str, extra: Option<Value>) {
    // `extra` is dropped when absent, exactly as JSON.stringify drops `undefined` values.
    let mut line = json!({ "ts": iso_now(), "level": level, "msg": msg });
    if let Some(extra) = extra {
        line["extra"] = extra;
    }
    println!("{line}");
}

pub fn info(msg: &str) {
    log("info", msg, None);
}
pub fn warn(msg: &str, extra: Option<Value>) {
    log("warn", msg, extra);
}
pub fn error(msg: &str, extra: Option<Value>) {
    log("error", msg, extra);
}
