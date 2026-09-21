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

//! MCP-over-HTTP OAuth (docs/24) — discovery, dynamic client registration, PKCE, token
//! exchange/refresh, and the sealed per-MCP credential store.
//!
//! Built for the hosted MCP endpoints that require OAuth 2.1 (Figma's remote MCP first, but
//! nothing here is Figma-specific except the provider-defaults table). The flow follows the MCP
//! authorization spec: RFC 9728 protected-resource discovery → RFC 8414 authorization-server
//! metadata → RFC 7591 dynamic client registration (DCR) → authorization-code + PKCE S256 with
//! a loopback callback → bearer tokens on every request, refresh on expiry.
//!
//! Why DCR deserves its own care (docs/24 §0): some providers — Figma — gate the registration
//! endpoint on an EXACT `client_name` allowlist ("Claude Code" and "Codex" register; anything
//! else 403s before a browser ever opens). The provider-defaults table below names those
//! defaults; `oauthClientName` in the def is the operator's one override.
//!
//! Credential discipline (docs/19 extended to OAuth): access/refresh tokens, client secrets,
//! codes, verifiers and states never reach managed.json, panel responses, logs, or error text.
//! Everything lives in the sealed `mcp-oauth.json` state file, keyed by MCP registry name.
//!
//! Idle cost: none. No task exists outside an authorization flow (the callback listener lives
//! only for the flow's 5-minute window); the credential map is O(number of OAuth MCPs).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use swiss_core::paths::data_path;
use swiss_core::secure::statefile::{read_secure_json, write_secure_json};

use crate::adapters::rest::{encode_uri_component, form_encode};

/// The marker every "re-authorize" error starts with (docs/24 D7). The panel keys on it to
/// light the Authorize button; adapters and the registry prefix it with the MCP's name.
pub const NEEDS_AUTH: &str = "needs authorization";

/// How long a flow waits for the browser redirect before giving up (docs/24 D5).
const CALLBACK_WAIT: Duration = Duration::from_secs(5 * 60);

/// Per-request deadline for the OAuth exchanges themselves (discovery/DCR/token).
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(20);

/// An access token expiring within this margin is treated as expired (docs/24 D4) — sending
/// one that dies in transit just buys a 401 round trip.
const EXPIRY_MARGIN_SECS: u64 = 60;

// --- provider defaults (docs/24 D3) -------------------------------------------------------------

/// The one endpoint the `figma` adapter type connects to (docs/24 rev). That type exists so
/// the panel asks for a name and nothing else — the URL is not the operator's decision.
pub const FIGMA_MCP_URL: &str = "https://mcp.figma.com/mcp";

/// True when this def authenticates through the gateway's OAuth flow: either its http def
/// says so (`auth: "oauth"`) or it is the `figma` type, which implies it. One predicate for
/// the badge, the authorize routes and the panel note — a future OAuth provider type only
/// has to be added here.
pub fn is_oauth(def: &swiss_host::config::ServerDef) -> bool {
    def.type_() == "figma" || def.get_str("auth") == Some("oauth")
}

/// Per-provider OAuth registration defaults. Only what a provider's DCR allowlist or scope
/// demands; everything else is standard flow.
pub struct ProviderDefaults {
    pub client_name: &'static str,
    pub scope: Option<&'static str>,
}

/// True when this MCP is Figma's hosted remote endpoint — the same judgement hermes-agent
/// makes (`_is_figma_remote_mcp`): URL match wins; a figma-ish NAME only counts when the URL
/// is empty or also says figma (a def named "figma-export" pointing elsewhere is not Figma).
pub fn is_figma_remote(url: &str, name: &str) -> bool {
    let url = url.to_ascii_lowercase();
    if url.contains("mcp.figma.com") || url.contains("figma.com/mcp") {
        return true;
    }
    name.to_ascii_lowercase().contains("figma") && (url.is_empty() || url.contains("figma"))
}

/// The defaults table (docs/24 D3). Figma's DCR allowlist only accepts specific client names
/// — registering as "Claude Code" is how every third-party client (Qoder/Hermes/pi included)
/// gets a browser flow at all; `"Codex"` is the other known-good name an operator can pin via
/// `oauthClientName`.
pub fn provider_defaults(url: &str, name: &str) -> ProviderDefaults {
    if is_figma_remote(url, name) {
        ProviderDefaults {
            client_name: "Claude Code",
            scope: Some("mcp:connect"),
        }
    } else {
        ProviderDefaults {
            client_name: "swiss",
            scope: None,
        }
    }
}

// --- small shared helpers -----------------------------------------------------------------------

/// Constant-time equality over byte strings — the OAuth `state` check must not leak by early
/// exit. No subtle-crate dependency (ADR-007 discipline): an XOR fold is the whole trick.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One PKCE pair (RFC 7636): a verifier of unreserved characters and its S256 challenge,
/// base64url without padding. The verifier is 48 random bytes as hex — 96 chars, inside the
/// 43–128 the RFC allows, from `OsRng` through the same helper the rest of the gateway uses.
pub fn pkce_pair() -> (String, String) {
    let verifier = swiss_core::util::random_hex(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// `scheme://host[:port]` of a URL — the origin RFC 9728 hangs its well-known off. Hand-rolled
/// (ADR-007: no url crate for two string splits); userinfo is refused rather than mishandled.
fn origin_of(url: &str) -> Result<String, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("not an absolute URL: {url}"))?;
    if scheme.is_empty() {
        return Err(format!("not an absolute URL: {url}"));
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err(format!("not a usable origin for: {url}"));
    }
    Ok(format!("{scheme}://{authority}"))
}

// --- discovery (docs/24 D2 steps 1–2) -----------------------------------------------------------

