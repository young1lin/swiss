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

//! The Process plugin's root wrapper (SPEC §host.plugins, §process.apps): the
//! descriptor with the #process page and the /api/process routes, the two
//! capability actions it has always contributed, and the apps system the page
//! drives. Moved out of src/builtin.rs when it grew a page: composition stays
//! one factory plus one register line, now here instead of there.
//!
//! What disable really does: the two capabilities leave the action registry
//! and the /api/process routes answer the structured 503 (the state slot is
//! withdrawn) - but a STARTED APP KEEPS RUNNING (SPEC §process.apps): the
//! apps are outside programs someone started through the gateway, not
//! children of it, and a disabled plugin is not a kill switch. The tracked
//! record and the history stay on disk for the next start.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Notify;

use swiss_core::log;
use swiss_host::host::descriptor::{PageDescriptor, PluginDescriptor};
use swiss_host::host::factory::{ApplyOutcome, PluginFactory, PluginInstance};
use swiss_host::host::scope::PluginScope;
use swiss_host::services::actions::{LegacyCommandAction, ProcessExecAction};
use swiss_host::services::RuntimeServices;

use swiss_process::api::{ProcessState, ProcessSystem};
use swiss_process::config::{validate_config_for_start, ConfigRules};
use swiss_process::control::{
    AppControl, ControlDeps, SysKiller, SysProbe, SysSpawner, WallClock,
};

/// The plugin id. It is a boot built-in, so the composition table names it too
/// (`swiss::builtin`, beside mcp, data, tunnels and jobs).
pub const PROCESS_ID: &str = "process";

/// The capabilities this plugin contributes (the same two the bare plugin
/// always had): withdrawn again by its stop.
const PROCESS_ACTIONS: &[&str] = &["process.exec", "process.legacy-command"];

/// The factory: one per boot, holding only what the routes and the engines
/// need to be built - the shared services (actions registry, children
/// registry, BootInfo) and the state slot the boot also handed to mount().
pub struct ProcessPlugin {
    services: Arc<RuntimeServices>,
    state: Arc<ProcessState>,
}

impl ProcessPlugin {
    pub fn new(services: Arc<RuntimeServices>, state: Arc<ProcessState>) -> Self {
        ProcessPlugin { services, state }
    }

    /// The validation rules every config read derives from THIS instance: the
    /// port it serves (an app may never declare it) and its own image name
    /// (never a valid app image - a 19998 instance must not be able to read,
    /// stop or kill 19999 through its own config).
    fn rules(&self) -> ConfigRules {
        ConfigRules {
            gateway_port: self.services.boot.port,
            gateway_image: gateway_image_name(),
        }
    }
}

/// The config the crate's validators read: this object with the host's own
/// "enabled" flag (allowed here like every plugin's) stripped, so the crate's
/// strict unknown-key door only ever sees the apps table's door.
fn apps_config(config: &Value) -> Value {
    let mut cleaned = config.clone();
    if let Some(obj) = cleaned.as_object_mut() {
        obj.remove("enabled");
    }
    cleaned
}

/// This exe's basename as the platform reports it ("swiss.exe"); unknown
/// falls back to the product name so the refusal still holds.
fn gateway_image_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "swiss.exe".to_string())
}

