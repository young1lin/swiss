//! Composition glue between the MCP registry and the tunnel subsystem — the one place that
//! knows both types.
//!
//! The tunnel subsystem addresses the MCP world ONLY through its own read-only traits
//! (`McpView` for the manager's stop guard, `McpDisplay` for the API's rows and suggestions),
//! which is what keeps it independent of the registry's type. The registry-backed
//! implementations of those traits cannot live in either module without creating the very
//! dependency that split exists to avoid, so they live here, in the composition layer, next
//! to the wiring that constructs them. Pure def analysis ("does this def point at loopback?")
//! stays with the tunnel side (`tunnel::mcpmatch::mcp_loopback_port`).

use std::sync::Arc;

use swiss_mcp::registry::{EntryInner, Health, Lifecycle, Registry};
use swiss_tunnels::tunnel::api::McpDisplay;
use swiss_tunnels::tunnel::manager::McpView;
use swiss_tunnels::tunnel::mcpmatch::mcp_loopback_port;

/// The manager's read-only registry window (has/stateOf/isStarted/startedAt). Nothing here can
/// start or stop an MCP.
pub fn registry_view(registry: Arc<Registry>) -> Box<dyn McpView> {
    Box::new(RegistryView(registry))
}

/// The API's read-only registry window: every MCP name, and the loopback-port suggestion the
/// rule editor pre-checks.
pub fn registry_display(registry: Arc<Registry>) -> Arc<dyn McpDisplay> {
    Arc::new(RegistryDisplay(registry))
}

struct RegistryView(Arc<Registry>);

impl RegistryView {
    fn entry(&self, name: &str) -> Option<Arc<EntryInner>> {
        self.0.get(name)
    }
}

impl McpView for RegistryView {
    fn has(&self, name: &str) -> bool {
        self.0.has(name)
    }

    fn state_of(&self, name: &str) -> Option<String> {
        let entry = self.entry(name)?;
        let d = entry.data.read().ok()?;
        Some(match d.lifecycle {
            Lifecycle::Started => match d.status {
                Health::Up => "up".into(),
                Health::Down => "down".into(),
                Health::Unknown => "unknown".into(),
            },
            other => other.as_str().to_string(),
        })
    }

    fn is_started(&self, name: &str) -> bool {
        self.entry(name)
            .and_then(|e| {
                e.data
                    .read()
                    .ok()
                    .map(|d| d.lifecycle == Lifecycle::Started)
            })
            .unwrap_or(false)
    }

    fn started_at(&self, name: &str) -> Option<String> {
        self.entry(name)
            .and_then(|e| e.data.read().ok()?.started_at.clone())
    }
}

struct RegistryDisplay(Arc<Registry>);

impl McpDisplay for RegistryDisplay {
    fn names(&self) -> Vec<String> {
        self.0.names()
    }

    fn suggest(&self, local_port: u16) -> Vec<String> {
        self.0
            .all()
            .iter()
            .filter_map(|entry| {
                let def = entry.data.read().ok().map(|d| d.def.clone())?;
                if mcp_loopback_port(&def) == Some(local_port) {
                    Some(entry.name())
                } else {
                    None
                }
            })
            .collect()
    }
}
