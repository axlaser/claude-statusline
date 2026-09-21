#Requires -Version 5.1
<#
.SYNOPSIS
    Paired script-vs-binary measurement for Windows.

.DESCRIPTION
    Produces the end-to-end fresh-process medians required before a
    component's scripts are deleted, for the script and the binary, on one host,
    with their runs interleaved.

    docs/performance.md section 3 governs the method and this file implements it
    literally:

      - One fresh process per probe. The hot cost here is process creation, and
        a warm loop understates it by up to 100x -- production spawns a new
        interpreter every tick, so the harness must too.
      - Median of >= 7 runs, with the host and interpreter versions recorded
        beside the numbers.
      - Interleaved, because a machine that gets busier halfway through would
        otherwise charge the whole drift to whichever variant ran second.
      - Isolated USERPROFILE and TEMP, so a probe cannot read or write the real
        profile and cannot race a live session over a shared state file.

    Like the capture harness this is a development tool, not a runtime script:
    it fails loudly. A measurement that silently measured nothing is worse than
    no measurement, because the number still looks like evidence.

.EXAMPLE
    .\measure.ps1 -Component subagent
    .\measure.ps1 -Component subagent -Runs 15 -Json out.json
#>

[CmdletBinding()]
param(
    [ValidateSet('subagent', 'statusline')]
    [string] $Component = 'subagent',
    [int]    $Runs = 11,
    [string] $Payload,
    [int] $TranscriptBytes = 0,
    [switch] $ColdCache,
    [string] $Binary,
    [string] $Json
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$HarnessDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot   = (& git -C $HarnessDir rev-parse --show-toplevel).Trim() -replace '/', '\'

function Fail([string] $Message) { Write-Host "measure: $Message" -ForegroundColor Red; throw $Message }

if ($Runs -lt 7) { Fail "docs/performance.md section 3 requires at least 7 runs; got $Runs" }

# What each component's two variants are. The script side is invoked the way
# Claude Code invokes it -- a fresh interpreter reading the payload on stdin --
# because that, not the script body, is where the cost lives.
if ($Component -notin @('subagent', 'statusline')) { Fail "unknown component '$Component'" }

$script = switch ($Component) {
    'subagent'   { Join-Path $RepoRoot 'windows\subagent-statusline.ps1' }
    'statusline' { Join-Path $RepoRoot 'windows\statusline.ps1' }
}
if (-not $Payload) {
    $Payload = switch ($Component) {
        'subagent'   { Join-Path $RepoRoot 'tests\harness\payloads\tasks-feed.json' }
        'statusline' { Join-Path $RepoRoot 'tests\harness\payloads\full.json' }
    }
}
if (-not $Binary) { $Binary = Join-Path $RepoRoot 'target\release\claude-statusline.exe' }

foreach ($required in @($script, $Payload, $Binary)) {
    if (-not (Test-Path -LiteralPath $required)) {
        Fail "missing $required (build the release binary first: cargo build --release)"
    }
}

$subcommand = switch ($Component) { 'subagent' { 'subagent' } 'statusline' { 'statusline' } }
$feedName   = switch ($Component) { 'subagent' { 'statusline-tasks-fixture-session-0001.json' } 'statusline' { $null } }

$RealPowerShell = (Get-Command powershell.exe).Source

# The statusline pair must cover the large-transcript state. The
# transcript is GENERATED to a target size rather than pointed at a real one:
# a machine-local session file is not reproducible on CI, on another machine,
# or next month, and a performance number nobody else can reproduce is an
# anecdote. Repeating one pinned record keeps the token totals a pure function
# of the size.
function New-MeasureTranscript([string] $Path, [int] $TargetBytes) {
    $record = [System.IO.File]::ReadAllText((Join-Path $RepoRoot 'tests\harness\inputs\transcript.jsonl'))
    $line = ($record -split "`n" | Where-Object { $_.Trim() -ne '' } | Select-Object -First 1) + "`n"
    $builder = [System.Text.StringBuilder]::new()
    while ($builder.Length -lt $TargetBytes) { [void]$builder.Append($line) }
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Path) | Out-Null
    [System.IO.File]::WriteAllText($Path, $builder.ToString(), (New-Object System.Text.UTF8Encoding $false))
}

