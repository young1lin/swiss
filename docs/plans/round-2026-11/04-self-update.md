# 04 — `swiss update --apply`: an incremental self-update that never runs in the gateway

Implementation brief, not SPEC. It is delivered in **three sessions, U1 → U2 → U3**, each
reviewed before the next starts. The SPEC amendments in §9 land in the commit that makes each
paragraph true. Shared rules: [00-handoff.md](00-handoff.md). Depends on 02 (the About section
and `build_info`) and 03 (the skill refresh it calls).

## 1. Goal

The owner's words: "swiss update 能做成增量更新就更好了，每次只更新小部分" — `swiss update` should
install the new release itself, downloading only a small part each time.

Today `swiss update` only checks (`src/update_check.rs`), and the README lists "No
self-updating binary" as a non-goal, because "a downloader inside a resident process would be the
opposite of the memory budget". This brief keeps the reason and drops the conclusion: the
**download, patch and swap run in a short-lived CLI process, never in the resident gateway**.
ADR-041 records the change (§9).

## 2. Facts (verified 2026-11-12)

- Release CI is `.github/workflows/build.yml`:
  - `dist` builds five targets with `cargo build --release --locked --target <t>` and packs
    `swiss-<ver>-<target>.zip` (Windows) or `.tar.gz` (the rest). macOS also gets
    `swiss-<target>.app.zip`.
  - `release` (ubuntu) downloads every artifact, writes `SHA256SUMS` with
    `sha256sum swiss-*.zip swiss-*.tar.gz SETUP.md`, and publishes with
    `softprops/action-gh-release@v3`.
  - Releases v0.2.1 and v0.2.2 exist.
- The release profile is `opt-level="z"`, fat LTO, `codegen-units=1`, `strip="symbols"`. Fat LTO
  moves code around between versions, so **how small a patch is must be measured, not assumed**.
  That measurement is gate E0 in U1.
- The binaries are not code-signed, so the bytes are fully determined by the build. macOS
  `.app` bundles are ad-hoc signed by `scripts/package_macos.py`.
- `zstd 0.13.4` is in `Cargo.lock` (used by swiss-memory and swiss-plugins) but is not a root
  dependency. `zstd::stream::read::Decoder::with_ref_prefix(reader, &[u8])` and
  `raw::Encoder::with_ref_prefix` are **not** behind the `experimental` feature (checked in the
  registry source), and `DParameter::WindowLogMax` is stable. A ref-prefix decode is how
  `zstd --patch-from` patches are applied (the CLI uses `ZSTD_DCtx_refPrefix` in patch-from mode).
- `src/daemon.rs`:
  - `StartOptions {port, entry: Option<PathBuf>, args, timeout_ms}`. `entry` defaults to
    `current_exe()` (`server_entry()`).
  - `start_daemon` persists the port when `port` is `Some`, so pass `None`.
  - `stop_daemon(StopOptions)` POSTs `/api/shutdown`, waits up to 10 s, then
    `tree_kill(pid)`. `tree_kill` skips the caller's own pid on both platforms.
- `src/pidfile.rs` `PidRecord {pid, port, entry, node, started_at}`. `entry` is the absolute path
  of the exe that serves, so the updater can tell whether the gateway on the port runs **this**
  binary.
- `swiss-host/src/services/detached.rs` `spawn_detached(&DetachedSpec)` is the only way to
  start something that outlives the gateway.
  - It returns `DetachedChild {pid, created_at, child, shares_job}`.
  - `shares_job: true` means the breakaway was refused, and the child **dies with the gateway**.
- SPEC §host.session: the panel's cookie signing key is kept across restarts (sealed
  `session.json`), so a signed-in page stays signed in through a restart. `bus/link.ts:286`
  reloads the page when the build changes after a reconnect.
- The HOST bus module has subs 1–6. The next free one is `HOST_SUB_UPDATE = 7` in
  `crates/swiss-bus/proto/swiss/bus/v1/host.proto`.
- `current_exe()` on Linux resolves symlinks, so a `swiss path on` symlink in `~/.local/bin`
  leads to the real file, and replacing that file keeps the symlink valid.

