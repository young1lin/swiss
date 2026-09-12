# The isolated 19998 test instance (docs/16 H2).
#
# One script, one rule: live verification of this repo runs on an instance whose state lives
# in its OWN home (%LOCALAPPDATA%\swiss-test-home), so a save on 19998 can never write the
# user's production config in ~\.mcp-gateway (the 2026-09-11 footgun).
#
# Why copying master.key works: every state file is AES-256-GCM sealed under a master key
# that is itself DPAPI-protected for the CURRENT USER (docs/05 "The master key"). A copy of
# that blob stays decryptable on the same machine under the same user - exactly what a local
# test instance needs. Cross-machine the copy is useless BY DESIGN; do not try to make this
# a portable snapshot.
#
# Usage:
#   scripts/test-instance.ps1            # -Start (default): snapshot state, serve on 19998
#   scripts/test-instance.ps1 -Fresh     # wipe the test home first (a clean instance)
#   scripts/test-instance.ps1 -Stop      # kill whatever owns port 19998 - by pid, never name
#
# Stop deliberately finds its victim ONLY through Get-NetTCPConnection's OwningProcess:
# Get-Process swiss would kill the user's 19999 daemon too, which is also an swiss.exe.
# The instance is a hard kill (the serve path writes no pid file, so 'swiss stop' has nothing
# to act on); orphaned proc children are reaped from the port-scoped ledger on the next boot.

[CmdletBinding()]
param(
    [switch]$Start,
    [switch]$Stop,
    [switch]$Fresh
)

$ErrorActionPreference = 'Stop'

$Port = 19998
$TestHome = Join-Path $env:LOCALAPPDATA 'swiss-test-home'
$ProdHome = Join-Path $env:USERPROFILE '.mcp-gateway'
$Exe = Join-Path $PSScriptRoot '..\target-test\release\swiss.exe'
$HealthUrl = "http://127.0.0.1:$Port/health"

# Sealed state worth snapshotting: keys, config, managed MCPs, tunnels, jobs and their run
# facts, the env store, the secret vault (docs/19). Copied, never linked - the test home must
# be a point-in-time snapshot the test instance may then scribble over freely.
$StateFiles = @(
    'master.key',
    'gateway.config.json',
    'managed.json',
    'tunnels.json',
    'jobs.json',
    'jobs-state.json',
    'env.json',
    'secrets.json'
)

function Get-PortOwnerPid {
    $owners = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty OwningProcess -Unique
    if ($owners) { return @($owners)[0] }
    return $null
}

function Test-Healthy {
    try {
        $res = Invoke-WebRequest -Uri $HealthUrl -UseBasicParsing -TimeoutSec 2
        return ($res.StatusCode -eq 200)
    } catch {
        return $false
    }
}

if ($Stop) {
    $owner = Get-PortOwnerPid
    if (-not $owner) {
        Write-Host "nothing is listening on $Port"
        exit 0
    }
    Write-Host "killing pid $owner (the process that owns port $Port)"
    Stop-Process -Id $owner -Force
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-PortOwnerPid)) { break }
        Start-Sleep -Milliseconds 200
    }
    if (Get-PortOwnerPid) {
        Write-Error "port $Port is still held after killing pid $owner"
        exit 1
    }
    Write-Host "19998 stopped; 19999 (production) untouched"
    exit 0
}

# --- -Start (default) ---------------------------------------------------------------------------

$running = Get-PortOwnerPid
if ($running) {
    Write-Host "something is already listening on $Port (pid $running); run this script with -Stop first"
    exit 0
}

if (-not (Test-Path $Exe)) {
    Write-Error ("no test binary at " + $Exe + " - build it first: CARGO_TARGET_DIR=target-test cargo build --release")
    exit 1
}

if ($Fresh -and (Test-Path $TestHome)) {
    Remove-Item -Recurse -Force $TestHome
}

New-Item -ItemType Directory -Force -Path $TestHome | Out-Null

$copied = @()
foreach ($name in $StateFiles) {
    $src = Join-Path $ProdHome $name
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $TestHome $name) -Force
        $copied += $name
    }
}
Write-Host "test home: $TestHome"
Write-Host ("snapshotted from " + $ProdHome + " : " + ($copied -join ', '))

# Env vars, never --port: the flag writes itself into the config (AGENTS.md), and the env is
# exactly what this script is for - scoped to this serve process and its children only.
$env:SWISS_HOME = $TestHome
$env:SWISS_PORT = "$Port"

$out = Join-Path $TestHome 'serve.out'
$err = Join-Path $TestHome 'serve.err'
$proc = Start-Process -FilePath $Exe -ArgumentList 'serve' -WindowStyle Hidden -PassThru -RedirectStandardOutput $out -RedirectStandardError $err

Write-Host "started pid $($proc.Id); waiting for /health (20s budget)"
$deadline = (Get-Date).AddSeconds(20)
$healthy = $false
while ((Get-Date) -lt $deadline) {
    if ($proc.HasExited) { break }
    if (Test-Healthy) { $healthy = $true; break }
    Start-Sleep -Milliseconds 250
}

if (-not $healthy) {
    Write-Host "FAILED to come up on $Port"
    if ($proc.HasExited) { Write-Host "the process already exited (code $($proc.ExitCode))" }
    foreach ($log in @('serve.err', 'serve.out')) {
        $p = Join-Path $TestHome $log
        if (Test-Path $p) {
            Write-Host "--- last lines of $log ---"
            Get-Content $p -Tail 20 | ForEach-Object { Write-Host $_ }
        }
    }
    exit 1
}

Write-Host "19998 is up: pid $($proc.Id), $HealthUrl"
Write-Host "state writes go to $TestHome - production config untouched"
exit 0

