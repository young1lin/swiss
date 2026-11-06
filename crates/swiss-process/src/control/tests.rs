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

//! The control's tests (SPEC §process.apps): start, stop, restart and start with swiss over an
//! injected Probe, Spawner and Killer and a paused clock.

use super::conflict_sentence;

#[test]
fn the_conflict_sentence_names_an_unreadable_image_as_such() {
    assert_eq!(conflict_sentence(0, "", 135), "port 135 is owned by another user");
    assert_eq!(
        conflict_sentence(2100, "", 135),
        "port 135 is held by pid 2100 (image unreadable)"
    );
    assert_eq!(
        conflict_sentence(5, "node.exe", 4317),
        "port 4317 is held by pid 5 (node.exe)"
    );
}
use super::*;
use serde_json::json;

struct FakeProbe {
    state: Mutex<ProbeState>,
}

#[derive(Default)]
struct ProbeState {
    listeners: Vec<PortFact>,
    procs: HashMap<u32, OwnerFact>,
    listener_reads: usize,
    // Scripted failure: a listener table that cannot be read at all.
    listeners_err: bool,
    process_reads: HashMap<u32, usize>,
}

#[async_trait::async_trait]
impl Probe for FakeProbe {
    async fn listeners(&self) -> std::io::Result<Vec<PortFact>> {
        let mut state = self.state.lock().unwrap();
        state.listener_reads += 1;
        if state.listeners_err {
            return Err(std::io::Error::other("the listener table is unreadable"));
        }
        Ok(state.listeners.clone())
    }

    async fn process(&self, pid: u32) -> Option<OwnerFact> {
        let mut state = self.state.lock().unwrap();
        *state.process_reads.entry(pid).or_default() += 1;
        state.procs.get(&pid).cloned()
    }
}

/// The seam's scriptable half: what a spawn should do, as a boxed closure.
type SpawnHook = Box<dyn FnMut(&SpawnSpec) -> Result<Spawned, String> + Send + Sync>;

struct FakeSpawner {
    count: Mutex<Vec<String>>,
    on_spawn: Mutex<SpawnHook>,
}

#[async_trait::async_trait]
impl Spawner for FakeSpawner {
    async fn spawn(&self, spec: &SpawnSpec) -> Result<Spawned, String> {
        self.count
            .lock()
            .unwrap()
            .push(spec.program.clone());
        (self.on_spawn.lock().unwrap())(spec)
    }
}

impl FakeSpawner {
    fn times(&self, program: &str) -> usize {
        self.count.lock().unwrap().iter().filter(|p| *p == program).count()
    }
}

struct FakeKiller {
    killed: Mutex<Vec<u32>>,
    survivors: Mutex<Vec<u32>>,
}

#[async_trait::async_trait]
impl Killer for FakeKiller {
    async fn kill_tree(&self, pid: u32) -> Vec<u32> {
        self.killed.lock().unwrap().push(pid);
        self.survivors.lock().unwrap().clone()
    }
}

/// A frozen clock: history stamps are stable, uptime is a number the
/// assertions can predict.
struct FakeClock;

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }
}

struct Rig {
    control: Arc<AppControl>,
    /// The scratch history store, opened lazily by the tests that assert
    /// history lines (see [Rig::open_history]).
    history: Arc<ProcessHistory>,
    probe: Arc<FakeProbe>,
    spawner: Arc<FakeSpawner>,
    killer: Arc<FakeKiller>,
    dir: PathBuf,
}

impl Rig {
    /// Open the scratch history store on a dedicated REAL-time runtime.
    /// These tests pause tokio's clock for their retry windows, and a
    /// paused runtime auto-advances the instant it goes idle - which fires
    /// the sqlx pool's acquire deadline before the blocking sqlite open
    /// on its blocking thread has finished. So the open (and the store's
    /// worker, spawned at open) run on a parked current_thread runtime of
    /// its own; the engine's submit/barrier calls wake it cross-runtime
    /// through the seam's own channels. The barrier still does not make a
    /// caller-side read clock-safe: sqlx arms the pool's acquire deadline
    /// on every acquire, idle or not, so a read polled on the paused
    /// runtime can idle-leap straight into that deadline. The shared
    /// [Rig::history_read_paused] helper holds that boundary instead:
    /// it resumes the caller's clock for the read and pauses it again
    /// before returning, so later virtual windows still see a paused
    /// clock.
    async fn open_history(&self) {
        let store = self.history.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("scratch-history-runtime".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the scratch runtime builds");
                let opened = runtime.block_on(store.open());
                let ok = opened.is_ok();
                let _ = tx.send(opened);
                if ok {
                    // Parked, not dead: the runtime still services the
                    // store's worker for the test's whole life.
                    runtime.block_on(std::future::pending::<()>());
                }
            })
            .expect("the scratch runtime thread spawns");
        rx.recv()
            .expect("the scratch history store opens")
            .expect("the scratch history store opens");
    }

    /// A paused fixture's real history read, on a short real-time
    /// boundary: sqlx arms the pool's acquire deadline on every acquire,
    /// and a paused runtime would idle-leap straight into it, so the
    /// caller's clock resumes for the SQLite read itself and pauses
    /// again before returning - every virtual control window after the
    /// call sees the same paused clock as before it. Only for
    /// start_paused tests whose Rig history [Rig::open_history] opened.
    async fn history_read_paused(&self, app: Option<&str>, limit: usize) -> Vec<HistoryEntry> {
        tokio::time::resume();
        let lines = self.control.history().read(app, limit).await;
        tokio::time::pause();
        lines
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // Best-effort: an open store's database is locked on Windows, so a
        // rig that read history leaves its scratch dir for the OS. The
        // tests pin correctness, not temp hygiene.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rig(apps: Vec<AppConfig>, gateway_pid: u32) -> Rig {
    rig_with(apps, gateway_pid, true, |_| {})
}