## 3. Decisions

- **D1 — patch format:** `zstd --patch-from` (the old exe is the reference prefix), level 19,
  with the window sized to cover the old exe.
  - It was chosen over bsdiff (slow, memory-hungry, extra crate) and Courgette (Windows-only,
    huge).
  - It was chosen over rsync/zsync because those need a server or a block index, and GitHub
    serves static files. The remote delta upload of 136a3b7f solves a different problem: it has
    python on the target.
  - The decoder is already in the dependency graph.
- **D2 — base identity:** the SHA-256 of the local exe file. A patch applies only when
  `fromSha256` equals that hash. Any other exe (a dev build, a hand-patched one, an older release
  with no patch) gets the full download.
- **D3 — the manifest:** one `update.json` per release, read from
  `https://github.com/young1lin/swiss/releases/latest/download/update.json`. Schema in §4.2. It
  names assets, not URLs. The client builds
  `https://github.com/young1lin/swiss/releases/download/<tag>/<asset>`.
- **D4 — which patches:**
  - Patches go from each of the previous **3** releases to the new one, per target.
  - A patch bigger than 60 % of the full `.zst` is not published, because the full download is
    then the better deal.
  - The full `swiss-<ver>-<target>.zst` is always published. It is the fallback and already
    smaller than the zip.
- **D5 — where it runs:** only in a CLI process: `swiss update --apply`.
  - The panel's Update button makes the gateway spawn that same CLI detached and then step back.
  - The resident gateway never downloads, never holds an exe in memory and never patches.
- **D6 — replacing the exe:**
  1. Write `<exe>.new` in the exe's own directory, so the rename is on the same volume.
  2. Verify its SHA-256.
  3. Probe-run `<exe>.new --version` and parse the version line.
  4. Rename `<exe>` → `<exe>.old`.
  5. Rename `<exe>.new` → `<exe>`. If this rename fails, rename `.old` back.

  Notes:
  - Windows allows renaming a running exe (not deleting it), so this works while the CLI and the
    gateway still run the old file.
  - The probe catches an antivirus quarantine (Bitdefender deleted `bin/swiss.exe` here once) and
    Smart App Control refusals (os error 4551) **before** anything is renamed.
  - No `self-replace` crate: the dance is about 60 lines and must be testable with failure
    injection.
- **D7 — refusals (exit 2, nothing touched):**
  - A dev build, unless `--force`:
    - `build.rs` stamps `SWISS_CHANNEL`, which is `dev` unless the env var says otherwise.
    - The CI `dist` Build step sets `SWISS_CHANNEL=release`.
    - The owner's `deploy.ps1` builds are dev, so they stay on deploy.ps1.
  - An exe inside a macOS `.app` bundle (`…/X.app/Contents/MacOS/…`): the user gets the manual
    steps.
  - An exe directory that is not writable (probe with a temp file).
  - Another update holding `update.lock`.
  - The gateway on the port runs a **different exe** than the one being replaced (PidRecord
    `entry`): the refusal names both paths.
- **D8 — the sequence:** in §5.2. Everything that can fail runs before anything is stopped.
- **D9 — reporting:**
  - Every attempt writes `<data_dir>/update/last.json`, ok or not.
  - The gateway exposes `GET /api/update/status` and `POST /api/update/apply`, and publishes
    `HOST.UPDATE` (a NOTICE: empty body, the page re-reads).
- **D10 — trust:** HTTPS to an allowlisted host, plus a SHA-256 for every asset from the manifest.
  - `update.json` is listed in `SHA256SUMS` like every other asset.
  - A signed manifest (ed25519, public key in the binary) is the documented next step. It is out
    of scope here and goes in ADR-041 as an open item.
- **D11 — direction:** only a strictly newer version applies.
  - `--force` additionally allows the **same** version, as a reinstall or repair, and also the
    dev-build refusal.
  - Nothing ever downgrades.
