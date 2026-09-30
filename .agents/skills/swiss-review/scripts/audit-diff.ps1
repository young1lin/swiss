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

# Mechanical red-flag scan over an outgoing swiss diff - a lens, not a gate.
#
# Runs the cheap, textual half of the swiss-review audit table: greps ADDED lines of the
# diff for the patterns that breach this repo's load-bearing rules. The semantic half
# (does the change actually respect the rule?) stays with the reviewer. Untracked files
# are not covered; exit code is always 0 - findings are printed, not enforced.

[CmdletBinding()]
param(
    # The diff base. Default HEAD covers staged + unstaged tracked changes;
    # pass a branch or ref to audit an outgoing diff against it.
    [string]$Base = 'HEAD'
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
Push-Location $Root
try {
    $diff = git diff --unified=0 $Base -- 2>$null
} finally { Pop-Location }

# Each rule: content pattern (regex over added lines), optional path filter, finding text.
$Rules = @(
    @{ Pat = 'Command::new\(["'']powershell';     Path = '';                                   Msg = 'no subprocess where a syscall exists - a PowerShell spawn costs ~65 MB transient working set' },
    @{ Pat = 'multi_thread';                       Path = '';                                   Msg = 'runtime is current_thread by design - multi_thread needs a measured reason in the commit message' },
    @{ Pat = 'unsafe';                         Path = '^(?!.*platform).*.(rs|toml)$';     Msg = 'unsafe belongs at the Windows FFI boundary (platform/), nowhere else' },
    @{ Pat = 'serde_json::Value';                  Path = 'adapters|proc|http|rest';           Msg = 'forwarding paths pass payloads as &RawValue - no serde_json::Value materialisation' },
    @{ Pat = '.unwrap()';                       Path = 'secure|adapters';                    Msg = 'no .unwrap() on config/network/db/filesystem paths - one failing MCP must not take down the others' },
    @{ Pat = 'swiss-core/src/secure/envelope';     Path = '';                                   Msg = 'the sealed envelope format is FROZEN (SPEC §formats) - envelope construction changes are stop-and-discuss' },
    @{ Pat = '^[a-zA-Z0-9_-]+\s*=\s*"[^"]*"\s*$'; Path = 'Cargo.toml$';                    Msg = 'new dependency? default-features = false first, justify weight in the commit message (swiss-dependency-review)' },
    @{ Pat = '.';                                  Path = '(^|/)(master.key|gateway.config.json|managed.json|tunnels.json|jobs.json|.env|.log|.cast)$'; Msg = 'never-commit file in the diff - state files and recordings carry real secrets' }
)

$file = ''
$findings = 0
foreach ($line in ($diff -split "\r?\n")) {
    if ($line -match '^\+\+\+ b/(.*)$') { $file = $Matches[1] -replace '\\','/'; continue }
    if ($file -eq '' -or -not $line.StartsWith('+') -or $line.StartsWith('+++')) { continue }
    $added = $line.Substring(1)
    foreach ($r in $Rules) {
        $pathOk = ($r.Path -eq '') -or ($file -match $r.Path)
        if ($pathOk -and $added -match $r.Pat) {
            # Skip test code for rules that only govern production paths.
            if ($added -match '#[cfg(test)]|mod tests' -and $r.Pat -eq '.unwrap()') { continue }
            $findings++
            Write-Host "[$findings] $file :: $added"
            Write-Host "      -> $($r.Msg)"
        }
    }
}

Write-Host ''
if ($findings -eq 0) {
    Write-Host "no mechanical red flags in the diff against $Base (untracked files not scanned)"
} else {
    Write-Host "$findings mechanical finding(s) against $Base - judge each against the rule; this scan never blocks"
}
exit 0
