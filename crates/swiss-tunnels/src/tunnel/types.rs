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

//! Tunnel-domain types — port of `tunnels/types.ts`.
//!
//! Every JSON shape here is byte-compatible with the Node build: camelCase keys, absent-not-null
//! optionals, and the exact field order the Node object literals produced (the store writes these
//! objects to tunnels.json, so order is part of the file format). Serialization is hand-written
//! against `serde_json::Map` rather than derived for that reason.

use serde_json::{json, Map, Value};

use swiss_core::util;

// --- JS value coercion --------------------------------------------------------------------------

/// `Number(v)` for a JSON value: numbers pass through, numeric strings parse, "" is 0, booleans
/// are 1/0, null is 0, anything else is NaN. The Node store and the api layer both coerce through
/// JS `Number()` before validating, and the error strings render the coerced value — these two
/// helpers reproduce that pipeline.
pub(crate) fn value_number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return 0.0;
            }
            t.parse::<f64>().unwrap_or(f64::NAN)
        }
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}

/// Render an f64 the way JS template interpolation would, for error strings that embed a raw
/// input value ("invalid local port: …"). NaN prints as "NaN"; integral values print without a
/// trailing ".0" (JS prints 22, not 22.0).
pub(crate) fn fmt_number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    format!("{v}")
}

/// `typeof v === "string" ? v : undefined` — absent when the value is not a string.
pub(crate) fn opt_str(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// Is this f64 a whole number in the TCP port range? (`Number.isInteger(n) && n >= 1 && n <= 65535`)
pub(crate) fn is_port_f(n: f64) -> bool {
    n.fract() == 0.0 && (1.0..=65535.0).contains(&n)
}

// --- ids -----------------------------------------------------------------------------------------

/// `randomUUID()` — a v4 UUID, hand-rolled (16 random bytes, RFC 4122 version/variant bits).
pub fn new_id() -> String {
    // TryRngCore per rand 0.9; an OS CSPRNG error is a broken machine, not a case to handle.
    use rand::TryRngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng
        .try_fill_bytes(&mut b)
        .expect("the OS CSPRNG answered an error");
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10
    let h = util::to_hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

// --- definitions ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthType {
    #[default]
    Key,
    Password,
}

impl AuthType {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthType::Key => "key",
            AuthType::Password => "password",
        }
    }
}

/// One SSH server, shared by every forwarding rule that names it.
#[derive(Debug, Clone, PartialEq)]
pub struct SshConnDef {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: AuthType,
    /// Panel-only grouping; None means the implicit "default" group.
    pub group: Option<String>,
    /// authType=key. May start with `~`, expanded at connect time.
    pub key_path: Option<String>,
    /// authType=key, optional.
    pub passphrase: Option<String>,
    /// authType=password.
    pub password: Option<String>,
    /// `SHA256:…`, learned on first successful connect; a change refuses the connection.
    pub host_key: Option<String>,
    /// docs/27 §1.1: `scheme://host[:port]`, scheme ∈ {http, socks5}. Stored port-normalized
    /// (http -> 80, socks5 -> 1080) with no credentials inside — those are the two fields
    /// below, resolved at connect time like every other credential.
    pub proxy: Option<String>,
    /// Whole-field `${...}` reference or literal; consumed by the proxy dialer (docs/27 §2).
    pub proxy_username: Option<String>,
    /// Same contract as proxy_username.
    pub proxy_password: Option<String>,
    /// Another connection's id (OpenSSH `-J`): this connection dials through that one's
    /// live session. The jump is itself an ordinary connection with its own auth/TOFU row.
    pub jump: Option<String>,
}

impl SshConnDef {
    /// The exact object shape the Node build stores — id, name, host, port, username, authType,
    /// then the optionals in validConn's assignment order, absent when unset.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("host".into(), json!(self.host));
        m.insert("port".into(), json!(self.port));
        m.insert("username".into(), json!(self.username));
        m.insert("authType".into(), json!(self.auth_type.as_str()));
        if let Some(v) = &self.key_path {
            m.insert("keyPath".into(), json!(v));
        }
        if let Some(v) = &self.passphrase {
            m.insert("passphrase".into(), json!(v));
        }
        if let Some(v) = &self.password {
            m.insert("password".into(), json!(v));
        }
        if let Some(v) = &self.host_key {
            m.insert("hostKey".into(), json!(v));
        }
        if let Some(v) = &self.group {
            m.insert("group".into(), json!(v));
        }
        // docs/27 §1.2: the new keys append AFTER the historical order, absent when unset —
        // the stored prefix stays byte-identical, so old files and old readers are untouched.
        if let Some(v) = &self.proxy {
            m.insert("proxy".into(), json!(v));
        }
        if let Some(v) = &self.proxy_username {
            m.insert("proxyUsername".into(), json!(v));
        }
        if let Some(v) = &self.proxy_password {
            m.insert("proxyPassword".into(), json!(v));
        }
        if let Some(v) = &self.jump {
            m.insert("jump".into(), json!(v));
        }
        Value::Object(m)
    }
}