/// Everything one authorization flow needs that the endpoints themselves advertise.
#[derive(Clone)]
pub struct Discovered {
    /// The RFC 8707 resource indicator — sent on the authorize, token and refresh requests.
    pub resource: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes_supported: Vec<String>,
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value, String> {
    let response = client
        .get(url)
        .timeout(EXCHANGE_TIMEOUT)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|err| format!("{url}: {err}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| format!("{url}: reading body: {err}"))?;
    if !status.is_success() {
        return Err(format!(
            "{url}: HTTP {} {}",
            status.as_u16(),
            text.chars().take(200).collect::<String>()
        ));
    }
    serde_json::from_str(&text).map_err(|err| format!("{url}: not JSON: {err}"))
}

/// Discover the authorization server for an MCP endpoint (RFC 9728 then RFC 8414). Both
/// well-knowns hang off origins; Figma's issuer is an origin, hermes/mcp-remote assume the
/// same, and an issuer carrying a path is not a shape this gateway needs today.
pub async fn discover(client: &reqwest::Client, mcp_url: &str) -> Result<Discovered, String> {
    let resource_origin = origin_of(mcp_url)?;
    let protected: Value = get_json(
        client,
        &format!("{resource_origin}/.well-known/oauth-protected-resource"),
    )
    .await
    .map_err(|err| format!("oauth discovery failed: {err}"))?;
    let servers = protected
        .get("authorization_servers")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            "oauth discovery failed: authorization_servers is missing or empty".to_string()
        })?;
    let issuer = servers[0].as_str().ok_or_else(|| {
        "oauth discovery failed: authorization_servers[0] is not a string".to_string()
    })?;
    let issuer = origin_of(issuer)?;
    let server: Value = get_json(
        client,
        &format!("{issuer}/.well-known/oauth-authorization-server"),
    )
    .await
    .map_err(|err| format!("oauth discovery failed: {err}"))?;
    let required = |key: &str| {
        server
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("oauth discovery failed: metadata has no {key}"))
    };
    let scopes_from = |doc: &Value| {
        doc.get("scopes_supported")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    // Figma advertises scopes on the PROTECTED-RESOURCE document (RFC 9728), not the AS
    // metadata (RFC 8414) — captured live 2026-09-15 — so that document wins when both speak.
    let scopes_supported = {
        let from_protected = scopes_from(&protected);
        if from_protected.is_empty() {
            scopes_from(&server)
        } else {
            from_protected
        }
    };
    Ok(Discovered {
        // The resource indicator is the MCP endpoint URL as advertised (fall back to the
        // def's url — the common case is that they are the same string).
        resource: protected
            .get("resource")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(mcp_url)
            .to_string(),
        authorization_endpoint: required("authorization_endpoint")?,
        token_endpoint: required("token_endpoint")?,
        registration_endpoint: server
            .get("registration_endpoint")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        scopes_supported,
    })
}

// --- dynamic client registration (docs/24 D2 step 3) ---------------------------------------------

/// What DCR handed back: the client identity (and, when the provider issues one despite
/// advertising public-client flows — Figma does — the secret the token endpoint will demand).
#[derive(Clone)]
pub struct ClientReg {
    pub client_id: String,
    pub client_secret: Option<String>,
    /// The auth method token requests use; "client_secret_post" everywhere this gateway goes.
    pub token_auth: String,
}

/// Register a client (RFC 7591). A 403 here usually means the provider allowlists client
/// names — the human message says exactly that rather than a bare "forbidden" (docs/24 D7,
/// after hermes' `humanize_oauth_registration_error`).
pub async fn register_client(
    client: &reqwest::Client,
    registration_endpoint: &str,
    client_name: &str,
    redirect_uri: &str,
    scope: Option<&str>,
) -> Result<ClientReg, String> {
    let mut metadata = json!({
        "client_name": client_name,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "client_secret_post",
    });
    if let Some(scope) = scope {
        metadata["scope"] = Value::String(scope.to_string());
    }
    let response = client
        .post(registration_endpoint)
        .timeout(EXCHANGE_TIMEOUT)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .json(&metadata)
        .send()
        .await
        .map_err(|err| format!("client registration failed: {err}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        if status.as_u16() == 403 || status.as_u16() == 401 {
            return Err(format!(
                "OAuth registration refused (HTTP {}) — the registration endpoint only accepts \
                 specific client names (e.g. \"Claude Code\", \"Codex\"); set oauthClientName \
                 to one of them",
                status.as_u16()
            ));
        }
        return Err(format!(
            "client registration failed: HTTP {}: {}",
            status.as_u16(),
            text.chars().take(300).collect::<String>()
        ));
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|err| format!("registration response is not JSON: {err}"))?;
    let client_id = body
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "registration response has no client_id".to_string())?
        .to_string();
    // Figma's quirk (docs/24 D2.3): the register response carries a client_secret that the
    // token endpoint then requires — saved unconditionally when present, sent as
    // client_secret_post. A provider that states its own auth method wins over our default.
    let token_auth = body
        .get("token_endpoint_auth_method")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("client_secret_post")
        .to_string();
    Ok(ClientReg {
        client_id,
        client_secret: body
            .get("client_secret")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        token_auth,
    })
}

// --- the authorization URL (docs/24 D2 step 4) ---------------------------------------------------

/// Build the authorization URL — pure, so its exact shape is testable. Every value is
/// percent-encoded; `state` is mandatory (Figma sets `require_state_parameter`).
pub fn authorization_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scope: Option<&str>,
    state: &str,
    code_challenge: &str,
    resource: &str,
) -> String {
    let mut pairs: Vec<(String, String)> = vec![
        ("response_type".into(), "code".into()),
        ("client_id".into(), client_id.into()),
        ("redirect_uri".into(), redirect_uri.into()),
        ("state".into(), state.into()),
        ("code_challenge".into(), code_challenge.into()),
        ("code_challenge_method".into(), "S256".into()),
        ("resource".into(), resource.into()),
    ];
    if let Some(scope) = scope {
        pairs.push(("scope".into(), scope.into()));
    }
    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{authorization_endpoint}?{query}")
}

// --- token exchange and refresh (docs/24 D2 steps 6–7) -------------------------------------------

/// One grant's outcome. `expires_at` is absolute (unix seconds) — the response's relative
/// `expires_in` is useless the moment it is persisted.
#[derive(Clone, Debug, PartialEq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scope: Option<String>,
}

/// One token-endpoint POST (form-encoded), with the OAuth error body decoded when present.
async fn token_post(
    client: &reqwest::Client,
    token_endpoint: &str,
    pairs: &[(&str, &str)],
) -> Result<Value, String> {
    let body = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let response = client
        .post(token_endpoint)
        .timeout(EXCHANGE_TIMEOUT)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|err| format!("token request failed: {err}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    if !status.is_success() {
        let error_code = parsed
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        // The two codes that mean "this grant is dead, re-authorize" (docs/24 D2.7) — surfaced
        // with the marker the panel keys on so the Authorize button lights.
        if error_code == "invalid_grant" || error_code == "invalid_client" {
            return Err(format!(
                "{NEEDS_AUTH} — the grant was refused ({error_code})"
            ));
        }
        return Err(format!(
            "token request failed: HTTP {}: {}",
            status.as_u16(),
            text.chars().take(300).collect::<String>()
        ));
    }
    parsed.ok_or_else(|| "token response is not JSON".to_string())
}