#[async_trait]
impl PluginFactory for ProcessPlugin {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            id: PROCESS_ID.into(),
            kind: "process".into(),
            label: "Process".into(),
            version: "0.2".into(),
            config_schema_version: 2,
            // The apps table (SPEC §process.apps): an empty schema object
            // would still refuse unknown keys, but the real door is
            // validate_config below - every field of every app named.
            config_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            // One page (SPEC §process.apps panel): the apps table - status,
            // start/stop/restart, logs, history, and the children of the
            // gateway itself. Before Terminal's 70, after the data views.
            pages: vec![PageDescriptor {
                id: "process".into(),
                plugin_id: PROCESS_ID.into(),
                label: "Process".into(),
                order: 55,
                path: "#process".into(),
                entry: "/admin/js/views/process.js".into(),
                sidebar: false,
                layout: "page",
            }],
            routes: vec!["/api/process".into()],
            // Config applies in place (the app table changes under a running
            // plugin); a PUT never restarts the instance.
            restart_on_config_change: false,
            requires: Vec::new(),
        }
    }

    fn validate_config(&self, config: &Value) -> Result<(), String> {
        // The host owns "enabled"; everything else is the apps table, door by
        // door (SPEC §process.apps): the PUT refuses the whole save and every
        // error names its apps[i].field.
        let obj = config
            .as_object()
            .ok_or_else(|| "process config must be an object".to_string())?;
        if let Some(key) = obj.keys().find(|k| k.as_str() != "enabled" && k.as_str() != "apps") {
            return Err(format!("process config has no {key:?} field"));
        }
        // The crate's validator reads the cleaned object (an absent apps row
        // is an empty table, its own contract).
        swiss_process::config::validate_config(&apps_config(config), &self.rules())
    }

    fn config_warnings(&self, config: &Value) -> Vec<String> {
        // What is storable but not runnable NOW (SPEC §process.apps): the
        // panel renders these under the form. The resolution check is the
        // SPAWN's own PATH and resolver, so a warning here means the next
        // start would genuinely fail - never a second opinion.
        match swiss_process::config::read_config(&apps_config(config), &self.rules()) {
            Ok(parsed) => {
                let path = swiss_host::services::detached::detached_path();
                swiss_process::config::config_warnings(&parsed, &|program| {
                    swiss_host::pathenv::resolve_command(program, &path).is_some()
                })
            }
            // A config that cannot even parse is the PUT's own refusal; the
            // warnings row is not a second error channel.
            Err(_) => Vec::new(),
        }
    }

    async fn create(&self, config: &Value) -> Result<Arc<dyn PluginInstance>, String> {
        let rules = self.rules();
        let boot = self.services.boot;

        // Boot's door: an invalid entry is dropped with a warning naming it,
        // and the rest load (SPEC §process.apps).
        let (process_config, warnings) = validate_config_for_start(&apps_config(config), &rules);
        for warning in &warnings {
            log::warn(
                "the process config dropped an app",
                Some(json!({ "why": warning })),
            );
        }

        // The real seams: platform reads, the detached spawn, the tree kill.
        // Exit lines flow to the pump task below through the channel the
        // spawner holds (SPEC §process.apps: an app that dies on its own is
        // one history line, not a watchdog event).
        let (exit_tx, exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let clock = Arc::new(WallClock);
        let spawner = Arc::new(SysSpawner::new(exit_tx, clock.clone()));
        let history = swiss_process::history::ProcessHistory::new(swiss_core::paths::data_path(&["logs"]));
        let control = AppControl::new(ControlDeps {
            rules,
            boot,
            probe: Arc::new(SysProbe),
            spawner,
            killer: Arc::new(SysKiller),
            clock,
            record_path: swiss_core::paths::data_path(&[&format!(
                ".process-apps-{}.json",
                boot.port
            )]),
            history: history.clone(),
            capture_dir: swiss_core::paths::data_path(&["logs", "apps"]),
            gateway_pid: std::process::id(),
        });
        control.set_apps(process_config.apps);

        Ok(Arc::new(ProcessInstance {
            services: self.services.clone(),
            state: self.state.clone(),
            control,
            history,
            exit_rx: std::sync::Mutex::new(Some(exit_rx)),
            stop_pump: Arc::new(Notify::new()),
        }))
    }
}

struct ProcessInstance {
    services: Arc<RuntimeServices>,
    state: Arc<ProcessState>,
    control: Arc<AppControl>,
    /// The history store (SPEC §host.history): opened inside the enabled scope
    /// at start, closed bounded at stop - a disabled process opens nothing.
    history: Arc<swiss_process::history::ProcessHistory>,
    /// The spawner's exit channel, taken by the pump task on start. A std
    /// Mutex: the guard is never held across an await, only for the take.
    exit_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<swiss_process::control::ExitLine>>>,
    /// Notified on stop: the pump task ends with the plugin, not with the
    /// last spawner reference.
    stop_pump: Arc<Notify>,
}