# ---------------------------------------------------------------------------
# Isolated roots
# ---------------------------------------------------------------------------

$root = Join-Path ([System.IO.Path]::GetTempPath()) ("statusline-measure-" + [System.IO.Path]::GetRandomFileName())
$home_ = Join-Path $root 'home'
$tmp   = Join-Path $root 'tmp'
New-Item -ItemType Directory -Force -Path (Join-Path $home_ '.claude'), $tmp | Out-Null

$savedHome = $env:USERPROFILE
$savedTemp = $env:TEMP
$savedTmp  = $env:TMP
$savedDebug = $env:STATUSLINE_DEBUG

# Both variants pay for debug logging or neither does. Leaving it set from the
# ambient shell would charge one side an extra file append per tick.
$env:STATUSLINE_DEBUG = $null
$env:USERPROFILE = $home_
$env:TEMP = $tmp
$env:TMP  = $tmp

function Invoke-Probe([string] $CommandLine) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & cmd.exe /c $CommandLine | Out-Null
    $sw.Stop()
    if ($LASTEXITCODE -ne 0) { Fail "a probe exited $LASTEXITCODE" }
    return $sw.Elapsed.TotalMilliseconds
}

# The statusline reads state the payload points at, so the payload's
# placeholders are resolved into the isolated roots and the state it names is
# staged there. Without this both variants would parse no transcript and the
# pair would time an empty session.
$probePayload = $Payload
if ($Component -eq 'statusline') {
    $transcript = Join-Path $home_ '.claude\projects\fixtures\transcript.jsonl'
    if ($TranscriptBytes -gt 0) {
        New-MeasureTranscript $transcript $TranscriptBytes
    } else {
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $transcript) | Out-Null
        Copy-Item (Join-Path $RepoRoot 'tests\harness\inputs\transcript.jsonl') $transcript
    }
    Copy-Item (Join-Path $RepoRoot 'tests\harness\inputs\model-windows.json') `
        (Join-Path $home_ '.claude\statusline-model-windows.json')

    $body = [System.IO.File]::ReadAllText($Payload)
    $body = $body.Replace('{HOME}', ($home_ -replace '\\', '/'))
    $body = $body.Replace('{TMP}',  ($tmp   -replace '\\', '/'))
    $body = $body.Replace('{REPO}', ($root  -replace '\\', '/'))
    $probePayload = Join-Path $root 'payload.json'
    [System.IO.File]::WriteAllText($probePayload, $body, (New-Object System.Text.UTF8Encoding $false))
}

$scriptLine = '"' + $RealPowerShell + '" -NoProfile -File "' + $script + '" < "' + $probePayload + '"'
$binaryLine = '"' + $Binary + '" ' + $subcommand + ' < "' + $probePayload + '"'

$scriptTimes = @()
$binaryTimes = @()

try {
    # Each variant is proven to do its work before anything is timed. A probe
    # that silently no-opped -- a missing dependency, a changed payload
    # contract -- would otherwise be reported as a spectacular speed-up.
    foreach ($pair in @(@('script', $scriptLine), @('binary', $binaryLine))) {
        if ($Component -eq 'statusline') {
            # The statusline's observable is stdout, so that is what proves it
            # worked. A box with the model row in it cannot be produced by a
            # variant that failed to parse the payload.
            $out = (& cmd.exe /c $pair[1]) -join "`n"
            if ($out -notmatch '┏' -or $out -notmatch 'Opus 5') {
                Fail "the $($pair[0]) variant rendered no box -- refusing to report a measurement of nothing"
            }
        } else {
            Remove-Item -LiteralPath (Join-Path $tmp $feedName) -Force -ErrorAction SilentlyContinue
            & cmd.exe /c $pair[1] | Out-Null
            $produced = Join-Path $tmp $feedName
            if (-not (Test-Path -LiteralPath $produced) -or (Get-Item -LiteralPath $produced).Length -eq 0) {
                Fail "the $($pair[0]) variant wrote no feed -- refusing to report a measurement of nothing"
            }
        }
    }

    # -ColdCache measures the state where both variants actually do their work.
    # Without it the script serves a warm output cache -- keyed on a 5-second
    # bucket, and a whole run finishes inside one -- so most of its probes
    # render nothing at all, and the pair reads as the script's best case
    # against the binary's only case.
    # Both layouts are cleared. The scripts wrote flat into the temp root; the
    # binary groups its files under claude-statusline-<owner>. Clearing only the
    # flat filter would leave the binary's caches warm while the run still
    # labelled itself cold, which reads as a flattering median rather than an
    # error -- the mislabelled-sample failure
    # docs/solutions/workflow-issues/isolate-profile-and-temp-when-benchmarking-statusline.md
    # exists to prevent.
    function Clear-TickCaches {
        Get-ChildItem -LiteralPath $tmp -Filter 'statusline-*' -ErrorAction SilentlyContinue |
            Remove-Item -Force -ErrorAction SilentlyContinue
        Get-ChildItem -LiteralPath $tmp -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^claude-statusline-\d+$' } |
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }

    for ($i = 0; $i -lt $Runs; $i++) {
        if ($ColdCache) { Clear-TickCaches }
        $scriptTimes += Invoke-Probe $scriptLine
        if ($ColdCache) { Clear-TickCaches }
        $binaryTimes += Invoke-Probe $binaryLine
    }
} finally {
    $env:USERPROFILE = $savedHome
    $env:TEMP = $savedTemp
    $env:TMP  = $savedTmp
    if ($null -eq $savedDebug) {
        Remove-Item -Path 'env:STATUSLINE_DEBUG' -ErrorAction SilentlyContinue
    } else {
        $env:STATUSLINE_DEBUG = $savedDebug
    }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

function Get-Median([double[]] $Values) {
    $sorted = @($Values | Sort-Object)
    $n = $sorted.Count
    if ($n % 2 -eq 1) { return $sorted[[int](($n - 1) / 2)] }
    return ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2
}

$scriptMedian = Get-Median $scriptTimes
$binaryMedian = Get-Median $binaryTimes
$delta = if ($scriptMedian -gt 0) { ($binaryMedian - $scriptMedian) / $scriptMedian * 100 } else { 0 }

$osVersion = [System.Environment]::OSVersion.VersionString
$psVersion = $RealPowerShell + ' ' + (& $RealPowerShell -NoProfile -Command '$PSVersionTable.PSVersion.ToString()').Trim()
$commit = (& git -C $RepoRoot rev-parse HEAD).Trim()

$result = [ordered]@{
    schema     = 1
    component  = $Component
    host_class = 'maintainer machine'
    host       = "$osVersion; $psVersion"
    runner     = $null
    image      = $null
    commit     = $commit
    payload    = $Payload.Substring($RepoRoot.Length + 1) -replace '\\', '/'
    runs       = $Runs
    script_ms  = [ordered]@{ median = [math]::Round($scriptMedian, 1); min = [math]::Round(($scriptTimes | Measure-Object -Minimum).Minimum, 1); max = [math]::Round(($scriptTimes | Measure-Object -Maximum).Maximum, 1) }
    binary_ms  = [ordered]@{ median = [math]::Round($binaryMedian, 1); min = [math]::Round(($binaryTimes | Measure-Object -Minimum).Minimum, 1); max = [math]::Round(($binaryTimes | Measure-Object -Maximum).Maximum, 1) }
    delta_pct  = [math]::Round($delta, 1)
}

Write-Host ""
Write-Host "component:  $Component"
Write-Host "host:       $($result.host)"
Write-Host "commit:     $commit"
Write-Host "runs:       $Runs interleaved pairs"
Write-Host ""
Write-Host ("script  median {0,7:N1} ms   (min {1,7:N1}  max {2,7:N1})" -f $result.script_ms.median, $result.script_ms.min, $result.script_ms.max)
Write-Host ("binary  median {0,7:N1} ms   (min {1,7:N1}  max {2,7:N1})" -f $result.binary_ms.median, $result.binary_ms.min, $result.binary_ms.max)
Write-Host ("delta          {0,7:N1} %" -f $result.delta_pct)

if ($Json) {
    [System.IO.File]::WriteAllText($Json, ($result | ConvertTo-Json -Depth 5), (New-Object System.Text.UTF8Encoding $false))
    Write-Host "wrote $Json"
}