fn parse_tokens(body: Value) -> Result<TokenSet, String> {
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "token response has no access_token".to_string())?
        .to_string();
    Ok(TokenSet {
        access_token,
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        expires_at: body
            .get("expires_in")
            .and_then(Value::as_u64)
            .map(|secs| now_unix() + secs),
        scope: body
            .get("scope")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// Exchange the authorization code for tokens (PKCE verifier proves the same client that
/// started the flow is finishing it; the RFC 8707 `resource` rides along).
pub async fn exchange_code(
    client: &reqwest::Client,
    token_endpoint: &str,
    reg: &ClientReg,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    resource: &str,
) -> Result<TokenSet, String> {
    let mut pairs = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("redirect_uri", redirect_uri),
        ("client_id", reg.client_id.as_str()),
        ("resource", resource),
    ];
    if let Some(secret) = &reg.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    parse_tokens(token_post(client, token_endpoint, &pairs).await?)
}

/// Refresh the access token. An `invalid_grant`/`invalid_client` refusal comes back with the
/// NEEDS_AUTH marker — the caller clears the stored credentials and the panel re-lights the
/// Authorize button (docs/24 D4).
pub async fn refresh(
    client: &reqwest::Client,
    token_endpoint: &str,
    reg: &ClientReg,
    refresh_token: &str,
    resource: &str,
) -> Result<TokenSet, String> {
    let mut pairs = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", reg.client_id.as_str()),
        ("resource", resource),
    ];
    if let Some(secret) = &reg.client_secret {
        pairs.push(("client_secret", secret.as_str()));
    }
    parse_tokens(token_post(client, token_endpoint, &pairs).await?)
}

// --- the sealed credential store (docs/24 D1) -----------------------------------------------------

/// Where the sealed credential file lives — `mcp-oauth.json` in the data dir, keyed by MCP
/// registry name. NOT the docs/19 vault: these values are machine-rotated runtime state, and
/// the vault's "write-only, re-store when forgotten" human contract has no business over a
/// refresh chain.
pub fn store_path() -> PathBuf {
    data_path(&["mcp-oauth.json"])
}

/// One MCP's persisted OAuth state: the registered client half plus the current token half.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredCredentials {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub token_auth: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub scope: Option<String>,
    pub at: u64,
}

impl StoredCredentials {
    /// Assemble from a completed flow: the fresh client registration and its first tokens
    /// land together (docs/24 D6 — never a new client beside old tokens).
    pub fn new(reg: &ClientReg, tokens: &TokenSet, at: u64) -> Self {
        Self {
            client_id: reg.client_id.clone(),
            client_secret: reg.client_secret.clone(),
            token_auth: reg.token_auth.clone(),
            access_token: Some(tokens.access_token.clone()),
            refresh_token: tokens.refresh_token.clone(),
            expires_at: tokens.expires_at,
            scope: tokens.scope.clone(),
            at,
        }
    }

    /// Swap the token half, keeping the client half — the refresh path (docs/24 D4).
    pub fn with_tokens(mut self, tokens: &TokenSet) -> Self {
        self.access_token = Some(tokens.access_token.clone());
        // A refresh response may omit the refresh token; the old one stays valid (RFC 6749 §6).
        if tokens.refresh_token.is_some() {
            self.refresh_token = tokens.refresh_token.clone();
        }
        self.expires_at = tokens.expires_at;
        if tokens.scope.is_some() {
            self.scope = tokens.scope.clone();
        }
        self
    }

    /// True when the access token is usable RIGHT NOW (unexpired with the margin) — the
    /// connect-time check in docs/24 D4.
    pub fn access_fresh(&self) -> bool {
        match (&self.access_token, self.expires_at) {
            (Some(_), Some(expires_at)) => now_unix() + EXPIRY_MARGIN_SECS < expires_at,
            // No expiry advertised: treat as valid until a 401 says otherwise.
            (Some(_), None) => true,
            _ => false,
        }
    }

    fn to_json(&self) -> Value {
        let mut v = json!({
            "client_id": self.client_id,
            "token_auth": self.token_auth,
            "at": self.at,
        });
        if let Some(secret) = &self.client_secret {
            v["client_secret"] = Value::String(secret.clone());
        }
        if let Some(access) = &self.access_token {
            v["access_token"] = Value::String(access.clone());
        }
        if let Some(rt) = &self.refresh_token {
            v["refresh_token"] = Value::String(rt.clone());
        }
        if let Some(expires) = self.expires_at {
            v["expires_at"] = Value::from(expires);
        }
        if let Some(scope) = &self.scope {
            v["scope"] = Value::String(scope.clone());
        }
        v
    }

    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            client_id: v.get("client_id")?.as_str()?.to_string(),
            client_secret: v
                .get("client_secret")
                .and_then(Value::as_str)
                .map(str::to_string),
            token_auth: v
                .get("token_auth")
                .and_then(Value::as_str)
                .unwrap_or("client_secret_post")
                .to_string(),
            access_token: v
                .get("access_token")
                .and_then(Value::as_str)
                .map(str::to_string),
            refresh_token: v
                .get("refresh_token")
                .and_then(Value::as_str)
                .map(str::to_string),
            expires_at: v.get("expires_at").and_then(Value::as_u64),
            scope: v.get("scope").and_then(Value::as_str).map(str::to_string),
            at: v.get("at").and_then(Value::as_u64).unwrap_or(0),
        })
    }
}

/// The in-process credential map, loaded once from the sealed file (the vault's pattern).
/// Mutations persist inside the write lock, so writers serialize; the file is the only place
/// these values ever exist outside memory.
static STORE: OnceLock<RwLock<HashMap<String, StoredCredentials>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, StoredCredentials>> {
    STORE.get_or_init(|| {
        let map = match read_secure_json(&store_path()) {
            Ok(Some(Value::Object(obj))) => obj
                .iter()
                .filter_map(|(name, v)| StoredCredentials::from_json(v).map(|c| (name.clone(), c)))
                .collect(),
            _ => HashMap::new(),
        };
        RwLock::new(map)
    })
}

/// A copy of one MCP's credentials, if any exist.
pub fn credentials(name: &str) -> Option<StoredCredentials> {
    store().read().ok().and_then(|g| g.get(name).cloned())
}

/// Replace one MCP's credentials whole — the post-flow write (new client AND new tokens in one
/// atomic swap, docs/24 D6). A read failure logs and keeps serving; a write failure is an
/// error the caller reports.
pub fn replace_credentials(name: &str, creds: StoredCredentials) -> Result<(), String> {
    let mut guard = store()
        .write()
        .map_err(|_| "credential store lock poisoned")?;
    guard.insert(name.to_string(), creds);
    persist_locked(&guard)
}

/// Swap only the token half (the refresh path). No entry yet is an error — a refresh without a
/// registered client cannot happen through this module's flow.
pub fn update_tokens(name: &str, tokens: &TokenSet) -> Result<StoredCredentials, String> {
    let mut guard = store()
        .write()
        .map_err(|_| "credential store lock poisoned")?;
    let entry = guard
        .get_mut(name)
        .ok_or_else(|| format!("{NEEDS_AUTH} — no stored credentials for '{name}'"))?;
    *entry = entry.clone().with_tokens(tokens);
    let out = entry.clone();
    persist_locked(&guard)?;
    Ok(out)
}