#[async_trait]
impl PluginInstance for ProcessInstance {
    async fn start(self: Arc<Self>, scope: &mut PluginScope) -> Result<(), String> {
        // The two capabilities first (the same pair the bare plugin had), with
        // the same half-registered rollback: a provider reporting active with
        // one of its two capabilities missing is a broken one.
        let supervisor = self.services.supervisor.clone();
        self.services
            .actions
            .register(Arc::new(ProcessExecAction::new(supervisor.clone())))?;
        if let Err(err) = self
            .services
            .actions
            .register(Arc::new(LegacyCommandAction::new(supervisor)))
        {
            self.services.actions.unregister("process.exec");
            return Err(err);
        }

        // The history store (SPEC §host.history): open before the first line
        // can land - load_tracked and the boot pass both write. A failed open
        // fails the start with the sentence, the two capabilities withdrawn.
        if let Err(err) = self.history.open().await {
            for name in PROCESS_ACTIONS {
                self.services.actions.unregister(name);
            }
            return Err(err);
        }

        // The tracked record from the last gateway: what survived, kept; what
        // was reused or gone, dropped (SPEC §process.apps).
        self.control.load_tracked().await;

        // The exit pump: one task, for the life of the instance, turning
        // late exits of started apps into history lines.
        let pump_control = self.control.clone();
        let stop = self.stop_pump.clone();
        let rx = self
            .exit_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(mut rx) = rx {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        line = rx.recv() => {
                            match line {
                                Some(line) => pump_control.handle_exit(&line),
                                None => break,
                            }
                        }
                        _ = stop.notified() => break,
                    }
                }
            });
        }

        // The hourly capture sweep (SPEC §process.apps: "The check runs on
        // the plugin's hourly tick and before each start"): one more task
        // for the life of the instance, on the pump's own stop signal. The
        // first sweep is a full hour out - the pre-start sweep covers boot,
        // so the tick fires on each hourly boundary only, never promptly.
        spawn_sweep_tick(self.control.clone(), self.stop_pump.clone());

        // The routes' seat: installed last so no request can reach a control
        // that has not read its record yet.
        self.state.install(Arc::new(ProcessSystem {
            control: self.control.clone(),
            children: self.services.children.clone(),
        }));

        // Start with swiss (SPEC §process.apps): daemons only, declared
        // order, one retry, conflicts left alone - and never blocking the
        // boot: a 30 s start window is an app's business, not the gateway's.
        // The pass is taken from the services ONCE per gateway process: an
        // enable after a disable is not a reboot, so a second start finds the
        // latch spent ("Enabling the plugin later ... never starts one
        // either"). On the scope, not a bare spawn: disable cancels the retry
        // gap instead of leaving a task that starts apps under a plugin whose
        // routes already answer 503.
        if self.services.take_boot_pass() {
            let boot_control = self.control.clone();
            let cancel = scope.cancel_signal();
            scope.spawn(async move {
                for (name, outcome) in boot_control.start_with_swiss_until(Some(cancel)).await {
                    log::log(
                        "info",
                        "start with swiss finished",
                        Some(json!({
                            "app": name,
                            "ok": outcome.ok,
                            "message": outcome.message,
                        })),
                    );
                }
            });
        }
        Ok(())
    }

    async fn stop(&self) {
        // The seat first: no request reaches a dying plugin. Then the
        // capabilities, then the cancellation of runs already executing them:
        // nothing can start a new run of a capability that is no longer
        // resolvable while the in-flight ones are reaped (child tree killed,
        // pipe readers joined) before stop returns.
        self.state.withdraw();
        for name in PROCESS_ACTIONS {
            self.services.actions.unregister(name);
        }
        let canceled = self
            .services
            .runs
            .cancel_action_types(PROCESS_ACTIONS)
            .await;
        if canceled > 0 {
            log::log(
                "info",
                "process plugin stopped; in-flight runs canceled",
                Some(json!({ "runs": canceled })),
            );
        }
        self.stop_pump.notify_waiters();
        // The history store closes bounded (SPEC §host.history): withdraw,
        // drain, release. An unresolved close is this warning, never a swallow.
        if let Err(err) = self.history.close().await {
            log::log(
                "warn",
                "the process history store did not close cleanly",
                Some(json!({ "err": err.to_string() })),
            );
        }
        // What stop deliberately does NOT do: touch a started app. A disabled
        // process plugin leaves the Jobs scheduler ticking and its
        // process-backed jobs report the missing capability - the
        // dependency-unavailable outcome SPEC §host.plugins asks for - while
        // the apps someone started through the page keep running until someone
        // stops them (SPEC §process.apps).
    }

    /// The apps table applies in place (SPEC §process.apps): the PUT already
    /// refused anything invalid (validate_config); here the valid rows load
    /// under a running plugin and the tracked record keeps only what the new
    /// table still declares.
    async fn apply_config(&self, config: &Value) -> ApplyOutcome {
        let rules = ConfigRules {
            gateway_port: self.services.boot.port,
            gateway_image: gateway_image_name(),
        };
        let (process_config, warnings) =
            validate_config_for_start(&apps_config(config), &rules);
        for warning in &warnings {
            log::warn(
                "the process config dropped an app",
                Some(json!({ "why": warning })),
            );
        }
        self.control.set_apps(process_config.apps);
        ApplyOutcome::Applied
    }
}

