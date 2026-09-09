//! The touchstone plugin (docs/09 §9, docs/12 W4): ONE http tool, added from zero
//! WITHOUT touching src/host/ — the whole point is proving the plugin contract is
//! complete enough that a new tool is a new module plus ONE register line in
//! server.rs. If you are ever tempted to edit host/ to make this file work, stop:
//! the contract gap is the bug — fix the contract in its own commit and note it in
//! docs/09 §4, then come back.
//!
//! The plugin offers one action, `http.request` (method/url/headers/body/timeoutMs),
//! contributed to the shared ActionRegistry like any capability provider: registered
//! on start, withdrawn on stop, listed by GET /api/actions, runnable through
//! POST /api/runs and schedulable by Jobs — none of which know this module exists.
//! The panel page (admin views/http-tools.js, authored in the Node build and copied
//! byte-for-byte) renders its form ENTIRELY from the action's schema, so the tool and
//! its documentation never drift apart.
//!
//! Reuse over reimplementation: env-ref resolution and output masking are
//! services::process/actions' utilities — a second scanner here would be a second
//! place for the strictness rules to drift.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::host::descriptor::{PageDescriptor, PluginDescriptor};
use crate::host::factory::{PluginFactory, PluginInstance};
use crate::host::scope::PluginScope;
use crate::services::action::{Action, ActionError, ActionOutcome, CancelHandle};
use crate::services::actions::resolve_with_secrets;
use crate::services::process::mask_secrets;
use crate::services::RuntimeServices;

/// The capability id (stable, data in job definitions and run submissions).
pub const ACTION_ID: &str = "http.request";
const PLUGIN_ID: &str = "http-tools";

/// How much response body one run keeps — the same Tail policy and default ceiling
/// jobs' OutputPolicy uses (docs/11 §3.2), so a scheduled http.request and a manual
/// one cost the same in run history.
const OUTPUT_TAIL_CHARS: usize = 16_384;
/// A response larger than this is not downloaded at all (Content-Length is checked
/// first; without it the request timeout bounds the read). reqwest's stream feature
/// would allow exact chunked capping, but pulling futures-util into the shipping
/// feature set for a touchstone is not a trade this repo makes (AGENTS.md: every
/// dependency justifies its weight).
const READ_CAP_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
/// Methods a developer actually sends from a request builder. reqwest would accept
/// any token, but the form is generated from this enum — an open method field here
/// would just be a free-text box with a validation error at run time.
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

// --- the action ----------------------------------------------------------------------------------

/// One `http.request` capability. State is exactly what the config row said at start
/// (the allowlist) plus one shared reqwest client — no per-run state, so Arc-sharing
/// the action across concurrent runs is safe by construction.
struct HttpRequestAction {
    allowed_hosts: Vec<String>,
    client: reqwest::Client,
}

