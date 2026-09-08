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

/// The object a log call prints. Split out from the print so its shape can be asserted without
/// capturing stdout — the field names and their order are what a log reader parses.
fn line(level: &str, msg: &str, extra: Option<Value>) -> Value {
    // `extra` is dropped when absent, exactly as JSON.stringify drops `undefined` values.
    let mut line = json!({ "ts": iso_now(), "level": level, "msg": msg });
    if let Some(extra) = extra {
        line["extra"] = extra;
    }
    line
}

pub fn log(level: &str, msg: &str, extra: Option<Value>) {
    println!("{}", line(level, msg, extra));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_rfc3339_with_a_z_suffix() {
        // `new Date().toISOString()` is what the ported code and every log reader expect; a local
        // offset here would silently shift every timestamp the gateway writes.
        let ts = iso_now();
        assert!(ts.ends_with('Z'), "{ts}");
        assert!(ts.contains('T'), "{ts}");
        assert!(crate::util::parse_iso_ms(&ts).is_some(), "{ts}");
    }

    #[test]
    fn a_line_carries_the_stamp_the_level_and_the_message() {
        let l = line("info", "started", None);
        assert_eq!(l["level"], json!("info"));
        assert_eq!(l["msg"], json!("started"));
        assert!(crate::util::parse_iso_ms(l["ts"].as_str().unwrap_or("")).is_some(), "{l}");
        // Absent extra is DROPPED, not written as null — JSON.stringify's behaviour, and what a
        // reader that checks for the key relies on.
        assert!(l.get("extra").is_none(), "{l}");
    }

    #[test]
    fn extra_rides_along_untouched_when_there_is_some() {
        let l = line("warn", "slow probe", Some(json!({ "name": "redis", "ms": 1200 })));
        assert_eq!(l["level"], json!("warn"));
        assert_eq!(l["extra"], json!({ "name": "redis", "ms": 1200 }));
    }

    #[test]
    fn the_line_is_one_json_object_per_line() {
        // It is printed with `println!`, so a newline inside the serialized form would split one
        // event into two unparseable lines.
        let rendered = line("error", "boom\nsecond line", Some(json!({ "a": "b\nc" }))).to_string();
        assert!(!rendered.contains('\n'), "{rendered}");
        let back: Value = serde_json::from_str(&rendered).expect("one line, one object");
        assert_eq!(back["msg"], json!("boom\nsecond line"));
    }
}