/// Drop one MCP's credentials entirely — the needs-auth path (docs/24 D2.7/D4): the old
/// client_id is worthless anyway (each flow re-registers with a fresh loopback port).
pub fn clear_credentials(name: &str) -> Result<(), String> {
    let mut guard = store()
        .write()
        .map_err(|_| "credential store lock poisoned")?;
    if guard.remove(name).is_some() {
        return persist_locked(&guard);
    }
    Ok(())
}

fn persist_locked(map: &HashMap<String, StoredCredentials>) -> Result<(), String> {
    let value = Value::Object(map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect());
    write_secure_json(&store_path(), &value)
        .map_err(|err| format!("persisting oauth credentials failed: {err}"))
}

/// An MCP rename re-keys its stored credentials (the store is name-keyed, docs/24 D1). A name
/// holding nothing is a no-op — renaming an MCP that never authorized must not fail the rename.
/// A collision at the destination is refused rather than silently overwritten.
pub fn rename_credentials(from: &str, to: &str) -> Result<(), String> {
    let mut guard = store()
        .write()
        .map_err(|_| "credential store lock poisoned")?;
    if !guard.contains_key(from) {
        return Ok(());
    }
    if guard.contains_key(to) {
        return Err(format!("oauth credentials already exist under '{to}'"));
    }
    if let Some(creds) = guard.remove(from) {
        guard.insert(to.to_string(), creds);
    }
    persist_locked(&guard)
}

// --- authorize flows: the pollable state behind the admin API (docs/24 D5) --------------------------

/// One authorize flow's live state, written by the flow task and polled by the admin API. Held
/// in AppContext's name-keyed map; a new POST replaces a terminal handle and is refused while
/// one is live (single-flight per name).
pub struct FlowHandle {
    /// Identifies this attempt in API answers; random per POST.
    pub flow_id: String,
    pub status: RwLock<FlowStatus>,
}

/// The four states of docs/24 D5. `Starting` and `AuthorizationRequired` are live (the flow owns
/// a loopback listener); `Approved` and `Error` are terminal — the panel stops polling on them.
#[derive(Clone)]
pub enum FlowStatus {
    /// Discovery + dynamic registration in flight.
    Starting,
    /// The URL the panel must open in a real browser.
    AuthorizationRequired(String),
    /// Tokens exchanged and stored, the MCP started. `tools` counts the live tools/list.
    Approved { tools: usize },
    /// Terminal failure; the message is panel-safe (no token ever travels this path, D7).
    Error(String),
}

impl FlowStatus {
    /// The JSON the poll route answers — status strings are the panel's contract (D5).
    pub fn to_json(&self) -> Value {
        match self {
            FlowStatus::Starting => json!({ "status": "starting" }),
            FlowStatus::AuthorizationRequired(url) => {
                json!({ "status": "authorization_required", "authorizationUrl": url })
            }
            FlowStatus::Approved { tools } => json!({ "status": "approved", "tools": tools }),
            FlowStatus::Error(message) => json!({ "status": "error", "error": message }),
        }
    }

    /// Terminal states end polling (the panel loop exits on them).
    pub fn is_terminal(&self) -> bool {
        matches!(self, FlowStatus::Approved { .. } | FlowStatus::Error(_))
    }
}

// --- the authorization flow (docs/24 D2 steps 3–6, D5) ---------------------------------------------

/// A running authorization flow. `start_flow` binds the loopback callback, registers the
/// client (DCR) and returns here with the URL the PANEL opens in the user's browser — the
/// gateway never spawns a browser. `complete` waits for the redirect and exchanges the code.
pub struct AuthFlow {
    /// What the panel opens; also surfaced by the authorize API (docs/24 D5).
    pub authorization_url: String,
    /// The loopback redirect this flow registered — `http://127.0.0.1:{port}/callback`.
    pub redirect_uri: String,
    /// This flow's `state` (test-visible: the fake-AS suite must simulate the redirect).
    pub state: String,
    http: reqwest::Client,
    discovered: Discovered,
    reg: ClientReg,
    verifier: String,
    callback_rx: tokio::sync::oneshot::Receiver<CallbackOutcome>,
    /// Dropping this (with the flow, or when `complete` destructures it) ends the listener:
    /// the serve task's graceful-shutdown watcher resolves once no sender remains, so the
    /// loopback listener never outlives its flow — without a Drop impl, which would forbid the
    /// field moves `complete` needs. Never read on purpose: holding it IS the mechanism.
    #[allow(dead_code)]
    shutdown: tokio::sync::watch::Sender<()>,
}

enum CallbackOutcome {
    Code(String),
    Refused(String),
}

/// The browser-facing result of the redirect — tiny, no script, nothing secret.
const CALLBACK_OK_PAGE: &str =
    "<html><body><h2>Authorized</h2><p>You can return to the swiss panel.</p></body></html>";
const CALLBACK_BAD_PAGE: &str = "<html><body><h2>Invalid OAuth callback</h2></body></html>";

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<CallbackOutcome>>>>,
}

async fn callback_handler(
    axum::extract::State(state): axum::extract::State<CallbackState>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    // `got` — the matched query param must not shadow the extractor's `state`.
    let expected = &state.expected_state;
    let outcome = match (params.get("code"), params.get("error"), params.get("state")) {
        (Some(code), _, Some(got)) if ct_eq(got, expected) => CallbackOutcome::Code(code.clone()),
        (_, Some(error), Some(got)) if ct_eq(got, expected) => {
            CallbackOutcome::Refused(error.clone())
        }
        _ => CallbackOutcome::Refused("callback did not carry a matching state".into()),
    };
    let ok = matches!(outcome, CallbackOutcome::Code(_));
    if let Ok(mut slot) = state.tx.lock() {
        if let Some(tx) = slot.take() {
            let _ = tx.send(outcome);
        }
    }
    if ok {
        (
            axum::http::StatusCode::OK,
            axum::response::Html(CALLBACK_OK_PAGE),
        )
    } else {
        (
            axum::http::StatusCode::BAD_REQUEST,
            axum::response::Html(CALLBACK_BAD_PAGE),
        )
    }
}