fn rig_with(
    apps: Vec<AppConfig>,
    gateway_pid: u32,
    daemon: bool,
    tune: impl FnOnce(&FakeProbe),
) -> Rig {
    // Parallel tests can start in the same millisecond: an atomic counter
    // keeps every rig's scratch dir its own.
    static RIG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "swiss-process-control-{}-{}-{}",
        std::process::id(),
        swiss_core::util::now_ms(),
        RIG.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let probe = Arc::new(FakeProbe {
        state: Mutex::new(ProbeState::default()),
    });
    tune(&probe);
    let spawner = Arc::new(FakeSpawner {
        count: Mutex::new(Vec::new()),
        on_spawn: Mutex::new(Box::new(|_spec| {
            Ok(Spawned {
                pid: 4321,
                created_at: Some(1_700_000_000_000),
                shares_job: false,
                exits: None,
            })
        })),
    });
    let killer = Arc::new(FakeKiller {
        killed: Mutex::new(Vec::new()),
        survivors: Mutex::new(Vec::new()),
    });
    let history = ProcessHistory::new(dir.join("logs"));
    let control = AppControl::new(ControlDeps {
        rules: ConfigRules {
            gateway_port: 19999,
            gateway_image: "swiss.exe".to_string(),
        },
        boot: BootInfo {
            port: 19999,
            is_daemon: daemon,
        },
        probe: probe.clone(),
        spawner: spawner.clone(),
        killer: killer.clone(),
        clock: Arc::new(FakeClock),
        record_path: dir.join("record.json"),
        history: history.clone(),
        capture_dir: dir.join("apps"),
        gateway_pid,
    });
    control.set_apps(apps);
    Rig {
        control,
        history,
        probe,
        spawner,
        killer,
        dir,
    }
}

fn otel_app() -> AppConfig {
    read_app(&json!({
        "name": "cc-otel",
        "start": { "program": "cc-otel", "args": ["start"] },
        "stop": { "program": "cc-otel", "args": ["stop"] },
        "ports": [4317, 4318],
        "image": "cc-otel.exe",
        "autostart": true
    }))
}

fn read_app(value: &serde_json::Value) -> AppConfig {
    let rules = ConfigRules {
        gateway_port: 19999,
        gateway_image: "swiss.exe".to_string(),
    };
    crate::config::read_config(
        &json!({ "apps": [value] }),
        &rules,
    )
    .expect("valid app")
    .apps
    .pop()
    .expect("one app")
}

fn listen(probe: &FakeProbe, port: u16, pid: u32, image: &str) {
    let mut state = probe.state.lock().unwrap();
    state.listeners.push(PortFact { port, pid });
    state.procs.insert(
        pid,
        OwnerFact {
            pid,
            image: image.to_string(),
            created_at: 1_700_000_000_000,
        },
    );
}

#[tokio::test(start_paused = true)]
async fn a_second_action_on_a_busy_app_is_409() {
    let rig = rig(vec![otel_app()], 999);
    // The first start is in its (never-listening) window; the second call
    // refuses with Busy before any engine runs.
    rig.control.start("cc-otel", "panel").await.expect("first");
    let err = rig
        .control
        .start("cc-otel", "panel")
        .await
        .expect_err("busy");
    assert!(matches!(err, ControlError::Busy("start")), "{err:?}");
    let err = rig
        .control
        .stop("cc-otel", "panel", false)
        .await
        .expect_err("busy too");
    assert!(matches!(err, ControlError::Busy(_)));
}

#[tokio::test(start_paused = true)]
async fn run_start_refuses_a_busy_app_without_spawning_or_recording() {
    // A stop in flight holds the mark: a direct run_start (the boot pass's
    // path) must refuse it, spawn nothing and write no history line - not
    // overwrite the mark and double-run the app (round-4 review).
    let rig = rig(vec![otel_app()], 999);
    rig.open_history().await;
    let _mark = rig.control.try_claim("cc-otel", "stop").expect("the mark is free");
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_start(&app, "boot").await;
    assert!(!outcome.ok, "{outcome:?}");
    assert!(outcome.message.contains("busy"), "{outcome:?}");
    assert_eq!(rig.spawner.times("cc-otel"), 0, "the busy app spawned nothing");
    assert!(
        rig.history_read_paused(None, 10).await.is_empty(),
        "the refusal writes no history line - the mark holder records its own",
    );
    // The mark survives the refusal: the holder keeps its claim.
    assert!(rig.control.busy_action("cc-otel").is_some(), "the holder's mark stands");
}

#[tokio::test(start_paused = true)]
async fn the_boot_pass_leaves_a_busy_app_to_its_mark_holder() {
    // A user start in flight (its mark held): the boot pass records the
    // refusal for the app and never retries a spawn behind the holder's
    // back. The pass's one 60 s retry meets the same held mark (the
    // paused clock advances it instantly), so nothing ever spawns.
    let rig = rig(vec![otel_app()], 999);
    rig.open_history().await;
    let _mark = rig.control.try_claim("cc-otel", "start").expect("the mark is free");
    let results = rig.control.start_with_swiss().await;
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(!results[0].1.ok, "{:?}", results[0].1);
    assert!(results[0].1.message.contains("busy"), "{:?}", results[0].1);
    assert_eq!(
        rig.spawner.times("cc-otel"),
        0,
        "neither the first attempt nor the retry spawned behind the mark",
    );
}

#[tokio::test(start_paused = true)]
async fn start_refuses_conflict_and_partial() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 99, "node.exe");
    });
    let err = rig
        .control
        .start("cc-otel", "panel")
        .await
        .expect_err("conflict");
    match err {
        ControlError::Refused(sentence) => {
            assert!(sentence.contains("4317"), "{sentence}");
            assert!(sentence.contains("99"), "{sentence}");
            assert!(sentence.contains("node.exe"), "{sentence}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Partial: one port up by our own image, one down.
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 10, "cc-otel.exe");
    });
    let err = rig
        .control
        .start("cc-otel", "panel")
        .await
        .expect_err("partial");
    match err {
        ControlError::Refused(sentence) => {
            assert!(sentence.contains("4318"), "{sentence}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn rows_share_one_listener_snapshot_and_probe_each_owner_once() {
    let second = read_app(&json!({
        "name": "other",
        "start": { "program": "cc-otel" },
        "ports": [4319],
        "image": "cc-otel.exe"
    }));
    let tracked = read_app(&json!({
        "name": "tracked",
        "start": { "program": "sleeper" }
    }));
    let rig = rig(vec![otel_app(), second, tracked], 999);
    for port in [4317, 4318, 4319, 15000] {
        listen(&rig.probe, port, 5, "cc-otel.exe");
    }
    rig.probe.state.lock().unwrap().procs.insert(
        10,
        OwnerFact {
            pid: 10,
            image: "sleeper.exe".to_string(),
            created_at: 1_700_000_000_000,
        },
    );
    {
        let mut records = rig.control.record.lock().unwrap();
        for name in ["cc-otel", "other"] {
            records.insert(name.to_string(), TrackedRecord {
                pid: 5,
                created_at: 1_700_000_000_000,
                image: "cc-otel.exe".to_string(),
            });
        }
        records.insert("tracked".to_string(), TrackedRecord {
            pid: 10,
            created_at: 1_700_000_000_000,
            image: "sleeper.exe".to_string(),
        });
    }

    let rows = rig.control.rows().await;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.status == "running"));
    for row in &rows[..2] {
        assert_eq!(row.pid, Some(5));
        assert_eq!(row.held_ports, vec![4317, 4318, 4319, 15000]);
    }
    assert_eq!(rows[2].pid, Some(10));
    assert!(rows[2].held_ports.is_empty());
    let state = rig.probe.state.lock().unwrap();
    assert_eq!(state.listener_reads, 1, "one listener table for the whole answer");
    assert_eq!(state.process_reads.get(&5), Some(&1), "one fact read per shared owner");
    assert_eq!(state.process_reads.get(&10), Some(&1), "one fact read per tracked pid");
}

