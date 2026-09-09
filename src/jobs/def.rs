//! The v2 job definition model (docs/11 §3): pure data and pure functions, no IO.
//!
//! Scope of this stage (docs/11 §9 S1): the types, [JobsConfig::parse] as the one
//! authoritative validator of the `plugins.jobs.config` row, and the v1<->v2 projections.
//! Nothing here is wired into [super::JobSystem] yet - the scheduler still reads jobs.json;
//! config-side definitions are validated and stored, and that is all this build claims they
//! are (the plugin descriptor's config_schema says the same thing out loud).
//!
//! Two rules shape the parser:
//! - Every error carries the DOTTED PATH of the offending field
//!   ("definitions.nightly.retry.maxAttempts"), because the panel points its forms at
//!   exactly that (docs/11 §3.5). Unknown fields are refused naming the layer's legal
//!   field list, the way services::actions refuses exec input.
//! - Accepted-but-unexecuted config would be a lie (docs/11 §2 rule 5). Fields whose
//!   semantics only exist in a later stage - retry past its default, misfire "run-once",
//!   overlap "queue-one", output.capture "none" - are REFUSED with "not implemented until
//!   S5" instead of silently ignored. A 400 beats a config this build will not honour.
//!
//! Numbers are whole and non-negative: a string, a fraction or a negative is a type error,
//! never a silent default (docs/10 §4). The 60.0 spelling of 60 is tolerated the way
//! jobs::api's whole_number tolerates it for JS clients.

use serde_json::{json, Map, Value};

use super::runlog::valid_name;
use super::schedule::CronExpr;
use super::{JobDef, JOBS_ACTION};

/// Plugin-config level defaults (docs/11 §3.2). Applied when the row omits the field.
pub const DEFAULT_MAX_CONCURRENT_RUNS: usize = 2;
pub const DEFAULT_MAX_QUEUED_RUNS: usize = 32;
/// One day: the ceiling every definition's total retry deadline is checked against.
const ONE_DAY_MS: u64 = 86_400_000;
/// At most this many definitions in one config row. The cap keeps a hand-grown config
/// file from becoming a table the per-second tick has to scan on a slow machine.
const MAX_DEFINITIONS: usize = 512;

/// A config rejection that knows WHICH field it is about. The panel's editor points its
/// controls at the dotted path; a bare string cannot do that (docs/11 §3.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Dotted path of the offending field, e.g. "definitions.nightly.retry.maxAttempts".
    pub path: String,
    pub message: String,
}

impl ConfigError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The parsed `plugins.jobs.config` row (docs/11 §3.1-§3.2). No Clone: this is the
/// one parsed view of a revision, not a value to copy around (the trigger's compiled
/// cron is part of its identity).
#[derive(Clone)]
pub struct JobsConfig {
    pub max_concurrent_runs: usize,
    pub max_queued_runs: usize,
    pub retention: Retention,
    /// Definitions in configuration order (serde_json keeps insertion order); the key is
    /// the job id, which is also the run-log file name.
    pub definitions: Vec<JobDefinition>,
}

/// How long and how large the per-job run history may grow (docs/11 §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    pub days: u32,
    pub max_bytes_per_job: u64,
    pub max_history_bytes: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            days: 180,
            max_bytes_per_job: 2 * 1024 * 1024,
            max_history_bytes: 64 * 1024 * 1024,
        }
    }
}

/// One v2 job definition. The stable identity is `id`; `title` is display-only, so
/// renaming a job means deleting one id and creating another (docs/11 §3.1).
#[derive(Clone)]
pub struct JobDefinition {
    pub id: String,
    pub title: String,
    pub labels: Vec<String>,
    pub disabled: bool,
    pub trigger: Trigger,
    pub action: ActionRef,
    pub timeout_ms: u64,
    pub overlap: Overlap,
    pub misfire: Misfire,
    pub retry: RetryPolicy,
    pub output: OutputPolicy,
}

/// When a job fires (docs/11 §3.3). Exactly one kind per definition, chosen by `kind`.
#[derive(Clone)]
pub enum Trigger {
    Manual,
    Interval {
        every_ms: u64,
        first_run: FirstRun,
    },
    /// The expression is compiled ONCE, at parse time - never per tick (docs/11 §6.5).
    Cron {
        expression: String,
        compiled: CronExpr,
    },
}

/// The anchor rule of an interval's first firing (docs/11 §3.3, §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRun {
    AfterInterval,
    Immediate,
}

/// Which capability a job runs and with what input (docs/11 §3.4). The input is kept
/// VERBATIM: `${ENV_VAR}` refs stay refs on disk and resolve at run time, and the
/// capability itself owns the input's schema - this layer only checks it is an object.
#[derive(Clone)]
pub struct ActionRef {
    pub type_: String,
    pub input: Value,
    pub schema_version: u32,
}

/// What to do when a job's previous run is still going at the next occurrence
/// (docs/11 §6.2). Only Skip exists yet; the parser refuses QueueOne until it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlap {
    Skip,
    QueueOne,
}

/// What to do with occurrences missed while the gateway was down (docs/11 §6.3).
/// Only Skip exists yet; the parser refuses RunOnce until it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misfire {
    Skip,
    RunOnce,
}

/// Retry belongs to one occurrence, not to the shared coordinator (docs/11 §6.4). The
/// default is "one attempt, nothing retried" ON PURPOSE: automatically repeating a
/// side-effecting command that failed is a decision, never a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub delay_ms: u64,
    pub backoff: Backoff,
    pub retry_on: Vec<RetryOn>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 1,
            delay_ms: 0,
            backoff: Backoff::Fixed,
            retry_on: vec![RetryOn::Failure],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backoff {
    Fixed,
    Exponential,
}

/// Which terminal states may trigger another attempt (docs/11 §6.4). Canceled and
/// refusals never retry; that rule lives in the future runner, not in the config shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOn {
    Failure,
    Timeout,
}

/// How much of a run's output is kept (docs/11 §3.2). Only Tail exists yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPolicy {
    pub capture: OutputCapture,
    pub max_bytes: usize,
}

impl Default for OutputPolicy {
    fn default() -> Self {
        OutputPolicy {
            capture: OutputCapture::Tail,
            max_bytes: 16_384,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputCapture {
    Tail,
    None,
}

// --- parsing helpers ---

/// The field's dotted path: bare at the config root ("maxConcurrentRuns"), nested under
/// its parent ("definitions.nightly.retry.maxAttempts").
fn field_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn type_err(path: impl Into<String>, what: &str) -> ConfigError {
    ConfigError::new(path, format!("must be {what}"))
}

/// A whole, non-negative number out of JSON: u64 directly, or the 60.0 spelling of 60.
/// Strings, fractions, negatives and non-finite floats are all None. A float too large
/// for u64 saturates the cast and is then rejected by the caller's bounds.
fn whole_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n);
    }
    let f = v.as_f64()?;
    if f.is_finite() && f >= 0.0 && f.fract() == 0.0 {
        Some(f as u64)
    } else {
        None
    }
}

/// A number field with inclusive bounds; the error path points at the field itself.
fn bounded_u64(v: &Value, path: &str, lo: u64, hi: u64) -> Result<u64, ConfigError> {
    let n = whole_u64(v).ok_or_else(|| {
        type_err(
            path,
            "a whole number (strings, fractions and negatives are not accepted)",
        )
    })?;
    if n < lo || n > hi {
        return Err(ConfigError::new(
            path,
            format!("must be between {lo} and {hi}, got {n}"),
        ));
    }
    Ok(n)
}

/// An optional bounded number: absent or null means the documented default.
fn opt_bounded(
    obj: &Map<String, Value>,
    key: &str,
    parent: &str,
    default: u64,
    lo: u64,
    hi: u64,
) -> Result<u64, ConfigError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => bounded_u64(v, &field_path(parent, key), lo, hi),
    }
}

/// A required bounded number.
fn required_bounded(
    v: Option<&Value>,
    parent: &str,
    key: &str,
    lo: u64,
    hi: u64,
) -> Result<u64, ConfigError> {
    match v {
        None | Some(Value::Null) => Err(ConfigError::new(
            field_path(parent, key),
            "is required".to_string(),
        )),
        Some(v) => bounded_u64(v, &field_path(parent, key), lo, hi),
    }
}

/// `additionalProperties: false` semantics: every key must be known, and the error names
/// the layer's legal fields - the house style services::actions uses for exec input.
fn check_known(obj: &Map<String, Value>, parent: &str, known: &[&str]) -> Result<(), ConfigError> {
    for key in obj.keys() {
        if !known.contains(&key.as_str()) {
            return Err(ConfigError::new(
                field_path(parent, key),
                format!("unknown field {key:?} (known fields: {})", known.join(", ")),
            ));
        }
    }
    Ok(())
}

/// An optional string enum: None when absent/null, the value when legal, an error naming
/// the legal values otherwise.
fn enum_str<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    parent: &str,
    legal: &[&str],
) -> Result<Option<&'a str>, ConfigError> {
    let legal_list = legal
        .iter()
        .map(|s| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if legal.contains(&s.as_str()) => Ok(Some(s.as_str())),
        Some(Value::String(s)) => Err(ConfigError::new(
            field_path(parent, key),
            format!("must be one of: {legal_list}, got {s:?}"),
        )),
        Some(_) => Err(type_err(field_path(parent, key), "a string")),
    }
}