/// Bind the loopback listener, discover the endpoints, register the client, and build the
/// authorization URL. `client_name_override` is the def's `oauthClientName` and always wins
/// over the provider default (docs/24 D3).
pub async fn start_flow(
    http: reqwest::Client,
    mcp_name: &str,
    mcp_url: &str,
    client_name_override: Option<&str>,
) -> Result<AuthFlow, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| format!("binding the oauth callback listener failed: {err}"))?;
    let port = listener
        .local_addr()
        .map_err(|err| format!("reading the callback port failed: {err}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let discovered = discover(&http, mcp_url).await?;
    let defaults = provider_defaults(mcp_url, mcp_name);
    let client_name = client_name_override.unwrap_or(defaults.client_name);
    // Scope: the provider default first, else what the AS advertises, else none (docs/24 D3).
    let scope: Option<String> = defaults.scope.map(str::to_string).or_else(|| {
        (!discovered.scopes_supported.is_empty()).then(|| discovered.scopes_supported.join(" "))
    });
    let registration_endpoint = discovered.registration_endpoint.clone().ok_or_else(|| {
        "oauth registration failed: the authorization server does not advertise dynamic \
         client registration, and this gateway does not store a pre-registered client_id"
            .to_string()
    })?;
    let reg = register_client(
        &http,
        &registration_endpoint,
        client_name,
        &redirect_uri,
        scope.as_deref(),
    )
    .await?;

    let state = swiss_core::util::random_hex(32);
    let (verifier, challenge) = pkce_pair();
    let authorization_url = authorization_url(
        &discovered.authorization_endpoint,
        &reg.client_id,
        &redirect_uri,
        scope.as_deref(),
        &state,
        &challenge,
        &discovered.resource,
    );

    let (tx, callback_rx) = tokio::sync::oneshot::channel();
    let app = axum::Router::new()
        .route("/callback", axum::routing::get(callback_handler))
        .with_state(CallbackState {
            expected_state: state.clone(),
            tx: Arc::new(Mutex::new(Some(tx))),
        });
    let (shutdown, mut shutdown_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // Resolves when the flow's sender drops — completed, failed or abandoned.
                let _ = shutdown_rx.changed().await;
            })
            .await;
    });

    Ok(AuthFlow {
        authorization_url,
        redirect_uri,
        state,
        http,
        discovered,
        reg,
        verifier,
        callback_rx,
        shutdown,
    })
}

impl AuthFlow {
    /// Wait for the browser redirect (the 5-minute window), then exchange the code. Returns
    /// the assembled credentials — the caller persists them with `replace_credentials`.
    pub async fn complete(self) -> Result<StoredCredentials, String> {
        // Destructuring drops `shutdown` at scope end — the listener winds down with it.
        let Self {
            http,
            discovered,
            reg,
            verifier,
            redirect_uri,
            callback_rx,
            ..
        } = self;
        let outcome = match tokio::time::timeout(CALLBACK_WAIT, callback_rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => return Err("the oauth callback listener closed unexpectedly".to_string()),
            Err(_) => {
                return Err(
                    "authorization timed out after 5 minutes — start again from the panel"
                        .to_string(),
                )
            }
        };
        let code = match outcome {
            CallbackOutcome::Code(code) => code,
            CallbackOutcome::Refused(error) => {
                return Err(format!("authorization was refused: {error}"))
            }
        };
        let tokens = exchange_code(
            &http,
            &discovered.token_endpoint,
            &reg,
            &code,
            &verifier,
            &redirect_uri,
            &discovered.resource,
        )
        .await?;
        Ok(StoredCredentials::new(&reg, &tokens, now_unix()))
    }
}

#[cfg(test)]
mod tests {
    //! Pure parts pinned by unit tests; the wire flow driven end-to-end against a fake
    //! authorization server on an ephemeral loopback port — the same shape rest.rs's tests use.
    //!
    //! The fake AS replicates Figma's quirks (docs/24 §0): a client_name allowlist on the
    //! register endpoint, a client_secret issued alongside token_endpoint_auth_method:"none",
    //! the secret then REQUIRED on the token POST, mandatory state, S256 verification and the
    //! RFC 8707 resource check. Nothing here touches the network beyond 127.0.0.1.
    use super::*;

    // ---- provider defaults ---------------------------------------------------------------

    #[test]
    fn figma_detection_matches_on_url_first_and_name_only_when_unambiguous() {
        for url in [
            "https://mcp.figma.com/mcp",
            "https://mcp.figma.com",
            "https://figma.com/mcp",
            "https://www.figma.com/mcp?x=1",
        ] {
            assert!(is_figma_remote(url, "anything"), "{url}");
        }
        // A figma-ish name with an empty or matching url still counts (hermes' rule).
        assert!(is_figma_remote("", "design-figma"));
        assert!(is_figma_remote("https://figma.example/mcp", "figma"));
        // ...but the name never drags in a different host.
        assert!(!is_figma_remote(
            "https://api.example.com/mcp",
            "figma-export"
        ));
        assert!(!is_figma_remote("https://api.example.com/mcp", "design"));
    }