impl HttpRequestAction {
    /// Strict input parse: known keys only, right types, ${ENV_VAR} resolved via the
    /// shared strict scanner (a missing variable is a typed refusal naming it, never a
    /// silent empty string) and every resolved value >= 8 chars is collected for
    /// masking, so a token echoed back by a server can never land in run output.
    fn parse_input(&self, input: &Value) -> Result<ParsedRequest, ActionError> {
        let invalid = |m: String| ActionError::InvalidInput(m);
        let obj = input
            .as_object()
            .ok_or_else(|| invalid("input must be an object".into()))?;
        for key in obj.keys() {
            if !matches!(
                key.as_str(),
                "method" | "url" | "headers" | "body" | "timeoutMs"
            ) {
                return Err(invalid(format!(
                    "input: unknown field {key:?} (known: method, url, headers, body, timeoutMs)"
                )));
            }
        }
        let mut secrets: Vec<String> = Vec::new();
        let method = match obj.get("method") {
            None | Some(Value::Null) => "GET".to_string(),
            Some(Value::String(s)) => {
                let up = s.trim().to_ascii_uppercase();
                if !METHODS.contains(&up.as_str()) {
                    return Err(invalid(format!(
                        "input.method: must be one of {}",
                        METHODS.join(", ")
                    )));
                }
                up
            }
            Some(_) => return Err(invalid("input.method: must be a string".into())),
        };
        let url_raw = obj
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| invalid("input.url: required non-empty string".into()))?;
        let url = resolve_with_secrets(url_raw, "input.url", &mut secrets)?;
        let parsed = reqwest::Url::parse(&url).map_err(|e| invalid(format!("input.url: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(invalid(format!(
                "input.url: scheme must be http or https, not {}",
                parsed.scheme()
            )));
        }
        // The allowlist is the plugin's one config knob: empty = every host; otherwise
        // an exact (case-insensitive) host match. The refusal names the host and the
        // knob — a VISIBLE no, not a request that mysteriously never happens.
        let host = parsed
            .host_str()
            .ok_or_else(|| invalid("input.url: has no host".into()))?
            .to_ascii_lowercase();
        if !self.allowed_hosts.is_empty()
            && !self
                .allowed_hosts
                .iter()
                .any(|allowed| allowed.to_ascii_lowercase() == host)
        {
            return Err(invalid(format!(
                "host {host} is not in allowedHosts (plugins.http-tools.config)"
            )));
        }
        let mut headers = Vec::new();
        match obj.get("headers") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for (name, raw) in map {
                    let value = raw.as_str().ok_or_else(|| {
                        invalid(format!("input.headers.{name}: must be a string"))
                    })?;
                    let value = resolve_with_secrets(
                        value,
                        &format!("input.headers.{name}"),
                        &mut secrets,
                    )?;
                    headers.push((name.clone(), value));
                }
            }
            Some(_) => {
                return Err(invalid(
                    "input.headers: must be an object of name -> string".into(),
                ));
            }
        }
        let body = match obj.get("body") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(resolve_with_secrets(s, "input.body", &mut secrets)?),
            Some(_) => return Err(invalid("input.body: must be a string".into())),
        };
        let timeout_ms = match obj.get("timeoutMs") {
            None | Some(Value::Null) => DEFAULT_TIMEOUT_MS,
            Some(v) => match v.as_u64() {
                Some(ms) if (1..=MAX_TIMEOUT_MS).contains(&ms) => ms,
                _ => {
                    return Err(invalid(format!(
                        "input.timeoutMs: must be an integer between 1 and {MAX_TIMEOUT_MS}"
                    )))
                }
            },
        };
        Ok(ParsedRequest {
            method,
            url: parsed,
            headers,
            body,
            timeout_ms,
            secrets,
        })
    }
}

#[derive(Debug)]
struct ParsedRequest {
    method: String,
    url: reqwest::Url,
    headers: Vec<(String, String)>,
    body: Option<String>,
    timeout_ms: u64,
    secrets: Vec<String>,
}

/// Shape the run output: a status header line (the run view carries no action meta,
/// and the response viewer needs the status more than it needs whitespace), then the
/// masked, tail-capped body.
fn render_output(status: u16, content_type: Option<&str>, body: &str) -> String {
    let mut out = format!("HTTP {status}");
    if let Some(ct) = content_type {
        out.push_str("\nContent-Type: ");
        out.push_str(ct);
    }
    out.push_str("\n\n");
    out.push_str(body);
    out
}

/// Tail-cap like OutputPolicy::Tail: keep the LAST `cap` chars, count the full length
/// so the reader knows there was more.
fn tail_cap(text: &str, cap: usize) -> (String, bool, usize) {
    let chars = text.chars().count();
    if chars <= cap {
        return (text.to_string(), false, chars);
    }
    let tail: String = text.chars().skip(chars - cap).collect();
    (tail, true, chars)
}