// --- the parser ---

impl JobsConfig {
    /// Parse and fully validate a `plugins.jobs.config` object (docs/11 §3). This is the
    /// server-side authority for every SAVE of the row: a body that fails here is a 400,
    /// because accepting config this build cannot execute would be the
    /// accepted-but-unexecuted lie (docs/11 §2 rule 5). The scheduler does not read the
    /// row yet (that wiring is a later stage), which the descriptor states honestly.
    pub fn parse(config: &Value) -> Result<JobsConfig, ConfigError> {
        parse_at(config, false).map(|parsed| parsed.config)
    }

    /// The boot-time compatibility path, and why it differs from [JobsConfig::parse]:
    /// a config row written BEFORE this validator existed passed through a checker whose
    /// only rule was "every definitions entry is an object", so an upgraded gateway can
    /// legitimately find placeholder entries (`{"x": {}}`) on disk. Failing the whole
    /// row at plugin start would trade a one-definition leftover for an all-jobs outage:
    /// the plugin goes Failed and the scheduler never runs (engine.rs validates before
    /// start). So at BOOT an entry that is an object but fails v2 parse is DROPPED and
    /// reported back for a warn, exactly the way JobStore::open drops a hand-mangled
    /// jobs.json entry. A PUT must stay strict: a human saving that same body NOW is
    /// told it is wrong, otherwise the leniency becomes a new way to save dead config.
    /// Non-object entries and config-level errors stay fatal on both paths, because the
    /// old validator never accepted those and they cannot be pre-upgrade leftovers.
    pub fn parse_boot(config: &Value) -> Result<BootParsed, ConfigError> {
        parse_at(config, true)
    }

    /// The S3 save path (docs/11 §3.4, §9): strict parse PLUS per-action input
    /// validation through the resolved capability, and one warning per definition
    /// whose action type is not currently registered. Registration is not required to
    /// SAVE - the provider may be disabled - but the PUT response carries the warning,
    /// and the listing reports `actionAvailable: false` for those rows.
    pub fn parse_with_actions(
        config: &Value,
        actions: &crate::services::action::ActionRegistry,
    ) -> Result<(JobsConfig, Vec<String>), ConfigError> {
        let parsed = parse_at(config, false)?;
        let mut warnings = Vec::new();
        for def in &parsed.config.definitions {
            match actions.get(&def.action.type_) {
                Some(action) => {
                    if let Err(err) = action.validate_input(&def.action.input) {
                        return Err(ConfigError::new(
                            format!("definitions.{}.action.input", def.id),
                            err,
                        ));
                    }
                }
                None => warnings.push(format!(
                    "action {} is not registered; runs will be refused until its plugin is enabled",
                    def.action.type_
                )),
            }
        }
        Ok((parsed.config, warnings))
    }

    /// The S3 boot path: the placeholder leniency of [JobsConfig::parse_boot], with input
    /// validation layered on the same way. A definition whose action input the resolved
    /// capability rejects is DROPPED with its reason (a leftover, not a new save), while
    /// an unregistered action type is only warned about: the provider may simply be
    /// disabled right now (docs/11 §3.4).
    pub fn parse_boot_with_actions(
        config: &Value,
        actions: &crate::services::action::ActionRegistry,
    ) -> Result<(BootParsed, Vec<String>), ConfigError> {
        let mut parsed = parse_at(config, true)?;
        let mut warnings = Vec::new();
        // Dropped-by-input reasons collect here first: `definitions` is mutably borrowed
        // by retain, so the report list cannot be touched inside the closure.
        let mut input_dropped: Vec<(String, String)> = Vec::new();
        parsed
            .config
            .definitions
            .retain(|def| match actions.get(&def.action.type_) {
                Some(action) => match action.validate_input(&def.action.input) {
                    Ok(()) => true,
                    Err(err) => {
                        input_dropped.push((def.id.clone(), format!("action.input: {err}")));
                        false
                    }
                },
                None => {
                    warnings.push(format!(
                    "action {} is not registered; runs will be refused until its plugin is enabled",
                    def.action.type_
                ));
                    true
                }
            });
        parsed.dropped.extend(input_dropped);
        Ok((parsed, warnings))
    }
}

/// What [JobsConfig::parse_boot] produced: the parsed row plus the definitions the
/// placeholder leniency dropped (id and rendered reason), which the caller turns into
/// warn log lines.
pub struct BootParsed {
    pub config: JobsConfig,
    pub dropped: Vec<(String, String)>,
}

/// One parser, two policies. `boot` selects the placeholder leniency described on
/// [JobsConfig::parse_boot]; the dropped list reports what the leniency removed.
fn parse_at(config: &Value, boot: bool) -> Result<BootParsed, ConfigError> {
    let obj = config
        .as_object()
        .ok_or_else(|| type_err("config", "an object"))?;
    check_known(
        obj,
        "",
        &[
            "schemaVersion",
            "maxConcurrentRuns",
            "maxQueuedRuns",
            "retention",
            "definitions",
        ],
    )?;
    // Absent means 2; any other value refuses the WHOLE row. A config written for
    // another schema is never parsed "down" into something it does not say
    // (docs/11 §3.2).
    if let Some(v) = obj.get("schemaVersion") {
        if v != &Value::Null {
            let n = whole_u64(v).ok_or_else(|| type_err("schemaVersion", "a whole number"))?;
            if n != 2 {
                return Err(ConfigError::new(
                    "schemaVersion",
                    format!("only 2 is accepted, got {n}"),
                ));
            }
        }
    }
    let max_concurrent_runs = opt_bounded(
        obj,
        "maxConcurrentRuns",
        "",
        DEFAULT_MAX_CONCURRENT_RUNS as u64,
        1,
        64,
    )? as usize;
    let max_queued_runs = opt_bounded(
        obj,
        "maxQueuedRuns",
        "",
        DEFAULT_MAX_QUEUED_RUNS as u64,
        0,
        1024,
    )? as usize;
    let retention = parse_retention(obj.get("retention"))?;
    let (definitions, dropped) = parse_definitions(obj.get("definitions"), boot)?;
    Ok(BootParsed {
        config: JobsConfig {
            max_concurrent_runs,
            max_queued_runs,
            retention,
            definitions,
        },
        dropped,
    })
}

fn parse_retention(v: Option<&Value>) -> Result<Retention, ConfigError> {
    let obj = match v {
        None | Some(Value::Null) => return Ok(Retention::default()),
        Some(v) => v
            .as_object()
            .ok_or_else(|| type_err("retention", "an object"))?,
    };
    check_known(
        obj,
        "retention",
        &["days", "maxBytesPerJob", "maxHistoryBytes"],
    )?;
    let default = Retention::default();
    Ok(Retention {
        days: opt_bounded(obj, "days", "retention", u64::from(default.days), 1, 3650)? as u32,
        max_bytes_per_job: opt_bounded(
            obj,
            "maxBytesPerJob",
            "retention",
            default.max_bytes_per_job,
            64 * 1024,
            64 * 1024 * 1024,
        )?,
        max_history_bytes: opt_bounded(
            obj,
            "maxHistoryBytes",
            "retention",
            default.max_history_bytes,
            1024 * 1024,
            1024 * 1024 * 1024,
        )?,
    })
}

/// One dropped definitions entry: the id and the rendered parse error.
pub type Dropped = (String, String);

/// Parse the definitions map. In `boot` mode an entry shaped like an old placeholder
/// (an object - the one shape the pre-v2 validator ever accepted) that fails v2 parse is
/// dropped and reported instead of refused; see [JobsConfig::parse_boot] for why boot and
/// PUT deliberately disagree. Returns the definitions plus the (id, reason) pairs dropped.
fn parse_definitions(
    v: Option<&Value>,
    boot: bool,
) -> Result<(Vec<JobDefinition>, Vec<Dropped>), ConfigError> {
    let map = match v {
        None | Some(Value::Null) => return Ok((Vec::new(), Vec::new())),
        Some(v) => v
            .as_object()
            .ok_or_else(|| type_err("definitions", "an object mapping job ids to definitions"))?,
    };
    if map.len() > MAX_DEFINITIONS {
        return Err(ConfigError::new(
            "definitions",
            format!(
                "holds {} entries; at most {MAX_DEFINITIONS} are accepted",
                map.len()
            ),
        ));
    }
    let mut definitions = Vec::with_capacity(map.len());
    let mut dropped = Vec::new();
    for (id, body) in map {
        // The leniency predicate is "body is an object": that is exactly the set of rows
        // the old validator could have persisted. Anything else was never writable
        // before the upgrade and stays a hard error even at boot.
        let placeholder = boot && body.is_object();
        if !valid_name(id) {
            if placeholder {
                dropped.push((
                    id.clone(),
                    "job id must be 1-64 chars of letters, digits, . _ -".to_string(),
                ));
                continue;
            }
            return Err(ConfigError::new(
                format!("definitions.{id}"),
                "job id must be 1-64 chars of letters, digits, . _ - (it is also the run-log file name)",
            ));
        }
        match parse_definition(id, body, &format!("definitions.{id}")) {
            Ok(def) => definitions.push(def),
            Err(err) => {
                if placeholder {
                    dropped.push((id.clone(), err.to_string()));
                    continue;
                }
                return Err(err);
            }
        }
    }
    Ok((definitions, dropped))
}