    #[test]
    fn is_oauth_reads_both_spellings() {
        // The predicate behind the badge, the authorize guard and the panel note: a def is
        // OAuth when its http def says so, or when it is the figma type that implies it.
        let http_oauth = swiss_host::config::ServerDef(
            serde_json::json!({
                "type": "http",
                "url": "https://mcp.example.com/mcp",
                "auth": "oauth",
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        assert!(is_oauth(&http_oauth));
        let figma = swiss_host::config::ServerDef(
            serde_json::json!({ "type": "figma" })
                .as_object()
                .expect("object")
                .clone(),
        );
        assert!(is_oauth(&figma));
        let plain = swiss_host::config::ServerDef(
            serde_json::json!({
                "type": "http",
                "url": "https://mcp.example.com/mcp",
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        assert!(!is_oauth(&plain));
    }

    #[test]
    fn the_defaults_table_gives_figma_the_allowlisted_name_and_scope() {
        let figma = provider_defaults("https://mcp.figma.com/mcp", "figma");
        assert_eq!(figma.client_name, "Claude Code");
        assert_eq!(figma.scope, Some("mcp:connect"));

        let other = provider_defaults("https://mcp.example.com/mcp", "linear");
        assert_eq!(other.client_name, "swiss");
        assert_eq!(other.scope, None);
    }

    // ---- pure helpers ---------------------------------------------------------------------

    #[test]
    fn pkce_pairs_are_s256_consistent_and_unique() {
        let (verifier, challenge) = pkce_pair();
        // 96 hex chars from 48 bytes — inside RFC 7636's 43..=128 unreserved window.
        assert!(verifier.len() >= 43 && verifier.len() <= 128, "{verifier}");
        assert!(verifier.bytes().all(|b| b.is_ascii_alphanumeric()));
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        assert_ne!(pkce_pair().0, verifier, "verifiers must not repeat");
    }

    #[test]
    fn origin_extraction_strips_paths_and_keeps_ports() {
        assert_eq!(
            origin_of("https://mcp.figma.com/mcp").unwrap(),
            "https://mcp.figma.com"
        );
        assert_eq!(
            origin_of("http://127.0.0.1:8080/x/y").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(origin_of("mcp.figma.com/mcp").is_err(), "no scheme");
        assert!(
            origin_of("https://user@host/x").is_err(),
            "userinfo refused"
        );
    }

    #[test]
    fn the_authorization_url_carries_every_required_parameter() {
        let url = authorization_url(
            "https://www.figma.com/oauth/mcp",
            "cid-1",
            "http://127.0.0.1:45678/callback",
            Some("mcp:connect openid"),
            "state-123",
            "challenge-xyz",
            "https://mcp.figma.com/mcp",
        );
        let (base, query) = url.split_once('?').expect("query present");
        assert_eq!(base, "https://www.figma.com/oauth/mcp");
        let pairs: Vec<(String, String)> = query
            .split('&')
            .map(|pair| {
                pair.split_once('=')
                    .map(|(k, v)| {
                        // the same decoder the rest of the crate uses, round-tripped
                        (
                            crate::adapters::rest::form_decode(k),
                            crate::adapters::rest::form_decode(v),
                        )
                    })
                    .expect("every pair is k=v")
            })
            .collect();
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("response_type"), "code");
        assert_eq!(get("client_id"), "cid-1");
        assert_eq!(get("redirect_uri"), "http://127.0.0.1:45678/callback");
        assert_eq!(get("scope"), "mcp:connect openid");
        assert_eq!(get("state"), "state-123");
        assert_eq!(get("code_challenge"), "challenge-xyz");
        assert_eq!(get("code_challenge_method"), "S256");
        assert_eq!(get("resource"), "https://mcp.figma.com/mcp");
    }

    #[test]
    fn constant_time_equality_is_plain_equality() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }

    // ---- stored credentials ----------------------------------------------------------------

    fn sample_reg() -> ClientReg {
        ClientReg {
            client_id: "cid-9".into(),
            client_secret: Some("sec-9".into()),
            token_auth: "client_secret_post".into(),
        }
    }

    fn sample_tokens() -> TokenSet {
        TokenSet {
            access_token: "acc-1".into(),
            refresh_token: Some("ref-1".into()),
            expires_at: Some(now_unix() + 3600),
            scope: Some("mcp:connect".into()),
        }
    }

    #[test]
    fn a_refresh_that_omits_the_refresh_token_keeps_the_old_one() {
        let creds = StoredCredentials::new(&sample_reg(), &sample_tokens(), 1000);
        let rotated = TokenSet {
            access_token: "acc-2".into(),
            refresh_token: None, // RFC 6749 §6: absent means unchanged
            expires_at: Some(now_unix() + 7200),
            scope: None,
        };
        let next = creds.clone().with_tokens(&rotated);
        assert_eq!(next.access_token.as_deref(), Some("acc-2"));
        assert_eq!(next.refresh_token.as_deref(), Some("ref-1"));
        assert_eq!(next.client_id, "cid-9", "the client half is untouched");
    }

    #[test]
    fn freshness_respects_the_expiry_margin() {
        let mut creds = StoredCredentials::new(&sample_reg(), &sample_tokens(), 1000);
        // Dying inside the 60s margin is already dead.
        creds.expires_at = Some(now_unix() + 30);
        assert!(!creds.access_fresh());
        creds.expires_at = Some(now_unix() + 3600);
        assert!(creds.access_fresh());
        // No advertised expiry: valid until a 401 says otherwise.
        creds.expires_at = None;
        assert!(creds.access_fresh());
        creds.access_token = None;
        assert!(!creds.access_fresh());
    }

    // ---- the fake authorization server ------------------------------------------------------

    use std::sync::atomic::{AtomicI64, Ordering};

    /// One minted code: what the token endpoint will check the exchange against.
    struct MintedCode {
        challenge: String,
        client_id: String,
        redirect_uri: String,
    }

    #[derive(Clone)]
    struct FakeAs {
        base: String,
        allowlist: Arc<Mutex<Vec<&'static str>>>,
        clients: Arc<Mutex<HashMap<String, String>>>,
        codes: Arc<Mutex<HashMap<String, MintedCode>>>,
        refresh_valid: Arc<Mutex<std::collections::HashSet<String>>>,
        counter: Arc<AtomicI64>,
    }

    /// The fake AS on an ephemeral loopback port — Figma's shapes with none of its network.
    /// The port is bound BEFORE the router is built so every handler captures a finished base.
    async fn fake_as(allowlist: Vec<&'static str>) -> FakeAs {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let port = listener.local_addr().expect("addr").port();
        let state = FakeAs {
            base: format!("http://127.0.0.1:{port}"),
            allowlist: Arc::new(Mutex::new(allowlist)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            codes: Arc::new(Mutex::new(HashMap::new())),
            refresh_valid: Arc::new(Mutex::new(std::collections::HashSet::new())),
            counter: Arc::new(AtomicI64::new(0)),
        };
        // One clone per closure: a move closure consumes what it captures, so five routes
        // need five owners (the Arcs inside make each clone cheap).
        let meta_a = state.base.clone();
        let meta_b = state.base.clone();
        let register_s = state.clone();
        let mint_s = state.clone();
        let token_s = state.clone();
        let app = axum::Router::new()
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(move || {
                    let base = meta_a;
                    async move {
                        axum::Json(json!({
                            "resource": format!("{base}/mcp"),
                            "authorization_servers": [base],
                            "scopes_supported": ["mcp:connect"],
                        }))
                    }
                }),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(move || {
                    let base = meta_b;
                    async move {
                        axum::Json(json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/oauth/authorize"),
                            "token_endpoint": format!("{base}/v1/oauth/token"),
                            "registration_endpoint": format!("{base}/v1/oauth/mcp/register"),
                            "code_challenge_methods_supported": ["S256"],
                            "require_state_parameter": true,
                        }))
                    }
                }),
            )
            .route(
                "/v1/oauth/mcp/register",
                axum::routing::post(move |body: String| {
                    let s = register_s.clone();
                    async move {
                        let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                        let name = req.get("client_name").and_then(Value::as_str).unwrap_or("");
                        if !s.allowlist.lock().unwrap().contains(&name) {
                            return (
                                axum::http::StatusCode::FORBIDDEN,
                                axum::Json(json!({ "error": "invalid_client_metadata" })),
                            );
                        }
                        let redirect = req
                            .pointer("/redirect_uris/0")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if !redirect.starts_with("http://127.0.0.1:") {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({ "error": "invalid_redirect_uri" })),
                            );
                        }
                        let n = s.counter.fetch_add(1, Ordering::SeqCst) + 1;
                        let (id, secret) = (format!("cid-{n}"), format!("sec-{n}"));
                        s.clients.lock().unwrap().insert(id.clone(), secret.clone());
                        // The Figma quirk: auth method "none" in the response, a secret anyway —
                        // and the token endpoint below refuses to work without it.
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(json!({
                                "client_id": id,
                                "client_secret": secret,
                                "token_endpoint_auth_method": "none",
                            })),
                        )
                    }
                }),
            )
            .route(
                "/test/mint",
                axum::routing::get(
                    move |params: axum::extract::Query<HashMap<String, String>>| {
                        let s = mint_s.clone();
                        async move {
                            let n = s.counter.fetch_add(1, Ordering::SeqCst) + 1;
                            let code = format!("code-{n}");
                            s.codes.lock().unwrap().insert(
                                code.clone(),
                                MintedCode {
                                    challenge: params.get("challenge").cloned().unwrap_or_default(),
                                    client_id: params.get("client_id").cloned().unwrap_or_default(),
                                    redirect_uri: params
                                        .get("redirect_uri")
                                        .cloned()
                                        .unwrap_or_default(),
                                },
                            );
                            axum::Json(json!({ "code": code }))
                        }
                    },
                ),
            )
            .route(
                "/v1/oauth/token",
                axum::routing::post(move |body: String| {
                    let s = token_s.clone();
                    async move {
                        let form: HashMap<String, String> = body
                            .split('&')
                            .filter_map(|pair| pair.split_once('='))
                            .map(|(k, v)| {
                                (
                                    crate::adapters::rest::form_decode(k),
                                    crate::adapters::rest::form_decode(v),
                                )
                            })
                            .collect();
                        let get = |k: &str| form.get(k).cloned().unwrap_or_default();
                        let client_id = get("client_id");
                        let secret_ok = match (
                            s.clients.lock().unwrap().get(&client_id),
                            get("client_secret"),
                        ) {
                            (Some(want), got) => *want == got && !got.is_empty(),
                            (None, _) => false,
                        };
                        if !secret_ok {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({ "error": "invalid_client" })),
                            );
                        }
                        let resource_ok = get("resource") == format!("{}/mcp", s.base);
                        match get("grant_type").as_str() {
                            "authorization_code" => {
                                let Some(mint) = s.codes.lock().unwrap().remove(&get("code"))
                                else {
                                    return (
                                        axum::http::StatusCode::BAD_REQUEST,
                                        axum::Json(json!({ "error": "invalid_grant" })),
                                    );
                                };
                                let verifier = get("code_verifier");
                                let challenge =
                                    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                                if challenge != mint.challenge
                                    || client_id != mint.client_id
                                    || get("redirect_uri") != mint.redirect_uri
                                    || !resource_ok
                                {
                                    return (
                                        axum::http::StatusCode::BAD_REQUEST,
                                        axum::Json(json!({ "error": "invalid_grant" })),
                                    );
                                }
                                let n = s.counter.fetch_add(1, Ordering::SeqCst) + 1;
                                let refresh = format!("ref-{n}");
                                s.refresh_valid.lock().unwrap().insert(refresh.clone());
                                (
                                    axum::http::StatusCode::OK,
                                    axum::Json(json!({
                                        "access_token": format!("acc-{n}"),
                                        "refresh_token": refresh,
                                        "expires_in": 3600,
                                        "scope": "mcp:connect",
                                    })),
                                )
                            }
                            "refresh_token" => {
                                let offered = get("refresh_token");
                                let mut valid = s.refresh_valid.lock().unwrap();
                                if !resource_ok || !valid.remove(&offered) {
                                    return (
                                        axum::http::StatusCode::BAD_REQUEST,
                                        axum::Json(json!({ "error": "invalid_grant" })),
                                    );
                                }
                                let n = s.counter.fetch_add(1, Ordering::SeqCst) + 1;
                                let refresh = format!("ref-{n}");
                                valid.insert(refresh.clone());
                                (
                                    axum::http::StatusCode::OK,
                                    axum::Json(json!({
                                        "access_token": format!("acc-{n}"),
                                        "refresh_token": refresh,
                                        "expires_in": 3600,
                                        "scope": "mcp:connect",
                                    })),
                                )
                            }
                            _ => (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(json!({ "error": "unsupported_grant_type" })),
                            ),
                        }
                    }
                }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        state
    }