#[tokio::test(start_paused = true)]
async fn rows_carry_the_ports_the_pid_actually_holds() {
    let rig = rig(vec![otel_app()], 999);
    // A live tracked pid holding both declared ports AND one the config
    // never declared: the machine's fact, not the promise - the row must
    // say the pid holds it too.
    listen(&rig.probe, 4317, 5, "cc-otel.exe");
    listen(&rig.probe, 4318, 5, "cc-otel.exe");
    listen(&rig.probe, 15000, 5, "cc-otel.exe");
    rig.control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            "cc-otel".to_string(),
            TrackedRecord {
                pid: 5,
                created_at: 1,
                image: "cc-otel.exe".to_string(),
            },
        );
    let rows = rig.control.rows().await;
    let row = rows.iter().find(|r| r.name == "cc-otel").expect("row");
    assert_eq!(row.status, "running");
    assert_eq!(row.held_ports, vec![4317, 4318, 15000]);
    assert!(row.down_ports.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_start_window_that_ends_never_kills_a_live_process() {
    let rig = rig(vec![otel_app()], 999);
    // The fake spawner's pid is alive in the probe and never listens.
    rig.probe.state.lock().unwrap().procs.insert(
        4321,
        OwnerFact {
            pid: 4321,
            image: "cc-otel.exe".to_string(),
            created_at: 1_700_000_000_000,
        },
    );
    let outcome = rig
        .control
        .run_start(&rig.control.app("cc-otel").unwrap(), "panel")
        .await;
    assert!(!outcome.ok, "{:?}", outcome.message);
    assert!(
        outcome.message.contains("not listening on 4317"),
        "{:?}",
        outcome.message
    );
    assert!(
        outcome.message.contains("still running, not killed"),
        "{:?}",
        outcome.message
    );
    // Nothing was killed and the process is still there.
    assert!(rig.killer.killed.lock().unwrap().is_empty());
    assert!(rig.probe.state.lock().unwrap().procs.contains_key(&4321));
}

#[tokio::test(start_paused = true)]
async fn a_launcher_that_exits_0_polls_on_and_fails_only_at_the_window_end() {
    let rig = rig(vec![otel_app()], 999);
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(|_spec| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(0);
        Ok(Spawned {
            pid: 5,
            created_at: Some(1),
            shares_job: false,
            exits: Some(rx),
        })
    });
    let outcome = rig
        .control
        .run_start(&rig.control.app("cc-otel").unwrap(), "panel")
        .await;
    // Polling went on to the end of the window, then the launcher row of
    // the outcome table decided it (SPEC §process.apps).
    assert!(!outcome.ok);
    assert_eq!(outcome.exit_code, Some(0));
    assert!(
        outcome.message.contains("launcher exited 0 but nothing listens on 4317"),
        "{:?}",
        outcome.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_launcher_that_exits_0_oks_when_the_child_binds_and_pins_the_owner() {
    let rig = rig(vec![otel_app()], 999);
    let probe = rig.probe.clone();
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(move |_spec| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(0);
        // The launcher dies at once; its child binds a beat later, well
        // inside the window.
        let probe = probe.clone();
        tokio::spawn(async move {
            tokio::time::sleep(POLL_STEP * 2).await;
            let mut state = probe.state.lock().unwrap();
            state.listeners.push(PortFact { port: 4317, pid: 77 });
            state.listeners.push(PortFact { port: 4318, pid: 77 });
            state.procs.insert(
                77,
                OwnerFact {
                    pid: 77,
                    image: "cc-otel.exe".to_string(),
                    created_at: 1_700_000_000_042,
                },
            );
        });
        Ok(Spawned {
            pid: 5,
            created_at: Some(1),
            shares_job: false,
            exits: Some(rx),
        })
    });
    let outcome = rig
        .control
        .run_start(&rig.control.app("cc-otel").unwrap(), "panel")
        .await;
    assert!(outcome.ok, "{:?}", outcome.message);
    assert_eq!(outcome.pid, Some(77), "the owner, not the dead launcher");
    assert!(
        outcome.message.contains("listening on 4317"),
        "{:?}",
        outcome.message
    );
    // The record pins the OWNER (the exit watcher follows the child).
    let record = rig
        .control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get("cc-otel")
        .cloned();
    let record = record.expect("record pinned");
    assert_eq!(record.pid, 77);
    assert_eq!(record.created_at, 1_700_000_000_042);
}

#[tokio::test(start_paused = true)]
async fn an_exit_0_without_ports_is_an_ok() {
    let app = read_app(&json!({
        "name": "oneshot",
        "start": { "program": "oneshot-tool" }
    }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(|_spec| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(0);
        Ok(Spawned {
            pid: 9,
            created_at: Some(1),
            shares_job: false,
            exits: Some(rx),
        })
    });
    let outcome = rig
        .control
        .run_start(&rig.control.app("oneshot").unwrap(), "panel")
        .await;
    assert!(outcome.ok, "{:?}", outcome.message);
    assert_eq!(outcome.exit_code, Some(0));
    assert!(
        outcome.message.contains("declare ports"),
        "{:?}",
        outcome.message
    );
    // Nothing is alive to track: no record.
    assert!(rig
        .control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get("oneshot")
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn a_nonzero_exit_fails_with_the_masked_tail() {
    // What a REFERENCE resolved to is masked; what the user typed as a
    // literal is config and stays. The tail below must lose the resolved
    // value and gain the bullets.
    std::env::set_var("SWISS_PROCESS_TAIL_TOKEN", "supersecret-value");
    let app = read_app(&json!({
        "name": "svc",
        "start": { "program": "svc", "args": ["--flag"] },
        "ports": [7001],
        "image": "svc.exe",
        "env": { "TOKEN": "${SWISS_PROCESS_TAIL_TOKEN}" }
    }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    // The capture file the real spawner would have written: the secret the
    // env resolved to must come back as bullets, never as itself.
    std::fs::create_dir_all(rig.dir.join("apps")).expect("capture dir");
    std::fs::write(
        rig.dir.join("apps").join("svc.log"),
        "svc: booting with supersecret-value\n",
    )
    .expect("capture");
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(|_spec| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(1);
        Ok(Spawned {
            pid: 6,
            created_at: Some(1),
            shares_job: false,
            exits: Some(rx),
        })
    });
    let outcome = rig
        .control
        .run_start(&rig.control.app("svc").unwrap(), "panel")
        .await;
    assert!(!outcome.ok);
    assert_eq!(outcome.exit_code, Some(1));
    assert!(!outcome.message.contains("supersecret-value"), "{:?}", outcome.message);
    assert!(outcome.message.contains('•'), "{:?}", outcome.message);
    std::env::remove_var("SWISS_PROCESS_TAIL_TOKEN");
}

#[tokio::test(start_paused = true)]
async fn a_missing_reference_fails_the_start_naming_it_and_never_a_value() {
    let app = read_app(&json!({
        "name": "svc",
        "start": { "program": "svc", "args": ["--flag"] },
        "ports": [7002],
        "image": "svc.exe",
        "env": { "TOKEN": "${SWISS_PROCESS_NO_SUCH_REF}" }
    }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    let outcome = rig
        .control
        .run_start(&rig.control.app("svc").unwrap(), "panel")
        .await;
    assert!(!outcome.ok);
    assert!(
        outcome.message.contains("SWISS_PROCESS_NO_SUCH_REF"),
        "{:?}",
        outcome.message
    );
    // Nothing ran at all.
    assert_eq!(rig.spawner.times("svc"), 0);
}

struct ChangedPortOwnerProbe {
    initial: Arc<FakeProbe>,
    reads: std::sync::atomic::AtomicUsize,
    readable: bool,
}

#[async_trait::async_trait]
impl Probe for ChangedPortOwnerProbe {
    async fn listeners(&self) -> std::io::Result<Vec<PortFact>> {
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match read {
            0 | 1 => self.initial.listeners().await,
            2 => Ok(vec![
                PortFact { port: 4317, pid: 888 },
                PortFact { port: 4318, pid: 888 },
            ]),
            _ => Ok(Vec::new()),
        }
    }

    async fn process(&self, pid: u32) -> Option<OwnerFact> {
        if pid == 888 {
            self.readable.then(|| OwnerFact {
                pid,
                image: "node.exe".to_string(),
                created_at: 1_700_000_000_001,
            })
        } else {
            self.initial.process(pid).await
        }
    }
}

fn rig_with_changed_port_owner(readable: bool) -> Rig {
    let mut rig = rig(vec![otel_app()], 999);
    listen(&rig.probe, 4317, 777, "cc-otel.exe");
    listen(&rig.probe, 4318, 777, "cc-otel.exe");
    Arc::get_mut(&mut rig.control).expect("unique control").deps.probe = Arc::new(ChangedPortOwnerProbe {
        initial: rig.probe.clone(),
        reads: std::sync::atomic::AtomicUsize::new(0),
        readable,
    });
    rig
}

#[tokio::test(start_paused = true)]
async fn a_hard_kill_never_targets_a_foreign_owner_after_reidentification() {
    let rig = rig_with_changed_port_owner(true);
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", true).await;
    let killed = rig.killer.killed.lock().unwrap();
    assert!(killed.is_empty(), "a changed foreign owner must not be killed: {killed:?}");
    assert!(!outcome.ok, "a changed foreign owner must refuse the stop");
    assert!(outcome.message.contains("888") && outcome.message.contains("node.exe"), "{}", outcome.message);
}

#[tokio::test(start_paused = true)]
async fn a_hard_kill_never_targets_an_unreadable_owner_after_reidentification() {
    let rig = rig_with_changed_port_owner(false);
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", true).await;
    let killed = rig.killer.killed.lock().unwrap();
    assert!(killed.is_empty(), "an unreadable owner must not be killed: {killed:?}");
    assert!(!outcome.ok, "an unreadable owner must refuse the stop");
    assert!(outcome.message.contains("888"), "{}", outcome.message);
}

#[tokio::test(start_paused = true)]
async fn a_hard_kill_checks_tracked_creation_time_after_reidentification() {
    struct ReusedTrackedProbe {
        reads: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Probe for ReusedTrackedProbe {
        async fn listeners(&self) -> std::io::Result<Vec<PortFact>> {
            Ok(Vec::new())
        }

        async fn process(&self, pid: u32) -> Option<OwnerFact> {
            let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(OwnerFact {
                pid,
                image: "sleeper.exe".to_string(),
                created_at: if read < 2 { 1_700_000_000_000 } else { 1_700_000_000_001 },
            })
        }
    }
    let app = read_app(&json!({ "name": "tracked", "start": { "program": "sleeper" } }));
    let mut rig = rig(vec![app.clone()], 999);
    rig.control.record.lock().unwrap().insert(app.name.clone(), TrackedRecord {
        pid: 777,
        created_at: 1_700_000_000_000,
        image: "sleeper.exe".to_string(),
    });
    Arc::get_mut(&mut rig.control).expect("unique control").deps.probe = Arc::new(ReusedTrackedProbe {
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    let outcome = rig.control.run_stop(&app, "panel", true).await;
    let killed = rig.killer.killed.lock().unwrap();
    assert!(killed.is_empty(), "a reused tracked pid must not be killed: {killed:?}");
    assert!(!outcome.ok, "a reused tracked pid must refuse the stop");
    assert!(outcome.message.contains("777"), "{}", outcome.message);
}

#[tokio::test(start_paused = true)]
async fn a_hard_kill_refuses_when_identity_changes_after_the_pin() {
    // The window between kill_targets pinning a target and
    // kill_identified re-reading it is the last place a reused pid can
    // slip in. The pin sees the original identity; the final re-read
    // sees a different creation time and must refuse without killing.
    struct PinThenReusedProbe {
        initial: Arc<FakeProbe>,
        reads: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Probe for PinThenReusedProbe {
        async fn listeners(&self) -> std::io::Result<Vec<PortFact>> {
            self.initial.listeners().await
        }

        async fn process(&self, pid: u32) -> Option<OwnerFact> {
            let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match self.initial.process(pid).await {
                // Reads 0..2 serve the early refusal, the verdict and the
                // pin; read 3 is kill_identified's re-verify, where the
                // pid now answers for a newer process.
                Some(fact) if read >= 3 => Some(OwnerFact { created_at: fact.created_at + 1, ..fact }),
                other => other,
            }
        }
    }
    let mut rig = rig(vec![otel_app()], 999);
    listen(&rig.probe, 4317, 777, "cc-otel.exe");
    listen(&rig.probe, 4318, 777, "cc-otel.exe");
    Arc::get_mut(&mut rig.control).expect("unique control").deps.probe = Arc::new(PinThenReusedProbe {
        initial: rig.probe.clone(),
        reads: std::sync::atomic::AtomicUsize::new(0),
    });
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", true).await;
    let killed = rig.killer.killed.lock().unwrap();
    assert!(killed.is_empty(), "a pid reused after the pin must not be killed: {killed:?}");
    assert!(!outcome.ok, "a changed identity after the pin must refuse the stop");
    assert!(
        outcome.message.contains("777")
            && outcome.message.contains("changed creation time or image"),
        "the refusal must come from the final re-read, not an earlier boundary: {}",
        outcome.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_hard_kill_targets_only_the_reresolved_owner() {
    let rig = rig(vec![otel_app()], 999);
    // A stale record names an old pid; the port's owner is someone else.
    // The kill goes to the OWNER (re-resolved now), not the record.
    listen(&rig.probe, 4317, 777, "cc-otel.exe");
    listen(&rig.probe, 4318, 777, "cc-otel.exe");
    rig.control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            "cc-otel".to_string(),
            TrackedRecord {
                pid: 111,
                created_at: 1,
                image: "cc-otel.exe".to_string(),
            },
        );
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", true)
        .await;
    assert!(outcome.ok, "{:?}", outcome.message);
    let killed = rig.killer.killed.lock().unwrap().clone();
    assert_eq!(killed, vec![777], "the re-resolved owner, alone");
    // A hard kill is a stop too: the capture carries the boundary header
    // the panel reads (SPEC §process.panel), not starts alone.
    let capture = rig.control.capture_file("cc-otel");
    let text = std::fs::read_to_string(&capture).unwrap_or_default();
    assert!(text.contains(" stop by panel ---"), "header written: {text}");
    // The stop closed the ports in the probe: fake it so down_state passes.
    rig.probe.state.lock().unwrap().listeners.clear();
}

/// One numeric port can have TWO owners (an IPv4 row and an IPv6 row, two
/// pids). The old find(port) took the first of each port only: the stop killed
/// half the owners, called success, and the app kept serving on the survivor.
/// Every matching row is a target now - deduped across ports, each identity
/// re-read, and success still reflects what is really listening.
#[tokio::test(start_paused = true)]
async fn a_hard_kill_takes_every_owner_of_each_port() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 700, "cc-otel.exe");
        listen(probe, 4317, 701, "cc-otel.exe"); // the second owner of 4317
        listen(probe, 4318, 702, "cc-otel.exe"); // the sole owner of 4318
    });
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", true)
        .await;
    assert!(outcome.ok, "{:?}", outcome.message);
    let mut killed = rig.killer.killed.lock().unwrap().clone();
    killed.sort_unstable();
    assert_eq!(killed, vec![700, 701, 702], "every owner of every declared port");
    // The stop closed the ports in the probe: fake it so down_state passes.
    rig.probe.state.lock().unwrap().listeners.clear();
}

/// A foreign co-owner of a port refuses the WHOLE stop: the identity check
/// covers every matching row, not the first, so the kill set never leaves a
/// stranger unexamined behind an honest-looking first owner.
#[tokio::test(start_paused = true)]
async fn a_foreign_second_owner_of_a_port_refuses_the_whole_stop() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 700, "cc-otel.exe");
        listen(probe, 4317, 701, "unrelated.exe"); // the foreign co-owner
        listen(probe, 4318, 702, "cc-otel.exe");
    });
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", true)
        .await;
    assert!(!outcome.ok, "one stranger in the owner set fails the stop");
    assert!(
        outcome.message.contains("held by pid 701") && outcome.message.contains("unrelated.exe"),
        "{:?}",
        outcome.message
    );
    assert!(rig.killer.killed.lock().unwrap().is_empty(), "nothing is killed when one owner is foreign");
}

/// One fallback rule, both phases (SPEC §process.apps): polling judges an
/// unreadable listener table by the spawned pid, and the settle used to demand
/// AllUp anyway - the same healthy process, misjudged dead two seconds later,
/// reported failed and never tracked. The settle falls back exactly like the
/// polling does, and the tracking pin is saved.
#[tokio::test(start_paused = true)]
async fn an_unreadable_listener_table_settles_by_the_spawned_pid_like_polling_does() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        let mut state = probe.state.lock().unwrap();
        state.listeners_err = true;
        state.procs.insert(
            4321,
            OwnerFact {
                pid: 4321,
                image: "cc-otel.exe".to_string(),
                created_at: 1_700_000_000_000,
            },
        );
    });
    let outcome = rig
        .control
        .run_start(&rig.control.app("cc-otel").unwrap(), "panel")
        .await;
    assert!(outcome.ok, "the settle misjudged a healthy process: {:?}", outcome.message);
    assert_eq!(outcome.pid, Some(4321), "the tracking pin is the spawned pid");
}

#[tokio::test(start_paused = true)]
async fn a_hard_kill_refuses_the_gateway_by_pid_and_by_image() {
    // By pid: the owner IS this gateway.
    let rig = rig_with(vec![otel_app()], 4242, true, |probe| {
        listen(probe, 4317, 4242, "swiss.exe");
        listen(probe, 4318, 4242, "swiss.exe");
    });
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", true)
        .await;
    assert!(!outcome.ok);
    assert!(outcome.message.contains("this gateway"), "{:?}", outcome.message);
    assert!(rig.killer.killed.lock().unwrap().is_empty());

    // By image: another pid whose image is the gateway's own.
    let rig = rig_with(vec![otel_app()], 4242, true, |probe| {
        listen(probe, 4317, 8888, "SWISS.EXE");
        listen(probe, 4318, 8888, "SWISS.EXE");
    });
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", true)
        .await;
    assert!(!outcome.ok);
    assert!(outcome.message.contains("the gateway's own"), "{:?}", outcome.message);
    assert!(rig.killer.killed.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_kill_survivor_is_reported_not_swallowed() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 777, "cc-otel.exe");
        listen(probe, 4318, 777, "cc-otel.exe");
    });
    // The kill leaves 777 alive (a survivor), and the fake probe agrees:
    rig.killer.survivors.lock().unwrap().push(777);
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", true)
        .await;
    assert!(!outcome.ok, "a survivor is a failed stop");
    assert!(outcome.message.contains("777"), "{:?}", outcome.message);
    assert!(outcome.message.contains("survived"), "{:?}", outcome.message);
}

