# Running it

Part 05 of [§host](README.md), chapter 0004 of the [swiss specification](../../SPEC.md).

## §host.gui — The panel in its own window

`swiss gui` — the sign-in link `open` mints, spent by a window instead of a tab. On Windows
and macOS (ADR-038), tao's message loop, wry's system WebView2/WKWebView surface and tray-icon
provide the shell; no browser engine, web bundler or updater rides along. Linux keeps the
headless shape — the gateway serves, whatever browser is at hand looks.

On Windows or macOS, launching the executable without arguments takes the same GUI path.
The macOS `swiss.app` bundle names `Contents/MacOS/swiss` as its executable, so Finder opens
the window directly; `swiss start` retains its explicit daemon/browser behavior.
An explicit `swiss gui` instead re-enters the binary as `gui --foreground`, with its streams in
the home's `gui-<port>.log` and a detached process session, reports the child's pid and returns
like `swiss start`; it never holds the terminal or a wrapper's output pipe until the window exits. `gui -f` keeps the native
event loop in the calling process for debugging. A double-clicked app stays in its own process,
so Finder/Explorer continues to own the application lifetime. Early window/WebView2 failures
use a native error dialog on Windows. Explicit commands and `--help`/`--version` keep their CLI
dispatch; `gui --help` and `gui --version` finish without opening a window.