fn parse_definition(id: &str, body: &Value, parent: &str) -> Result<JobDefinition, ConfigError> {
    let obj = body
        .as_object()
        .ok_or_else(|| type_err(parent, "an object"))?;
    check_known(
        obj,
        parent,
        &[
            "title",
            "labels",
            "disabled",
            "trigger",
            "action",
            "timeoutMs",
            "overlap",
            "misfire",
            "retry",
            "output",
        ],
    )?;
    let title = match obj.get("title") {
        None | Some(Value::Null) => id.to_string(),
        Some(Value::String(s)) => {
            if s.chars().count() > 200 {
                return Err(ConfigError::new(
                    field_path(parent, "title"),
                    format!("must be at most 200 characters, got {}", s.chars().count()),
                ));
            }
            s.clone()
        }
        Some(_) => return Err(type_err(field_path(parent, "title"), "a string")),
    };
    let labels = parse_labels(obj.get("labels"), &field_path(parent, "labels"))?;
    let disabled = match obj.get("disabled") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(type_err(field_path(parent, "disabled"), "a boolean")),
    };
    let trigger = parse_trigger(obj.get("trigger"), &field_path(parent, "trigger"))?;
    let action = parse_action(obj.get("action"), &field_path(parent, "action"))?;
    let timeout_ms = opt_bounded(
        obj,
        "timeoutMs",
        parent,
        super::runner::DEFAULT_TIMEOUT_MS,
        1000,
        86_400_000,
    )?;
    let overlap = if enum_str(obj, "overlap", parent, &["skip", "queue-one"])? == Some("queue-one")
    {
        Overlap::QueueOne
    } else {
        Overlap::Skip
    };
    let misfire = if enum_str(obj, "misfire", parent, &["skip", "run-once"])? == Some("run-once") {
        Misfire::RunOnce
    } else {
        Misfire::Skip
    };
    let retry = parse_retry(obj.get("retry"), &field_path(parent, "retry"))?;
    let output = parse_output(obj.get("output"), &field_path(parent, "output"))?;

    // The total-deadline ceiling is checked at VALIDATION time (docs/11 §3.2): a job
    // that could retry its way past 24 h is refused here, not discovered at run time.
    // This runs BEFORE the not-implemented gates below, so an impossible deadline
    // reports as the deadline error it is, whatever stage the retry fields belong to.
    // Bounds have already capped every term, so the product cannot overflow.
    let total = u64::from(retry.max_attempts) * (timeout_ms + retry.delay_ms);
    if total > ONE_DAY_MS {
        return Err(ConfigError::new(
            field_path(parent, "retry"),
            format!(
                "total deadline maxAttempts * (timeoutMs + delayMs) = {total} ms exceeds the {ONE_DAY_MS} ms (24 h) ceiling"
            ),
        ));
    }
    // docs/11 §2 rule 5, applied to this stage's scope: the four semantics below are not
    // implemented yet, so their non-default spellings are refused rather than accepted
    // and ignored. Each message says so out loud (S5 lifts these branches).
    if overlap == Overlap::QueueOne {
        return Err(ConfigError::new(
            field_path(parent, "overlap"),
            "overlap \"queue-one\" is not implemented until S5; use \"skip\"",
        ));
    }
    if misfire == Misfire::RunOnce {
        return Err(ConfigError::new(
            field_path(parent, "misfire"),
            "misfire \"run-once\" is not implemented until S5; use \"skip\"",
        ));
    }
    if retry != RetryPolicy::default() {
        return Err(ConfigError::new(
            field_path(parent, "retry"),
            "retry is not implemented until S5; only the default policy (maxAttempts 1, delayMs 0, backoff \"fixed\", retryOn [\"failure\"]) is accepted",
        ));
    }
    if output.capture == OutputCapture::None {
        return Err(ConfigError::new(
            field_path(parent, "output.capture"),
            "output.capture \"none\" is not implemented until S5; use \"tail\"",
        ));
    }
    Ok(JobDefinition {
        id: id.to_string(),
        title,
        labels,
        disabled,
        trigger,
        action,
        timeout_ms,
        overlap,
        misfire,
        retry,
        output,
    })
}

fn parse_labels(v: Option<&Value>, parent: &str) -> Result<Vec<String>, ConfigError> {
    let items = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(type_err(parent, "an array of strings")),
    };
    if items.len() > 16 {
        return Err(ConfigError::new(
            parent,
            format!(
                "holds {} entries; at most 16 labels are accepted",
                items.len()
            ),
        ));
    }
    let mut labels = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let path = format!("{parent}[{i}]");
        let s = item.as_str().ok_or_else(|| type_err(path, "a string"))?;
        if s.chars().count() > 64 {
            return Err(ConfigError::new(
                format!("{parent}[{i}]"),
                format!("must be at most 64 characters, got {}", s.chars().count()),
            ));
        }
        labels.push(s.to_string());
    }
    Ok(labels)
}

fn parse_trigger(v: Option<&Value>, parent: &str) -> Result<Trigger, ConfigError> {
    let body = v.ok_or_else(|| ConfigError::new(parent, "is required".to_string()))?;
    let obj = body
        .as_object()
        .ok_or_else(|| type_err(parent, "an object"))?;
    let kind = obj.get("kind").and_then(Value::as_str).ok_or_else(|| {
        ConfigError::new(
            field_path(parent, "kind"),
            "is required and selects the trigger: \"manual\", \"interval\" or \"cron\"".to_string(),
        )
    })?;
    match kind {
        "manual" => {
            check_known(obj, parent, &["kind"])?;
            Ok(Trigger::Manual)
        }
        "interval" => {
            check_known(obj, parent, &["kind", "everyMs", "firstRun"])?;
            // 1 second to 365 days (docs/11 §3.3).
            let every_ms =
                required_bounded(obj.get("everyMs"), parent, "everyMs", 1000, 31_536_000_000)?;
            let first_run = if enum_str(obj, "firstRun", parent, &["after-interval", "immediate"])?
                == Some("immediate")
            {
                FirstRun::Immediate
            } else {
                FirstRun::AfterInterval
            };
            Ok(Trigger::Interval {
                every_ms,
                first_run,
            })
        }
        "cron" => {
            check_known(obj, parent, &["kind", "expression", "timezone"])?;
            let expression = match obj.get("expression") {
                None | Some(Value::Null) => {
                    return Err(ConfigError::new(
                        field_path(parent, "expression"),
                        "is required".to_string(),
                    ))
                }
                Some(Value::String(s)) if !s.trim().is_empty() => s,
                Some(Value::String(s)) => {
                    return Err(ConfigError::new(
                        field_path(parent, "expression"),
                        format!("must be a 5-field cron expression, got {s:?}"),
                    ))
                }
                Some(_) => return Err(type_err(field_path(parent, "expression"), "a string")),
            };
            // Compile once, here: the vixie DOM/DOW semantics stay exactly where they are
            // (docs/11 §3.3 says not to touch schedule.rs's meaning).
            let compiled = CronExpr::parse(expression)
                .map_err(|err| ConfigError::new(field_path(parent, "expression"), err))?;
            match obj.get("timezone") {
                None | Some(Value::Null) => {}
                Some(Value::String(tz)) if tz == "local" => {}
                Some(Value::String(tz)) => {
                    return Err(ConfigError::new(
                        field_path(parent, "timezone"),
                        format!(
                            "only \"local\" is supported, got {tz:?}; named zones would need chrono-tz, which docs/11 §10 rules out"
                        ),
                    ))
                }
                Some(_) => return Err(type_err(field_path(parent, "timezone"), "a string")),
            }
            Ok(Trigger::Cron {
                expression: expression.to_string(),
                compiled,
            })
        }
        other => Err(ConfigError::new(
            field_path(parent, "kind"),
            format!("must be \"manual\", \"interval\" or \"cron\", got {other:?}"),
        )),
    }
}

fn parse_action(v: Option<&Value>, parent: &str) -> Result<ActionRef, ConfigError> {
    let body = v.ok_or_else(|| ConfigError::new(parent, "is required".to_string()))?;
    let obj = body
        .as_object()
        .ok_or_else(|| type_err(parent, "an object"))?;
    check_known(obj, parent, &["type", "input", "schemaVersion"])?;
    let type_ = match obj.get("type") {
        None | Some(Value::Null) => {
            return Err(ConfigError::new(
                field_path(parent, "type"),
                "is required".to_string(),
            ))
        }
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(Value::String(s)) => {
            return Err(ConfigError::new(
                field_path(parent, "type"),
                format!("must be a non-empty capability id, got {s:?}"),
            ))
        }
        Some(_) => return Err(type_err(field_path(parent, "type"), "a string")),
    };
    // The capability owns its input schema (docs/11 §3.4): this layer only checks the
    // input IS an object and keeps it verbatim - `${ENV_VAR}` refs stay refs, resolved
    // at run time. Registration is deliberately NOT required: a provider may be disabled
    // right now, and a definition that cannot run yet must still be savable (its runs
    // will be refused until the plugin is back, which the run log reports).
    // TODO(S3): this module cannot reach the ActionRegistry, so the two remaining §3.4
    // promises are deferred, not dropped - once the plugin can hand a registry in,
    // validate input against the resolved capability's own schema here, and surface
    // "action <type> is not registered" as a PUT warning plus actionAvailable: false in
    // the listing. Both are pinned as S3 test items in docs/11 §9 S3.
    let input = match obj.get("input") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(v) if v.is_object() => v.clone(),
        Some(_) => return Err(type_err(field_path(parent, "input"), "an object")),
    };
    let schema_version = match obj.get("schemaVersion") {
        None | Some(Value::Null) => 1,
        Some(v) => {
            let n = whole_u64(v)
                .ok_or_else(|| type_err(field_path(parent, "schemaVersion"), "a whole number"))?;
            if n != 1 {
                return Err(ConfigError::new(
                    field_path(parent, "schemaVersion"),
                    format!("only 1 exists today, got {n}"),
                ));
            }
            1
        }
    };
    Ok(ActionRef {
        type_,
        input,
        schema_version,
    })
}