fn stop_command_rig(completed: bool) -> Rig {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 777, "cc-otel.exe");
        listen(probe, 4318, 777, "cc-otel.exe");
    });
    rig.probe.state.lock().unwrap().procs.insert(4321, OwnerFact {
        pid: 4321,
        image: "stopper.exe".to_string(),
        created_at: 1_700_000_000_000,
    });
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(move |_| {
        let exits = if completed {
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = tx.send(0);
            Some(rx)
        } else {
            None
        };
        Ok(Spawned {
            pid: 4321,
            created_at: Some(1_700_000_000_000),
            shares_job: false,
            exits,
        })
    });
    rig
}

#[tokio::test(start_paused = true)]
async fn a_completed_stop_command_is_never_killed() {
    let rig = stop_command_rig(true);
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", false).await;
    let killed = rig.killer.killed.lock().unwrap();
    assert!(killed.is_empty(), "a completed stop command must not be killed: {killed:?}");
    assert!(!outcome.ok && outcome.can_force, "the app still runs: {outcome:?}");
}

async fn changed_stop_command(mode: &'static str) -> (Outcome, Vec<u32>) {
    let rig = stop_command_rig(false);
    let probe = rig.probe.clone();
    let changed = tokio::spawn(async move {
        // The identity changes while the command is waiting, not before
        // its initial pin was read. Tokio's paused clock owns this wait.
        tokio::time::sleep(STOP_COMMAND_WINDOW / 2).await;
        let mut state = probe.state.lock().unwrap();
        match mode {
            "reused" => state.procs.get_mut(&4321).unwrap().created_at += 1,
            "image" => state.procs.get_mut(&4321).unwrap().image = "node.exe".to_string(),
            "unreadable" => { state.procs.remove(&4321); }
            _ => panic!("unknown mode"),
        }
    });
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", false).await;
    changed.await.expect("identity changed");
    let killed = rig.killer.killed.lock().unwrap().clone();
    (outcome, killed)
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_stop_command_never_kills_a_reused_pid() {
    let (outcome, killed) = changed_stop_command("reused").await;
    assert!(killed.is_empty(), "a reused stop command pid must not be killed: {killed:?}");
    assert!(!outcome.ok && outcome.can_force, "the app still runs: {outcome:?}");
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_stop_command_never_kills_a_changed_image() {
    let (outcome, killed) = changed_stop_command("image").await;
    assert!(killed.is_empty(), "a changed stop command image must not be killed: {killed:?}");
    assert!(!outcome.ok && outcome.can_force, "the app still runs: {outcome:?}");
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_stop_command_never_kills_an_unreadable_pid() {
    let (outcome, killed) = changed_stop_command("unreadable").await;
    assert!(killed.is_empty(), "an unreadable stop command pid must not be killed: {killed:?}");
    assert!(!outcome.ok && outcome.can_force, "the app still runs: {outcome:?}");
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_stop_command_with_matching_identity_is_killed_and_offers_force() {
    let rig = stop_command_rig(false);
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", false).await;
    assert_eq!(*rig.killer.killed.lock().unwrap(), vec![4321]);
    assert!(!outcome.ok && outcome.can_force, "the app still runs: {outcome:?}");
}

#[tokio::test(start_paused = true)]
async fn a_stop_command_that_leaves_the_app_running_offers_force() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 10, "cc-otel.exe");
        listen(probe, 4318, 10, "cc-otel.exe");
    });
    // The stop command "runs" (the fake spawner counts it) and the ports
    // never close: 30 s later the row offers force.
    let outcome = rig
        .control
        .run_stop(&rig.control.app("cc-otel").unwrap(), "panel", false)
        .await;
    assert!(!outcome.ok);
    assert!(outcome.can_force, "{:?}", outcome);
    assert!(outcome.message.contains("force is available"), "{:?}", outcome.message);
    assert_eq!(rig.spawner.times("cc-otel"), 1, "the stop command ran");
    // The app itself was never killed - only the OVERRUNNING stop command
    // is ours to tree-kill (SPEC §process.apps); pid 10 (the app) must not
    // be in the kill list, whatever the stop command's own pid was.
    assert!(
        !rig.killer.killed.lock().unwrap().contains(&10),
        "the app was not killed: {:?}",
        rig.killer.killed.lock().unwrap()
    );
}