#[async_trait]
impl Action for HttpRequestAction {
    fn type_name(&self) -> &'static str {
        ACTION_ID
    }

    fn title(&self) -> String {
        "HTTP request".into()
    }

    fn provider(&self) -> &'static str {
        PLUGIN_ID
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "method": {
                    "type": "string",
                    "enum": METHODS,
                    "default": "GET",
                    "description": "HTTP method"
                },
                "url": {
                    "type": "string",
                    "description": "Absolute http(s) URL. ${ENV_VAR} references resolve at run time; an unset variable is an error naming it.",
                },
                "headers": {
                    "type": "object",
                    "description": "Header name to value. Values resolve ${ENV_VAR} references (strict) and resolved secrets are masked from the output.",
                },
                "body": {
                    "type": "string",
                    "description": "Request body, sent as-is. ${ENV_VAR} references resolve at run time (strict).",
                },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_MS,
                    "default": DEFAULT_TIMEOUT_MS,
                    "description": "Whole-request timeout in milliseconds",
                },
            },
            "required": ["url"],
            "x-env-refs": "${ENV_VAR} references in url, header values and body resolve at run time; a missing variable fails the run naming it, and resolved values are masked from the output.",
        })
    }

    fn validate_input(&self, input: &Value) -> Result<(), String> {
        // Same parse the run path applies, minus the side effects: a config row naming
        // this action is checked by the SAME rules before it is ever saved (docs/11
        // §3.4). The allowlist check runs too, so a job pointing at a forbidden host
        // is refused at save time, not first-run time.
        self.parse_input(input)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn execute(
        &self,
        input: &Value,
        cancel: CancelHandle,
    ) -> Result<ActionOutcome, ActionError> {
        let started = std::time::Instant::now();
        let req = self.parse_input(input)?;
        let mut builder = self
            .client
            .request(
                reqwest::Method::from_bytes(req.method.as_bytes())
                    .map_err(|e| ActionError::InvalidInput(format!("input.method: {e}")))?,
                req.url.clone(),
            )
            .timeout(Duration::from_millis(req.timeout_ms));
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }
        // Cancellable by contract: the select makes cancel answer even while the
        // socket read is parked, which for a slow endpoint is the whole point.
        let sent = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(ActionOutcome {
                    ok: false,
                    ms: started.elapsed().as_millis() as u64,
                    canceled: true,
                    error: Some("canceled".into()),
                    ..ActionOutcome::ok()
                });
            }
            sent = builder.send() => sent.map_err(|e| ActionError::Failed(e.to_string()))?,
        };
        let status = sent.status();
        let content_type = sent
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        // Content-Length over the read cap: do not download it. Without a length
        // (chunked), the request timeout above bounds the read instead.
        let declared = sent.content_length();
        let (mut body_text, read_truncated) = match declared {
            Some(n) if n > READ_CAP_BYTES => (String::new(), true),
            _ => {
                let bytes = sent
                    .bytes()
                    .await
                    .map_err(|e| ActionError::Failed(e.to_string()))?;
                (String::from_utf8_lossy(&bytes).into_owned(), false)
            }
        };
        // Mask BEFORE capping: the cap must never cut a secret back into view, and the
        // mask marker surviving a tail cut is fine — the secret does not.
        mask_secrets(&mut body_text, &req.secrets);
        let (tail, output_truncated, chars) = tail_cap(&body_text, OUTPUT_TAIL_CHARS);
        let mut meta = Map::new();
        meta.insert("status".into(), json!(status.as_u16()));
        if let Some(ct) = &content_type {
            meta.insert("contentType".into(), json!(ct));
        }
        if read_truncated {
            meta.insert("readTruncated".into(), json!(declared.unwrap_or(0)));
        }
        if output_truncated {
            meta.insert("outputTruncated".into(), json!(true));
        }
        let output = render_output(status.as_u16(), content_type.as_deref(), &tail);
        let ok = status.is_success();
        Ok(ActionOutcome {
            ok,
            ms: started.elapsed().as_millis() as u64,
            error: if ok {
                None
            } else {
                Some(format!("HTTP {status}"))
            },
            chars,
            output,
            meta,
            ..ActionOutcome::ok()
        })
    }
}

// --- the plugin ----------------------------------------------------------------------------------

/// The factory: descriptor + config validation + instance construction. Everything the
/// host needs is said here, in data — the host never learns the id "http-tools".
pub struct HttpToolsPlugin {
    services: Arc<RuntimeServices>,
}

impl HttpToolsPlugin {
    pub fn new(services: Arc<RuntimeServices>) -> Self {
        HttpToolsPlugin { services }
    }
}