/// One local (-L) forward: 127.0.0.1:localPort -> targetHost:targetPort, through connection_id.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleDef {
    pub id: String,
    pub name: String,
    pub connection_id: String,
    pub local_port: u16,
    pub target_host: String,
    pub target_port: u16,
    /// Always present ("" when empty) — the Node def always carries the field.
    pub remark: String,
    pub auto_reconnect: bool,
    /// Seconds between reconnect attempts.
    pub reconnect_interval: u64,
    /// "was running when the gateway last stopped" — the semantics managed.json uses for MCPs.
    pub enabled: bool,
    /// Names of MCPs this tunnel serves. Display and guard rail only — never automation.
    pub mcps: Vec<String>,
    /// Panel-only grouping; None means the implicit "default" group.
    pub group: Option<String>,
}

impl RuleDef {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), json!(self.id));
        m.insert("name".into(), json!(self.name));
        m.insert("connectionId".into(), json!(self.connection_id));
        m.insert("localPort".into(), json!(self.local_port));
        m.insert("targetHost".into(), json!(self.target_host));
        m.insert("targetPort".into(), json!(self.target_port));
        m.insert("remark".into(), json!(self.remark));
        m.insert("autoReconnect".into(), json!(self.auto_reconnect));
        m.insert("reconnectInterval".into(), json!(self.reconnect_interval));
        m.insert("enabled".into(), json!(self.enabled));
        m.insert(
            "mcps".into(),
            Value::Array(self.mcps.iter().map(|s| json!(s)).collect()),
        );
        if let Some(v) = &self.group {
            m.insert("group".into(), json!(v));
        }
        Value::Object(m)
    }
}

// --- states --------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleState {
    Stopped,
    Starting,
    Up,
    Reconnecting,
    Error,
}

impl RuleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleState::Stopped => "stopped",
            RuleState::Starting => "starting",
            RuleState::Up => "up",
            RuleState::Reconnecting => "reconnecting",
            RuleState::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Idle,
    Connecting,
    Connected,
    Error,
}

impl ConnState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnState::Idle => "idle",
            ConnState::Connecting => "connecting",
            ConnState::Connected => "connected",
            ConnState::Error => "error",
        }
    }
}

/// Which list a group operation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Rules,
    Connections,
}

impl GroupKind {
    pub fn from_segment(seg: &str) -> Option<GroupKind> {
        match seg {
            "rules" => Some(GroupKind::Rules),
            "connections" => Some(GroupKind::Connections),
            _ => None,
        }
    }
}

// --- failure taxonomy ----------------------------------------------------------------------------

/// Why a tunnel operation failed, and therefore whether retrying could ever help.
///
/// `auth` and `hostkey` must NOT be retried: hammering a server with bad credentials every 10
/// seconds earns a fail2ban ban or an account lockout, and a changed host key needs a human to
/// decide, not a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Auth,
    HostKey,
    Network,
    Port,
    Config,
    /// An in-flight remote operation was canceled through its handle — never retried,
    /// and mapped to RemoteError::Canceled at the transport seam (docs/34).
    Canceled,
}

impl FailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureKind::Auth => "auth",
            FailureKind::HostKey => "hostkey",
            FailureKind::Network => "network",
            FailureKind::Port => "port",
            FailureKind::Config => "config",
            FailureKind::Canceled => "canceled",
        }
    }
}

/// The tunnel subsystem's error: a message for the panel, a kind for the retry policy, and an
/// optional detail record (host-key mismatches carry `{expected, actual}`).
#[derive(Debug, Clone)]
pub struct TunnelError {
    pub message: String,
    pub kind: FailureKind,
    pub detail: Option<Map<String, Value>>,
}

impl TunnelError {
    pub fn new(message: impl Into<String>, kind: FailureKind) -> Self {
        TunnelError {
            message: message.into(),
            kind,
            detail: None,
        }
    }