/// The hourly capture sweep's tick (SPEC §process.apps): one task for the
/// life of the instance, the exit pump's own shape - the same stop Notify,
/// the same clone-into-the-task pattern. Every fired hour it sweeps every
/// declared capture (metadata only, a warning per file that fails); the
/// first sweep is a full hour out, because the pre-start sweep already
/// covers boot.
fn spawn_sweep_tick(control: Arc<AppControl>, stop: Arc<Notify>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {
                    control.sweep_captures();
                }
                _ = stop.notified() => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use swiss_process::config::{AppConfig, CmdSpec};

    /// A control the way create() builds the real one, with every path in
    /// this scratch dir: the sweep needs only the declared apps and the
    /// capture dir, and the sealed record set_apps writes pins the test
    /// master key through swiss-core's test-utils feature.
    fn sweep_control(dir: &std::path::Path, apps: Vec<AppConfig>) -> Arc<AppControl> {
        let (exit_tx, _exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let clock = Arc::new(WallClock);
        let control = AppControl::new(ControlDeps {
            rules: ConfigRules {
                gateway_port: 19998,
                gateway_image: "swiss.exe".to_string(),
            },
            boot: swiss_host::services::BootInfo::default(),
            probe: Arc::new(SysProbe),
            spawner: Arc::new(SysSpawner::new(exit_tx, clock.clone())),
            killer: Arc::new(SysKiller),
            clock,
            record_path: dir.join("record.json"),
            history: swiss_process::history::ProcessHistory::new(dir.join("logs")),
            capture_dir: dir.join("apps"),
            gateway_pid: std::process::id(),
        });
        control.set_apps(apps);
        control
    }

    /// The hourly tick, wired the way start() wires it: no prompt first
    /// sweep (the pre-start sweep owns boot), a sweep on each hourly
    /// boundary, and the task ends with the instance's stop signal.
    #[tokio::test(start_paused = true)]
    async fn the_sweep_tick_waits_the_hour_sweeps_and_ends_on_stop() {
        let dir = std::env::temp_dir().join(format!(
            "swiss-process-tick-{}-{}",
            std::process::id(),
            swiss_core::util::now_ms()
        ));
        std::fs::create_dir_all(dir.join("apps")).expect("scratch capture dir");
        let capture = dir.join("apps").join("tick.log");
        std::fs::write(
            &capture,
            vec![b'x'; swiss_process::logs::CAPTURE_SWEEP_ABOVE as usize + 1],
        )
        .expect("oversized capture");

        let app = AppConfig {
            name: "tick".to_string(),
            start: CmdSpec {
                program: "never-started".to_string(),
                args: Vec::new(),
            },
            stop: None,
            dir: None,
            ports: Vec::new(),
            image: None,
            log: None,
            ui: None,
            autostart: false,
            env: std::collections::BTreeMap::new(),
        };
        let stop = Arc::new(Notify::new());
        let task = spawn_sweep_tick(sweep_control(&dir, vec![app]), stop.clone());
        // Let the task register its first hour before any clock move.
        tokio::task::yield_now().await;

        // A hair under the first hour: nothing swept yet - no prompt sweep.
        tokio::time::advance(std::time::Duration::from_secs(3599)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            std::fs::metadata(&capture).expect("capture still there").len(),
            swiss_process::logs::CAPTURE_SWEEP_ABOVE + 1,
            "the tick must not sweep before its first hourly boundary"
        );

        // Across the boundary: the declared capture is swept through the
        // public path.
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            std::fs::metadata(&capture).expect("capture swept").len(),
            0,
            "the hourly tick must sweep the oversized capture"
        );

        // The task ends with the instance, not with the last sweep.
        stop.notify_waiters();
        task.await.expect("the sweep tick ends on stop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