#[async_trait]
impl PluginFactory for HttpToolsPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PLUGIN_ID.into(),
            kind: "tools".into(),
            label: "HTTP Tools".into(),
            version: "0.1".into(),
            config_schema_version: 1,
            config_schema: json!({
                "type": "object",
                "properties": {
                    "allowedHosts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Empty = every host is allowed. Otherwise an exact list of host names http.request may reach; anything else is refused with a visible error.",
                    },
                },
                "additionalProperties": false,
            }),
            pages: vec![PageDescriptor {
                id: "http-tools".into(),
                plugin_id: PLUGIN_ID.into(),
                label: "HTTP".into(),
                order: 60,
                path: "#http-tools".into(),
                entry: "/admin/js/views/http-tools.js".into(),
                sidebar: true,
            }],
            routes: Vec::new(),
            // The allowlist is read at start; the instance owns nothing long-lived, so
            // restart-on-config-change is honest AND cheap — no children, no pools.
            restart_on_config_change: true,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        crate::plugins::http_tools::parse_allowed_hosts(config).map(|_| ())
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        let allowed_hosts = parse_allowed_hosts(config)?;
        Ok(Arc::new(HttpToolsInstance {
            services: self.services.clone(),
            allowed_hosts,
        }))
    }
}

/// One config row -> the allowlist. Empty/absent means "every host"; a present list
/// must be an array of non-empty strings — anything else is a 400, because a typo'd
/// allowlist that silently allowed everything would be a security hole wearing a
/// config badge.
fn parse_allowed_hosts(config: &Value) -> Result<Vec<String>, String> {
    match config.get("allowedHosts") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut hosts = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let host = item
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| format!("allowedHosts[{i}]: must be a non-empty string"))?;
                hosts.push(host.trim().to_string());
            }
            Ok(hosts)
        }
        Some(_) => Err("allowedHosts: must be an array of host names".into()),
    }
}

struct HttpToolsInstance {
    services: Arc<RuntimeServices>,
    allowed_hosts: Vec<String>,
}