- **D12 — download limits:**
  - The manifest may be at most 1 MiB, and an asset at most 256 MiB.
  - Downloads stream to disk with a running SHA-256.
  - Timeouts: connect 10 s, between reads 30 s, 10 min for the whole download.
  - Redirects: at most 5, only to `github.com` and `*.githubusercontent.com`.
  - The decoder has `WindowLogMax(28)`. A patch that needs more is refused, not decoded.
- **D13 — temp files:**
  - On any failure the attempt removes its `.new`, its downloads under `<data_dir>/update/` and
    any `.old` it created and rolled back.
  - At boot, the gateway deletes `<exe>.old*` siblings best-effort. Windows cannot delete a
    running image, so a GUI still running the old file only delays this.

## 4. U1 — release assets and the manifest (CI only)

### 4.0 Gate E0: measure before building anything

Measure in WSL (install `zstd` there if it is missing) or in a scratch workflow run:

1. Download the v0.2.1 and v0.2.2 archives for `x86_64-pc-windows-msvc` and
   `x86_64-unknown-linux-gnu` from the public release pages (`curl -L`), and extract the exes.
2. Run `zstd -19 --long=27 --patch-from=<old> <new> -o p.zst` and `zstd -19 --long=27 <new> -o full.zst`.
3. Report, per target: the exe size, zip size, `full.zst` size, patch size, patch ÷ full ratio and
   the time each command took.

**If the Windows ratio is ≥ 0.6, stop and report** (the owner decides whether patches are worth
it, or whether the full `.zst` alone is the deliverable). Below 0.6, go on.

### 4.1 The release job

Extend `release` (keep `dist` unchanged unless you need to):

1. `sudo apt-get install -y zstd`.
2. For each of the four non-bundle targets, extract the new exe from its archive and compress
   it: `zstd -19 --long=27` → `swiss-<ver>-<target>.zst`.
3. Find the previous 3 published, non-draft, non-prerelease tags lower than the current tag, with
   `gh release list` and `GH_TOKEN: ${{ github.token }}`.
4. For each of those tags and each target:
   1. `gh release download <tag> --pattern …` the archive.
   2. Extract the exe and build `swiss-<from>-to-<ver>-<target>.patch.zst` with
      `zstd -19 --long=27 --patch-from=<old> <new>`.
   3. Verify it immediately with `zstd -d --long=27 --patch-from=<old>`, and require the output
      to compare equal to the new exe (`cmp`).
   4. Drop the patch if it is over 60 % of the full `.zst` (D4).
5. Generate `update.json` with `scripts/release_manifest.py` (python3, standard library only,
   following `scripts/package_macos.py`). It reads a directory of exes, fulls and patches and
   writes the manifest.
6. Add `update.json`, `*.zst` and `*.patch.zst` to the `sha256sum` line, and keep
   `sha256sum -c SHA256SUMS` after it.
7. Publish as today (`files: dist-bin/*` already takes everything in the directory).

The first release with this job has v0.2.1 and v0.2.2 as its bases. Neither has a manifest,
which does not matter: bases are found by tag, not by manifest.

### 4.2 `update.json`

```json
{
  "schema": 1,
  "version": "0.2.3",
  "tag": "v0.2.3",
  "notes": "https://github.com/young1lin/swiss/releases/tag/v0.2.3",
  "targets": {
    "x86_64-pc-windows-msvc": {
      "exe": "swiss.exe",
      "sha256": "<sha256 of the new exe>",
      "size": 31234567,
      "full":    { "asset": "swiss-0.2.3-x86_64-pc-windows-msvc.zst", "sha256": "…", "size": 9876543 },
      "patches": [
        { "from": "0.2.2", "fromSha256": "<sha256 of the 0.2.2 exe>",
          "asset": "swiss-0.2.2-to-0.2.3-x86_64-pc-windows-msvc.patch.zst",
          "sha256": "…", "size": 1234567, "windowLog": 27 }
      ]
    }
  }
}
```

- Every size is in bytes, and every hash is lowercase hex.
- Readers ignore fields they do not know.
- A higher `schema` means "this client is too old for that release": print the manual steps,
  exit 1.

### 4.3 U1 tests and proof

