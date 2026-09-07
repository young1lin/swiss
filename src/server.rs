//! The gateway's boot sequence — the port of `index.ts`, shared by `lmg serve` (foreground) and
//! the detached daemon: PATH repair, proc-PID reaping, tunnel boot (with the forward-port
//! first-run import), MCP registration, and the shutdown order (tunnels close before the
//! registry, so every forwarded local port is released while MCP connections still drain).

use std::sync::Arc;

use crate::adapters::make_adapter;
use crate::app::{build_app, AppContext};
use crate::bootstrap::ensure_first_run;
use crate::config::{config_path, load_config};
use crate::log;
use crate::managed::ManagedStore;
use crate::registry::{is_lazy, Registry, Source};
use crate::secure::envstore::{env_store_path, inject_env_store};
use crate::token::TokenManager;
use serde_json::json;

pub async fn run_gateway() -> Result<(), String> {
    // A detached `lmg start` (and Cursor's agent shell) often inherit a PATH that is missing
    // user-level bins — uv/uvx live in ~/.local/bin. Put them back before any proc MCP spawns.
    // Safe to set here: boot is single-threaded on the current_thread runtime, before any task
    // that could read the environment concurrently has been spawned.
    let login_path = crate::pathenv::login_path_from_env();
    unsafe { std::env::set_var("PATH", &login_path) };

    // Create the data dir, seed a default config, and guarantee a token exists — before
    // load_config reads that token. A no-op on every boot after the first.
    ensure_first_run();
    // Load the sealed env store into the in-process overlay (the .env replacement), then read
    // the config through it.
    inject_env_store(&env_store_path());
    let cfg = load_config(&config_path())?;

    // Reap proc-MCP children a PREVIOUS instance orphaned when it was hard-killed (task /End,
    // crash) before close() could tree-kill them. Read from a persisted ledger of spawned PIDs,
    // so it catches ANY proc command. Runs before any proc MCP of ours starts. The ledger file
    // is PORT-scoped: pidfiles promise two instances on different ports coexist
    // (gateway-<port>.pid), and a shared ledger let instance B's reap kill instance A's LIVE
    // proc children — "alive and not my descendant" cannot tell an orphan from a neighbour's
    // child. (The Node build also ran a command-line sweep for known MCP packages here; it
    // needs WMI for command lines, which is not ported — the ledger covers every child this
    // build spawns.)
    crate::proc_pids::set_proc_pid_file(crate::proc_pids::proc_pid_file(cfg.port));
    // Probe the port BEFORE reaping: the pid ledger is port-scoped, so a live gateway on this
    // port shares the very file about to be swept — reaping under its feet would tree-kill ITS
    // proc children. Something answering means this boot fails at bind anyway; the ledger stays
    // with its live owner. (`0.0.0.0` is a bind-all, not a connectable peer.)
    let probe_host = if cfg.host.is_empty() || cfg.host == "0.0.0.0" || cfg.host == "::" {
        "127.0.0.1".to_string()
    } else {
        cfg.host.clone()
    };
    if tokio::net::TcpStream::connect((probe_host.as_str(), cfg.port))
        .await
        .is_ok()
    {
        log::warn(
            "port is already in use — skipping the orphan-proc reap",
            Some(json!({ "host": cfg.host, "port": cfg.port })),
        );
    } else {
        crate::proc_pids::reap_proc_pids(std::process::id());
    }

    let registry = Registry::new(15_000);
    let store = Arc::new(ManagedStore::open());

    // Tunnels come up BEFORE the MCP registration loops (Node index.ts's order): an SSH
    // handshake takes seconds and must never sit behind a slow MCP start, and an MCP whose
    // database is only reachable through a tunnel gets its tunnel first. The store loads before
    // requests are accepted so the panel sees persisted connections and rules.
    let tunnel_store = Arc::new(std::sync::Mutex::new(crate::tunnel::TunnelStore::new(
        crate::paths::data_path(&["tunnels.json"]),
        cfg.port,
    )));
    // First run with no tunnels.json at all: adopt the forward-port config this gateway replaced
    // (its rules become this store's first generation). Failure is logged, never fatal.
    if let Ok(mut store) = tunnel_store.lock() {
        if store.is_fresh() {
            if let Some((rules, connections)) =
                crate::tunnel::import::import_forward_port(&mut store, None)
            {
                log::log(
                    "info",
                    "imported forward-port tunnels on first run",
                    Some(json!({ "rules": rules, "connections": connections })),
                );
            }
        }
    }
    let tunnel_manager = crate::tunnel::TunnelManager::new(
        tunnel_store.clone(),
        Some(crate::tunnel::registry_view(registry.clone())),
    );
    let tunnels = Arc::new(crate::tunnel::Tunnels {
        store: tunnel_store,
        manager: tunnel_manager.clone(),
        registry: Some(registry.clone()),
    });
    // Tunnel handshakes may take seconds or fail independently; never delay the gateway listener.
    let tunnel_boot = tunnel_manager.clone();
    tokio::spawn(async move {
        for result in tunnel_boot.start_enabled().await {
            if !result.ok {
                log::warn(
                    "tunnel boot failed",
                    Some(json!({ "rule": result.name, "error": result.error })),
                );
            }
        }
    });

    // Register + start every config-defined MCP; one failure must not take down the rest.
    for (name, def) in cfg.servers.clone() {
        register_one(&registry, &store, &name, def, Source::Config).await;
    }

    // Restore user-added MCPs from managed.json; start the ones marked enabled. An `override`
    // entry replaces a config-file MCP's def (edits to config MCPs persist here).
    for m in store.all() {
        if m.override_ && registry.has(&m.name) {
            // `start: m.enabled` — an override must not resurrect an MCP the user stopped, and
            // the config loop above already left a disabled one unstarted.
            let adapter = match make_adapter(&m.def, &m.name) {
                Ok(adapter) => adapter,
                Err(err) => {
                    log::error(
                        "config mcp override failed",
                        Some(json!({ "name": m.name, "err": err })),
                    );
                    continue;
                }
            };
            if let Err(err) = registry
                .update_def(&m.name, m.def.clone(), adapter, m.enabled)
                .await
            {
                log::error(
                    "config mcp override failed",
                    Some(json!({ "name": m.name, "err": err })),
                );
                continue;
            }
            log::log(
                "info",
                "config mcp override applied",
                Some(json!({ "name": m.name, "type": m.def.type_(), "enabled": m.enabled })),
            );
            continue;
        }
        if registry.has(&m.name) {
            continue; // never overwrite a config MCP with a non-override
        }
        register_one(&registry, &store, &m.name, m.def.clone(), Source::Managed).await;
    }

    registry.start_timer();

    // Named per-client tokens, seeded from the existing secret so clients already configured
    // keep authenticating (as the "default" token). A pre-multi-token rotation in managed.json
    // takes precedence over the .env seed.
    let tokens = Arc::new(TokenManager::new(
        store.clone(),
        crate::managed::load_managed_token(&crate::paths::data_path(&["managed.json"]))
            .as_deref()
            .or(Some(cfg.token.as_str())),
    ));

    // Restore the traffic ring's pre-restart tail before the server accepts requests, so the
    // first panel poll sees the history that was there before the restart.
    crate::traffic::init_traffic_log();

    let ctx = AppContext::new(
        registry.clone(),
        tokens,
        store.clone(),
        cfg.token_env.clone(),
        cfg.port,
    );
    if let Ok(mut links) = ctx.tunnel_links.write() {
        *links = Some(tunnel_manager.clone());
    }
    // Mounted INSIDE build_app's loopback guard: axum's layer() only covers routes present at
    // the call, so merging the tunnel API after build_app would leave every /api/tunnels route
    // outside the boundary — and the guard is the only auth that tree has.
    let app = build_app(ctx.clone(), Some(crate::tunnel::api::mount(tunnels.clone())));

    let listener = tokio::net::TcpListener::bind((cfg.host.as_str(), cfg.port))
        .await
        .map_err(|err| {
            log::error(
                "listen failed",
                Some(json!({ "host": cfg.host, "port": cfg.port, "err": err.to_string() })),
            );
            format!(
                "listen {host}:{port} failed: {err}",
                host = cfg.host,
                port = cfg.port
            )
        })?;
    log::log(
        "info",
        "gateway listening",
        Some(json!({ "host": cfg.host, "port": cfg.port, "paths": registry.names() })),
    );

    // Retention runs on a boot sweep plus an hourly timer: the per-append check inside
    // record_call only ever fires for an MCP still being called, which is the opposite of the
    // log that needs ageing out.
    tokio::spawn(async {
        loop {
            crate::calls::sweep_call_logs().await;
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
        }
    });

    // The shutdown trigger, shared by the graceful-shutdown hook and the loop below: the admin
    // API's watch channel (the port of Node's process.emit("SIGTERM") — there is no deliverable
    // signal on Windows) or Ctrl-C.
    let mut shutdown_rx = ctx.shutdown.subscribe();
    let mut graceful_rx = ctx.shutdown.subscribe();
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::select! {
            changed = graceful_rx.changed() => { let _ = changed; },
            _ = tokio::signal::ctrl_c() => {},
        }
    });
    // Run until the trigger fires, then give in-flight requests 3s to drain: a connected client
    // can hold a request open, and Node force-exited after 3s rather than wait.
    use std::future::IntoFuture as _;
    let mut server = Box::pin(server.into_future());
    tokio::select! {
        _ = &mut server => {}
        changed = shutdown_rx.changed() => {
            let _ = changed;
            if tokio::time::timeout(std::time::Duration::from_secs(3), &mut server).await.is_err() {
                log::warn("graceful shutdown timed out — continuing", None);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            if tokio::time::timeout(std::time::Duration::from_secs(3), &mut server).await.is_err() {
                log::warn("graceful shutdown timed out — continuing", None);
            }
        }
    }

    log::info("shutting down");
    registry.stop_timer();
    // Tunnels first: this releases every local port, and it leaves each rule's `enabled` flag
    // alone so the next boot brings back exactly the set that was running.
    tunnel_manager.close_all().await;
    registry.close_all().await;
    crate::calls::flush_calls(None).await; // the last calls before a restart are the ones worth having on disk
    crate::traffic::flush_traffic().await; // and the last traffic rows
    Ok(())
}