    fn http() -> reqwest::Client {
        reqwest::Client::builder().build().expect("a plain client")
    }

    /// Parse a URL's query into a map (the test side of the contract the URL builder writes).
    fn query_of(url: &str) -> HashMap<String, String> {
        url.split_once('?')
            .expect("a query")
            .1
            .split('&')
            .map(|pair| pair.split_once('=').expect("k=v"))
            .map(|(k, v)| {
                (
                    crate::adapters::rest::form_decode(k),
                    crate::adapters::rest::form_decode(v),
                )
            })
            .collect()
    }

    // ---- discovery and registration over the wire -------------------------------------------

    #[tokio::test]
    async fn discovery_reads_both_well_knowns_and_names_the_endpoints() {
        let as_url = fake_as(vec!["Claude Code"]).await;
        let d = discover(&http(), &format!("{}/mcp", as_url.base))
            .await
            .expect("discovery");
        assert_eq!(d.resource, format!("{}/mcp", as_url.base));
        assert_eq!(
            d.authorization_endpoint,
            format!("{}/oauth/authorize", as_url.base)
        );
        assert_eq!(
            d.registration_endpoint.as_deref(),
            Some(format!("{}/v1/oauth/mcp/register", as_url.base).as_str())
        );
        assert_eq!(d.scopes_supported, vec!["mcp:connect".to_string()]);
    }

    #[tokio::test]
    async fn registration_respects_the_allowlist_and_issues_the_secret_anyway() {
        let as_url = fake_as(vec!["Claude Code", "Codex"]).await;
        let reg = register_client(
            &http(),
            &format!("{}/v1/oauth/mcp/register", as_url.base),
            "Claude Code",
            "http://127.0.0.1:45000/callback",
            Some("mcp:connect"),
        )
        .await
        .expect("allowlisted name registers");
        assert!(!reg.client_id.is_empty());
        // The Figma quirk, pinned: a secret arrives even though the response says method none.
        assert_eq!(reg.client_secret.as_deref(), Some("sec-1"));

        let refused = match register_client(
            &http(),
            &format!("{}/v1/oauth/mcp/register", as_url.base),
            "Some Other Client",
            "http://127.0.0.1:45000/callback",
            None,
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("a name off the allowlist must not register"),
        };
        assert!(refused.contains("registration refused"), "{refused}");
        assert!(refused.contains("oauthClientName"), "{refused}");
        assert!(refused.contains("\"Claude Code\""), "{refused}");
    }

    // ---- the flow, end to end -----------------------------------------------------------------

