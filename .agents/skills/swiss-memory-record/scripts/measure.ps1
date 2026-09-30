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

# Memory measurement loop for SPEC §product.memory - records, not gates.
#
# Builds into target-test, boots the isolated 19998 instance on a REAL-state snapshot
# (the workload is the point; never 19999), reads /health + /api/memory + swiss status,
# prints a ready-to-append row for the SPEC §product.memory table next to its last row, then stops the
# instance. The skill body owns what the numbers mean and the append rule.

[CmdletBinding()]
param(
    [switch]$SkipBuild,   # reuse the existing target-test exe
    [switch]$Keep         # leave the instance running instead of stopping it
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
$Boot = Join-Path $Root '.agents\skills\swiss-live-verify\scripts\live-check.ps1'
$TestInstance = Join-Path $Root 'scripts\test-instance.ps1'
$Exe = Join-Path $Root 'target-test\release\swiss.exe'
$Spec = Join-Path $Root 'docs\SPEC.md'
$Port = 19998

# Boot via the live-verify helper (it owns build, stale-port guard, token pin, hash proof).
# A hashtable, not an array: an array splat hands '-SkipBuild' over as a positional string.
$bootArgs = @{}
if ($SkipBuild) { $bootArgs.SkipBuild = $true }
& $Boot @bootArgs
if ($LASTEXITCODE -ne 0) { Write-Error 'boot failed - no measurement'; exit 1 }

$health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 5

# Against the test home (env scoped to these calls only). /api/* needs the admin session
# (SPEC §host.session), never the bearer: `swiss api` signs the call with the test home's CLI key.
$env:SWISS_HOME = Join-Path $env:LOCALAPPDATA 'swiss-test-home'
$env:SWISS_PORT = "$Port"
$mem = (& $Exe api GET /api/memory) | Out-String | ConvertFrom-Json
$status = (& $Exe status 2>&1 | Out-String)
Remove-Item Env:\SWISS_HOME -ErrorAction SilentlyContinue
Remove-Item Env:\SWISS_PORT -ErrorAction SilentlyContinue

$date = Get-Date -Format 'yyyy-MM-dd'
$hash = $health.build.hash
$private = "$($mem.heapUsedMb)/$($mem.heapTotalMb)"
$row = "| $date | Rust $hash | real-state snapshot on 19998 | $($mem.gatewayMb) MB | private ~$private MB; children $($mem.childrenMb) MB ($($mem.processCount) proc) |"

Write-Host ''
Write-Host '=== ready-to-append row for the SPEC product.memory table (docs/SPEC.md; never overwrite a dated row) ==='
Write-Host $row
# the last dated row of the table under the section heading, nothing else in the spec
$inSection = $false; $prev = $null
foreach ($l in Get-Content -LiteralPath $Spec -Encoding utf8) {
    if ($l -match '^### \S*product\.memory') { $inSection = $true; continue }
    if ($inSection -and $l -match '^#') { break }
    if ($inSection -and $l -match '^\| 20') { $prev = $l }
}
if ($prev) { Write-Host "previous record: $prev" }
Write-Host ''
Write-Host '--- swiss status (memory rows) ---'
$lines = $status -split "\r?\n"
$lines | Where-Object { $_ -match 'memory|build|running|MB' } | ForEach-Object { Write-Host $_.TrimEnd() }

if (-not $Keep) {
    Write-Host ''
    Write-Host '=== stopping 19998'
    & $TestInstance -Stop
}
exit 0