fn parse_retry(v: Option<&Value>, parent: &str) -> Result<RetryPolicy, ConfigError> {
    let obj = match v {
        None | Some(Value::Null) => return Ok(RetryPolicy::default()),
        Some(v) => v.as_object().ok_or_else(|| type_err(parent, "an object"))?,
    };
    check_known(
        obj,
        parent,
        &["maxAttempts", "delayMs", "backoff", "retryOn"],
    )?;
    let max_attempts = opt_bounded(obj, "maxAttempts", parent, 1, 1, 10)? as u32;
    let delay_ms = opt_bounded(obj, "delayMs", parent, 0, 0, 3_600_000)?;
    let backoff =
        if enum_str(obj, "backoff", parent, &["fixed", "exponential"])? == Some("exponential") {
            Backoff::Exponential
        } else {
            Backoff::Fixed
        };
    let retry_on = match obj.get("retryOn") {
        None | Some(Value::Null) => vec![RetryOn::Failure],
        Some(Value::Array(items)) => {
            let mut list = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                let path = format!("{parent}.retryOn[{i}]");
                let s = match item.as_str() {
                    Some(s) => s,
                    None => return Err(type_err(path, "a string")),
                };
                list.push(match s {
                    "failure" => RetryOn::Failure,
                    "timeout" => RetryOn::Timeout,
                    other => {
                        return Err(ConfigError::new(
                            path,
                            format!("must be \"failure\" or \"timeout\", got {other:?}"),
                        ))
                    }
                });
            }
            // An empty array is legal and means "never retry" (docs/11 §3.2).
            list
        }
        Some(_) => {
            return Err(type_err(
                field_path(parent, "retryOn"),
                "an array of strings",
            ))
        }
    };
    Ok(RetryPolicy {
        max_attempts,
        delay_ms,
        backoff,
        retry_on,
    })
}

fn parse_output(v: Option<&Value>, parent: &str) -> Result<OutputPolicy, ConfigError> {
    let obj = match v {
        None | Some(Value::Null) => return Ok(OutputPolicy::default()),
        Some(v) => v.as_object().ok_or_else(|| type_err(parent, "an object"))?,
    };
    check_known(obj, parent, &["capture", "maxBytes"])?;
    let capture = if enum_str(obj, "capture", parent, &["tail", "none"])? == Some("none") {
        OutputCapture::None
    } else {
        OutputCapture::Tail
    };
    let max_bytes = opt_bounded(obj, "maxBytes", parent, 16_384, 1024, 1_048_576)? as usize;
    Ok(OutputPolicy { capture, max_bytes })
}

// --- v1 <-> v2 projections ---

impl JobDefinition {
    /// The docs/11 §5.2 migration mapping, pure half: one v1 jobs.json row becomes one
    /// v2 definition. The action is process.legacy-command ON PURPOSE - the old
    /// tokenizer and the lenient `${VAR}` expansion are the semantics the saved command
    /// already has, and re-spelling it as process.exec would quietly change them.
    /// lastRunAt/lastOk do NOT come along: they are runtime state, headed for a separate
    /// file, never into the config row.
    pub fn from_v1(def: &JobDef) -> JobDefinition {
        let trigger = match (def.every_sec, def.cron.as_deref()) {
            (Some(secs), _) => Trigger::Interval {
                // Saturating: a hand-written everySec near u64::MAX must not wrap the
                // millisecond product; validating the migrated row rejects what does not
                // fit the interval bounds.
                every_ms: secs.saturating_mul(1000),
                first_run: FirstRun::AfterInterval,
            },
            (None, Some(expr)) => match CronExpr::parse(expr) {
                Ok(compiled) => Trigger::Cron {
                    expression: expr.to_string(),
                    compiled,
                },
                // Unreachable for a validated v1 row (JobDef::validate parses the cron);
                // Manual is the safe non-firing fallback if it ever happens anyway.
                Err(_) => Trigger::Manual,
            },
            (None, None) => Trigger::Manual,
        };
        let mut input = Map::new();
        input.insert("command".into(), json!(def.command));
        if let Some(cwd) = &def.cwd {
            input.insert("cwd".into(), json!(cwd));
        }
        JobDefinition {
            id: def.name.clone(),
            // v1 had no separate title; the name is the honest one (docs/11 §5.2).
            title: def.name.clone(),
            labels: Vec::new(),
            disabled: !def.enabled,
            trigger,
            action: ActionRef {
                type_: JOBS_ACTION.to_string(),
                input: Value::Object(input),
                schema_version: 1,
            },
            timeout_ms: def.timeout_ms,
            overlap: Overlap::Skip,
            misfire: Misfire::Skip,
            retry: RetryPolicy::default(),
            output: OutputPolicy::default(),
        }
    }

    /// The v1 API row for this definition (docs/11 §7.1): the pure, definition-level
    /// half - runtime facts (running, lastRunAt, nextDueAt) are layered on by whoever
    /// serves the row, not fabricated here. everySec is absent unless the interval is a
    /// whole number of seconds; a v1-inexpressible trigger (manual, sub-second interval)
    /// leaves both schedule fields absent rather than lying with a rounded value.
    pub fn to_v1_view(&self) -> Value {
        let mut m = Map::new();
        m.insert("name".into(), json!(self.id));
        m.insert("enabled".into(), json!(!self.disabled));
        m.insert("timeoutMs".into(), json!(self.timeout_ms));
        m.insert("command".into(), json!(self.display_command()));
        match &self.trigger {
            Trigger::Interval { every_ms, .. } => {
                if every_ms % 1000 == 0 {
                    m.insert("everySec".into(), json!(every_ms / 1000));
                }
            }
            Trigger::Cron { expression, .. } => {
                m.insert("cron".into(), json!(expression));
            }
            Trigger::Manual => {}
        }
        if self.action.type_ == JOBS_ACTION {
            if let Some(cwd) = self.action.input.get("cwd").and_then(Value::as_str) {
                m.insert("cwd".into(), json!(cwd));
            }
        }
        m.insert("editableInV1".into(), json!(self.editable_in_v1()));
        Value::Object(m)
    }

    /// The v1 row's "command" string (docs/11 §7.1): verbatim for the legacy
    /// compatibility action, a quote-wrapped-args display string for process.exec, and a
    /// bracketed placeholder for anything else. Only the first round-trips; the others
    /// are read-only display, which editable_in_v1 says out loud.
    fn display_command(&self) -> String {
        if self.action.type_ == JOBS_ACTION {
            if let Some(command) = self.action.input.get("command").and_then(Value::as_str) {
                return command.to_string();
            }
        }
        if let Some(program) = self.action.input.get("program").and_then(Value::as_str) {
            let mut line = String::from(program);
            if let Some(Value::Array(args)) = self.action.input.get("args") {
                for arg in args {
                    if let Some(text) = arg.as_str() {
                        line.push(' ');
                        line.push_str(&display_quote(text));
                    }
                }
            }
            return line;
        }
        format!("[{}]", self.action.type_)
    }

    /// Whether the v1 PUT shape can rewrite this definition without losing anything:
    /// exactly the legacy action (command/cwd only), a trigger v1 can spell, and no v2
    /// decoration. Everything else must go through PUT /api/plugins/jobs/config, and the
    /// v1 PUT answers 409 for it (docs/11 §7.1) - overwriting would silently drop fields.
    pub fn editable_in_v1(&self) -> bool {
        let legacy_input = self.action.type_ == JOBS_ACTION
            && self.action.schema_version == 1
            && self.action.input.as_object().is_some_and(|o| {
                o.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| !c.trim().is_empty())
                    && o.iter()
                        .all(|(k, v)| matches!(k.as_str(), "command" | "cwd") && v.is_string())
            });
        let v1_trigger = match &self.trigger {
            Trigger::Cron { .. } => true,
            Trigger::Interval { every_ms, .. } => every_ms % 1000 == 0,
            // v1 has no manual trigger: the row would gain a schedule it does not have.
            Trigger::Manual => false,
        };
        legacy_input
            && v1_trigger
            && self.title == self.id
            && self.labels.is_empty()
            && self.overlap == Overlap::Skip
            && self.misfire == Misfire::Skip
            && self.retry == RetryPolicy::default()
            && self.output == OutputPolicy::default()
    }
}

