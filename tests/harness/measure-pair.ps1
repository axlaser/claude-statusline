#Requires -Version 5.1
<#
.SYNOPSIS
    Paired binary-vs-binary measurement for Windows.

.DESCRIPTION
    measure.ps1 pairs a runtime script against the binary that replaced it. That
    pairing cannot answer "did this commit make the binary slower", and it
    cannot run at all now that the script trees are deleted, so a hot-path
    change that adds work to the binary had no mandated tool. This is that tool:
    two builds of this crate, interleaved, on one host.

    docs/performance.md section 3 governs the method and this file implements it
    literally, the same way measure.ps1 does:

      - One fresh process per probe. Production spawns a new process every tick.
      - Median of >= 7 runs, host and toolchain recorded beside the numbers.
      - Interleaved, so a machine that gets busier halfway through does not
        charge the drift to whichever variant ran second.
      - Isolated USERPROFILE and TEMP, so a probe cannot read or write the real
        profile and cannot race a live session over a shared state file.
      - Warm and cold, because a warm-only pair flatters whichever variant
        caches more.

    Proof of work comes first. Each variant is run once and inspected before any
    sample is taken, and a variant that did not do the work this measurement is
    about aborts the run. A probe that silently no-ops reads as a spectacular
    speed-up, which is the one failure that still looks like evidence.

    Like the rest of the harness this is a development tool, not a runtime
    script: it fails loudly.

.EXAMPLE
    .\measure-pair.ps1 -Before ..\before\target\release\claude-statusline.exe -After .\target\release\claude-statusline.exe
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)] [string] $Before,
    [Parameter(Mandatory = $true)] [string] $After,
    [int] $Runs = 15,
    [string] $Scratch = $env:TEMP
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$BS = [char]92

function Fail([string] $Message) { Write-Host "measure-pair: $Message" -ForegroundColor Red; throw $Message }

if ($Runs -lt 7) { Fail "docs/performance.md section 3 requires at least 7 runs; got $Runs" }
foreach ($exe in @($Before, $After)) {
    if (-not (Test-Path -LiteralPath $exe)) { Fail "no binary at $exe" }
}

$HarnessDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = (& git -C $HarnessDir rev-parse --show-toplevel).Trim()

# Captured here, before this script repoints USERPROFILE at the isolated home.
# rustup keeps its toolchains under USERPROFILE and resolves which one to run
# from the working directory a native process inherits, which Set-Location does
# not change -- so asked any later, or from anywhere else, it reports nothing
# and the numbers would land with no toolchain beside them. docs/performance.md
# section 3 requires one.
$prevCwd = [Environment]::CurrentDirectory
[Environment]::CurrentDirectory = $RepoRoot.Replace('/', $BS)
$toolchain = try { (& rustc --version 2>$null | Out-String).Trim() } catch { '' }
[Environment]::CurrentDirectory = $prevCwd
if (-not $toolchain) { Fail 'rustc --version did not report a toolchain; the number needs one beside it' }

$root = Join-Path $Scratch 'claude-statusline-measure-pair'
if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
$home2 = Join-Path $root 'home'
$tmp2  = Join-Path $root 'tmp'
New-Item -ItemType Directory -Force -Path (Join-Path $home2 '.claude/cache') | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $home2 '.claude/projects/p') | Out-Null
New-Item -ItemType Directory -Force -Path $tmp2 | Out-Null

# The real cache's shape: several hundred KB, newest heading first. The heading
# names the running version, so the row renders the up-to-date branch, which is
# the common case and the cheaper one to measure.
$changelog = Join-Path $home2 '.claude/cache/changelog.md'
$body = "# Changelog`n`n## 2.1.270`n`n" + ('- a changelog entry that is about this long, give or take' * 12 + "`n") * 1100
Set-Content -LiteralPath $changelog -Value $body -NoNewline -Encoding UTF8

# A transcript with real content, so the tick does the work a tick does.
$transcript = Join-Path $home2 '.claude/projects/p/t.jsonl'
$line = '{"type":"assistant","message":{"usage":{"input_tokens":12,"output_tokens":34,"cache_creation_input_tokens":5,"cache_read_input_tokens":900}}}'
Set-Content -LiteralPath $transcript -Value ((1..4000 | ForEach-Object { $line }) -join "`n") -Encoding UTF8

