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

//! JSON values the way the Node build coerced them (`Number()`, `String()`), and integers that
//! must cross the JSON wire exactly (SPEC §data.browse).

use serde_json::{json, Value};

// --- JS value coercion ---------------------------------------------------------------------------

/// `Number(x)` for the loose-typed option values the browser methods accept: numbers pass
/// through, strings parse after a JS-style trim ("" → 0), booleans → 0/1, null → 0. None is the
/// NaN branch — the caller's fallback takes over, exactly where the Node build's
/// `Number.isFinite` check fell through. (A couple of JS exotics — hex strings, "Infinity" —
/// coerce differently; none of them is a page/limit argument any caller sends.)
pub fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Some(0.0)
            } else {
                t.parse::<f64>().ok()
            }
        }
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

/// `String(x)` for values the panel posted: absent → "undefined", null → "null", numbers render
/// the way JS prints them (integers plainly, floats in plain notation below 1e21). Objects and
/// arrays fall back to their compact JSON text — JS would say "[object Object]", but no code path
/// here stringifies an object for anything but an error message.
pub fn js_to_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => number_to_js_string(n),
        Some(other) => other.to_string(),
    }
}

/// A JSON number the way `String(number)` renders it in JS: integers plainly (i64 is always
/// below JS's 1e21 exponent threshold), floats plainly inside [1e-6, 1e21) and in exponential
/// notation outside it (JS spells positive exponents "1e+21"; Rust's `{:e}` says "1e21" — one
/// character apart, on values that only ever reach error text).
pub(super) fn number_to_js_string(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    match n.as_f64() {
        Some(f) => float_to_js_string(f),
        None => n.to_string(),
    }
}

pub(super) fn float_to_js_string(f: f64) -> String {
    if !f.is_finite() {
        // serde_json numbers are always finite; unreachable, but never panic over a value.
        return String::new();
    }
    let a = f.abs();
    if a != 0.0 && (a < 1e-6 || a >= 1e21) {
        format!("{f:e}")
    } else {
        format!("{f}")
    }
}

// --- exact-precision serialization (SPEC §data.browse) ------------------------------------------------

/// An i64 cell as an exact JSON string. The panel is JavaScript: JSON.parse turns any number
/// beyond +/-2^53 into the nearest double and silently rounds the low digits away
/// (9223372036854775807 reads back as 9223372036854776000), so 18-19 digit ids (snowflakes,
/// PG bigserials) must cross the wire as STRINGS. Small values stringify too, so one column
/// keeps one shape end to end; the grid right-aligns numeric strings (dbCellView) and the
/// adapters bind them back with a typed/implicit cast. The Node build's mysql2
/// `bigNumberStrings` + pgweb's int64 rule made the same call.
pub fn exact_int64(v: i64) -> Value {
    json!(v.to_string())
}

/// The u64 spelling of the same contract (MySQL BIGINT UNSIGNED decodes as u64 only).
pub fn exact_uint64(v: u64) -> Value {
    json!(v.to_string())
}

/// Every element of an INT8[] cell under the same rule — one array, one shape.
pub fn exact_int64_list(v: Vec<i64>) -> Value {
    json!(v.into_iter().map(|x| x.to_string()).collect::<Vec<_>>())
}

/// An f64 cell that JSON can actually carry: NaN and +/-Infinity have no JSON spelling and
/// would abort serialization at write time, so those cells read as NULL — the same fallback
/// the Node build chose for its JSON.stringify boundary.
pub fn finite_f64(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int64_precision_survives_the_json_boundary_as_a_string() {
        // SPEC §data.browse: the panel is JavaScript; a JSON number beyond +/-2^53 is parsed by
        // JSON.parse into the nearest double, silently rounding the low digits away
        // (9223372036854775807 becomes 9223372036854776000). serde_json keeps the digits,
        // but the value only stays exact if it crosses the wire as a STRING — which is what
        // the adapters serialize BIGINT/INT8 as, small values included ("42"), so every value
        // of the column takes the same shape. These helpers name that contract so a future
        // adapter arm cannot regress to json!(v).
        assert_eq!(exact_int64(42), json!("42"));
        assert_eq!(exact_int64(9007199254740993), json!("9007199254740993")); // 2^53+1, first int a double cannot hold
        assert_eq!(exact_int64(i64::MAX), json!("9223372036854775807"));
        assert_eq!(exact_int64(i64::MIN), json!("-9223372036854775808"));
        assert_eq!(exact_uint64(u64::MAX), json!("18446744073709551615"));
        assert_eq!(
            exact_int64_list(vec![i64::MAX, 7]),
            json!(["9223372036854775807", "7"])
        );
    }

    #[test]
    fn non_finite_floats_never_reach_the_json_wire() {
        // JSON has no NaN/Infinity spelling; json!(f64::NAN) would panic at serialization
        // time. The adapters null those cells instead (the Node build did the same), and
        // finite doubles pass through as numbers.
        assert_eq!(finite_f64(f64::NAN), Value::Null);
        assert_eq!(finite_f64(f64::INFINITY), Value::Null);
        assert_eq!(finite_f64(f64::NEG_INFINITY), Value::Null);
        assert_eq!(finite_f64(1.5), json!(1.5));
        assert_eq!(finite_f64(0.0), json!(0.0));
    }
}