// --- S3: the v2 fields of the /api/jobs row (docs/11 §7.1) -----------------------------
//
// The listing must carry the definition's own fields (trigger, action, policies) so
// a v2 client never needs a second endpoint, and so the panel's future form (S6) can
// round-trip through the config editor losslessly. Serializers, not projections: they
// spell the config grammar back, the same names [JobsConfig::parse] reads.
impl JobDefinition {
    /// The definition as the config row spells it (docs/11 §3) - the exact grammar
    /// [JobsConfig::parse] reads back. The v1 edit path writes rows through this, so a
    /// folded edit can never smuggle in a shape the strict validator would refuse.
    pub fn to_config_json(&self) -> Value {
        json!({
            "title": self.title,
            "labels": self.labels,
            "disabled": self.disabled,
            "trigger": self.trigger_json(),
            "action": self.action_json(),
            "timeoutMs": self.timeout_ms,
            "overlap": self.overlap.as_str(),
            "misfire": self.misfire.as_str(),
            "retry": self.retry_json(),
            "output": self.output_json(),
        })
    }

    /// The definition-level v2 fields, as the /api/jobs row carries them.
    pub fn v2_view_fields(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("title".into(), json!(self.title));
        m.insert("labels".into(), json!(self.labels));
        m.insert("trigger".into(), self.trigger_json());
        m.insert("action".into(), self.action_json());
        m.insert("overlap".into(), json!(self.overlap.as_str()));
        m.insert("misfire".into(), json!(self.misfire.as_str()));
        m.insert("retry".into(), self.retry_json());
        m.insert("output".into(), self.output_json());
        m
    }

    /// The trigger in config spelling (docs/11 §3.3).
    pub fn trigger_json(&self) -> Value {
        match &self.trigger {
            Trigger::Manual => json!({ "kind": "manual" }),
            Trigger::Interval {
                every_ms,
                first_run,
            } => json!({
                "kind": "interval",
                "everyMs": every_ms,
                "firstRun": first_run.as_str(),
            }),
            Trigger::Cron { expression, .. } => json!({
                "kind": "cron",
                "expression": expression,
                "timezone": "local",
            }),
        }
    }

    /// The action ref in config spelling (docs/11 §3.4). The input rides verbatim -
    /// `${ENV_VAR}` refs stay refs on the wire, exactly as they were saved.
    pub fn action_json(&self) -> Value {
        json!({
            "type": self.action.type_,
            "input": self.action.input,
            "schemaVersion": self.action.schema_version,
        })
    }

    fn retry_json(&self) -> Value {
        json!({
            "maxAttempts": self.retry.max_attempts,
            "delayMs": self.retry.delay_ms,
            "backoff": self.retry.backoff.as_str(),
            "retryOn": self.retry.retry_on.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
        })
    }

    fn output_json(&self) -> Value {
        json!({
            "capture": self.output.capture.as_str(),
            "maxBytes": self.output.max_bytes,
        })
    }
}

impl Trigger {
    /// The config spelling of each first-run rule.
    pub fn first_run_str(&self) -> Option<&'static str> {
        match self {
            Trigger::Interval { first_run, .. } => Some(first_run.as_str()),
            _ => None,
        }
    }
}

impl FirstRun {
    /// The config spelling (docs/11 §3.3).
    pub fn as_str(&self) -> &'static str {
        match self {
            FirstRun::AfterInterval => "after-interval",
            FirstRun::Immediate => "immediate",
        }
    }
}

impl Overlap {
    /// The config spelling (docs/11 §3.2).
    pub fn as_str(&self) -> &'static str {
        match self {
            Overlap::Skip => "skip",
            Overlap::QueueOne => "queue-one",
        }
    }
}

impl Misfire {
    /// The config spelling (docs/11 §3.2).
    pub fn as_str(&self) -> &'static str {
        match self {
            Misfire::Skip => "skip",
            Misfire::RunOnce => "run-once",
        }
    }
}

impl Backoff {
    /// The config spelling (docs/11 §3.2).
    pub fn as_str(&self) -> &'static str {
        match self {
            Backoff::Fixed => "fixed",
            Backoff::Exponential => "exponential",
        }
    }
}

impl RetryOn {
    /// The config spelling (docs/11 §3.2).
    pub fn as_str(&self) -> &'static str {
        match self {
            RetryOn::Failure => "failure",
            RetryOn::Timeout => "timeout",
        }
    }
}

impl OutputCapture {
    /// The config spelling (docs/11 §3.2).
    pub fn as_str(&self) -> &'static str {
        match self {
            OutputCapture::Tail => "tail",
            OutputCapture::None => "none",
        }
    }
}

/// Whether a definition fires at "now" - the pure half of the S3 tick (docs/11 §6).
/// Manual never auto-fires (Run now is its only door); interval anchors on the last run
/// or the given boot anchor; cron matches this LOCAL minute and skips when the last run
/// already sits inside it - the same rules the v1 scheduler ran, now against the v2
/// trigger with the PRE-COMPILED expression.
pub fn definition_due(
    def: &JobDefinition,
    now: i64,
    now_local: &chrono::NaiveDateTime,
    anchor_ms: i64,
    last_run_ms: Option<i64>,
) -> bool {
    use chrono::{Datelike, Timelike};
    match &def.trigger {
        Trigger::Manual => false,
        Trigger::Interval { every_ms, .. } => {
            let base = last_run_ms.unwrap_or(anchor_ms);
            now.saturating_sub(base) >= *every_ms as i64
        }
        Trigger::Cron { compiled, .. } => {
            if !compiled.matches(&super::schedule::fields_of(now_local)) {
                return false;
            }
            match last_run_ms
                .and_then(super::local_minute_of_ms)
                .map(|t| (t.year(), t.ordinal(), t.hour(), t.minute()))
            {
                Some(last) => {
                    let cur = (
                        now_local.year(),
                        now_local.ordinal(),
                        now_local.hour(),
                        now_local.minute(),
                    );
                    last != cur
                }
                None => true,
            }
        }
    }
}

/// The ISO stamp of a definition's next firing (docs/11 §7.1's nextDueAt): intervals
/// anchor on the last run (or boot), cron scans forward from now with the compiled
/// expression, manual has none.
pub fn next_due_of(
    def: &JobDefinition,
    last_run_ms: Option<i64>,
    now: i64,
    anchor_ms: i64,
) -> Option<String> {
    use chrono::TimeZone;
    match &def.trigger {
        Trigger::Manual => None,
        Trigger::Interval { every_ms, .. } => {
            let base = last_run_ms.unwrap_or(anchor_ms);
            Some(super::state::iso_of_ms(base + *every_ms as i64))
        }
        Trigger::Cron { compiled, .. } => {
            let now_local = super::local_minute_of_ms(now)?;
            let next = compiled.next_after(&now_local)?;
            chrono::Local
                .from_local_datetime(&next)
                .single()
                .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
        }
    }
}