- `scripts/test_release_manifest.py` (unittest, the `test_package_macos.py` pattern) covers:
  - the manifest generated from a fixture directory of small fake files;
  - the 60 % drop rule;
  - a refusal on a missing full asset;
  - stable key order.
- The workflow cannot be fully run without tagging. Prove it with `workflow_dispatch` on a
  branch, or with `act` if available, **or** run the release-job script body by hand in WSL over
  the E0 downloads, and attach the resulting `update.json` and the file listing to the report.
  Do not push a tag, and do not create a release.
- `update_check`'s existing tests stay green (U1 touches no Rust).

## 5. U2 — the CLI

### 5.1 Commands

- `swiss update` stays the check, with exit codes as today. When the newest release has an
  `update.json`, it adds:
  - `download: patch from 0.2.2, 1.4 MB (full 9.9 MB)`, or `download: full, 9.9 MB`;
  - `run "swiss update --apply" to install it`.

  On a dev build it says `this build was made from source (channel dev); --apply refuses it`.
- `swiss update --apply [--force]` runs §5.2.
- Exit codes:
  - 0: updated, or already the newest;
  - 1: failed, with nothing changed or rolled back;
  - 2: refused (D7).
- Stable output lines, which scripts and the panel's log read. One per phase, in this order:

```
current: 0.2.2 (a5a12a9c)
latest: v0.2.3
download: patch from 0.2.2, 1.4 MB
verified: sha256 ok, probe says swiss 0.2.3 (b1c2d3e4, 2026-11-20T10:00:00Z)
replaced: C:\Users\jdoe\bin\swiss.exe (previous kept as swiss.exe.old)
gateway: restarted on 19999, serving 0.2.3 (b1c2d3e4)
skills: 3 copies refreshed in 2 seats
```

`gateway:` may instead read `not running, left stopped` or `runs another exe (<path>), left alone`.

### 5.2 The sequence

1. **Refusals (D7):** channel, bundle, writable directory, and `update.lock`. The lock is
   `<data_dir>/update/update.lock`, made with `create_new`, holding the pid; a lock whose pid is
   dead is stale and is taken over. Then the PidRecord exe check.
2. **Manifest:** fetch it, pick `targets[SWISS_TARGET]` (no entry: print the manual steps, exit
   1), compare versions (D11), and pick the patch whose `fromSha256` equals the local exe's hash,
   else the full asset.
3. **Download** to `<data_dir>/update/<asset>.part`, with D12's limits, and verify the asset's
   SHA-256.
4. **Build `<exe>.new`:**
   - for a patch: read the old exe into memory, `Decoder::with_ref_prefix`, `WindowLogMax(28)`,
     stream to `.new`;
   - for the full asset: a plain streaming decode.

   Then verify `.new`'s SHA-256 against the target's `sha256`, and set the mode to 0755 on Unix.
5. **Probe:** run `<exe>.new --version` with a 10 s timeout and parse it with `daemon::parse_version_hash`
   (`src/daemon.rs:98`, the contract half `swiss status` and `disk_build_of` already use) plus a sibling
   `parse_version_number` written next to it and tested against `version_line()`. Its version must equal the manifest's, and its hash becomes the expected
   hash.
6. **Stop** the gateway on the resolved port with `stop_daemon` (graceful first). If nothing was
   running, remember that and skip steps 8 and 9.
7. **Swap** (D6). If the second rename fails, roll back, restart the old gateway if one was
   running, and exit 1.
8. **Start** the new exe with `start_daemon(StartOptions { entry: Some(<exe path as computed at
   step 1>), port: None, .. })`.
   - **Never** call `current_exe()` after the rename. On Linux it now names `swiss.old` or the
     new inode; on Windows it still names the old path. Compute the path once, before anything
     moves, and pass it everywhere.
   - Wait for `/health` to report the expected hash (the deploy.ps1 proof, in Rust).
   - If health fails within the start timeout: stop the new gateway, swap back (`<exe>` →
     `<exe>.new`, `<exe>.old` → `<exe>`), start the old one, write `rolledBack: true`, and exit 1.