#[async_trait]
impl PluginInstance for HttpToolsInstance {
    async fn start(self: Arc<Self>, _scope: &mut PluginScope) -> Result<(), String> {
        // Everything eager belongs here: the client (TLS init can fail) is built
        // before the action is visible, so a run can never resolve an action whose
        // transport is half-constructed.
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("http client init: {e}"))?;
        self.services
            .actions
            .register(Arc::new(HttpRequestAction {
                allowed_hosts: self.allowed_hosts.clone(),
                client,
            }))
            .map_err(|e| format!("action registration: {e}"))
    }

    async fn stop(&self) {
        // The contract's withdrawal (docs/09 §4): new submissions find no action; runs
        // in flight hold their Arc and finish on their own. Nothing else to release —
        // the client is dropped with the last run that holds the action.
        self.services.actions.unregister(ACTION_ID);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn action(allowed: &[&str]) -> HttpRequestAction {
        HttpRequestAction {
            allowed_hosts: allowed.iter().map(|s| s.to_string()).collect(),
            client: reqwest::Client::builder().build().expect("test client"),
        }
    }

    #[test]
    fn the_host_never_learns_this_plugins_id() {
        // docs/12 W4's contract-integrity check, rough on purpose: no file under
        // src/host/ may mention the plugin's id or its action id. If this fails, a
        // "small fix" in the host just special-cased a plugin — the exact drift the
        // touchstone exists to catch.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/host");
        let mut checked = 0;
        let entries = std::fs::read_dir(&dir).expect("src/host exists in this checkout");
        for entry in entries {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read host source");
            assert!(
                !text.contains("http-tools") && !text.contains("http.request"),
                "{} mentions the http-tools plugin — a plugin special-case in the host",
                path.display()
            );
            checked += 1;
        }
        assert!(
            checked >= 5,
            "the walk really read src/host ({checked} files)"
        );
    }

    #[test]
    fn allowed_hosts_parse_empty_means_everyone_and_typos_are_refused() {
        assert!(parse_allowed_hosts(&json!({})).unwrap().is_empty());
        assert!(parse_allowed_hosts(&json!({ "allowedHosts": null }))
            .unwrap()
            .is_empty());
        assert_eq!(
            parse_allowed_hosts(&json!({ "allowedHosts": ["a.example", "b.example"] })).unwrap(),
            vec!["a.example", "b.example"]
        );
        assert!(parse_allowed_hosts(&json!({ "allowedHosts": "a.example" })).is_err());
        assert!(parse_allowed_hosts(&json!({ "allowedHosts": [42] })).is_err());
        // An empty string would read as "allow the empty host" — refuse it at save time.
        assert!(parse_allowed_hosts(&json!({ "allowedHosts": [""] })).is_err());
    }

    #[test]
    fn the_tail_cap_keeps_the_end_and_counts_the_whole() {
        let (kept, truncated, chars) = tail_cap(&"x".repeat(20), 5);
        assert_eq!(kept, "xxxxx");
        assert!(truncated);
        assert_eq!(chars, 20);
        let (kept, truncated, chars) = tail_cap("abc", 5);
        assert_eq!(kept, "abc");
        assert!(!truncated);
        assert_eq!(chars, 3);
    }

    #[test]
    fn input_parsing_is_strict_about_every_field() {
        let a = action(&[]);
        assert!(a
            .parse_input(&json!({ "url": "http://example.test/x" }))
            .is_ok());
        // Unknown keys, wrong types, non-http schemes: typed refusals, never guesses.
        assert!(a
            .parse_input(&json!({ "url": "http://x.test", "nope": 1 }))
            .is_err());
        assert!(a
            .parse_input(&json!({ "url": "ftp://example.test" }))
            .is_err());
        assert!(a
            .parse_input(&json!({ "url": "http://x.test", "method": "FETCH" }))
            .is_err());
        assert!(a
            .parse_input(&json!({ "url": "http://x.test", "method": "get" }))
            .is_ok());
        assert!(a
            .parse_input(&json!({ "url": "http://x.test", "timeoutMs": 0 }))
            .is_err());
        assert!(a
            .parse_input(&json!({ "url": "http://x.test", "headers": [1] }))
            .is_err());
        assert!(a
            .parse_input(&json!({ "url": "http://x.test", "body": 7 }))
            .is_err());
        // The allowlist refusal names the host AND the knob.
        let a = action(&["api.example.test"]);
        let err = a
            .parse_input(&json!({ "url": "http://evil.example.test/x" }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("evil.example.test"), "{err}");
        assert!(err.contains("allowedHosts"), "{err}");
        assert!(a
            .parse_input(&json!({ "url": "http://API.EXAMPLE.TEST/x" }))
            .is_ok());
    }

    #[test]
    fn env_refs_resolve_strictly_and_register_for_masking() {
        // A unique name: set_var is process-global, so the test owns this one.
        std::env::set_var("HTTPTOOLS_UNIT_SECRET", "unit-secret-value-1");
        let a = action(&[]);
        let parsed = a
            .parse_input(&json!({
                "url": "http://echo.example.test/ping?token=${HTTPTOOLS_UNIT_SECRET}",
                "headers": { "authorization": "Bearer ${HTTPTOOLS_UNIT_SECRET}" }
            }))
            .expect("resolves");
        assert!(parsed.url.as_str().contains("unit-secret-value-1"));
        assert_eq!(parsed.secrets, vec!["unit-secret-value-1"]);
        // And a missing variable is a refusal naming it — the strict rule.
        let err = a
            .parse_input(&json!({ "url": "http://x.test/${HTTPTOOLS_UNIT_UNSET_9}" }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTPTOOLS_UNIT_UNSET_9"), "{err}");
        std::env::remove_var("HTTPTOOLS_UNIT_SECRET");
    }

    #[test]
    fn output_rendering_leads_with_the_status_line() {
        let out = render_output(200, Some("application/json"), "{\"ok\":true}");
        assert_eq!(
            out,
            "HTTP 200\nContent-Type: application/json\n\n{\"ok\":true}"
        );
        assert_eq!(render_output(404, None, "nope"), "HTTP 404\n\nnope");
    }
}