# The pinned payload, with its placeholders resolved into the isolated roots.
# full-with-version.json is full.json plus the top-level version field, which is
# what gates the changelog read: without it the read never happens and the pair
# measures nothing.
$payloadSrc = Join-Path $HarnessDir 'payloads/full-with-version.json'
if (-not (Test-Path -LiteralPath $payloadSrc)) { Fail "no payload at $payloadSrc" }
$payload = Join-Path $root 'payload.json'
(Get-Content -LiteralPath $payloadSrc -Raw).Replace('{REPO}', $root.Replace($BS, '/')).Replace('{HOME}', $home2.Replace($BS, '/')).Replace('{TMP}', $tmp2.Replace($BS, '/')) | Set-Content -LiteralPath $payload -Encoding ASCII -NoNewline

$env:USERPROFILE = $home2
$env:TEMP = $tmp2
$env:TMP  = $tmp2

function Invoke-Probe([string] $exe) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & cmd.exe /c "`"$exe`" < `"$payload`" > nul" | Out-Null
    $sw.Stop()
    if ($LASTEXITCODE -ne 0) { Fail "a probe exited $LASTEXITCODE" }
    return $sw.Elapsed.TotalMilliseconds
}

function Get-Median([double[]] $xs) {
    $s = $xs | Sort-Object
    return $s[[math]::Floor($s.Count / 2)]
}

# The guard that makes the numbers evidence. Both variants must render a box,
# and the after variant must render the version segment the changelog read
# produces -- if it does not, the read was gated off and the pair is timing the
# same code twice.
function Assert-Work([string] $exe, [bool] $expectVersion) {
    $text = (& cmd.exe /c "`"$exe`" < `"$payload`"") -join "`n"
    if ($text -notmatch 'Opus 5') { Fail "$exe rendered no model row" }
    if ($expectVersion -and ($text -notmatch 'v2[.]1[.]270')) {
        Fail "$exe rendered no version segment: the changelog read did not run, so this pair would measure nothing"
    }
    if ((-not $expectVersion) -and ($text -match 'v2[.]1[.]270')) {
        Fail "$exe rendered a version segment it should not know about"
    }
}

# Warm each variant once so the state files it reads exist, then prove the work.
Invoke-Probe $Before | Out-Null
Invoke-Probe $After  | Out-Null
Assert-Work $Before $false
Assert-Work $After  $true

$results = @{}
foreach ($mode in 'warm', 'cold') {
    $b = New-Object System.Collections.Generic.List[double]
    $a = New-Object System.Collections.Generic.List[double]
    for ($i = 0; $i -lt $Runs; $i++) {
        if ($mode -eq 'cold') { Get-ChildItem $tmp2 -Recurse -Force | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue }
        $b.Add((Invoke-Probe $Before))
        if ($mode -eq 'cold') { Get-ChildItem $tmp2 -Recurse -Force | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue }
        $a.Add((Invoke-Probe $After))
    }
    $bm = Get-Median $b.ToArray(); $am = Get-Median $a.ToArray()
    $results[$mode] = [pscustomobject]@{
        before = [math]::Round($bm, 2)
        after  = [math]::Round($am, 2)
        delta  = [math]::Round($am - $bm, 2)
        pct    = [math]::Round((($am - $bm) / $bm) * 100, 1)
        bmin   = [math]::Round(($b | Measure-Object -Minimum).Minimum, 2)
        amin   = [math]::Round(($a | Measure-Object -Minimum).Minimum, 2)
    }
}

Write-Host ""
Write-Host "host:       $([Environment]::OSVersion.VersionString)"
Write-Host "toolchain:  $toolchain"
Write-Host "changelog:  $((Get-Item -LiteralPath $changelog).Length) bytes"
Write-Host "transcript: $((Get-Item -LiteralPath $transcript).Length) bytes"
Write-Host "runs:       $Runs interleaved pairs per mode"
Write-Host ""
foreach ($mode in 'warm', 'cold') {
    $r = $results[$mode]
    Write-Host ("{0,-5} before {1,7:N2} ms (min {2,7:N2})   after {3,7:N2} ms (min {4,7:N2})   delta {5,6:N2} ms  ({6,5:N1} %)" -f $mode, $r.before, $r.bmin, $r.after, $r.amin, $r.delta, $r.pct)
}
