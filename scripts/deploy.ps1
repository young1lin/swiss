# Copyright 2026 young1lin
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     https://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# One-step deploy to production 19999 (SPEC §host.daemon, bin\ since 2026-09-21).
#
# The deploy used to be a three-step ritual (stop, build, start) where doing it out of order
# failed with "Access is denied (os error 5)" - the linker cannot overwrite the exe a running
# daemon holds - and nothing anywhere said WHICH build was running afterwards. This script
# fixes the order, then proves the result: /health must report the hash this build stamped
# into swiss.exe --version, or the script fails loudly with both values.
#
# Production runs from bin\swiss.exe, a copy of the build output, never from target\ itself
# (SPEC §host.ops): the linker and the daemon no longer share a file, so the build happens
# while the old daemon is still serving and the outage is stop + copy + start - seconds, not
# the four minutes a release build takes. Since 2026-09-28 the new daemon also binds the port
# BEFORE starting its plugins (src/server.rs), so "start" is a second rather than however long
# the slowest MCP takes to wake; the last line of a deploy states the measured outage. bin\ is
# a stable path for PATH and for
# `swiss autostart on`, and git ignores it. A daemon still running out of target\ (the
# pre-bin layout) is stopped BEFORE the build, once, so the linker can write.
#
# Run it from the root of the tree to deploy - the main checkout or one of its linked worktrees
# (.agents\worktrees\*): that tree is built, and the exe goes into the MAIN checkout's bin\
# either way, the one PATH and autostart name. A deploy is the operator's decision, from
# whichever shell they choose (H1 scrubs the daemon's environment, so an agent or CI shell is
# safe too). Until 2026-09-20 a shell that CAPTURED this script's output (`| Out-File`, a
# tool wrapper, `| tee`) hung after "gateway started": the daemon inherited the wrapper's
# stdout pipe and the wrapper waited for an EOF that never came, so the proof step below never
# ran and the lock stayed.
# `swiss start` now keeps its std handles out of the daemon's inheritance (swiss-core
# platform::keep_std_handles_from_children), and a captured deploy runs to the end.
# -SkipGates jumps the test/clippy gates for a hotfix; the default is to run them.
# -Quick is the operator's quick deploy, to see a change live: it skips the gates and builds the
# quick profile (Cargo.toml: no LTO, 16 codegen units, incremental) into target\quick, minutes
# sooner than the release profile and a larger exe; the release profile stays the shipping build.
#
# Cargo's default is one job per logical CPU. On a 20-thread box with a few GB free
# that pages, and then the whole machine stalls. Jobs are chosen from free RAM at
# the start of this process (dev gates ~1 GB per rustc, the opt-level=z release
# build ~2 GB), clamped so a tight box stays at 2 and a free one does not sit at 2
# for the whole gate run. sccache, when installed, caches dependency codegen so
# target\ and target-test\ do not each pay opt-level=z. The release profile itself
# (fat LTO, one codegen unit) is unchanged: that cost is the shipping binary.
#
# The single-deployer lock (deploy-lock.ps1) is taken FIRST and held for the whole run: the
# 2026-09-14 incident was two sessions deploying the same production four minutes apart,
# each unaware the other had already stopped it. Phase lines also land in the gateway home's
# deploy.log as they happen, so a background deploy can be watched even when its stdout is
# buffered away by a wrapper.
[CmdletBinding()]
param(
    [switch]$SkipGates,
    [switch]$Quick
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\deploy-lock.ps1"
$BuildProfile = if ($Quick) { 'quick' } else { 'release' }
$Built = "target\$BuildProfile\swiss.exe"  # what cargo writes, in the tree being deployed
$BinDir = Get-SwissProdBin               # the main checkout's bin\, from a worktree too
$Exe = Join-Path $BinDir 'swiss.exe'     # what 19999 runs
function Fail($message) {
    Write-Host $message -ForegroundColor Red
    Phase "FAILED: $message"
    exit 1
}
function Phase($message) {
    # Timestamped everywhere, immediately: console (flushed, so a piped consumer sees
    # progress as it happens) and the gateway home's deploy.log (survives wrappers that
    # buffer stdout into silence).
    # The home is resolved per line, not once, so every line lands in the home the
    # gateway is actually using — including one named by $SWISS_HOME.
    Write-Host "== $message"
    try {
        $log = Join-Path (Get-SwissProdHome) 'deploy.log'
        Add-Content -Path $log -Value ("{0} deploy: {1}" -f (Get-Date).ToString('s'), $message)
    } catch { }
    [Console]::Out.Flush()
}
function Get-FreeMemoryGiB {
    # kernel32, not CIM: a WMI query here costs longer than the decision. The smaller of free
    # RAM and the commit headroom (RAM plus paging file, less what is committed): the build
    # host's paging file is small, and on 2026-10-13 a gate run sized from 9 GB of free RAM ran out
    # of commit instead (os error 1455, seven rustc/rust-lld at once) with RAM still free.
    if (-not ('SwissDeploy.Kernel32' -as [type])) {
        Add-Type -Namespace SwissDeploy -Name Kernel32 -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)]
public struct MEMORYSTATUSEX {
    public uint dwLength;
    public uint dwMemoryLoad;
    public ulong ullTotalPhys;
    public ulong ullAvailPhys;
    public ulong ullTotalPageFile;
    public ulong ullAvailPageFile;
    public ulong ullTotalVirtual;
    public ulong ullAvailVirtual;
    public ulong ullAvailExtendedVirtual;
}
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool GlobalMemoryStatusEx(ref MEMORYSTATUSEX lpBuffer);
'@
    }
    $status = New-Object SwissDeploy.Kernel32+MEMORYSTATUSEX
    $status.dwLength = [System.Runtime.InteropServices.Marshal]::SizeOf($status)
    $ok = [SwissDeploy.Kernel32]::GlobalMemoryStatusEx([ref]$status)
    if (-not $ok) { return $null }
    return ([Math]::Min($status.ullAvailPhys, $status.ullAvailPageFile) / 1GB)
}
function Get-StampOf([string]$exe) {
    # The hash `swiss --version` reports - "swiss 0.1.4 (2741e2b8, 2026-10-20T07:56:21Z)" - or
    # $null for no exe or no readable line.
    if (-not (Test-Path $exe)) { return $null }
    $line = (& $exe --version) | Select-Object -First 1
    if ($line -match '^swiss \S+ \((?<hash>[^,]+), ') { return $Matches.hash }
    return $null
}
function Get-TreeStamp {
    # What build.rs stamps for this tree now: HEAD's short hash, -dirty while `git status
    # --porcelain` lists anything (an untracked file too).
    # No Select-Object here: one cutting the pipeline short leaves $LASTEXITCODE unset.
    $head = git rev-parse --short HEAD
    if ($LASTEXITCODE -ne 0 -or -not $head) { return 'unknown' }
    $head = "$head".Trim()
    if (git status --porcelain) { return "$head-dirty" }
    return $head
}
function Get-CargoJobs([string]$kind) {
    # Leave 2 GB for the editor. Dev rustc is about 1 GB; opt-level=z about 2 GB.
    # Floor 2 (a full box still makes progress), cap 4 for both: the workspace's test
    # binaries link through rust-lld at well over 1 GB each, and 4 is the ceiling
    # the agents' -j 4 rule set.
    $free = Get-FreeMemoryGiB
    if ($null -eq $free) { return 2 }
    $budget = [Math]::Max(0.0, $free - 2.0)
    if ($kind -eq 'release') {
        return [Math]::Min(4, [Math]::Max(2, [Math]::Floor($budget / 2)))
    }
    return [Math]::Min(4, [Math]::Max(2, [Math]::Floor($budget / 1)))
}
# Mutual exclusion before anything else - gates included: a 90-minute gate window is
# exactly long enough for another session to start a deploy of its own.
$lock = Acquire-DeployLock
if (-not $lock.Acquired) {
    $who = if ($lock.Holder) { "pid $($lock.Holder.pid) (started $($lock.Holder.started))" } else { "an unknown holder" }
    Fail "another deploy holds the lock - $who; if that pid is dead it is stale, delete $($lock.LockPath) and re-run"
}
if ($lock.StaleTookOver) {
    Phase "took over a STALE deploy lock (previous holder is gone)"
}
try {
$gateJobs = Get-CargoJobs 'dev'
$releaseJobs = Get-CargoJobs 'release'
$env:CARGO_BUILD_JOBS = "$gateJobs"
$sccache = Join-Path $env:USERPROFILE '.cargo\bin\sccache.exe'
if (Test-Path $sccache) { $env:RUSTC_WRAPPER = $sccache }
$freeGb = Get-FreeMemoryGiB
$freeText = if ($null -eq $freeGb) { 'free RAM unknown' } else { ('{0:N1} GB free' -f $freeGb) }
Phase "cargo: $gateJobs jobs for gates, $releaseJobs for release ($freeText)"
# Gates first, while production is still up: a failing gate must not take the daemon down.
if (-not $SkipGates -and -not $Quick) {
    Phase 'cargo test --workspace'
    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { Fail "tests failed - production left untouched" }
    # Gate 2 (SPEC §testing.it): the real-database suite. Docker lives in WSL at
    # tcp://127.0.0.1:2375 when localhost forwarding works - a NAT-mode WSL behind a
    # localhost proxy does not forward it, so resolve the endpoint instead of
    # hardcoding it: loopback first (Docker Desktop machines keep working), then the
    # WSL address of the first running distro, whose dockerd binds 0.0.0.0:2375. A diff
    # that never touched a database path can still afford the ~20 s: a deploy is the
    # last place to learn the engines broke.
    Phase 'cargo test -p swiss-it --features it'
    $dockerHost_ = 'tcp://127.0.0.1:2375'
    try { Invoke-RestMethod -Uri 'http://127.0.0.1:2375/version' -TimeoutSec 3 | Out-Null } catch {
        # wsl prints its NAT/proxy warning to stderr, which a native redirection under
        # ErrorActionPreference Stop turns into a terminating error (PS 5.1) - go through
        # cmd so the warning dies inside the child instead of killing the deploy.
        $wslIp_ = (cmd /c "wsl hostname -I 2>nul" | Select-Object -First 1)
        if (-not $wslIp_) { $wslIp_ = (wsl -u root -- hostname -I 2>$null | Select-Object -First 1) }
        if ($wslIp_) { $dockerHost_ = 'tcp://' + ($wslIp_.ToString().Trim().Split(' ')[0]) + ':2375' }
    }
    Phase ("integration gate docker at " + $dockerHost_)
    $env:DOCKER_HOST = $dockerHost_
    cargo test -p swiss-it --features it
    $gate2 = $LASTEXITCODE
    Remove-Item Env:\DOCKER_HOST -ErrorAction SilentlyContinue
    if ($gate2 -ne 0) { Fail "integration gate failed - production left untouched" }
    Phase 'cargo clippy --workspace --all-targets -- -D warnings'
    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { Fail "clippy failed - production left untouched" }
    # The panel's own gate (SPEC §panel.toolchain): typecheck + lint + emit-freshness + vitest, run
    # where its package.json lives. A stale emit is a failure here, so a deploy can never
    # serve a panel whose committed JS predates its TypeScript sources.
    Phase 'panel: npm run check (crates/swiss-panel/panel)'
    Push-Location 'crates\swiss-panel\panel'
    npm run check
    $panelCode = $LASTEXITCODE
    Pop-Location
    if ($panelCode -ne 0) { Fail "panel check failed - production left untouched" }
} else {
    $why = if ($Quick) { '-Quick' } else { '-SkipGates' }
    Phase "gates SKIPPED ($why)"
}
# The daemon that is up right now, from its pid file: only a daemon running out of target\
# (the pre-bin layout) has to stop before the build, because the linker must overwrite the
# very file it holds. One running from bin\ keeps serving through the build.
$stopper = if (Test-Path $Exe) { $Exe } else { $Built }
$entry = $null
try {
    $pidFile = Join-Path (Get-SwissProdHome) 'gateway-19999.pid'
    if (Test-Path $pidFile) { $entry = (Get-Content $pidFile -Raw | ConvertFrom-Json).entry }
} catch { }
$holdsBuildOutput = $entry -and ((Resolve-Path $Built -ErrorAction SilentlyContinue).Path -eq $entry)
if ($holdsBuildOutput) {
    Phase "stopping the daemon first: it runs out of $Built, which the build must overwrite"
    & $stopper stop
    if ($LASTEXITCODE -eq 1) { Fail "stop was refused - resolve it by hand (see above), then re-run" }
}
# The stamp must name the tree being deployed (2026-10-20). build.rs re-runs only when HEAD or
# a branch ref moves, so a build made while a stray file sat in the tree kept its -dirty stamp
# after the file went, and production reported 2741e2b8-dirty for a clean commit; an edit
# after a build kept a clean stamp on a dirty tree the same way. When the last build's stamp
# is not the tree's, build.rs is touched so cargo runs it again.
$wantStamp = Get-TreeStamp
$hadStamp = Get-StampOf $Built
if ($hadStamp -ne $wantStamp) {
    $had = if ($hadStamp) { $hadStamp } else { 'no build' }
    Phase "re-stamping: the last build says $had, the tree is $wantStamp"
    (Get-Item 'build.rs').LastWriteTime = Get-Date
}
$env:CARGO_BUILD_JOBS = "$releaseJobs"
Phase "cargo build --profile $BuildProfile ($releaseJobs jobs)"
cargo build --profile $BuildProfile
if ($LASTEXITCODE -ne 0) {
    if ($holdsBuildOutput) { Fail "build failed - start the old daemon again by hand: & $stopper start --no-open" }
    Fail "build failed - production left untouched"
}
# Checked before the stop: a binary that would report the wrong build never replaces the one
# serving. A tree edited during the build lands here too.
$builtStamp = Get-StampOf $Built
if ($builtStamp -ne $wantStamp) {
    $why = "the build is stamped $builtStamp but the tree is $(Get-TreeStamp)"
    if ($holdsBuildOutput) { Fail "$why - start the daemon again by hand: & $Built start --no-open; then re-run" }
    Fail "$why - production left untouched; re-run"
}
if ($wantStamp -like '*-dirty') { Phase "deploying a tree with uncommitted changes ($wantStamp)" }
# From here the outage clock runs: stop, copy, start. It is measured and reported (2026-09-28),
# because "how long was 19999 down" is the number this whole ordering exists to keep small and
# nothing used to state it.
$outage = [System.Diagnostics.Stopwatch]::StartNew()
if (-not $holdsBuildOutput) {
    Phase 'stopping the daemon'
    & $stopper stop
    if ($LASTEXITCODE -eq 1) { Fail "stop was refused - resolve it by hand (see above), then re-run" }
}
# A `stop` that found no pid record says "not running" and exits 0 - which is also what it says
# when a daemon IS serving whose record was lost (the 2026-09-28 start-timeout bug). Copying
# over an exe that process still holds fails with "Access is denied", eight words away from the
# cause. Ask the port instead: it answers for whatever is actually there.
$stillUp = $null
try { $stillUp = Invoke-WebRequest -Uri 'http://127.0.0.1:19999/health' -UseBasicParsing -TimeoutSec 2 } catch { }
if ($stillUp) {
    $owner = (Get-NetTCPConnection -LocalPort 19999 -State Listen -ErrorAction SilentlyContinue | Select-Object -First 1).OwningProcess
    Fail "something is still serving 19999 (pid $owner) after the stop - its pid record is probably gone; stop it by hand (POST http://127.0.0.1:19999/api/shutdown, or Stop-Process -Id $owner), then re-run"
}
Phase "installing $Exe"
if (-not (Test-Path $BinDir)) { [void](New-Item -ItemType Directory -Path $BinDir) }
# The stopped process can take a moment to let go of its exe; the copy retries, not fails.
$copied = $false
foreach ($attempt in 1..20) {
    try { Copy-Item -Path $Built -Destination $Exe -Force; $copied = $true; break } catch { Start-Sleep -Milliseconds 250 }
}
if (-not $copied) { Fail "could not overwrite $Exe - something still holds it (the old daemon?): start it again by hand: & $Exe start --no-open" }
Phase 'starting the new daemon'
& $Exe start --no-open
if ($LASTEXITCODE -ne 0) { Fail "start failed - log tail above" }
& $Exe status
if ($LASTEXITCODE -ne 0) { Fail "status says the daemon is not running" }
# The proof: the daemon now serving is THIS build, not the previous one still holding the port.
$built = Get-StampOf $Exe
if (-not $built) {
    Fail "cannot read a build hash from the version line: $((& $Exe --version) | Select-Object -First 1)"
}
$health = Invoke-WebRequest -Uri "http://127.0.0.1:19999/health" -UseBasicParsing -TimeoutSec 5
$running = ($health.Content | ConvertFrom-Json).build.hash
if ($running -ne $built) {
    Fail "deploy did not take: running daemon is $running, freshly built is $built"
}
$outage.Stop()
Phase ("deployed: $running is serving on 19999 from $Exe (19999 was down {0:N1} s)" -f $outage.Elapsed.TotalSeconds)
# Other programs reach `swiss remote ...` through PATH; the script only says when bin\ is
# missing from it - changing a user's PATH is their call.
$binAbs = (Resolve-Path $BinDir).Path
$onPath = ($env:PATH -split ';') | Where-Object { $_ -and ((Resolve-Path $_ -ErrorAction SilentlyContinue).Path -eq $binAbs) }
if (-not $onPath) {
    Write-Host "note: $binAbs is not on PATH; to call swiss from anywhere add it (User scope): [Environment]::SetEnvironmentVariable('Path', ([Environment]::GetEnvironmentVariable('Path','User') + ';$binAbs'), 'User')"
}
} finally {
    # The lock MUST go even on failure - a failed deploy that keeps the lock blocks the
    # retry that fixes it. Stale takeover inside the run is Release's problem, not ours.
    [void](Release-DeployLock)
}
exit 0