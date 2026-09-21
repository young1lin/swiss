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

# Memory measurement loop for docs/01 - records, not gates (user call 2026-09-11).
#
# Builds into target-test, boots the isolated 19998 instance on a REAL-state snapshot
# (the workload is the point; never 19999), reads /health + /api/memory + swiss status,
# prints a ready-to-append docs/01 row next to the previous recorded row, then stops the
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
$Docs01 = Join-Path $Root 'docs\01-goals-and-memory-budget.md'
$Port = 19998

# Boot via the live-verify helper (it owns build, stale-port guard, token pin, hash proof).
$bootArgs = @()
if ($SkipBuild) { $bootArgs += '-SkipBuild' }
& $Boot @bootArgs
if ($LASTEXITCODE -ne 0) { Write-Error 'boot failed - no measurement'; exit 1 }

$Token = 'acceptance-token-for-1998'   # pinned by live-check.ps1 before boot
$hdr = @{ Authorization = "Bearer $Token" }
$health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 5
$mem = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/memory" -Headers $hdr -TimeoutSec 5

# swiss status against the test home (env scoped to this invocation only).
$env:SWISS_HOME = Join-Path $env:LOCALAPPDATA 'swiss-test-home'
$env:SWISS_PORT = "$Port"
$status = (& $Exe status 2>&1 | Out-String)
Remove-Item Env:\SWISS_HOME -ErrorAction SilentlyContinue
Remove-Item Env:\SWISS_PORT -ErrorAction SilentlyContinue

$date = Get-Date -Format 'yyyy-MM-dd'
$hash = $health.build.hash
$private = "$($mem.heapUsedMb)/$($mem.heapTotalMb)"
$row = "| $date | $hash | gatewayMb=$($mem.gatewayMb) privateCommit~=$private childrenMb=$($mem.childrenMb) (proc children: $($mem.processCount)) | working set per /api/memory; workload: real-state snapshot on 19998 |"

Write-Host ''
Write-Host '=== ready-to-append docs/01 row (Measured result section; never overwrite a dated row) ==='
Write-Host $row
$prev = Select-String -Path $Docs01 -Pattern '\bMB\b' | Select-Object -Last 1
if ($prev) { Write-Host "previous record (docs/01 line $($prev.LineNumber)): $($prev.Line.Trim())" }
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
