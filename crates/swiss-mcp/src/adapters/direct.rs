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

//! What the three in-process DB adapters (mysql / pg / redis) have in common — port of
//! `adapters/direct.ts`: toggle seeding, one shared lazily-opened connection, and a fresh
//! [`ToolServer`] per request over it.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use swiss_host::config::ServerDef;

use super::tool_server::{Engine, ToolServer};
use super::{rmcp_endpoint, Adapter, McpEndpoint, ResourceToggle, ToolToggle};

/// A boxed, Send future — the crate deliberately carries no `futures` dependency, so the lazy
/// cell hand-rolls the one alias it needs.
pub type BoxFut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

/// A flag that may arrive as a JSON boolean or as a string: the panel posts form values, so
/// `"allowEval": "true"` and `"allowEval": true` both have to mean the same thing.
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
    log: std::sync::Arc<crate::calls::CallLog>,
}

impl<E: Engine> DirectAdapter<E> {
    /// `def` must already be a `resolve_def_checked()` clone (env refs expanded), exactly as
    /// `make_adapter` hands it over.
    pub fn new(
        def: &ServerDef,
        name: &str,
        engine: E,
        log: std::sync::Arc<crate::calls::CallLog>,
    ) -> Self {
        // Seed the toggles from the def at construction (boot + tests pass them on the def). The
        // live path mutates the toggle cells directly — `def` here is a resolve_def_checked() clone,
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
            log,
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
        let log = self.log.clone();
        Ok(rmcp_endpoint(move |source| ToolServer {
            engine: engine.clone(),
            disabled_tools: disabled.read().map(|g| g.clone()).unwrap_or_default(),
            resources_on: resources_on.load(std::sync::atomic::Ordering::SeqCst),
            source,
            log: log.clone(),
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

    fn browser(&self) -> Option<swiss_host::dbbrowser::BrowserFlavor> {
        self.engine.browser()
    }

    fn tool_toggle(&self) -> Option<ToolToggle> {
        Some(self.disabled.clone())
    }

    fn resource_toggle(&self) -> Option<ResourceToggle> {
        Some(self.resources_on.clone())
    }
}

#[cfg(test)]
mod tests {
    //! Ported from the `Lazy.dispose`, "resources toggle" and "tool toggle (disabledTools)"
    //! blocks of the Node build's `test/direct-adapters.test.ts` — the parts that belong to this
    //! shell rather than to one engine. The engine-specific halves live in each engine's module.
    use super::*;
    use crate::adapters::resources::{
        ResourceBody, ResourceEntry, ResourceFault, ResourcePage, ResourceProvider,
        ResourceTemplate,
    };
    use crate::adapters::tool_server::{ServerMeta, ToolDef};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn def(v: Value) -> ServerDef {
        match v {
            Value::Object(o) => ServerDef(o),
            other => panic!("a server def is an object, got {other}"),
        }
    }

    // ---- def_bool ---------------------------------------------------------------------

    #[test]
    fn def_bool_accepts_both_the_json_and_the_form_shapes() {
        // The panel posts form values, so "true" and true have to mean the same thing.
        for on in [json!(true), json!(1), json!("true"), json!("1")] {
            assert!(
                def_bool(&def(json!({ "type": "redis", "readonly": on })), "readonly"),
                "{on} must read as on"
            );
        }
        for off in [
            json!(false),
            json!(0),
            json!("false"),
            json!("yes"),
            json!(""),
            json!(null),
            json!(["true"]),
        ] {
            assert!(
                !def_bool(
                    &def(json!({ "type": "redis", "readonly": off })),
                    "readonly"
                ),
                "{off} must not read as on"
            );
        }
        // An absent key is off, not an error.
        assert!(!def_bool(&def(json!({ "type": "redis" })), "readonly"));
    }

    // ---- Lazy -------------------------------------------------------------------------

    /// A connection stand-in that counts how often it was opened and closed.
    struct Opens {
        count: Arc<AtomicUsize>,
        fail_first: Arc<AtomicUsize>,
        delay_ms: u64,
    }

    impl Opens {
        fn lazy(&self) -> Lazy<usize> {
            let count = self.count.clone();
            let fail_first = self.fail_first.clone();
            let delay_ms = self.delay_ms;
            Lazy::new(move || {
                let count = count.clone();
                let fail_first = fail_first.clone();
                Box::pin(async move {
                    if delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                    if fail_first.load(Ordering::SeqCst) > 0 {
                        fail_first.fetch_sub(1, Ordering::SeqCst);
                        return Err("no route to host".to_string());
                    }
                    Ok(n)
                })
            })
        }
    }

    fn opens(delay_ms: u64, fail_first: usize) -> Opens {
        Opens {
            count: Arc::new(AtomicUsize::new(0)),
            fail_first: Arc::new(AtomicUsize::new(fail_first)),
            delay_ms,
        }
    }

    #[tokio::test]
    async fn lazy_opens_once_and_hands_the_same_connection_back() {
        let o = opens(0, 0);
        let lazy = o.lazy();
        let first = lazy.get().await.expect("first open");
        let second = lazy.get().await.expect("second get");
        assert_eq!(o.count.load(Ordering::SeqCst), 1, "opened more than once");
        assert!(
            Arc::ptr_eq(&first, &second),
            "two handles for one connection"
        );
    }

    #[tokio::test]
    async fn two_concurrent_first_requests_share_one_connection() {
        // The single-flight IS the mutex: the first get holds it across the open, so a race does
        // not leave a second connection open with nothing referencing it.
        let o = opens(20, 0);
        let lazy = Arc::new(o.lazy());
        let a = tokio::spawn({
            let lazy = lazy.clone();
            async move { lazy.get().await }
        });
        let b = tokio::spawn({
            let lazy = lazy.clone();
            async move { lazy.get().await }
        });
        let (a, b) = (a.await.unwrap().unwrap(), b.await.unwrap().unwrap());
        assert_eq!(o.count.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[tokio::test]
    async fn a_failed_open_leaves_the_cell_empty_so_the_next_request_tries_again() {
        // Caching the rejection would hand every later request the same stale failure, long after
        // the database came back.
        let o = opens(0, 1);
        let lazy = o.lazy();
        assert_eq!(lazy.get().await.unwrap_err(), "no route to host");
        assert_eq!(*lazy.get().await.expect("the retry connects"), 2);
        assert_eq!(o.count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn dispose_closes_an_already_open_connection_exactly_once() {
        let o = opens(0, 0);
        let lazy = o.lazy();
        lazy.get().await.unwrap();

        let closed = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let record = |closed: Arc<std::sync::Mutex<Vec<usize>>>| {
            move |v: Arc<usize>| -> BoxFut<()> {
                Box::pin(async move {
                    closed.lock().unwrap().push(*v);
                })
            }
        };
        lazy.dispose(record(closed.clone())).await;
        // A second stop has nothing left to close.
        lazy.dispose(record(closed.clone())).await;
        assert_eq!(*closed.lock().unwrap(), vec![1]);
    }

    #[tokio::test]
    async fn dispose_has_nothing_to_close_when_the_connection_never_came_up() {
        let o = opens(0, 1);
        let lazy = o.lazy();
        assert!(lazy.get().await.is_err());
        let closed = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let sink = closed.clone();
        lazy.dispose(move |v: Arc<usize>| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(*v);
            })
        })
        .await;
        assert!(closed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dispose_closes_a_connection_that_finished_opening_after_it_was_called() {
        // Stopping an MCP while its first connection is still being established used to drop the
        // in-flight handle: the driver connected into a cell nothing would ever close, and the
        // pool it opened stayed open for the life of the process.
        let o = opens(30, 0);
        let lazy = Arc::new(o.lazy());
        let opening = tokio::spawn({
            let lazy = lazy.clone();
            async move { lazy.get().await.map(|v| *v) }
        });
        tokio::task::yield_now().await; // let the open actually start

        let closed = Arc::new(std::sync::Mutex::new(Vec::<usize>::new()));
        let sink = closed.clone();
        lazy.dispose(move |v: Arc<usize>| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(*v);
            })
        })
        .await;

        assert_eq!(opening.await.unwrap(), Ok(1), "the open still completed");
        assert_eq!(*closed.lock().unwrap(), vec![1], "and was then closed");
    }

    #[tokio::test]
    async fn a_get_after_dispose_opens_a_fresh_connection() {
        let o = opens(0, 0);
        let lazy = o.lazy();
        lazy.get().await.unwrap();
        lazy.dispose(|_| Box::pin(async {})).await;
        assert_eq!(*lazy.get().await.unwrap(), 2);
        assert_eq!(o.count.load(Ordering::SeqCst), 2);
    }

    // ---- DirectAdapter ----------------------------------------------------------------

    struct FakeResources;

    #[async_trait]
    impl ResourceProvider for FakeResources {
        async fn list(&self, _cursor: Option<&str>) -> Result<ResourcePage, ResourceFault> {
            Ok(ResourcePage {
                resources: vec![ResourceEntry {
                    uri: "fake://db/users".into(),
                    name: "users".into(),
                    description: None,
                    mime_type: None,
                }],
                next_cursor: None,
            })
        }
        fn templates(&self) -> Vec<ResourceTemplate> {
            Vec::new()
        }
        async fn read(&self, _uri: &str) -> Result<Vec<ResourceBody>, ResourceFault> {
            Err(ResourceFault("not in this test".into()))
        }
    }

    /// An engine that counts what the shell delegates to it.
    struct Fake {
        name: std::sync::RwLock<String>,
        pings: AtomicUsize,
        closes: AtomicUsize,
        ping_result: Option<Result<(), String>>,
        with_resources: bool,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                name: std::sync::RwLock::new("unnamed".into()),
                pings: AtomicUsize::new(0),
                closes: AtomicUsize::new(0),
                ping_result: Some(Ok(())),
                with_resources: true,
            })
        }
    }

    /// The engine handed to `DirectAdapter::new` is moved in, so tests keep their own handle and
    /// pass a thin forwarder — the shell never notices the indirection.
    struct Handle(Arc<Fake>);

    #[async_trait]
    impl Engine for Handle {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn tools(&self) -> Vec<ToolDef> {
            ["fake_read", "fake_write"]
                .iter()
                .map(|n| ToolDef {
                    name: (*n).into(),
                    description: format!("the {n} tool"),
                    input_schema: json!({ "type": "object" }),
                })
                .collect()
        }
        async fn call(&self, tool: &str, _args: &Value) -> Result<Value, String> {
            Ok(json!({ "called": tool }))
        }
        fn meta(&self) -> ServerMeta {
            ServerMeta {
                name: self.0.name.read().ok().map(|n| n.clone()),
                description: None,
                target: None,
                limits: None,
            }
        }
        fn resources(&self) -> Option<Arc<dyn ResourceProvider>> {
            self.0
                .with_resources
                .then(|| Arc::new(FakeResources) as Arc<dyn ResourceProvider>)
        }
        async fn ping(&self) -> Option<Result<(), String>> {
            self.0.pings.fetch_add(1, Ordering::SeqCst);
            self.0.ping_result.clone()
        }
        async fn close(&self) {
            self.0.closes.fetch_add(1, Ordering::SeqCst);
        }
        fn rename(&self, name: &str) {
            if let Ok(mut current) = self.0.name.write() {
                *current = name.to_string();
            }
        }
    }

    fn adapter(d: Value) -> (DirectAdapter<Handle>, Arc<Fake>) {
        let engine = Fake::new();
        (
            DirectAdapter::new(
                &def(d),
                "fake-mcp",
                Handle(engine.clone()),
                crate::calls::test_log(),
            ),
            engine,
        )
    }

    fn disabled_of(a: &DirectAdapter<Handle>) -> Vec<String> {
        let cell = a
            .tool_toggle()
            .expect("a direct adapter always has a tool toggle");
        let mut names: Vec<String> = cell.read().unwrap().iter().cloned().collect();
        names.sort();
        names
    }

    /// Tool names off a live tools/list, through the probe half of the endpoint — no transport
    /// and no connection needed, which is the whole point of building lazily.
    async fn tool_names(endpoint: &McpEndpoint) -> Vec<String> {
        let (items, _) = endpoint
            .probe
            .list("tools", None)
            .await
            .expect("tools/list");
        items
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    async fn resource_uris(endpoint: &McpEndpoint) -> Vec<String> {
        let (items, _) = endpoint
            .probe
            .list("resources", None)
            .await
            .expect("resources/list");
        items
            .iter()
            .filter_map(|r| r.get("uri").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    #[test]
    fn seeds_the_disabled_tool_set_from_the_def() {
        let (a, _) = adapter(json!({ "type": "fake", "disabledTools": ["fake_write"] }));
        assert_eq!(disabled_of(&a), vec!["fake_write"]);
    }

    #[test]
    fn treats_a_non_array_disabled_tools_as_empty() {
        // Defensive against bad config: a string where a list belongs disables nothing rather
        // than disabling a tool named after the whole string.
        let (a, _) = adapter(json!({ "type": "fake", "disabledTools": "fake_write" }));
        assert!(disabled_of(&a).is_empty());
    }

    #[test]
    fn drops_disabled_tools_entries_that_are_not_strings() {
        let (a, _) = adapter(json!({ "type": "fake", "disabledTools": ["fake_write", 5, null] }));
        assert_eq!(disabled_of(&a), vec!["fake_write"]);
    }

    #[test]
    fn resources_are_on_until_the_def_says_otherwise() {
        let on = |d: Value| {
            adapter(d)
                .0
                .resource_toggle()
                .expect("a direct adapter always has a resource toggle")
                .load(Ordering::SeqCst)
        };
        assert!(on(json!({ "type": "fake" })));
        assert!(on(json!({ "type": "fake", "exposeResources": true })));
        // Both the JSON and the form shape of "off" — the panel posts strings.
        assert!(!on(json!({ "type": "fake", "exposeResources": false })));
        assert!(!on(json!({ "type": "fake", "exposeResources": "false" })));
        // Anything else is not "off": an unreadable value must not silently hide the resources.
        assert!(on(json!({ "type": "fake", "exposeResources": "nope" })));
    }

    #[tokio::test]
    async fn kind_ping_and_close_delegate_to_the_engine() {
        let (a, engine) = adapter(json!({ "type": "fake" }));
        assert_eq!(a.kind(), "fake");
        assert_eq!(a.ping().await, Some(Ok(())));
        assert_eq!(engine.pings.load(Ordering::SeqCst), 1);
        a.close().await;
        assert_eq!(engine.closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_rename_reaches_the_engine_so_calls_keep_their_mcp_name() {
        let (a, engine) = adapter(json!({ "type": "fake" }));
        a.rename("renamed");
        assert_eq!(*engine.name.read().unwrap(), "renamed");
        assert_eq!(a.name.read().unwrap().as_str(), "renamed");
    }

    #[tokio::test]
    async fn hides_a_disabled_tool_from_tools_list() {
        let (a, _) = adapter(json!({ "type": "fake", "disabledTools": ["fake_write"] }));
        let endpoint = a.build().await.expect("build");
        assert_eq!(tool_names(&endpoint).await, vec!["fake_read"]);
    }

    #[tokio::test]
    async fn ignores_a_disabled_tools_entry_that_names_no_real_tool() {
        let (a, _) = adapter(json!({ "type": "fake", "disabledTools": ["no_such_tool"] }));
        let endpoint = a.build().await.expect("build");
        assert_eq!(tool_names(&endpoint).await.len(), 2);
    }

    #[tokio::test]
    async fn a_tool_toggle_is_live_on_the_very_next_request_with_no_rebuild() {
        // The toggles are read at FACTORY time, not at build time, so the panel's toggle takes
        // effect without restarting the MCP — that is the whole reason they are shared cells.
        let (a, _) = adapter(json!({ "type": "fake" }));
        let endpoint = a.build().await.expect("build");
        assert_eq!(tool_names(&endpoint).await.len(), 2);

        let toggle = a.tool_toggle().unwrap();
        toggle.write().unwrap().insert("fake_write".into());
        assert_eq!(tool_names(&endpoint).await, vec!["fake_read"]);

        toggle.write().unwrap().clear();
        assert_eq!(tool_names(&endpoint).await.len(), 2);
    }

    #[tokio::test]
    async fn resources_off_returns_an_empty_list_and_is_live_too() {
        let (a, _) = adapter(json!({ "type": "fake" }));
        let endpoint = a.build().await.expect("build");
        assert_eq!(resource_uris(&endpoint).await, vec!["fake://db/users"]);

        a.resource_toggle().unwrap().store(false, Ordering::SeqCst);
        assert!(resource_uris(&endpoint).await.is_empty());

        a.resource_toggle().unwrap().store(true, Ordering::SeqCst);
        assert_eq!(resource_uris(&endpoint).await.len(), 1);
    }

    #[tokio::test]
    async fn an_engine_with_nothing_to_expose_lists_no_resources_even_when_the_toggle_is_on() {
        let engine = Arc::new(Fake {
            name: std::sync::RwLock::new("fake-mcp".into()),
            pings: AtomicUsize::new(0),
            closes: AtomicUsize::new(0),
            ping_result: None,
            with_resources: false,
        });
        let a = DirectAdapter::new(
            &def(json!({ "type": "fake" })),
            "fake-mcp",
            Handle(engine),
            crate::calls::test_log(),
        );
        assert!(a.resource_toggle().unwrap().load(Ordering::SeqCst));
        let endpoint = a.build().await.expect("build");
        assert!(resource_uris(&endpoint).await.is_empty());
        // ...and an engine with no probe of its own reports no health, rather than a fake pass.
        assert_eq!(a.ping().await, None);
    }
}