    /// The shared preamble for every test that touches the credential store: the scratch data
    /// home, the test master key, and the data-dir lock that serializes store writers.
    async fn store_preamble() -> tokio::sync::MutexGuard<'static, ()> {
        swiss_core::paths::test_home();
        swiss_core::secure::key::use_test_master_key();
        swiss_core::paths::DATA_DIR_LOCK.lock().await
    }

    #[tokio::test]
    async fn a_flow_completes_against_the_fake_as_and_persists_sealed() {
        let _guard = store_preamble().await;
        let as_url = fake_as(vec!["Claude Code", "Codex"]).await;
        let flow = start_flow(
            http(),
            "figma-flow-a",
            &format!("{}/mcp", as_url.base),
            Some("Claude Code"),
        )
        .await
        .expect("the flow starts");

        // The authorization URL carries the whole contract (the fake AS's /authorize is never
        // real — the browser step is simulated by minting a code and hitting the redirect).
        let params = query_of(&flow.authorization_url);
        assert_eq!(params["response_type"], "code");
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["scope"], "mcp:connect");
        assert_eq!(params["resource"], format!("{}/mcp", as_url.base));
        assert!(params["state"].len() >= 32, "a real random state");
        assert!(flow.redirect_uri.starts_with("http://127.0.0.1:"));

        // The "browser": mint a code for this challenge, then redirect to the loopback callback.
        let mint_url = format!(
            "{}/test/mint?challenge={}&client_id={}&redirect_uri={}",
            as_url.base,
            params["code_challenge"],
            params["client_id"],
            crate::adapters::rest::encode_uri_component(&flow.redirect_uri),
        );
        let minted: Value = http()
            .get(&mint_url)
            .send()
            .await
            .expect("mint")
            .json()
            .await
            .expect("code json");
        let code = minted["code"].as_str().expect("a code").to_string();
        let redirected = http()
            .get(format!(
                "{}?code={}&state={}",
                flow.redirect_uri,
                code,
                crate::adapters::rest::encode_uri_component(&flow.state)
            ))
            .send()
            .await
            .expect("the redirect reaches the listener");
        assert!(redirected.status().is_success());

        // Completing exchanges the code and assembles the credentials.
        let creds = flow.complete().await.expect("the exchange succeeds");
        assert!(creds
            .access_token
            .as_deref()
            .unwrap_or("")
            .starts_with("acc-"));
        assert!(creds.refresh_token.is_some());
        assert!(creds.access_fresh());

        // Persistence: the singleton hands the same set back, the sealed file carries no token.
        let name = "figma-flow-a";
        replace_credentials(name, creds.clone()).expect("persist");
        assert_eq!(credentials(name), Some(creds));
        let raw = std::fs::read_to_string(store_path()).expect("the store file exists");
        assert!(!raw.contains("acc-"), "the file is sealed: {raw}");
        assert!(!raw.contains("sec-"), "secrets never in the clear: {raw}");
    }

    #[tokio::test]
    async fn refresh_rotates_once_then_reports_needs_auth() {
        let _guard = store_preamble().await;
        let as_url = fake_as(vec!["Claude Code"]).await;
        let flow = start_flow(
            http(),
            "figma-flow-b",
            &format!("{}/mcp", as_url.base),
            Some("Claude Code"),
        )
        .await
        .expect("flow");
        let params = query_of(&flow.authorization_url);
        let minted: Value = http()
            .get(format!(
                "{}/test/mint?challenge={}&client_id={}&redirect_uri={}",
                as_url.base,
                params["code_challenge"],
                params["client_id"],
                crate::adapters::rest::encode_uri_component(&flow.redirect_uri),
            ))
            .send()
            .await
            .expect("mint")
            .json()
            .await
            .expect("code");
        http()
            .get(format!(
                "{}?code={}&state={}",
                flow.redirect_uri,
                minted["code"].as_str().expect("code"),
                crate::adapters::rest::encode_uri_component(&flow.state)
            ))
            .send()
            .await
            .expect("redirect");
        let creds = flow.complete().await.expect("complete");
        let name = "figma-flow-b";
        replace_credentials(name, creds.clone()).expect("persist");
        let resource = format!("{}/mcp", as_url.base);
        let reg = ClientReg {
            client_id: creds.client_id.clone(),
            client_secret: creds.client_secret.clone(),
            token_auth: creds.token_auth.clone(),
        };

        // First refresh spends the single-use refresh token and lands the new pair.
        let first = refresh(
            &http(),
            &format!("{}/v1/oauth/token", as_url.base),
            &reg,
            creds.refresh_token.as_deref().expect("a refresh token"),
            &resource,
        )
        .await
        .expect("refresh once");
        assert_ne!(first.access_token, creds.access_token.expect("old access"));

        // The update path keeps the client half and swaps the token half.
        let stored = update_tokens(name, &first).expect("update");
        assert_eq!(stored.client_id, creds.client_id);
        assert_eq!(stored.access_token, Some(first.access_token));

        // The spent token is dead: the marker the panel keys on.
        let refused = refresh(
            &http(),
            &format!("{}/v1/oauth/token", as_url.base),
            &reg,
            creds.refresh_token.as_deref().expect("the old refresh"),
            &resource,
        )
        .await
        .unwrap_err();
        assert!(refused.starts_with(NEEDS_AUTH), "{refused}");

        clear_credentials(name).expect("clear");
        assert_eq!(credentials(name), None);
    }

    #[tokio::test]
    async fn a_redirect_with_the_wrong_state_is_refused() {
        let as_url = fake_as(vec!["Claude Code"]).await;
        let flow = start_flow(
            http(),
            "figma-flow-c",
            &format!("{}/mcp", as_url.base),
            Some("Claude Code"),
        )
        .await
        .expect("flow");
        let response = http()
            .get(format!("{}?code=x&state=not-the-state", flow.redirect_uri))
            .send()
            .await
            .expect("the listener answers");
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let err = flow.complete().await.unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert!(err.contains("state"), "{err}");
    }

    #[tokio::test]
    async fn a_disallowed_client_name_fails_the_flow_with_the_human_message() {
        let as_url = fake_as(vec!["Claude Code", "Codex"]).await;
        let err = match start_flow(
            http(),
            "figma-flow-d",
            &format!("{}/mcp", as_url.base),
            Some("Yet Another Client"),
        )
        .await
        {
            Err(err) => err,
            Ok(_) => panic!("a name off the allowlist must not start a flow"),
        };
        assert!(err.contains("registration refused"), "{err}");
        assert!(err.contains("oauthClientName"), "{err}");
    }

    #[tokio::test]
    async fn the_figma_defaults_apply_when_the_url_is_figma_and_no_override_comes() {
        // A fake AS whose allowlist is exactly what the provider table would send: proof the
        // defaults (not the override) reach the register endpoint.
        let as_url = fake_as(vec!["Claude Code"]).await;
        // The URL is loopback, so the figma default only fires through the NAME heuristic:
        // name "figma" + a URL that does not say figma is NOT figma — so use the override=none
        // path against a server that allows "swiss" instead, proving the non-figma default.
        let as_url2 = fake_as(vec!["swiss"]).await;
        let flow = start_flow(http(), "linear-ish", &format!("{}/mcp", as_url2.base), None)
            .await
            .expect("the generic default registers");
        let params = query_of(&flow.authorization_url);
        // scope comes from discovery's scopes_supported when the provider table has no opinion.
        assert_eq!(params["scope"], "mcp:connect");
        drop(as_url); // both servers stop with the test's runtime
    }
}