/// Register one MCP and start it per the panel's enabled state and its own laziness — the shared
/// body of the config boot loop and the managed-store restore.
async fn register_one(
    registry: &Arc<Registry>,
    store: &Arc<ManagedStore>,
    name: &str,
    mut def: crate::config::ServerDef,
    source: Source,
) {
    let result = async {
        // Apply persisted tool toggles before the adapter captures the def, so a toggle survives
        // restart.
        let disabled = store.disabled_tools(name);
        if !disabled.is_empty() {
            def.set("disabledTools", json!(disabled));
        }
        let adapter = make_adapter(&def, name)?;
        let resource = store.resource_enabled(name);
        if let Some(on) = resource {
            if let Some(toggle) = adapter.resource_toggle() {
                toggle.store(on, std::sync::atomic::Ordering::SeqCst);
            }
        }
        registry.register(name, source, def.clone(), adapter)?;
        // Honour a Stop the user made in the panel (config MCPs have no `enabled` of their own —
        // the state lives in managed.json mcpEnabled).
        let enabled = store.enabled_for(name) != Some(false);
        if source == Source::Config && !enabled {
            log::log(
                "info",
                "config mcp stays stopped (panel Stop)",
                Some(json!({ "name": name, "type": def.type_() })),
            );
        } else if is_lazy(&def) {
            // A lazy MCP (a proc by default, anything with lazy:true) stays idle at boot — the
            // first client request wakes it, which is the memory the gateway exists to save.
            log::log(
                "info",
                source_label(source),
                Some(json!({ "name": name, "type": def.type_(), "state": "idle" })),
            );
        } else {
            registry.start(name).await?;
            log::log(
                "info",
                source_label(source),
                Some(json!({ "name": name, "type": def.type_(), "state": "ready" })),
            );
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(err) = result {
        log::error(
            &format!("{} init failed", source_label(source)),
            Some(json!({ "name": name, "err": err })),
        );
    }
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Config => "config mcp",
        Source::Managed => "managed mcp",
    }
}