    pub fn with_detail(
        message: impl Into<String>,
        kind: FailureKind,
        detail: Map<String, Value>,
    ) -> Self {
        TunnelError {
            message: message.into(),
            kind,
            detail: Some(detail),
        }
    }
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TunnelError {}

/// Only `network` is worth retrying — a transient blip may clear on its own. `port` is NOT: a
/// port held by another process will not free itself, so retrying only thrashes (and walks the
/// TCP owner table each attempt); it needs Force free, not a loop. `auth`/`hostkey`/`config`
/// never were.
pub fn is_retryable(kind: FailureKind) -> bool {
    kind == FailureKind::Network
}

// --- stats ---------------------------------------------------------------------------------------

/// Live counters for one forwarding rule's listener (the panel's traffic columns).
#[derive(Debug, Clone, Default)]
pub struct ForwardStats {
    pub sockets: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub channel_failures: u64,
    /// Connections refused because the per-rule socket cap was reached.
    pub refused: u64,
    pub last_error: Option<String>,
}

/// Who holds a local port (from `port.rs`), as the API writes it.
#[derive(Debug, Clone, PartialEq)]
pub struct PortOwner {
    pub pid: u32,
    pub name: String,
}

impl PortOwner {
    pub fn to_json(&self) -> Value {
        json!({ "pid": self.pid, "name": self.name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_v4_shaped_and_unique() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4'); // version nibble
        assert!(
            a.as_bytes()[19] == b'8'
                || a.as_bytes()[19] == b'9'
                || a.as_bytes()[19] == b'a'
                || a.as_bytes()[19] == b'b'
        );
    }

    #[test]
    fn number_coercion_matches_js() {
        assert_eq!(value_number(&json!(22)), 22.0);
        assert_eq!(value_number(&json!("5433")), 5433.0);
        assert_eq!(value_number(&json!("")), 0.0);
        assert_eq!(value_number(&json!(null)), 0.0);
        assert_eq!(value_number(&json!(true)), 1.0);
        assert!(value_number(&json!("abc")).is_nan());
        assert!(value_number(&json!({ "a": 1 })).is_nan());
    }

    #[test]
    fn number_formatting_matches_js_interpolation() {
        assert_eq!(fmt_number(22.0), "22");
        assert_eq!(fmt_number(0.5), "0.5");
        assert_eq!(fmt_number(f64::NAN), "NaN");
    }

    fn conn_def(id: &str, with_proxy: bool) -> SshConnDef {
        SshConnDef {
            id: id.into(),
            name: "box".into(),
            host: "bastion".into(),
            port: 22,
            username: "root".into(),
            auth_type: AuthType::Key,
            group: Some("prod".into()),
            key_path: Some("~/.ssh/id_rsa".into()),
            passphrase: Some("pp".into()),
            password: Some("pw".into()),
            host_key: Some("SHA256:x".into()),
            proxy: with_proxy.then(|| "socks5://127.0.0.1:7890".into()),
            proxy_username: with_proxy.then(|| "pxuser".into()),
            proxy_password: with_proxy.then(|| "pxpass".into()),
            jump: with_proxy.then(|| "other-conn".into()),
        }
    }

    /// docs/27 §1.2: unset proxy fields are ABSENT and the historical key order is unchanged —
    /// an old reader sees byte-identical objects.
    #[test]
    fn conn_json_keeps_node_field_order_without_proxy_fields() {
        let v = conn_def("c1", false).to_json();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "name",
                "host",
                "port",
                "username",
                "authType",
                "keyPath",
                "passphrase",
                "password",
                "hostKey",
                "group"
            ]
        );
    }

    /// docs/27 §1.2: the four new keys append after the historical order, absent when unset.
    #[test]
    fn conn_json_appends_proxy_fields_in_spec_order() {
        let v = conn_def("c1", true).to_json();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "name",
                "host",
                "port",
                "username",
                "authType",
                "keyPath",
                "passphrase",
                "password",
                "hostKey",
                "group",
                "proxy",
                "proxyUsername",
                "proxyPassword",
                "jump"
            ]
        );
    }
    #[test]
    fn rule_json_keeps_node_field_order() {
        let def = RuleDef {
            id: "r1".into(),
            name: "pg".into(),
            connection_id: "c1".into(),
            local_port: 5433,
            target_host: "db".into(),
            target_port: 5432,
            remark: "".into(),
            auto_reconnect: true,
            reconnect_interval: 10,
            enabled: false,
            mcps: vec!["pg-main".into()],
            group: None,
        };
        let v = def.to_json();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(
            keys.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec![
                "id",
                "name",
                "connectionId",
                "localPort",
                "targetHost",
                "targetPort",
                "remark",
                "autoReconnect",
                "reconnectInterval",
                "enabled",
                "mcps"
            ]
        );
    }
}