- **Startup.** The command resolves its gateway exactly like every other (`--port`, then
  `SWISS_PORT`, then the home's config, then 19999) and runs the same idempotent
  `start_daemon` sequence on a side thread, so the window pumps from the first frame, loads
  the one-time link the moment it is minted, and shows the reason and the log tail in-page
  when the gateway never comes up (`gui::tests::a_gateway_that_never_comes_up_reports_the_log_tail`).
- **One window per gateway.** The single-instance mutex is named by port and home, not by
  desktop: a second `swiss gui` under another `SWISS_HOME` or another port is another gateway
  and gets its own window — a config switch can never end up looking at the standing one —
  while a second launcher for the SAME gateway signals a named event, brings the standing
  window (hidden in the tray, since closing hides) to the front, and exits
  (`gui::tests::one_window_per_gateway_not_per_desktop`).
- **macOS instance and Dock.** A file lock and private Unix socket under the selected home
  enforce the same per-gateway identity; a second launch raises the owner, and a stale socket
  is removed only after claiming the lock (`gui::unix_instance::tests`). Reopening from the
  Dock restores the hidden window. The application menu supplies About, Hide and Quit.
- **Tray.** Open — and a double-click on the icon — re-shows the window; Exit is the way out,
  because closing hides to the tray: the gateway outlives windows. The tray's and the title
  bar's icon is the brand mark itself (`assets/logo.svg`), drawn in code from the SVG's own
  geometry — tile, fanned tools, rivet (`gui::tests::the_brand_icon_draws_the_mark_not_a_plain_tile`).
- **Webview profile.** The webview keeps its supported profile data under `<home>/gui-webview`, following
  `SWISS_HOME` like every file the tool writes — never beside the exe.
  On macOS, WKWebView instead uses a persistent website-data-store identifier derived from
  the selected home and port, isolating cookies and localStorage between gateway windows
  (`gui::tests::the_mac_webview_store_is_stable_and_isolates_homes_and_ports`).
- **Window integration.** The native caption follows the panel's resolved light/dark theme
  and plugin name, while Windows keeps ownership of dragging, resizing, snapping and the
  window buttons. The narrow WebView bridge accepts only theme, caption and zoom messages
  from `http://127.0.0.1:<this port>/`; other origins, paths, ports and commands are refused.
  The startup HTML cannot be revisited after panel navigation: Back and Forward still traverse
  the panel's own history. The desktop replaces browser context menus with the panel's localized
  menu for editing, selected text and links, leaving page-owned menus and terminal copy/paste in
  control; browser tabs retain their own context menus (`desktop.test.ts`).
- **Window preferences.** `<home>/gui-window-<port>.json` holds the normal client size in
  logical pixels, physical position, maximized state, last caption theme and text zoom.
  A corrupt file falls back to defaults. A disconnected monitor's position is discarded;
  startup fits the saved position's connected screen (falling back to the primary screen)
  and keeps a 960×640 logical-pixel minimum. Ctrl+plus,
  Ctrl+minus and Ctrl+0 change/reset text zoom (80–150%), persisted across window launches.
  Closing, losing focus and the tray's Exit save preferences without changing gateway state.
  Normal placement is sampled after the native message batch settles, so maximize's
  intermediate border coordinates never replace the normal window's restore position.
  `gui::desktop::tests` covers preference recovery, monitor removal and bridge validation;
  native caption theme, minimize/restore and persistence are walked on 19998 (§testing.live).

## §host.daemon — Daemon, pid file, logs, build stamp

- **Detach.** `swiss start`, and any `swiss setup` or `swiss connect` that has to start a
  gateway, spawns `serve` with no job object: `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW` on
  Windows, a new session (`detach_command`, `setsid`) on Unix. So the daemon survives the CLI and
  the terminal or Agent shell that ran it. Only "my own child is still alive" counts as started —
  the loser of a racy double start cannot report success.
- **Attribution.** `start` hands its own pid and argv to the daemon in `SWISS_SPAWNER` (removed
  from the environment at boot); the first log line, `gateway booted`, records it with the
  parent process, so "who started this instance" is answerable after the fact.
- **The daemon signal.** `SWISS_SPAWNER`'s presence at boot, read before it is removed, is
  also what makes this process a daemon. It becomes `BootInfo {port, is_daemon}` in
  `RuntimeServices`, set once by `server.rs`; a test composition sets it explicitly.
  `is_daemon` is true only for a `serve` that `swiss start` spawned, the start at sign-in
  included. A foreground `start -f` and a bare `serve`, the test instance among them, are not
  daemons. Starting apps with swiss is the one thing it gates (§process.apps).
- **Std handles.** Both the detaching `start` and the `serve` path call
  `keep_std_handles_from_children()` before any spawn. A daemon's stdout and stderr are its
  log file, and a child that outlives the gateway (§process.detached) must not hold
  `gateway-<port>.log` open.
- **Launcher noise.** The daemon's environment is the user's, not the launching shell's. One
  list (`swiss_core::env`, case-insensitive) is removed on both paths (`start` via
  `env_remove`, `serve` in-process before any thread): `NO_COLOR`, `CI`, `TF_BUILD`,
  `GITHUB_ACTIONS`, `CLAUDECODE` and `CLAUDE_CODE_*`, `DSH_*`, `WT_SESSION`/`WT_PROFILE_ID`,
  `TERM_PROGRAM*`. It is a blocklist: `PATH`, `HOME`, proxies and `SWISS_*` stay.
- **PATH repair** at boot adds `~/.local/bin`, `%APPDATA%\npm` and `~/.cargo/bin`. Command
  resolution follows `PATHEXT` and never tries extensionless spellings on Windows (os error
  193).
- **Pid file** `gateway-<port>.pid`: five fields, parsed strictly (a partial record is
  rejected, a torn file means not running).
- **Stop.** `POST /api/shutdown` → wait up to 10 s for the port to close → tree-kill. A live pid
  that is not serving is refused a blind kill without `--force`.
- **Status** reports the running build (`/health`) against the binary on disk (`--version` of
  the pid file's entry) and says "restart to pick it up" when they differ; `--json` carries
  `build` and `diskBuild`.
- **Build stamp.** `build.rs` sets `SWISS_GIT_HASH` (`<short>` or `<short>-dirty`) and
  `SWISS_BUILD_TIME` (RFC 3339 UTC), both `unknown` without git — the build never fails for it.
  `swiss --version` prints `swiss <version> (<hash>, <time>)`.
- **Logs.** JSON lines `{ts, level, msg, extra?}` on stderr, redirected by the daemon into
  `gateway-<port>.log`, rotated at 10 MB keeping one generation. Stdout belongs to the command, so
  a state migration that logs during `swiss export` or `swiss api` cannot reach a redirected
  document. `swiss logs -f` follows by polling (Windows change notifications are unreliable for
  appends).
- **First run** creates the private home, seeds the echo config and ensures a token exists.
- **Orphan reaping.** The kill-on-close job holds only while the gateway lives; after a hard
  kill the next boot tree-kills the pids in `.proc-pids-<port>.json` — after probing the port
  (a live instance's ledger is left alone) and skipping any live pid that is not a descendant.

## §host.ops — Production, the test instance, deploys

- **19999 is production.** It runs from `bin\swiss.exe`, a copy of the build output that git
  ignores — never from `target\`. The linker and the daemon never share a file, so a build
  happens while the old daemon serves. `bin\` is the main checkout's, also for a deploy run
  from a linked worktree: that worktree's tree is built, and the exe lands where PATH and
  `swiss autostart on` look.
- **Deploy** only with `scripts/deploy.ps1`: gates (unit tests, the integration suite,
  clippy; `-SkipGates` skips all) → release build → stop → copy into `bin\` (retried while the
  old process releases its handle) → start → status → prove `/health`'s `build.hash` equals
  `bin\swiss.exe --version`, or fail with both values. The outage is stop + copy + start, and
  the script prints it. `-Quick` is the operator's quick deploy, to see a change live: no
  gates, and the `quick` profile (the release profile without LTO, 16 codegen units,
  incremental, into `target\quick`) in place of the release build — minutes sooner, a larger
  exe, the same proof; the release profile stays the shipping build.
- **The stamp names the tree.** `build.rs` re-runs only when HEAD or a branch ref moves, so a
  build's `-dirty` mark can outlive the change that caused it, or miss an edit made after it.
  Before the build the script asks git what the stamp should be (`git rev-parse --short HEAD`,
  `-dirty` while `git status --porcelain` lists anything) and touches `build.rs` when the last
  build's stamp differs; after it, a binary stamped otherwise fails the deploy before the stop,
  production untouched. A dirty tree still deploys, and the script says so.
- **19998 is the test instance** (`scripts/test-instance.ps1`): `-Start` copies the state files
  (not pid, log or terminal files) into `%LOCALAPPDATA%\swiss-test-home`, sets `SWISS_HOME` and
  `SWISS_PORT=19998` and serves; `-Stop` kills **only the process owning port 19998**, never by
  image name; `-Fresh` starts from an empty home. The copy works because DPAPI binds
  `master.key` to machine + user, not directory. Saves on 19998 touch only its own home.
- **The test instance never manages the operator's apps.** Once `/health` answers, `-Start`
  replaces `plugins.process.config.apps` through the API with the demo apps of
  §process.verify. The copied list would otherwise show the real cc-otel, whose Stop would
  reach the real process. A `serve` never starts apps on its own (§host.daemon), so nothing
  real starts in the meantime.