9. **Skills:** run `<exe> skill install --installed` as a child (the new binary's skills, brief
   03's every-seat refresh). A failure is a warning line, not a failure of the update.
10. Write `last.json`, release the lock, and delete the download.

**Memory:** the updater briefly holds the old exe (~35 MB) plus the decoder window. It is a
short-lived process, so this is fine, but read the old exe once and drop it before step 6.

### 5.3 `last.json`

```json
{ "from": "0.2.2 (a5a12a9c)", "to": "0.2.3 (b1c2d3e4)", "via": "patch", "bytes": 1234567,
  "ok": true, "phase": "done", "error": null, "rolledBack": false,
  "startedAt": "…", "finishedAt": "…", "by": "cli" }
```

- `phase` is the last phase reached: `refused`, `manifest`, `download`, `verify`, `probe`,
  `stop`, `swap`, `start`, `health`, `skills` or `done`.
- `by` is `cli` or `panel`.
- The file is overwritten each attempt. It is plain (no secret) and lives in the data dir, not the
  exe dir.

### 5.4 Module layout

New `src/update/`, replacing `src/update_check.rs`. Keep `update_check`'s public functions
working, or move them and fix the callers in the same commit:

- `mod.rs`: `check()`, `apply()`, output lines.
- `manifest.rs`: parse and select (pure).
- `fetch.rs`: a `Fetch` trait (`get_json`, `get_to_file`) with the reqwest implementation and the
  redirect policy. Tests use a fake.
- `patch.rs`: decode and verify (pure over readers and writers).
- `replace.rs`: the D6 dance. Its file operations go through a small trait so a test can make the
  second rename fail.
- `restart.rs`: stop, start, health and rollback, over a trait so tests need no real daemon.

Add `zstd = "0.13"` to the root `Cargo.toml` with the same features the workspace already uses,
and confirm `cargo tree -d` shows no second zstd. `build.rs` stamps `SWISS_CHANNEL`
(`cargo:rerun-if-env-changed=SWISS_CHANNEL`), and `build_info()` gains `channel`. The CI `dist`
Build step sets `env: SWISS_CHANNEL: release`. That one-line workflow edit belongs to U2's commit,
because the stamp is born there.

**Test source override:** `SWISS_UPDATE_SOURCE=<base url>` replaces both the manifest URL and the
asset base. It is honoured **only** for `http://127.0.0.1:<port>/…`, `http://localhost:<port>/…`
or `http://[::1]:<port>/…`; anything else exits 2 with "SWISS_UPDATE_SOURCE must be a loopback
URL". It exists so the live walk can serve a local manifest. A test pins the refusal.

### 5.5 U2 tests (targeted: `cargo test -j 2 … -- --test-threads=1 update`)

- **Manifest:**
  - patch chosen on a hash match, full otherwise;
  - an unknown target;
  - a higher schema;
  - a same version without `--force` is "already newest", and with `--force` it reinstalls;
  - an older version is always refused;
  - unknown fields are ignored.
- **Patch:**
  - a round trip with `Encoder::with_ref_prefix` over two generated buffers of a few MB;
  - a corrupt patch is an error, never a wrong file;
  - a SHA mismatch after decode is an error, and `.new` is removed;
  - a `windowLog` over 28 is refused before decoding.
- **Interop:** a small fixture made by the **zstd CLI** (`old.bin`, `new.bin`,
  `p.patch.zst`, a few KB each), committed under `tests/fixtures/update/`, decodes with the Rust
  path to `new.bin`. Add the short script that made it as `tests/fixtures/update/make.sh`.
- **Replace**, in a tempdir:
  - a happy path;
  - a failure in the first rename leaves the original untouched;
  - a failure in the second rename rolls back;
  - an existing `.old` that cannot be deleted gets `.old.<hex>`;
  - the boot cleanup removes `.old*` siblings.
- **Refusals:**
  - a dev build makes **zero** fetch calls without `--force`;
  - a bundle path, a read-only directory, a held lock and a foreign PidRecord exe each exit 2;
  - a stale lock is taken over.
- **Sequence:**
  - with fakes for fetch and restart: a health failure triggers a rollback and `rolledBack: true`;
  - the skills step failing still exits 0 with a warning;
  - `current_exe()` is not called after the swap (inject the exe path; assert the start got it).
- **`SWISS_UPDATE_SOURCE`:** a non-loopback URL exits 2.

### 5.6 U2 live proof (never 19999, never the real `bin/`)

1. Copy a `target-test` (or `--profile quick`) exe A to a scratch dir as `scratch/bin/swiss.exe`.
2. Make B: commit any trivial change, rebuild, and copy the exe. B then has a different hash and
   build time and the **same** version, so the walk uses `--force`.
3. In WSL, build a patch A→B and the full B with the zstd CLI. Write a manifest by hand or with
   `release_manifest.py`, version = the current version.
4. Serve the directory on `127.0.0.1:<free port>` with a tiny static server from the scratchpad.
5. Start the scratch exe as a gateway on 19998 with a scratch `SWISS_HOME` (plus the scratch
   `USERPROFILE`/`HOME`/`CLAUDE_CONFIG_DIR` from brief 03 §7, because step 9 refreshes skills).
6. Run `SWISS_UPDATE_SOURCE=http://127.0.0.1:<port>/ scratch/bin/swiss.exe update --apply --force`.
   - Paste the full output.
   - `/health` on 19998 shows B's hash.
   - `swiss.exe.old` exists.
   - `last.json` reads `via: "patch"`.
7. Then the negative walks: the manifest says the wrong sha (exit 1, nothing renamed); the probe is
   made to fail by serving a truncated exe whose sha is listed (exit 1, nothing renamed).

## 6. U3 — the panel

### 6.1 Gateway routes (root composition, `src/adminapi.rs` or a new `src/update/routes.rs`)

- `GET /api/update/status` → `{channel, canApply, refusal: null | "update.devBuild" |
  "update.bundle" | "update.readOnly" | "update.otherExe", running: bool, last: <last.json> | null}`.
  - `running` is true while the gateway's own spawned child lives, or while `update.lock` is
    held by a live pid.
  - These are reads only, so the route works in test compositions (`current_exe` there is the
    test binary, and the refusal says `update.devBuild`).
- `/api/update/check` (brief 02) gains `plan: {via, bytes, full} | null` when the release has a
  manifest, using the same selection code, so the button can say "Update to v0.2.3 (1.4 MB)".
- `POST /api/update/apply` → **202** `{started: true}`:
  - It spawns `spawn_detached({program: <current_exe>, args: ["update", "--apply", "--by-panel"],
    capture: <data_dir>/update/apply.log, ..})` through a new AppContext seam
    `update_apply: OnceLock<Arc<dyn UpdateApply>>`. The seam is unset in tests and answers 503,
    so no suite run can spawn an updater.
  - It refuses with 409 and the codes above, or `update.running`.
  - If the spawn reports `shares_job: true`, the child would die with the gateway mid-swap. Kill
    it at once and answer 409 `update.cannotDetach` ("run `swiss update --apply` in a terminal").
  - `--by-panel` only sets `by: "panel"` in `last.json`. Hide it from usage.
- The gateway keeps the spawned child's handle and waits on it in a task. If the child exits
  while the gateway still runs (a refusal or a failure before the stop), publish `HOST.UPDATE`.
  On success the gateway is stopped by the child, and the new gateway's boot needs no notice: the
  page reconnects, `link.ts` sees the new build and reloads.
- `HOST_SUB_UPDATE = 7` in `host.proto`, regenerated as the bus crate does today (follow the
  last sub added, `HOST_SUB_MEMORY`, commit by commit), plus the §bus.registry row.
- At boot: the `.old*` cleanup of D13, best-effort, off the hot path (a spawned blocking task).

### 6.2 System page

The Updates row of brief 02 gains these states:

- **newer + canApply:** a primary button "Update to v0.2.3 (1.4 MB)" opens a confirm sheet:
  - "swiss downloads the update, then restarts the gateway.";
  - "Open terminals and running runs end. Managed apps keep running." (ADR-036);
  - "This page reloads when the new version answers.";
  - Cancel / Update.
- **running:** "Updating…" with a spinner, and no button.
- **newer + refusal:**
  - the newer text plus the refusal's sentence: `update.devBuild` reads "This build was made from
    source; rebuild it to update.", and the bundle, read-only and other-exe codes get their own
    sentences;
  - plus "Release notes".
- **last.json present:** a muted line under the row, e.g. "Last update: 0.2.2 → 0.2.3 via patch
  (1.4 MB), 20 Nov 10:01". A failed attempt reads "Last update failed at <phase>: <error>", plus
  "(rolled back)" when it was.

The page subscribes to `HOST.UPDATE` while mounted and re-reads `/api/update/status` on it and
on WELCOME. **No timer.** Every served code goes into `SERVED_CODES` with en and zh keys.

### 6.3 U3 tests

- **Rust:**
  - apply with the seam unset answers 503;
  - with a fake seam, `shares_job: true` answers 409 `update.cannotDetach`;
  - running blocks a second apply;
  - the status route reports the refusal of a dev build;
  - `HOST.UPDATE` is published when the fake child exits.
- **Vitest:**
  - mount does no apply and no check;
  - the confirm sheet POSTs once;
  - each state renders;
  - `HOST.UPDATE` re-reads status;
  - the last-update line for ok, failed and rolled back.

### 6.4 U3 live proof

The U2 setup, with the scratch exe started as the 19998 gateway:

1. Open its panel, Check, then Update. Screenshot the confirm sheet, "Updating…", the page after
   its reload showing the new hash in the About row, and the last-update line.
2. Then do one forced failure (wrong sha in the manifest): the row returns with the failure line,
   and the gateway never went down (`/health` answered the whole time — say how you checked).

## 7. Out of scope

- Delta updates for the macOS `.app` bundle. It gets the manual steps.
- Background or automatic checks of any kind. Every check and every apply is a click or a
  command.
- Downgrades.
- A signed manifest (ADR-041 open item).
- Updating a gateway that runs a different exe than the CLI's.

## 8. Risks the reviewer will look for

- `current_exe()` after the swap.
- The updater being killed with the gateway (job sharing, a tree-kill that reaches it).
- An updater that leaves the gateway down: every failure after step 6 must end with **some**
  gateway running when one was running before.
- A download or decode inside the resident gateway process.
- An unbounded download, an open redirect, an unverified asset, or a decoder without a window
  cap.
- A test that touches the real `bin/` or 19999.

## 9. SPEC and docs

- **ADR-041** "swiss updates itself from a short-lived CLI process, by patch when it can". It
  records:
  - D1, D5, D6 and D10;
  - what changed from the old non-goal and why the memory argument still holds;
  - the open item, a signed manifest.

  It lands in U2's main commit.
- **§host.cli `update`:** the check, `--apply`, `--force`, exit codes, output lines, refusals and
  `last.json`. The "updating stays a manual exe swap" sentence goes in U2.
- **§release.ci:** the new assets, the manifest, the 3-release window, the 60 % rule and the
  verify step. Lands in U1.
- **§host.daemon:** the `.old` cleanup at boot (U3), and `build_info`'s `channel` (U2).
- **§bus.registry:** `HOST.UPDATE` (U3).
- **§panel.settings:** the update states (U3).
- **README:**
  - the Update section rewritten (`swiss update --apply`, with the manual swap kept as the bundle
    and fallback path);
  - the Non-goals line "No self-updating binary" replaced by "No updater in the resident gateway";
  - lands in U2.
- **SETUP.md**, if it describes updating: the same (U2).
- **CHANGELOG `[Unreleased]`:** one Added entry per session.

## 10. Delivery

- **U1:** `ci(release): publish zstd full and patch assets with an update manifest`
  (+ `scripts/release_manifest.py` and its test). It stops after E0 if the ratio is bad.
- **U2:** `feat(update): swiss update --apply patches or replaces the exe and restarts the
  gateway`, possibly split as manifest+patch / replace+restart / CLI+docs.
- **U3:** `feat(update): the panel starts an update and reports it` (+ proto, routes, panel and
  emit).