#[tokio::test(start_paused = true)]
async fn restart_ends_at_a_failed_stop() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        listen(probe, 4317, 10, "cc-otel.exe");
        listen(probe, 4318, 10, "cc-otel.exe");
    });
    rig.open_history().await;
    let outcome = rig
        .control
        .run_restart(&rig.control.app("cc-otel").unwrap(), "panel", false)
        .await;
    assert!(!outcome.ok, "{:?}", outcome);
    assert_eq!(outcome.action, "restart");
    assert!(outcome.message.contains("force is available"), "{:?}", outcome.message);
    // The start never ran: only the stop command's spawn happened.
    assert_eq!(rig.spawner.times("cc-otel"), 1);
    // ONE history line, the restart's.
    let lines = rig.history_read_paused(Some("cc-otel"), 10).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].action, "restart");
    assert!(!lines[0].ok);
}

#[tokio::test(start_paused = true)]
async fn start_with_swiss_runs_only_on_a_daemon_and_retries_once() {
    let app = read_app(&json!({
        "name": "flaky",
        "start": { "program": "flaky-server", "args": [] },
        "ports": [7003],
        "image": "flaky-server.exe",
        "autostart": true
    }));
    // A foreground gateway starts nothing at all.
    {
        let foreground = rig_with(vec![app.clone()], 999, false, |_| {});
        assert!(foreground.control.start_with_swiss().await.is_empty());
        assert_eq!(foreground.spawner.times("flaky-server"), 0);
    }

    // A daemon retries ONCE: the first spawn exits 1, the second listens.
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    let probe = rig.probe.clone();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts_for_closure = attempts.clone();
    *rig.spawner.on_spawn.lock().unwrap() = {
        let probe = probe.clone();
        Box::new(move |_spec| {
            let n = attempts_for_closure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let spawned = Spawned {
                pid: 700 + n as u32,
                created_at: Some(1),
                shares_job: false,
                exits: Some(rx),
            };
            if n == 0 {
                let _ = tx.send(1);
            } else {
                // The retry comes up listening before the window polls.
                let mut state = probe.state.lock().unwrap();
                state.listeners.push(PortFact {
                    port: 7003,
                    pid: 701,
                });
                state.procs.insert(
                    701,
                    OwnerFact {
                        pid: 701,
                        image: "flaky-server.exe".to_string(),
                        created_at: 1,
                    },
                );
            }
            Ok(spawned)
        })
    };
    let results = rig.control.start_with_swiss().await;
    assert_eq!(results.len(), 1);
    assert!(results[0].1.ok, "{:?}", results[0].1);
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn start_with_swiss_leaves_a_conflict_alone() {
    let rig = rig_with(vec![otel_app()], 999, true, |probe| {
        // A stranger already holds the port: refused with a conflict, and
        // the boot does NOT retry it into place.
        listen(probe, 4317, 99, "node.exe");
    });
    let results = rig.control.start_with_swiss().await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].1.ok);
    assert!(results[0].1.conflict, "{:?}", results[0].1);
    assert_eq!(rig.spawner.times("cc-otel"), 0, "no attempt, no retry");
}

