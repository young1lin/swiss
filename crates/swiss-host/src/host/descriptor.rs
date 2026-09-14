//! Plugin declarations: descriptors and page contributions. Declarative data only — no
//! behavior, so descriptors can be compared, logged and served as inventory verbatim.

use serde_json::{json, Value};

/// One page a plugin contributes to the shell (docs/09 §6). The shell renders the inventory's
/// page list; it never learns a plugin by name. `path` is the in-panel hash route, `entry` the
/// ES module the shell dynamic-imports on first click.
#[derive(Clone, Debug)]
pub struct PageDescriptor {
    /// Stable page id — the shell's routing key ("mcps", "traffic", ...).
    pub id: String,
    /// The plugin that contributes the page.
    pub plugin_id: String,
    /// Nav label.
    pub label: String,
    /// Nav sort key. Registration order breaks ties; the inventory lists pages in
    /// registration order regardless of this value.
    pub order: i64,
    /// Hash route inside the panel ("#mcps").
    pub path: String,
    /// Module the shell lazy-loads ("/admin/js/views/mcps.js").
    pub entry: String,
    /// Whether the page appears in the sidebar (vs. a secondary view).
    pub sidebar: bool,
    /// How the shell frames the PAGE BODY (docs/13 D5, as revised): "resource" (a
    /// master-detail page that owns the panel's sidebar), "page" (an ordinary content
    /// body) or "workspace" (a full-bleed, dense body - the terminal, the data
    /// explorer). Workspace changes body framing ONLY: the shell always draws its own
    /// chrome (rail and context bar) around it. Additive wire metadata: an older panel
    /// ignores it, and a newer panel reading an older gateway falls back to deriving it
    /// from sidebar (true = resource, false = page), so sidebar stays the compatibility
    /// contract.
    pub layout: &'static str,
}

impl PageDescriptor {
    /// The inventory wire shape — one JSON object, camelCase, absent-not-null.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "pluginId": self.plugin_id,
            "label": self.label,
            "order": self.order,
            "path": self.path,
            "entry": self.entry,
            "sidebar": self.sidebar,
            "layout": self.layout,
        })
    }
}

/// The static declaration of one plugin (docs/09 §4, contribution 1+5). Built by its factory
/// at registration and frozen there: descriptors describe what IS linked into this binary.
#[derive(Clone, Debug)]
pub struct PluginDescriptor {
    /// Stable instance id, also the config row key ("mcp", "tunnels", ...).
    pub id: String,
    /// Which built-in kind implements it. Same as `id` for every built-in of this build;
    /// distinct the day one kind serves several configured instances.
    pub kind: String,
    /// Human label for logs and future UI.
    pub label: String,
    pub version: String,
    pub config_schema_version: u32,
    /// Schema-lite metadata served by GET /api/plugins/{id}/config so a form can be built
    /// without the backend inventing a runtime for it (docs/09 §5).
    pub config_schema: Value,
    /// Pages contributed, in contribution order.
    pub pages: Vec<PageDescriptor>,
    /// API path prefixes this plugin OWNS. The host boundary answers 503 on every one of them
    /// while the plugin is not serving, and lets them through the moment it is. Client paths
    /// that cannot be a static prefix (the one-segment MCP catch-all) are guarded by their
    /// handler instead — see `crate::host::api::client_path_guard`.
    pub routes: Vec<String>,
    /// Whether a config PUT reconciles by restarting the running instance. `false` means the
    /// config is read at start only: a PUT is validated, persisted and noted (the revision
    /// moves) but the instance is not bounced — honest for plugins whose runtime reads
    /// nothing from the row today.
    pub restart_on_config_change: bool,
    /// CAPABILITY names (not plugin ids) this plugin needs at run time to be fully
    /// functional (docs/12 W3). The inventory surfaces them with a met/unmet verdict so
    /// the panel can say "Data depends on connection-catalog, currently no provider" —
    /// a dependency stated as data, not discovered by reading another plugin's source.
    pub requires: Vec<String>,
}

/// The lifecycle of one plugin (docs/09 §4). `Idle` is reserved for plugins that are enabled
/// but keep no eager instance until first use; the P1 built-ins all run straight to `Active`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginState {
    /// Config row says off. No instance exists.
    Disabled,
    /// Enabled, but nothing eager is running yet.
    Idle,
    /// A start is underway (single-flighted — one at a time per plugin).
    Starting,
    /// Serving. Routes pass the boundary.
    Active,
    /// A stop is underway; the instance is already out of the route slot.
    Stopping,
    /// Last start failed; `lastError` on the inventory row says why. A retry is one
    /// POST /api/plugins/{id}/enable away.
    Failed,
}

impl PluginState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginState::Disabled => "disabled",
            PluginState::Idle => "idle",
            PluginState::Starting => "starting",
            PluginState::Active => "active",
            PluginState::Stopping => "stopping",
            PluginState::Failed => "failed",
        }
    }
}
