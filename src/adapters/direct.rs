//! What the three in-process DB adapters (mysql / pg / redis) have in common — port of
//! `adapters/direct.ts`: toggle seeding, one shared lazily-opened connection, and a fresh
//! [`ToolServer`] per request over it.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::config::ServerDef;

use super::tool_server::{Engine, ToolServer};
use super::{rmcp_endpoint, Adapter, McpEndpoint, ResourceToggle, ToolToggle};

/// A boxed, Send future — the crate deliberately carries no `futures` dependency, so the lazy
/// cell hand-rolls the one alias it needs.
pub type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

/// A flag that may arrive as a JSON boolean or as a string: the panel posts form values, so
/// `"readonly": "true"` and `"readonly": true` both have to mean the same thing.
pub fn def_bool(def: &ServerDef, key: &str) -> bool {
    match def.get(key) {
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => s == "true" || s == "1",
        _ => false,
    }
}

/// A connection opened on first use and shared from then on — port of `Lazy<T>`.
///
/// The single-flight is the tokio mutex itself: the first `get()` holds it across the open, so
/// two concurrent first requests share one connection rather than racing two into existence and
/// leaking one. A failed attempt leaves the cell empty (the guard drops on the error path), so
/// the next request tries again instead of being handed the same rejection forever.
pub struct Lazy<T> {
    cell: tokio::sync::Mutex<Option<Arc<T>>>,
    open: Box<dyn Fn() -> BoxFut<Result<T, String>> + Send + Sync>,
}

impl<T: Send + Sync + 'static> Lazy<T> {
    pub fn new<F>(open: F) -> Self
    where
        F: Fn() -> BoxFut<Result<T, String>> + Send + Sync + 'static,
    {
        Self {
            cell: tokio::sync::Mutex::new(None),
            open: Box::new(open),
        }
    }

    pub async fn get(&self) -> Result<Arc<T>, String> {
        let mut guard = self.cell.lock().await;
        if let Some(value) = &*guard {
            return Ok(value.clone());
        }
        let opened = Arc::new((self.open)().await?);
        *guard = Some(opened.clone());
        Ok(opened)
    }

    /// Forget the connection and shut it down, waiting for one still being opened.
    ///
    /// Taking the cell while holding the mutex means a `get()` that queued behind this call
    /// re-opens only AFTER the close completes — the window `dispose` closed in the Node build:
    /// stop or delete an MCP while its first connection is still being established and the
    /// driver otherwise finished connecting into a cell nothing would ever close.
    pub async fn dispose(&self, close: impl FnOnce(Arc<T>) -> BoxFut<()>) {
        let mut guard = self.cell.lock().await;
        if let Some(value) = guard.take() {
            close(value).await;
        }
    }
}

/// The adapter shell every direct engine plugs into: toggle seeding from the def, rename
/// tracking, ping/close delegation, and a per-request [`ToolServer`] over the shared engine.
pub struct DirectAdapter<E: Engine> {
    engine: Arc<E>,
    /// The MCP's registry name, followed across renames — the call log is filed under the
    /// engine's view of this cell, read fresh at call time.
    name: Arc<std::sync::RwLock<String>>,
    disabled: ToolToggle,
    resources_on: ResourceToggle,
}

impl<E: Engine> DirectAdapter<E> {
    /// `def` must already be a `resolve_def()` clone (env refs expanded), exactly as
    /// `make_adapter` hands it over.
    pub fn new(def: &ServerDef, name: &str, engine: E) -> Self {
        // Seed the toggles from the def at construction (boot + tests pass them on the def). The
        // live path mutates the toggle cells directly — `def` here is a resolve_def() clone,
        // never the same object the registry holds, so writing to the def later would not reach
        // this adapter.
        let disabled: HashSet<String> = def
            .get("disabledTools")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let resources_on = match def.get("exposeResources") {
            Some(Value::Bool(false)) => false,
            Some(Value::String(s)) if s == "false" => false,
            _ => true,
        };
        Self {
            engine: Arc::new(engine),
            name: Arc::new(std::sync::RwLock::new(name.to_string())),
            disabled: Arc::new(std::sync::RwLock::new(disabled)),
            resources_on: Arc::new(std::sync::atomic::AtomicBool::new(resources_on)),
        }
    }
}

#[async_trait]
impl<E: Engine + 'static> Adapter for DirectAdapter<E> {
    fn kind(&self) -> &str {
        self.engine.kind()
    }

    async fn build(&self) -> Result<McpEndpoint, String> {
        // Drivers connect lazily (Lazy::get on the first tool call); ping() is what actually
        // validates reachability, so build() only wires the per-request factory. The toggles are
        // read at FACTORY time — the next request already reflects a toggle, with no restart.
        let engine: Arc<dyn Engine> = self.engine.clone();
        let disabled = self.disabled.clone();
        let resources_on = self.resources_on.clone();
        Ok(rmcp_endpoint(move |source| ToolServer {
            engine: engine.clone(),
            disabled_tools: disabled.read().map(|g| g.clone()).unwrap_or_default(),
            resources_on: resources_on.load(std::sync::atomic::Ordering::SeqCst),
            source,
        }))
    }

    async fn ping(&self) -> Option<Result<(), String>> {
        self.engine.ping().await
    }

    async fn close(&self) {
        self.engine.close().await;
    }

    fn rename(&self, name: &str) {
        if let Ok(mut current) = self.name.write() {
            *current = name.to_string();
        }
        self.engine.rename(name);
    }

    fn browser(&self) -> Option<crate::dbbrowser_api::BrowserFlavor> {
        self.engine.browser()
    }

    fn tool_toggle(&self) -> Option<ToolToggle> {
        Some(self.disabled.clone())
    }

    fn resource_toggle(&self) -> Option<ResourceToggle> {
        Some(self.resources_on.clone())
    }
}