#[tokio::test(start_paused = true)]
async fn an_already_stopped_pass_starts_nothing() {
    // The stop signal a disabled plugin's scope hands the pass: spent
    // before the first app, so not one spawn and not one history line.
    let rig = rig(vec![otel_app()], 999);
    rig.open_history().await;
    let (_tx, cancel) = tokio::sync::watch::channel(true);
    let results = rig.control.start_with_swiss_until(Some(cancel)).await;
    assert!(results.is_empty(), "{results:?}");
    assert_eq!(rig.spawner.times("cc-otel"), 0);
    assert!(rig.history_read_paused(None, 10).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stop_landing_in_the_retry_gap_cancels_the_retry() {
    // The flaky app of the retry test: first spawn exits 1, so the pass
    // parks in the 60 s gap - where a disable lands and the retry dies
    // unstarted (no second spawn, no second history line).
    let app = read_app(&json!({
        "name": "flaky",
        "start": { "program": "flaky-server", "args": [] },
        "ports": [7003],
        "image": "flaky-server.exe",
        "autostart": true
    }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    let (tx, rx) = tokio::sync::watch::channel(false);
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(|_spec| {
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        let _ = exit_tx.send(1);
        Ok(Spawned {
            pid: 700,
            created_at: Some(1),
            shares_job: false,
            exits: Some(exit_rx),
        })
    });
    let control = rig.control.clone();
    let pass = tokio::spawn(async move { control.start_with_swiss_until(Some(rx)).await });
    for _ in 0..200 {
        if rig.spawner.times("flaky-server") == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(rig.spawner.times("flaky-server"), 1, "the first attempt ran");
    tx.send(true).expect("the stop signal fires");
    let results = pass.await.expect("the pass ends");
    assert_eq!(rig.spawner.times("flaky-server"), 1, "the retry was canceled, not run");
    // A failed first attempt is reported by its RETRY alone (the boot
    // pass's own rule), so a pass canceled in the gap reports nothing -
    // and starts nothing more.
    assert!(results.is_empty(), "no retry outcome is reported: {results:?}");
}

fn exit_line(app: &str, pid: u32, code: i32) -> ExitLine {
    ExitLine {
        app: app.to_string(),
        pid,
        code,
        at: 0,
    }
}

/// A boot sets the table first and reads the record second. The first `set_apps` used to save the
/// record it held, still empty, so every gateway erased what the last one left: a running app
/// without ports read as `unknown` and could not be stopped (SPEC §process.apps; 2026-11-06, walked
/// in WSL).
#[tokio::test]
async fn a_new_gateway_reads_the_record_the_last_one_left() {
    let app = read_app(&json!({ "name": "svc", "start": { "program": "svc" } }));
    let last = rig(vec![app.clone()], 999);
    last.control.record.lock().unwrap().insert(app.name.clone(), TrackedRecord {
        pid: 50,
        created_at: 7,
        image: "svc".to_string(),
    });
    last.control.save_tracked();
    last.probe.state.lock().unwrap().procs.insert(50, OwnerFact {
        pid: 50,
        image: "svc".to_string(),
        created_at: 7,
    });

    let next = AppControl::new(ControlDeps {
        rules: ConfigRules {
            gateway_port: 19999,
            gateway_image: "swiss.exe".to_string(),
        },
        boot: BootInfo {
            port: 19999,
            is_daemon: true,
        },
        probe: last.probe.clone(),
        spawner: last.spawner.clone(),
        killer: last.killer.clone(),
        clock: Arc::new(FakeClock),
        record_path: last.dir.join("record.json"),
        history: last.history.clone(),
        capture_dir: last.dir.join("apps"),
        gateway_pid: 1000,
    });
    next.set_apps(vec![app]);
    next.load_tracked().await;

    let rows = next.rows().await;
    assert_eq!(rows[0].status, "running", "{}", rows[0].status);
    assert_eq!(rows[0].pid, Some(50));
}

#[tokio::test(start_paused = true)]
async fn an_exit_line_only_lands_for_the_tracked_process() {
    let app = read_app(&json!({ "name": "svc", "start": { "program": "svc" } }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    rig.control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            "svc".to_string(),
            TrackedRecord {
                pid: 50,
                created_at: 1,
                image: "svc.exe".to_string(),
            },
        );
    // A stop command's pid (or a dead launcher's) is NOT the tracked
    // process: no history line, no lastAction rewrite, nothing.
    rig.control.handle_exit(&exit_line("svc", 51, 0));
    assert!(rig.history_read_paused(Some("svc"), 10).await.is_empty());
    // The tracked process's death lands once and drops the record.
    rig.control.handle_exit(&exit_line("svc", 50, 3));
    let lines = rig.history_read_paused(Some("svc"), 10).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].action, "exit");
    assert_eq!(lines[0].exit_code, Some(3));
    assert!(rig
        .control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get("svc")
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn a_suppressed_pid_writes_no_exit_line_even_when_tracked() {
    let app = read_app(&json!({ "name": "svc", "start": { "program": "svc" } }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    rig.control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            "svc".to_string(),
            TrackedRecord {
                pid: 60,
                created_at: 1,
                image: "svc.exe".to_string(),
            },
        );
    rig.control
        .suppress
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(60);
    rig.control.handle_exit(&exit_line("svc", 60, 0));
    assert!(rig.history_read_paused(Some("svc"), 10).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_stop_on_an_unknown_app_is_refused() {
    // No ports, never started: unknown, and unknown is a refusal (SPEC
    // §process.apps) - not an ok, and not a spawned stop command either.
    let app = read_app(&json!({ "name": "ghost", "start": { "program": "ghost" } }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    let outcome = rig
        .control
        .run_stop(&rig.control.app("ghost").unwrap(), "panel", false)
        .await;
    assert!(!outcome.ok);
    assert!(!outcome.can_force);
    assert!(
        outcome.message.contains("nothing identified to stop"),
        "{:?}",
        outcome.message
    );
    assert_eq!(rig.spawner.times("ghost"), 0);
}

#[tokio::test(start_paused = true)]
async fn a_forced_restart_skips_the_stop_command() {
    let rig = stop_command_rig(true);
    let probe = rig.probe.clone();
    // The world plays honestly: the force-kill lands at once, the ports
    // free a beat later (inside the stop's own wait for them), and the
    // restarted app binds them anew under a new pid a beat after ITS
    // spawn - the start's pre-check must see closed ports, never the old
    // owner. The paused clock reaches each pending timer in order.
    tokio::spawn(async move {
        tokio::time::sleep(POLL_STEP).await;
        let mut state = probe.state.lock().unwrap();
        state.listeners.clear();
        state.procs.remove(&777);
    });
    let probe = rig.probe.clone();
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(move |_spec| {
        let probe = probe.clone();
        tokio::spawn(async move {
            tokio::time::sleep(POLL_STEP).await;
            let mut state = probe.state.lock().unwrap();
            state.listeners.push(PortFact { port: 4317, pid: 4322 });
            state.listeners.push(PortFact { port: 4318, pid: 4322 });
            state.procs.insert(
                4322,
                OwnerFact {
                    pid: 4322,
                    image: "cc-otel.exe".to_string(),
                    created_at: 1_700_000_000_099,
                },
            );
        });
        Ok(Spawned {
            pid: 4321,
            created_at: Some(1_700_000_000_000),
            shares_job: false,
            exits: None,
        })
    });
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_restart(&app, "panel", true).await;
    // Force took the kill path to the port's owner, then the start ran:
    // the restart completed without honoring the declared stop command.
    assert!(outcome.ok, "{:?}", outcome.message);
    assert!(outcome.message.contains("restarted"), "{:?}", outcome.message);
    assert!(
        rig.killer.killed.lock().unwrap().contains(&777),
        "the owner was force-killed: {:?}",
        rig.killer.killed.lock().unwrap()
    );
}

#[tokio::test(start_paused = true)]
async fn start_with_swiss_leaves_a_running_app_alone_with_no_history_lines() {
    let app = read_app(&json!({
        "name": "runner",
        "start": { "program": "runner-server", "args": [] },
        "ports": [7010],
        "image": "runner-server.exe",
        "autostart": true
    }));
    let rig = rig(vec![app], 999);
    rig.open_history().await;
    listen(&rig.probe, 7010, 55, "runner-server.exe");
    let results = rig.control.start_with_swiss().await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].1.ok, "recorded, not attempted");
    assert!(
        results[0].1.message.contains("left alone at boot: running"),
        "{:?}",
        results[0].1.message
    );
    assert_eq!(rig.spawner.times("runner-server"), 0);
    // The whole point: a running survivor of the last gateway must not
    // wake to failed history lines on every boot.
    assert!(rig.history_read_paused(Some("runner"), 10).await.is_empty());
}

struct LateBlindProbe {
    initial: Arc<FakeProbe>,
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl Probe for LateBlindProbe {
    async fn listeners(&self) -> std::io::Result<Vec<PortFact>> {
        let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if read < 3 {
            self.initial.listeners().await
        } else {
            Err(std::io::Error::other("the table went unreadable"))
        }
    }

    async fn process(&self, pid: u32) -> Option<OwnerFact> {
        self.initial.process(pid).await
    }
}

#[tokio::test(start_paused = true)]
async fn a_stop_command_never_claims_stopped_on_an_unreadable_table() {
    // The verdict saw the app running, the stop command completed, and
    // THEN the table went unreadable: unknown is never stopped (SPEC
    // §process.apps), so the stop fails offering force instead of
    // claiming the command won.
    let mut rig = stop_command_rig(true);
    Arc::get_mut(&mut rig.control).expect("unique control").deps.probe = Arc::new(
        LateBlindProbe {
            initial: rig.probe.clone(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        },
    );
    let app = rig.control.app("cc-otel").unwrap();
    let outcome = rig.control.run_stop(&app, "panel", false).await;
    assert!(!outcome.ok);
    assert!(outcome.can_force, "{:?}", outcome.message);
    assert!(
        outcome.message.contains("the listener table cannot be read"),
        "{:?}",
        outcome.message
    );
}

#[tokio::test(start_paused = true)]
async fn a_port_held_by_pid_0_is_a_stranger_in_the_window() {
    // The Linux shape (§process.detached): another user's socket reads as
    // pid 0, which is never "mine" - even when it grabs every declared
    // port the window cannot settle an ok on it. (It binds AFTER the
    // pre-start check, or the start would refuse as a conflict before
    // the window.)
    let rig = rig(vec![otel_app()], 999);
    let probe = rig.probe.clone();
    *rig.spawner.on_spawn.lock().unwrap() = Box::new(move |_spec| {
        let probe = probe.clone();
        tokio::spawn(async move {
            tokio::time::sleep(POLL_STEP).await;
            let mut state = probe.state.lock().unwrap();
            state.listeners.push(PortFact { port: 4317, pid: 0 });
            state.listeners.push(PortFact { port: 4318, pid: 0 });
        });
        Ok(Spawned {
            pid: 5,
            created_at: Some(1),
            shares_job: false,
            exits: None,
        })
    });
    let outcome = rig
        .control
        .run_start(&rig.control.app("cc-otel").unwrap(), "panel")
        .await;
    assert!(!outcome.ok, "a stranger's port never settles an ok");
    assert!(
        outcome.message.contains("not listening on"),
        "{:?}",
        outcome.message
    );
    assert!(rig
        .control
        .record
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get("cc-otel")
        .is_none());
}
