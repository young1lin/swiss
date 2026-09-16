# Copyright 2026 The swiss authors
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

# Prepend the Apache-2.0 notice to every source file in the code trees that lacks one.
# Idempotent (a file already carrying the notice is left alone), LF-only, UTF-8 without BOM.
# Re-run after adding files; never point it at docs/, assets/, vendored or generated trees.
[CmdletBinding()]
param()
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

$notice = @(
  "Copyright 2026 The swiss authors"
  ""
  "Licensed under the Apache License, Version 2.0 (the `"License`");"
  "you may not use this file except in compliance with the License."
  "You may obtain a copy of the License at"
  ""
  "    https://www.apache.org/licenses/LICENSE-2.0"
  ""
  "Unless required by applicable law or agreed to in writing, software"
  "distributed under the License is distributed on an `"AS IS`" BASIS,"
  "WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied."
  "See the License for the specific language governing permissions and"
  "limitations under the License."
)
$lf = [string][char]10
$starred = ($notice | ForEach-Object { " * $_" }) -join $lf
$block = "/*" + $lf + $starred + $lf + " */"
$hash = ($notice | ForEach-Object { if ($_ -eq "") { "#" } else { "# $_" } }) -join $lf
$xml = "<!--" + $lf + $starred + $lf + "-->"

$blockExt = ".rs", ".js", ".mjs", ".ts", ".mts", ".css"
$hashExt = ".ps1", ".yml", ".yaml"
$xmlExt = ".html"

# Code trees only. docs/ (prose), vendored third-party JS, dev node_modules and any nested
# docs directory stay out — their files carry their own terms or none at all.
$scan = @((Join-Path $root "src"), (Join-Path $root "crates"), (Join-Path $root "tests"), (Join-Path $root "scripts"), (Join-Path $root ".github"))
$scan += @(Join-Path $root "build.rs" | Where-Object { Test-Path $_ })

$utf8 = New-Object System.Text.UTF8Encoding($false)
$added = 0
$skipped = 0
foreach ($dir in $scan) {
  Get-ChildItem -LiteralPath $dir -Recurse -File -ErrorAction SilentlyContinue | Where-Object {
    $rel = $_.FullName.Substring($root.Length + 1)
    (($blockExt -contains $_.Extension) -or ($hashExt -contains $_.Extension) -or ($xmlExt -contains $_.Extension)) -and
    ($rel -notmatch '(^|[\\/])(vendor|node_modules|docs)([\\/])')
  } | ForEach-Object {
    $text = [System.IO.File]::ReadAllText($_.FullName)
    if ($text.Length -gt 0 -and $text[0] -eq [char]0xFEFF) { $text = $text.Substring(1) }
    if ($text.Contains("Licensed under the Apache License")) { $script:skipped++; return }
    $header = if ($blockExt -contains $_.Extension) { $block } elseif ($xmlExt -contains $_.Extension) { $xml } else { $hash }
    $body = if ($text.StartsWith("#!")) {
      # Keep the shebang on line one; the notice follows it.
      $first = $text.IndexOf($lf)
      if ($first -lt 0) { $text + $lf + $lf + $header } else { $text.Substring(0, $first + 1) + $header + $lf + $lf + $text.Substring($first + 1) }
    } else {
      $header + $lf + $lf + $text
    }
    [System.IO.File]::WriteAllText($_.FullName, $body, $utf8)
    $script:added++
  }
}
"headers added: $added; already carried one: $skipped"