/// One argv entry as display text: bare when it needs no quoting, double-quoted when it
/// contains whitespace or a quote. Round-tripping it back to argv is not promised - that
/// is exactly why the row carrying a synthesized command is marked not editable in v1.
fn display_quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t', '"']) {
        // Wrap in double quotes and escape the inner ones; built char by char so the
        // escaping is visible rather than buried in a replace literal.
        let mut out = String::with_capacity(arg.len() + 2);
        out.push('"');
        for c in arg.chars() {
            if c == '"' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        arg.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_json_round_trips_to_an_editable_definition() {
        let def = JobDef {
            name: "nightly".into(),
            command: "echo ok".into(),
            every_sec: None,
            cron: Some("30 3 * * *".into()),
            enabled: true,
            timeout_ms: 600_000,
            cwd: None,
        };
        let v2 = JobDefinition::from_v1(&def);
        let json = v2.to_config_json();
        let parsed = JobsConfig::parse(&json!({ "definitions": { "nightly": json } }))
            .expect("round trip parses")
            .definitions
            .into_iter()
            .next()
            .unwrap();
        assert!(
            parsed.editable_in_v1(),
            "title={:?} labels={:?} overlap={:?}",
            parsed.title,
            parsed.labels,
            parsed.overlap.as_str()
        );
    }

    /// A minimal valid definition body, mutated per case.
    fn def_with(trigger: Value) -> Value {
        json!({
            "trigger": trigger,
            "action": { "type": "process.legacy-command", "input": { "command": "echo hi" } },
        })
    }

    fn def_body() -> Value {
        def_with(json!({ "kind": "cron", "expression": "30 3 * * *" }))
    }

    fn one_def(body: Value) -> Value {
        json!({ "definitions": { "nightly": body } })
    }

    fn refused(config: &Value) -> ConfigError {
        match JobsConfig::parse(config) {
            Err(err) => err,
            // Value is Debug; JobsConfig deliberately is not (its compiled cron is not).
            Ok(_) => panic!("the config must be refused: {config}"),
        }
    }

    fn refused_def(body: Value) -> ConfigError {
        refused(&one_def(body))
    }

    fn parse_one(body: Value) -> JobDefinition {
        let parsed = JobsConfig::parse(&one_def(body)).expect("a valid definition");
        parsed
            .definitions
            .into_iter()
            .next()
            .expect("one definition")
    }

    /// Set (or overwrite) one key on a definition body.
    fn set(body: &mut Value, key: &str, value: Value) {
        body.as_object_mut()
            .expect("a definition object")
            .insert(key.into(), value);
    }

    fn v1_def(every_sec: Option<u64>, cron: Option<&str>, enabled: bool) -> JobDef {
        JobDef {
            name: "nightly".into(),
            command: "psql -d app -c VACUUM".into(),
            every_sec,
            cron: cron.map(str::to_string),
            enabled,
            timeout_ms: 120_000,
            cwd: Some("C:/work".into()),
        }
    }

    #[test]
    fn the_spec_example_parses_with_documented_defaults() {
        // The docs/11 §3.1 example, verbatim as the plugins.jobs.config row. Its policy
        // fields are all at their defaults, which is exactly why it is valid at THIS
        // stage: the example is aspirational, but nothing in it lies about S1 scope.
        let config = json!({
            "schemaVersion": 2,
            "maxConcurrentRuns": 2,
            "maxQueuedRuns": 32,
            "retention": { "days": 180, "maxBytesPerJob": 2097152, "maxHistoryBytes": 67108864 },
            "definitions": {
                "nightly-vacuum": {
                    "title": "Nightly vacuum",
                    "labels": ["db", "maintenance"],
                    "disabled": false,
                    "trigger": { "kind": "cron", "expression": "30 3 * * *", "timezone": "local" },
                    "action": {
                        "type": "process.exec",
                        "input": {
                            "program": "psql",
                            "args": ["-d", "app", "-c", "VACUUM ANALYZE"],
                            "env": { "PGPASSWORD": "${APP_DB_PASSWORD}" }
                        }
                    },
                    "timeoutMs": 600000,
                    "overlap": "skip",
                    "misfire": "skip",
                    "retry": { "maxAttempts": 1, "delayMs": 0, "backoff": "fixed", "retryOn": ["failure"] },
                    "output": { "capture": "tail", "maxBytes": 16384 }
                }
            }
        });
        let parsed = JobsConfig::parse(&config).expect("the spec example is valid");
        assert_eq!(parsed.max_concurrent_runs, 2);
        assert_eq!(parsed.max_queued_runs, 32);
        assert_eq!(parsed.retention, Retention::default());
        let def = &parsed.definitions[0];
        assert_eq!(def.id, "nightly-vacuum");
        assert_eq!(def.title, "Nightly vacuum");
        assert_eq!(
            def.labels,
            vec!["db".to_string(), "maintenance".to_string()]
        );
        assert!(!def.disabled);
        assert_eq!(def.timeout_ms, 600_000);
        // The credential stays a REFERENCE, never a resolved value (docs/11 §2 rule 2).
        assert_eq!(
            def.action.input["env"]["PGPASSWORD"],
            json!("${APP_DB_PASSWORD}")
        );
        assert!(matches!(def.trigger, Trigger::Cron { .. }));
    }

    #[test]
    fn absent_fields_mean_the_documented_defaults() {
        let parsed = JobsConfig::parse(&json!({})).expect("an empty row is all defaults");
        assert_eq!(parsed.max_concurrent_runs, DEFAULT_MAX_CONCURRENT_RUNS);
        assert_eq!(parsed.max_queued_runs, DEFAULT_MAX_QUEUED_RUNS);
        assert_eq!(parsed.retention, Retention::default());
        assert!(parsed.definitions.is_empty());

        let def = parse_one(def_with(json!({ "kind": "manual" })));
        assert_eq!(def.title, "nightly", "title defaults to the id");
        assert!(def.labels.is_empty());
        assert!(!def.disabled);
        assert_eq!(def.timeout_ms, super::super::runner::DEFAULT_TIMEOUT_MS);
        assert_eq!(def.overlap, Overlap::Skip);
        assert_eq!(def.misfire, Misfire::Skip);
        assert_eq!(def.retry, RetryPolicy::default());
        assert_eq!(def.output, OutputPolicy::default());
        assert!(matches!(def.trigger, Trigger::Manual));
        assert_eq!(def.action.schema_version, 1);
    }

    #[test]
    fn schema_version_refuses_anything_but_two() {
        for config in [
            json!({ "schemaVersion": 1 }),
            json!({ "schemaVersion": 3 }),
            json!({ "schemaVersion": "2" }),
            // A fraction is a TYPE error first (whole numbers only), not a version one.
            json!({ "schemaVersion": 2.5 }),
        ] {
            let err = refused(&config);
            assert_eq!(err.path, "schemaVersion", "{err}");
        }
        let err = refused(&json!({ "schemaVersion": 3 }));
        assert!(err.message.contains("only 2"), "{err}");
        // 2.0 is the JS spelling of 2 and is accepted as 2.
        JobsConfig::parse(&json!({ "schemaVersion": 2.0 })).expect("2.0 is 2");
    }

    #[test]
    fn config_level_fields_are_bounds_and_type_checked_with_paths() {
        for (config, path) in [
            (json!({ "maxConcurrentRuns": 0 }), "maxConcurrentRuns"),
            (json!({ "maxConcurrentRuns": 65 }), "maxConcurrentRuns"),
            (json!({ "maxConcurrentRuns": "2" }), "maxConcurrentRuns"),
            (json!({ "maxQueuedRuns": 1025 }), "maxQueuedRuns"),
            (json!({ "maxQueuedRuns": -1 }), "maxQueuedRuns"),
            (json!({ "maxQueuedRuns": 0.5 }), "maxQueuedRuns"),
            (json!({ "retention": 42 }), "retention"),
            (json!({ "retention": { "days": 0 } }), "retention.days"),
            (json!({ "retention": { "days": 3651 } }), "retention.days"),
            (json!({ "retention": { "days": "180" } }), "retention.days"),
            (
                json!({ "retention": { "maxBytesPerJob": 65535 } }),
                "retention.maxBytesPerJob",
            ),
            (
                json!({ "retention": { "maxBytesPerJob": 67108865 } }),
                "retention.maxBytesPerJob",
            ),
            (
                json!({ "retention": { "maxHistoryBytes": 1048575 } }),
                "retention.maxHistoryBytes",
            ),
            (
                json!({ "retention": { "maxHistoryBytes": 1073741825 } }),
                "retention.maxHistoryBytes",
            ),
            (json!({ "definitions": [] }), "definitions"),
        ] {
            let err = refused(&config);
            assert_eq!(err.path, path, "{err}");
        }
        // The bounds message names the range, so a form can explain itself.
        let err = refused(&json!({ "maxConcurrentRuns": 65 }));
        assert!(err.message.contains("between 1 and 64"), "{err}");
        // Both ends of every range are inclusive.
        JobsConfig::parse(&json!({
            "maxConcurrentRuns": 64,
            "maxQueuedRuns": 0,
            "retention": { "days": 3650, "maxBytesPerJob": 67108864, "maxHistoryBytes": 1073741824 },
        }))
        .expect("the boundary values are legal");
    }

    #[test]
    fn definition_level_fields_are_bounds_and_type_checked_with_paths() {
        for (key, value, path) in [
            ("timeoutMs", json!(999), "definitions.nightly.timeoutMs"),
            (
                "timeoutMs",
                json!(86_400_001),
                "definitions.nightly.timeoutMs",
            ),
            (
                "timeoutMs",
                json!("600000"),
                "definitions.nightly.timeoutMs",
            ),
            ("title", json!(5), "definitions.nightly.title"),
            ("labels", json!("db"), "definitions.nightly.labels"),
            ("labels", json!([0]), "definitions.nightly.labels[0]"),
            ("disabled", json!("yes"), "definitions.nightly.disabled"),
            ("trigger", json!(7), "definitions.nightly.trigger"),
            ("action", json!("x"), "definitions.nightly.action"),
            ("output", json!(7), "definitions.nightly.output"),
        ] {
            let mut body = def_body();
            set(&mut body, key, value);
            let err = refused_def(body);
            assert_eq!(err.path, path, "{err}");
        }
        // Length caps carry their own paths.
        let mut body = def_body();
        set(&mut body, "title", json!("x".repeat(201)));
        assert_eq!(refused_def(body).path, "definitions.nightly.title");
        let mut body = def_body();
        set(
            &mut body,
            "labels",
            json!((0..17).map(|i| format!("l{i}")).collect::<Vec<_>>()),
        );
        assert_eq!(refused_def(body).path, "definitions.nightly.labels");
        let mut body = def_body();
        set(&mut body, "labels", json!(["y".repeat(65)]));
        assert_eq!(refused_def(body).path, "definitions.nightly.labels[0]");
        // Retry: the object's own bounds apply before anything else.
        for (retry, path) in [
            (
                json!({ "maxAttempts": 0 }),
                "definitions.nightly.retry.maxAttempts",
            ),
            (
                json!({ "maxAttempts": 11 }),
                "definitions.nightly.retry.maxAttempts",
            ),
            (
                json!({ "maxAttempts": 1.5 }),
                "definitions.nightly.retry.maxAttempts",
            ),
            (
                json!({ "delayMs": 3_600_001 }),
                "definitions.nightly.retry.delayMs",
            ),
            (
                json!({ "retryOn": "failure" }),
                "definitions.nightly.retry.retryOn",
            ),
            (
                json!({ "retryOn": ["gone"] }),
                "definitions.nightly.retry.retryOn[0]",
            ),
            (
                json!({ "backoff": "linear" }),
                "definitions.nightly.retry.backoff",
            ),
        ] {
            let mut body = def_body();
            set(&mut body, "retry", retry);
            let err = refused_def(body);
            assert_eq!(err.path, path, "{err}");
        }
        // Output bounds.
        for max_bytes in [json!(1023), json!(1_048_577), json!("16384")] {
            let mut body = def_body();
            set(&mut body, "output", json!({ "maxBytes": max_bytes }));
            assert_eq!(
                refused_def(body).path,
                "definitions.nightly.output.maxBytes"
            );
        }
    }

    #[test]
    fn numbers_reject_strings_fractions_and_negatives_but_tolerate_js_spelling() {
        for bad in [
            json!("300000"),
            json!(300000.5),
            json!(-300000),
            json!(true),
        ] {
            let err = refused(&json!({ "definitions": { "nightly": def_with(json!({
                "kind": "interval", "everyMs": bad,
            })) } }));
            assert_eq!(err.path, "definitions.nightly.trigger.everyMs", "{err}");
            assert!(err.message.contains("whole number"), "{err}");
        }
        // 300000.0 is how a JS client spells 300000; it parses as the integer.
        let parsed = JobsConfig::parse(&json!({ "definitions": { "nightly": def_with(
            json!({ "kind": "interval", "everyMs": 300000.0 }),
        ) } }))
        .expect("300000.0 is a whole number");
        let Trigger::Interval { every_ms, .. } = &parsed.definitions[0].trigger else {
            panic!("expected an interval trigger");
        };
        assert_eq!(*every_ms, 300_000);
    }

    #[test]
    fn unknown_fields_are_rejected_listing_the_layers_legal_fields() {
        // Config root.
        let err = refused(&json!({ "late": true }));
        assert_eq!(err.path, "late");
        assert!(err.message.contains("schemaVersion"), "{err}");
        assert!(err.message.contains("definitions"), "{err}");

        // Definition layer: a v1-shaped body is exactly this error.
        let mut body = def_body();
        set(&mut body, "command", json!("x"));
        let err = refused_def(body);
        assert_eq!(err.path, "definitions.nightly.command");
        for legal in [
            "title",
            "labels",
            "disabled",
            "trigger",
            "action",
            "timeoutMs",
            "overlap",
            "misfire",
            "retry",
            "output",
        ] {
            assert!(err.message.contains(legal), "{err}");
        }

        // Trigger layers, per kind: mixing two kinds' fields is how "two triggers" is
        // spelled in a single object, and it lands on the stray field's path.
        for (trigger, path, legal) in [
            (
                json!({ "kind": "manual", "everyMs": 5000 }),
                "definitions.nightly.trigger.everyMs",
                "kind",
            ),
            (
                json!({ "kind": "interval", "everyMs": 60000, "expression": "30 3 * * *" }),
                "definitions.nightly.trigger.expression",
                "everyMs",
            ),
            (
                json!({ "kind": "cron", "expression": "30 3 * * *", "everyMs": 5000 }),
                "definitions.nightly.trigger.everyMs",
                "expression",
            ),
        ] {
            let err = refused_def(def_with(trigger));
            assert_eq!(err.path, path, "{err}");
            assert!(err.message.contains(legal), "{err}");
        }

        // Action, retry, output and retention layers.
        let mut body = def_body();
        set(
            &mut body,
            "action",
            json!({ "type": "process.exec", "program": "cargo" }),
        );
        let err = refused_def(body);
        assert_eq!(err.path, "definitions.nightly.action.program");
        assert!(err.message.contains("input"), "{err}");

        let mut body = def_body();
        set(&mut body, "retry", json!({ "times": 3 }));
        let err = refused_def(body);
        assert_eq!(err.path, "definitions.nightly.retry.times");
        assert!(err.message.contains("maxAttempts"), "{err}");

        let mut body = def_body();
        set(&mut body, "output", json!({ "lines": 10 }));
        let err = refused_def(body);
        assert_eq!(err.path, "definitions.nightly.output.lines");
        assert!(err.message.contains("capture"), "{err}");

        let err = refused(&json!({ "retention": { "weeks": 2 } }));
        assert_eq!(err.path, "retention.weeks");
        assert!(err.message.contains("days"), "{err}");
    }

    #[test]
    fn triggers_parse_or_are_refused_with_a_reason() {
        // Happy paths: manual, interval (both anchors), cron.
        assert!(matches!(
            parse_one(def_with(json!({ "kind": "manual" }))).trigger,
            Trigger::Manual
        ));
        let def = parse_one(def_with(json!({ "kind": "interval", "everyMs": 60000 })));
        let Trigger::Interval {
            every_ms,
            first_run,
        } = &def.trigger
        else {
            panic!("expected an interval trigger");
        };
        assert_eq!(*every_ms, 60_000);
        assert_eq!(*first_run, FirstRun::AfterInterval, "the default anchor");
        let def = parse_one(def_with(
            json!({ "kind": "interval", "everyMs": 60000, "firstRun": "immediate" }),
        ));
        let Trigger::Interval { first_run, .. } = &def.trigger else {
            panic!("expected an interval trigger");
        };
        assert_eq!(*first_run, FirstRun::Immediate);
        assert!(matches!(
            parse_one(def_with(
                json!({ "kind": "cron", "expression": "30 3 * * *" })
            ))
            .trigger,
            Trigger::Cron { .. }
        ));

        // A missing trigger is the definition's error, not a silent manual default.
        let err = refused(&json!({ "definitions": { "nightly": {
            "action": { "type": "process.legacy-command", "input": { "command": "x" } },
        } } }));
        assert_eq!(err.path, "definitions.nightly.trigger");
        assert!(err.message.contains("required"), "{err}");

        for (trigger, path, fragment) in [
            // kind missing, or not one of the three.
            (
                json!({ "everyMs": 60000 }),
                "definitions.nightly.trigger.kind",
                "manual",
            ),
            (
                json!({ "kind": "webhook" }),
                "definitions.nightly.trigger.kind",
                "cron",
            ),
            // interval: everyMs required and bounded; firstRun is an enum.
            (
                json!({ "kind": "interval" }),
                "definitions.nightly.trigger.everyMs",
                "required",
            ),
            (
                json!({ "kind": "interval", "everyMs": 999 }),
                "definitions.nightly.trigger.everyMs",
                "between 1000",
            ),
            (
                json!({ "kind": "interval", "everyMs": 31_536_000_001u64 }),
                "definitions.nightly.trigger.everyMs",
                "between 1000",
            ),
            (
                json!({ "kind": "interval", "everyMs": 60000, "firstRun": "now" }),
                "definitions.nightly.trigger.firstRun",
                "after-interval",
            ),
            // cron: field count and values come from CronExpr; timezone is local-only.
            (
                json!({ "kind": "cron", "expression": "30 3 * *" }),
                "definitions.nightly.trigger.expression",
                "5 fields",
            ),
            (
                json!({ "kind": "cron", "expression": "" }),
                "definitions.nightly.trigger.expression",
                "cron",
            ),
            (
                json!({ "kind": "cron", "expression": "99 * * * *" }),
                "definitions.nightly.trigger.expression",
                "0..=59",
            ),
            (
                json!({ "kind": "cron", "expression": "30 3 * * *", "timezone": "UTC" }),
                "definitions.nightly.trigger.timezone",
                "local",
            ),
        ] {
            let err = refused_def(def_with(trigger));
            assert_eq!(err.path, path, "{err}");
            assert!(err.message.contains(fragment), "{err}");
        }
    }

    #[test]
    fn a_total_deadline_over_24_hours_is_refused() {
        // 2 attempts * (43,200,001 + 0) ms is one millisecond past the 24 h ceiling; the
        // deadline error wins over the not-implemented gate because an impossible
        // deadline is invalid in every stage.
        let mut body = def_body();
        set(&mut body, "timeoutMs", json!(43_200_001));
        set(&mut body, "retry", json!({ "maxAttempts": 2 }));
        let err = refused_def(body);
        assert_eq!(err.path, "definitions.nightly.retry");
        assert!(err.message.contains("24 h"), "{err}");
        assert!(err.message.contains("86400002"), "{err}");

        // delayMs counts toward the same ceiling: 10 * (5,040,001 + 3,600,000) ms.
        let mut body = def_body();
        set(&mut body, "timeoutMs", json!(5_040_001));
        set(
            &mut body,
            "retry",
            json!({ "maxAttempts": 10, "delayMs": 3_600_000 }),
        );
        let err = refused_def(body);
        assert_eq!(err.path, "definitions.nightly.retry");
        assert!(err.message.contains("24 h"), "{err}");
    }

    #[test]
    fn unimplemented_semantics_are_refused_not_swallowed() {
        // docs/11 §2 rule 5, at S1 scope: a non-default spelling of a semantic this build
        // does not execute is refused naming the stage that will implement it. Accepting
        // it would be promising behaviour that does not exist.
        for (key, value, path) in [
            ("overlap", json!("queue-one"), "definitions.nightly.overlap"),
            ("misfire", json!("run-once"), "definitions.nightly.misfire"),
            (
                "retry",
                json!({ "maxAttempts": 2 }),
                "definitions.nightly.retry",
            ),
            (
                "retry",
                json!({ "delayMs": 1000 }),
                "definitions.nightly.retry",
            ),
            (
                "retry",
                json!({ "backoff": "exponential" }),
                "definitions.nightly.retry",
            ),
            (
                "retry",
                json!({ "retryOn": [] }),
                "definitions.nightly.retry",
            ),
            (
                "retry",
                json!({ "retryOn": ["timeout"] }),
                "definitions.nightly.retry",
            ),
            (
                "output",
                json!({ "capture": "none" }),
                "definitions.nightly.output.capture",
            ),
        ] {
            let mut body = def_body();
            set(&mut body, key, value);
            let err = refused_def(body);
            assert_eq!(err.path, path, "{err}");
            assert!(err.message.contains("not implemented until S5"), "{err}");
        }
        // The default spellings all parse - writing a default out explicitly is not a
        // non-default.
        let mut body = def_body();
        set(&mut body, "overlap", json!("skip"));
        set(&mut body, "misfire", json!("skip"));
        set(
            &mut body,
            "retry",
            json!({ "maxAttempts": 1, "delayMs": 0, "backoff": "fixed", "retryOn": ["failure"] }),
        );
        set(
            &mut body,
            "output",
            json!({ "capture": "tail", "maxBytes": 16384 }),
        );
        let def = parse_one(body);
        assert_eq!(def.overlap, Overlap::Skip);
        assert_eq!(def.misfire, Misfire::Skip);
        assert_eq!(def.retry, RetryPolicy::default());
        // output.maxBytes is not on the rule-5 list: a legal non-default parses fine.
        let mut body = def_body();
        set(&mut body, "output", json!({ "maxBytes": 1_048_576 }));
        assert_eq!(parse_one(body).output.max_bytes, 1_048_576);
    }

    #[test]
    fn definitions_have_a_count_cap_and_id_rules() {
        let mut definitions = Map::new();
        for i in 0..512 {
            definitions.insert(format!("job-{i}"), def_body());
        }
        JobsConfig::parse(&json!({ "definitions": Value::Object(definitions.clone()) }))
            .expect("512 is the cap, not 511");
        definitions.insert("one-too-many".into(), def_body());
        let err = refused(&json!({ "definitions": Value::Object(definitions) }));
        assert_eq!(err.path, "definitions");
        assert!(err.message.contains("512"), "{err}");

        // The id is the run-log file name too, so the same character rules apply.
        let mut too_long = "x".repeat(65);
        too_long.push('!');
        for bad in ["", "not ok", ".", "..", "a/b", "héllo", &too_long] {
            let mut definitions = Map::new();
            definitions.insert(bad.to_string(), def_body());
            let err = refused(&json!({ "definitions": Value::Object(definitions) }));
            assert_eq!(err.path, format!("definitions.{bad}"), "{err}");
            assert!(err.message.contains("1-64 chars"), "{err}");
        }
    }

    #[test]
    fn v1_definitions_round_trip_through_v2_losslessly() {
        // Interval, enabled, with a cwd.
        let view = JobDefinition::from_v1(&v1_def(Some(3600), None, true)).to_v1_view();
        assert_eq!(
            view,
            json!({
                "name": "nightly",
                "command": "psql -d app -c VACUUM",
                "everySec": 3600,
                "enabled": true,
                "timeoutMs": 120000,
                "cwd": "C:/work",
                "editableInV1": true,
            })
        );
        // Cron, disabled.
        let view = JobDefinition::from_v1(&v1_def(None, Some("30 3 * * *"), false)).to_v1_view();
        assert_eq!(
            view,
            json!({
                "name": "nightly",
                "command": "psql -d app -c VACUUM",
                "cron": "30 3 * * *",
                "enabled": false,
                "timeoutMs": 120000,
                "cwd": "C:/work",
                "editableInV1": true,
            })
        );
        // An absent cwd stays absent, not null (the panel convention).
        let mut def = v1_def(Some(60), None, true);
        def.cwd = None;
        let view = JobDefinition::from_v1(&def).to_v1_view();
        assert!(view.get("cwd").is_none(), "absent, not null: {view}");
    }

    #[test]
    fn v2_only_definitions_project_read_only_v1_rows() {
        // process.exec: a synthesized display command, and never v1-editable - the v1
        // PUT would overwrite the definition with the display string (docs/11 §7.1).
        let def = parse_one(json!({
            "trigger": { "kind": "manual" },
            "action": {
                "type": "process.exec",
                "input": { "program": "cargo", "args": ["check", "--all"] },
            },
        }));
        let view = def.to_v1_view();
        assert_eq!(view["command"], json!("cargo check --all"));
        assert_eq!(view["editableInV1"], json!(false));
        assert!(
            view.get("everySec").is_none() && view.get("cron").is_none(),
            "manual has no v1 schedule: {view}"
        );

        // Args that need quoting are quoted - display only.
        let def = parse_one(json!({
            "trigger": { "kind": "cron", "expression": "30 3 * * *" },
            "action": {
                "type": "process.exec",
                "input": { "program": "echo", "args": ["a b", "c\"d"] },
            },
        }));
        assert_eq!(
            def.to_v1_view()["command"],
            json!("echo \"a b\" \"c\\\"d\"")
        );

        // A capability with no command shape still projects a row: a bracketed
        // placeholder, read-only.
        let def = parse_one(json!({
            "trigger": { "kind": "interval", "everyMs": 1500 },
            "action": { "type": "http.request", "input": {} },
        }));
        let view = def.to_v1_view();
        assert_eq!(view["command"], json!("[http.request]"));
        assert_eq!(view["editableInV1"], json!(false));
        assert!(
            view.get("everySec").is_none(),
            "1500 ms is not a whole second: {view}"
        );

        // v2 decoration (title, labels) also makes the row not v1-editable: a v1
        // overwrite would drop it.
        let mut body = def_body();
        set(&mut body, "title", json!("Nightly vacuum"));
        assert!(!parse_one(body).editable_in_v1());
        let mut body = def_body();
        set(&mut body, "labels", json!(["db"]));
        assert!(!parse_one(body).editable_in_v1());
    }

    #[test]
    fn boot_parse_drops_old_placeholders_but_a_save_stays_strict() {
        // The exact leftover an upgraded gateway can find on disk: the old validator
        // accepted any object under definitions, so {} placeholders passed its PUT.
        let leftover = json!({ "definitions": { "x": {} } });
        let err = refused(&leftover);
        assert_eq!(err.path, "definitions.x.trigger", "{err}");
        let boot = JobsConfig::parse_boot(&leftover).expect("boot tolerates it");
        assert!(
            boot.config.definitions.is_empty(),
            "the placeholder does not run"
        );
        assert_eq!(boot.dropped.len(), 1);
        assert_eq!(
            boot.dropped[0].0, "x",
            "the drop report feeds the boot warn"
        );
        assert!(
            boot.dropped[0].1.contains("trigger"),
            "{}",
            boot.dropped[0].1
        );

        // A valid neighbour survives; only the stale entry goes.
        let mixed = json!({ "definitions": { "x": {}, "good": def_body() } });
        let boot = JobsConfig::parse_boot(&mixed).expect("boot tolerates it");
        assert_eq!(boot.config.definitions.len(), 1);
        assert_eq!(boot.config.definitions[0].id, "good");
        assert_eq!(boot.dropped.len(), 1);

        // A bad ID on an object body is the same kind of leftover.
        let mut definitions = Map::new();
        definitions.insert("not ok".into(), json!({}));
        let boot =
            JobsConfig::parse_boot(&json!({ "definitions": Value::Object(definitions.clone()) }))
                .expect("boot tolerates it");
        assert!(boot.config.definitions.is_empty());
        assert_eq!(boot.dropped[0].0, "not ok");

        // What the old validator NEVER accepted stays fatal at boot too: a non-object
        // entry cannot be a pre-upgrade leftover, so it is a real corruption.
        let err = refused(&json!({ "definitions": { "x": "not an object" } }));
        assert_eq!(err.path, "definitions.x", "{err}");
        let err = match JobsConfig::parse_boot(&json!({ "definitions": { "x": 7 } })) {
            Err(err) => err,
            Ok(_) => panic!("boot must refuse a non-object entry"),
        };
        assert_eq!(err.path, "definitions.x", "{err}");

        // Config-level errors are not placeholders either; boot stays strict there.
        let err = match JobsConfig::parse_boot(&json!({ "late": true })) {
            Err(err) => err,
            Ok(_) => panic!("boot must refuse config-level garbage"),
        };
        assert_eq!(err.path, "late", "{err}");
    }
    #[test]
    fn action_input_is_kept_verbatim_with_env_refs() {
        // An unregistered capability type is savable on purpose (docs/11 §3.4): its
        // provider may be disabled right now, and refusing the save would make the job
        // impossible to edit until the plugin came back.
        let def = parse_one(json!({
            "trigger": { "kind": "manual" },
            "action": {
                "type": "not.registered",
                "input": {
                    "program": "psql",
                    "env": { "PGPASSWORD": "${APP_DB_PASSWORD}" },
                },
            },
        }));
        assert_eq!(def.action.type_, "not.registered");
        assert_eq!(
            def.action.input["env"]["PGPASSWORD"],
            json!("${APP_DB_PASSWORD}")
        );
        // But the input must at least be an object, at its own path.
        let mut body = def_body();
        set(
            &mut body,
            "action",
            json!({ "type": "process.exec", "input": "cargo" }),
        );
        assert_eq!(refused_def(body).path, "definitions.nightly.action.input");
    }
}
