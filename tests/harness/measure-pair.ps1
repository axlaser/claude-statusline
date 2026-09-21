#Requires -Version 5.1

<#
.SYNOPSIS
    Interleaved before/after medians for two builds of claude-statusline.

.DESCRIPTION
    Measures two builds of THIS crate against each other on one host, under the
    rules in docs/performance.md section 3: one fresh process per probe, a
    median of at least seven runs, interleaved, isolated USERPROFILE and TEMP,
    a proof of work on every variant, and a real .git staged -- without which a
    change to src/git.rs measures nothing at all.

    Three tick shapes, because they cost differently and are caused differently:

      warm      nothing cleared between probes. The git cache is fresh and the
                token record matches, so the tick renders from stored state.
      git-miss  only the git caches are cleared. This is the shape a pause
                longer than the 5s TTL produces, and the one section 6 records
                at 132 ms on Windows -- the oracle a working driver reproduces.
      cold      every tick cache is cleared: a new message AND a git miss.
      append    one record is appended to the transcript and nothing is
                cleared -- the tick after a new message, which is what the
                resume exists for and what no mode reached before.

    The proof-of-work assertion is a parameter. The version-segment check this
    driver used to hardcode straddled one specific commit and refused every
    other pair; -Proof and -ProofAfterOnly express that case without being it.

    The harness is deliberately exempt from the silent-degradation contract in
    CLAUDE.md. It fails loudly, because a measurement that quietly measured
    nothing reads as a spectacular win.

.EXAMPLE
    .\tests\harness\measure-pair.ps1 -Before old\claude-statusline.exe -After target\release\claude-statusline.exe

.EXAMPLE
    .\tests\harness\measure-pair.ps1 -Before a.exe -After b.exe -Mode git-miss -GitState clean

.EXAMPLE
    .\tests\harness\measure-pair.ps1 -Before a.exe -After b.exe -Payload payloads/full-with-version.json -ProofAfterOnly 'v2[.]1[.]270'
#>

[CmdletBinding()]
param(
    [string] $Before,
    [string] $After,
    [int]    $Runs = 15,
    [string] $Scratch = $env:TEMP,
    # A comma-separated string rather than a [string[]] with a ValidateSet:
    # `powershell -File` hands every argument through as one string and never
    # splits on commas, so the array form silently refused every multi-mode run
    # that was not typed at an interactive prompt. Validated below instead.
    [string] $Mode = 'warm,git-miss,cold',
    # The record appended before each probe in `append` mode.
    [int]    $AppendBytes = 400,
    [string] $GitState = 'dirty',
    [int]    $TranscriptBytes = 8388608,
    # Stages one agent transcript beside the session's, so the subagent
    # fallback tier is measurable at all. 0 stages none, which is the shape
    # `payloads/full.json` describes on its own.
    [int]    $AgentBytes = 0,
    [string] $Payload = 'payloads/full.json',
    [string] $Proof = '',
    [string] $ProofAfterOnly = '',
    [string] $Json = '',
    [switch] $SelfTest
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# A literal backslash without writing one: PowerShell string escaping and this
# file's own quoting rules both get in the way otherwise.
$BS = [char]92

function Fail([string] $Message) {
    Write-Host "measure-pair: $Message" -ForegroundColor Red
    throw $Message
}

function Note([string] $Message) {
    Write-Host "measure-pair: $Message"
}

# --- guards and helpers, each reachable from -SelfTest ----------------------

# The scratch root, validated before anything is created. A relative root would
# scatter a multi-gigabyte transcript across whatever directory the driver
# happened to start in, and an empty one would resolve to the current directory
# without saying so.
function Resolve-ScratchRoot([string] $Candidate) {
    if ([string]::IsNullOrWhiteSpace($Candidate)) {
        Fail 'no scratch directory: pass -Scratch an absolute path, or set TEMP'
    }
    if (-not [System.IO.Path]::IsPathRooted($Candidate)) {
        Fail "the scratch directory must be absolute, got '$Candidate'"
    }
    return (Join-Path $Candidate 'claude-statusline-measure-pair')
}

# Both storage layouts. The binary groups its state under
# claude-statusline-<owner>/, and an older one wrote flat in the temp root;
# clearing one and calling the run cold leaves the other warm and reports a
# flattering median rather than an error. See
# docs/solutions/workflow-issues/isolate-profile-and-temp-when-benchmarking-statusline.md
function Clear-TickCaches([string] $Root, [string] $Filter = 'statusline-*') {
    Get-ChildItem -LiteralPath $Root -Filter $Filter -File -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
    Get-ChildItem -LiteralPath $Root -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'claude-statusline-*' } |
        ForEach-Object {
            Get-ChildItem -LiteralPath $_.FullName -Filter $Filter -File -ErrorAction SilentlyContinue |
                Remove-Item -Force -ErrorAction SilentlyContinue
        }
}

# What each mode does before a probe. `warm` does nothing by design.
#
# `append` is the odd one out: it does not clear, it *writes*. The tick after a
# new message is the shape the transcript resume exists for, and clearing the
# token record instead -- which is what `cold` does -- measures the full read
# the resume is meant to avoid. The two are not the same tick and were being
# conflated.
function Clear-ForMode([string] $Root, [string] $TickMode, [string] $Transcript, [int] $Append) {
    switch ($TickMode) {
        'warm' { }
        'git-miss' { Clear-TickCaches $Root 'statusline-git-*' }
        'cold' { Clear-TickCaches $Root 'statusline-*' }
        'append' {
            if ($Transcript) {
                $line = '{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-opus-5","usage":{"input_tokens":' + $Append + ',"output_tokens":7}}}'
                Add-Content -LiteralPath $Transcript -Value $line -NoNewline -Encoding UTF8
                Add-Content -LiteralPath $Transcript -Value "`n" -NoNewline -Encoding UTF8
            }
        }
        default { Fail "unknown mode '$TickMode'" }
    }
}

# The proof of work, as a predicate over what a variant actually rendered. A
# probe that silently no-opped -- a subcommand that does not exist, a payload
# contract that moved -- otherwise reads as a spectacular speed-up.
function Test-Proof([string] $Text, [string[]] $Patterns) {
    foreach ($p in $Patterns) {
        if ($Text -notmatch $p) { return $false }
    }
    return $true
}

function Get-Median([double[]] $Values) {
    $s = @($Values | Sort-Object)
    $n = $s.Count
    if ($n -eq 0) { Fail 'no samples to take a median of' }
    if ($n % 2 -eq 1) { return $s[([int](($n - 1) / 2))] }
    return (($s[$n / 2 - 1] + $s[$n / 2]) / 2)
}

# --- isolated environment ---------------------------------------------------

$script:SavedEnv = $null

function Set-IsolatedEnv([string] $HomeRoot, [string] $TempRoot) {
    $script:SavedEnv = @{
        USERPROFILE       = $env:USERPROFILE
        TEMP              = $env:TEMP
        TMP               = $env:TMP
        STATUSLINE_DEBUG  = $env:STATUSLINE_DEBUG
    }
    $env:USERPROFILE = $HomeRoot
    $env:TEMP = $TempRoot
    $env:TMP = $TempRoot
    # Cleared so one variant is not charged for a log append the other skips.
    Remove-Item -Path 'env:STATUSLINE_DEBUG' -ErrorAction SilentlyContinue
}

function Restore-Env {
    if ($null -eq $script:SavedEnv) { return }
    foreach ($name in @('USERPROFILE', 'TEMP', 'TMP', 'STATUSLINE_DEBUG')) {
        $value = $script:SavedEnv[$name]
        if ($null -eq $value) {
            Remove-Item -Path "env:$name" -ErrorAction SilentlyContinue
        }
        else {
            Set-Item -Path "env:$name" -Value $value
        }
    }
    $script:SavedEnv = $null
}

# --- git state staging ------------------------------------------------------

# states.json is the single description both capture drivers already read, so a
# measurement stages the same repository a fixture does rather than inventing a
# second idea of what "dirty" means.
function Invoke-HarnessGit([object] $States, [string] $Cwd, [string[]] $GitArgs) {
    $argv = New-Object System.Collections.Generic.List[string]
    if ($Cwd) { $argv.Add('-C'); $argv.Add($Cwd) }
    foreach ($kv in $States.git_config) { $argv.Add('-c'); $argv.Add([string]$kv) }
    foreach ($a in $GitArgs) { $argv.Add([string]$a) }
    $null = & git @argv 2>$null
    return ($LASTEXITCODE -eq 0)
}

function New-GitState([object] $States, [string] $Name, [string] $Work, [string] $Remote) {
    $state = $States.git_states | Where-Object { $_.name -eq $Name }
    if (-not $state) { Fail "unknown git state '$Name' in states.json" }

    $savedGit = @{}
    foreach ($p in $States.git_env.PSObject.Properties) {
        $savedGit[$p.Name] = [Environment]::GetEnvironmentVariable($p.Name)
        Set-Item -Path ("env:" + $p.Name) -Value ([string]$p.Value)
    }
    try {
        foreach ($step in $state.steps) {
            $parts = @($step | ForEach-Object { [string]$_ })
            $verb = $parts[0]
            $rest = @($parts[1..($parts.Count - 1)])
            switch ($verb) {
                'git' {
                    if (-not (Invoke-HarnessGit $States $Work $rest)) {
                        Fail "git step failed in state '$Name': $($rest -join ' ')"
                    }
                }
                'write' {
                    $target = Join-Path $Work $rest[0]
                    $parent = Split-Path -Parent $target
                    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
                    # Exact bytes, LF preserved: this content reaches a blob hash.
                    [System.IO.File]::WriteAllBytes($target, [System.Text.Encoding]::UTF8.GetBytes($rest[1]))
                }
                'mkdir' {
                    New-Item -ItemType Directory -Force -Path (Join-Path $Work $rest[0]) | Out-Null
                }
                { $_ -in @('remote-track', 'remote-only') } {
                    if (-not (Invoke-HarnessGit $States '' @('init', '--bare', '-b', 'main', $Remote))) {
                        Fail "bare remote init failed in state '$Name'"
                    }
                    if (-not (Invoke-HarnessGit $States $Work @('remote', 'add', 'origin', $Remote))) {
                        Fail "remote add failed in state '$Name'"
                    }
                    if ($verb -eq 'remote-track') {
                        if (-not (Invoke-HarnessGit $States $Work @('push', '-u', 'origin', 'HEAD'))) {
                            Fail "push failed in state '$Name'"
                        }
                    }
                }
                default { Fail "unknown step verb '$verb' in state '$Name'" }
            }
        }
    }
    finally {
        foreach ($name in @($savedGit.Keys)) {
            if ($null -eq $savedGit[$name]) {
                Remove-Item -Path ("env:" + $name) -ErrorAction SilentlyContinue
            }
            else {
                Set-Item -Path ("env:" + $name) -Value $savedGit[$name]
            }
        }
    }
}

# --- probing ----------------------------------------------------------------

# One fresh process, timed end to end, spawned directly. The driver used to time
# `cmd.exe /c "<exe> < payload"`, which charged every Windows figure for a shell
# the status line never spawns: measured at +15.2 ms warm and +16.2 ms cold.
function Invoke-Probe([string] $Exe, [byte[]] $PayloadBytes, [string] $Cwd, [string] $TempRoot) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    # Per-variant state, set on the child rather than on this process: the two
    # builds must not read each other's stored records. When a record format
    # changes between them -- a RECORD_VERSION bump is the ordinary case --
    # each rejects the other's and every probe measures a miss, which looks
    # exactly like a finding on whichever path the record was meant to skip.
    if ($TempRoot) {
        $psi.EnvironmentVariables['TEMP'] = $TempRoot
        $psi.EnvironmentVariables['TMP'] = $TempRoot
    }
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $psi.WorkingDirectory = $Cwd

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.StandardInput.BaseStream.Write($PayloadBytes, 0, $PayloadBytes.Length)
    $p.StandardInput.BaseStream.Flush()
    $p.StandardInput.Close()
    $out = $p.StandardOutput.ReadToEnd()
    $p.WaitForExit()
    $sw.Stop()

    $code = $p.ExitCode
    $p.Dispose()
    if ($code -ne 0) { Fail "a probe of $Exe exited $code" }
    return [PSCustomObject]@{ Ms = $sw.Elapsed.TotalMilliseconds; Out = $out }
}

function Assert-Work([string] $Exe, [string] $Label, [string[]] $Patterns, [string[]] $Forbidden, [byte[]] $PayloadBytes, [string] $Cwd, [string] $TempRoot) {
    $text = (Invoke-Probe $Exe $PayloadBytes $Cwd $TempRoot).Out
    if (-not (Test-Proof $text $Patterns)) {
        Fail "the $Label variant did not render what this pair claims to measure -- refusing to report a measurement of nothing"
    }
    foreach ($f in $Forbidden) {
        if ($text -match $f) { Fail "the $Label variant rendered '$f', which it was asserted not to know about" }
    }
}

# --- self-test --------------------------------------------------------------

# Exercises the guards that make a number evidence, without needing either
# binary. `cargo test` runs this, because the previous drivers rotted into
# unrunnable shape without anything noticing for two months.
function Invoke-SelfTest {
    $failures = New-Object System.Collections.Generic.List[string]
    function Check([string] $Name, [bool] $Ok, [string] $Detail) {
        if ($Ok) { Write-Host "  ok   $Name" }
        else { Write-Host "  FAIL $Name -- $Detail" -ForegroundColor Red; $failures.Add($Name) }
    }

    # A relative or empty scratch path is refused before any directory exists.
    foreach ($bad in @('', '   ', 'relative-scratch')) {
        $refused = $false
        try { Resolve-ScratchRoot $bad | Out-Null } catch { $refused = $true }
        Check "scratch-refused('$bad')" $refused 'the driver accepted a path it must refuse'
    }
    Check 'scratch-refusal-creates-nothing' (-not (Test-Path -LiteralPath 'relative-scratch')) 'a refused path was created anyway'
    $rooted = Resolve-ScratchRoot $env:TEMP
    Check 'scratch-accepted-absolute' ([System.IO.Path]::IsPathRooted($rooted)) "resolved to '$rooted'"

    # Cold mode clears both the flat and the claude-statusline-<owner>/ layouts.
    $t = Join-Path ([System.IO.Path]::GetTempPath()) ('measure-pair-selftest-' + [System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Force -Path (Join-Path $t 'claude-statusline-1000') | Out-Null
    $flat = Join-Path $t 'statusline-git-abc.txt'
    $nested = Join-Path $t 'claude-statusline-1000/statusline-git-abc.txt'
    $keep = Join-Path $t 'claude-statusline-1000/statusline-tokens-abc.txt'
    Set-Content -LiteralPath $flat -Value 'x' -NoNewline
    Set-Content -LiteralPath $nested -Value 'x' -NoNewline
    Set-Content -LiteralPath $keep -Value 'x' -NoNewline
    Clear-ForMode $t 'git-miss' '' 0
    Check 'git-miss-clears-flat' (-not (Test-Path -LiteralPath $flat)) 'the flat git cache survived'
    Check 'git-miss-clears-nested' (-not (Test-Path -LiteralPath $nested)) 'the state-directory git cache survived'
    Check 'git-miss-keeps-the-record' (Test-Path -LiteralPath $keep) 'the token record was cleared by a git-only mode'
    Clear-ForMode $t 'cold' '' 0
    Check 'cold-clears-the-record' (-not (Test-Path -LiteralPath $keep)) 'cold mode left a tick cache warm'
    Remove-Item -LiteralPath $t -Recurse -Force -ErrorAction SilentlyContinue

    # The proof-of-work guard fires on a binary that renders nothing. `sort.exe`
    # is the stub: it reads the payload, exits 0, and renders no box.
    $stub = Join-Path $env:SystemRoot 'System32/sort.exe'
    if (Test-Path -LiteralPath $stub) {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes('{}')
        $guarded = $false
        try {
            Assert-Work $stub 'stub' @([char]0x250F, 'Opus 5') @() $bytes ([System.IO.Path]::GetTempPath())
        }
        catch { $guarded = $true }
        Check 'proof-of-work-refuses-a-silent-probe' $guarded 'a binary that rendered no box was accepted'
    }
    else {
        Write-Host "  skip proof-of-work-refuses-a-silent-probe (no $stub)"
    }
    Check 'proof-of-work-accepts-a-real-render' (Test-Proof "$([char]0x250F) Opus 5" @([char]0x250F, 'Opus 5')) 'a real render failed its own guard'

    # The environment is left as it was found.
    $before = @{ USERPROFILE = $env:USERPROFILE; TEMP = $env:TEMP; TMP = $env:TMP }
    Set-IsolatedEnv 'C:/nowhere/home' 'C:/nowhere/tmp'
    Restore-Env
    $restored = ($env:USERPROFILE -eq $before.USERPROFILE) -and ($env:TEMP -eq $before.TEMP) -and ($env:TMP -eq $before.TMP)
    Check 'environment-restored' $restored 'the driver left USERPROFILE, TEMP or TMP pointing at its scratch root'

    $floorEnforced = [bool](Select-String -LiteralPath $PSCommandPath -Pattern 'requires at least 7 runs' -Quiet)
    Check 'runs-floor-is-enforced' $floorEnforced 'the section 3 sample floor is not enforced'

    if ($failures.Count -gt 0) { Fail ("self-test failures: " + ($failures -join ', ')) }
    Note "self-test passed"
}

if ($SelfTest) {
    Invoke-SelfTest
    return
}

# --- setup ------------------------------------------------------------------

if (-not $Before -or -not $After) { Fail 'both -Before and -After are required' }
if ($Runs -lt 7) { Fail "docs/performance.md section 3 requires at least 7 runs; got $Runs" }
$modes = @($Mode -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
if (-not $modes) { Fail 'no tick shape selected: -Mode takes warm, git-miss and cold' }
foreach ($m in $modes) {
    if ($m -notin @('warm', 'git-miss', 'cold', 'append')) { Fail "unknown mode '$m'" }
}
foreach ($exe in @($Before, $After)) {
    if (-not (Test-Path -LiteralPath $exe)) { Fail "no binary at $exe" }
}

$HarnessDir = Split-Path -Parent $PSCommandPath
$RepoRoot = (& git -C $HarnessDir rev-parse --show-toplevel).Trim()

# Before USERPROFILE moves: rustup resolves its toolchain from the working
# directory a native process inherits, and reports nothing once HOME is a
# scratch root.
$prevCwd = [Environment]::CurrentDirectory
[Environment]::CurrentDirectory = $RepoRoot.Replace('/', $BS)
$toolchain = try { (& rustc --version 2>$null | Out-String).Trim() } catch { '' }
[Environment]::CurrentDirectory = $prevCwd
if (-not $toolchain) { Fail 'rustc --version did not report a toolchain; the number needs one beside it' }

$root = Resolve-ScratchRoot $Scratch
if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }

$home2 = Join-Path $root 'home'
# The driver's own TEMP, so nothing it does lands in the real one. The
# probes never see it: they get $tmpBefore or $tmpAfter.
$tmp2 = Join-Path $root 'tmp'
# One state root per variant. The home root stays shared: what lives there is
# input -- the transcript, the changelog, the warmed model map -- while every
# per-tick record lives under TEMP.
$tmpBefore = Join-Path $root 'tmp-before'
$tmpAfter = Join-Path $root 'tmp-after'
$work = Join-Path $root 'repo/work'
$remote = Join-Path $root 'repo/remote.git'
foreach ($d in @((Join-Path $home2 '.claude/cache'), (Join-Path $home2 '.claude/projects/fixtures'), $tmp2, $tmpBefore, $tmpAfter, $work)) {
    New-Item -ItemType Directory -Force -Path $d | Out-Null
}

$states = Get-Content -LiteralPath (Join-Path $HarnessDir 'states.json') -Raw | ConvertFrom-Json
New-GitState $states $GitState $work $remote

# The learned model map, warmed before sampling: without it the first probes
# measure a miss the rest do not, and the harness writes wrong values into
# whatever profile it can reach.
Copy-Item -LiteralPath (Join-Path $HarnessDir 'inputs/model-windows.json') `
    -Destination (Join-Path $home2 '.claude/statusline-model-windows.json') -Force

# Claude Code's own cached changelog, read only when the payload carries a
# version. Staged regardless so -Payload payloads/full-with-version.json works.
$changelogLine = '- a changelog entry that is about this long, give or take'
$changelog = New-Object System.Text.StringBuilder
[void]$changelog.Append("# Changelog`n`n## 2.1.270`n`n")
for ($i = 0; $i -lt 900; $i++) { [void]$changelog.Append($changelogLine).Append("`n") }
[System.IO.File]::WriteAllText((Join-Path $home2 '.claude/cache/changelog.md'), $changelog.ToString(), (New-Object System.Text.UTF8Encoding $false))

# A transcript of the requested size, built from the pinned input's records so
# the token counts are the ones every other fixture reads.
$source = [System.IO.File]::ReadAllLines((Join-Path $HarnessDir 'inputs/transcript.jsonl')) | Where-Object { $_.Trim().Length -gt 0 }
if (-not $source) { Fail 'inputs/transcript.jsonl has no records' }
$transcriptPath = Join-Path $home2 '.claude/projects/fixtures/transcript.jsonl'
$sb = New-Object System.Text.StringBuilder
$i = 0
while ($sb.Length -lt $TranscriptBytes) {
    [void]$sb.Append($source[$i % $source.Count]).Append("`n")
    $i++
}
[System.IO.File]::WriteAllText($transcriptPath, $sb.ToString(), (New-Object System.Text.UTF8Encoding $false))
$transcriptBytesOnDisk = (Get-Item -LiteralPath $transcriptPath).Length

# The subagent fallback tier reads `<transcript dir>/<stem>/subagents/agent-*.jsonl`
# and re-parses one whole file whenever its (mtime, size) move -- which is every
# tick an agent is working. Nothing could measure that before.
$agentBytesOnDisk = 0
if ($AgentBytes -gt 0) {
    $agentDir = Join-Path $home2 '.claude/projects/fixtures/transcript/subagents'
    New-Item -ItemType Directory -Force -Path $agentDir | Out-Null
    $agentLine = '{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-opus-5","usage":{"input_tokens":1200,"cache_creation_input_tokens":400,"cache_read_input_tokens":41000}}}'
    $ab = New-Object System.Text.StringBuilder
    while ($ab.Length -lt $AgentBytes) { [void]$ab.Append($agentLine).Append("`n") }
    $agentPath = Join-Path $agentDir 'agent-probe.jsonl'
    [System.IO.File]::WriteAllText($agentPath, $ab.ToString(), (New-Object System.Text.UTF8Encoding $false))
    $agentBytesOnDisk = (Get-Item -LiteralPath $agentPath).Length
}

# The payload, with the same placeholders the fixture replay substitutes.
# {REPO} is the git work tree, not the scratch root: it is what reaches
# workspace.current_dir, and a directory with no .git measures no git at all.
$payloadSrc = Join-Path $HarnessDir $Payload
if (-not (Test-Path -LiteralPath $payloadSrc)) { Fail "no payload at $payloadSrc" }
$payloadRaw = Get-Content -LiteralPath $payloadSrc -Raw
# There is no single {TMP} any more: each variant has its own state root, so a
# payload naming one would point at neither. Refused rather than substituted,
# because the wrong root is a silently flattering measurement.
if ($payloadRaw -match '\{TMP\}') {
    Fail "$Payload uses {TMP}, which no longer has one value: each variant measures against its own state root"
}
$payloadText = $payloadRaw.
    Replace('{REPO}', $work.Replace($BS, '/')).
    Replace('{HOME}', $home2.Replace($BS, '/')).
    Replace('{SESSION}', 'fixture-session-0001')
$payloadBytes = [System.Text.Encoding]::UTF8.GetBytes($payloadText)

$patterns = @([char]0x250F, 'Opus 5')
if ($Proof) { $patterns += $Proof }

$results = [ordered]@{}
try {
    Set-IsolatedEnv $home2 $tmp2

    # Warm each variant once so the state files it reads exist, then prove the
    # work. Priming is untimed and happens before any assertion.
    Invoke-Probe $Before $payloadBytes $work $tmpBefore | Out-Null
    Invoke-Probe $After $payloadBytes $work $tmpAfter | Out-Null
    $afterPatterns = $patterns
    if ($ProofAfterOnly) { $afterPatterns = $patterns + $ProofAfterOnly }
    $beforeForbidden = @()
    if ($ProofAfterOnly) { $beforeForbidden = @($ProofAfterOnly) }
    Assert-Work $Before 'before' $patterns $beforeForbidden $payloadBytes $work $tmpBefore
    Assert-Work $After 'after' $afterPatterns @() $payloadBytes $work $tmpAfter

    foreach ($m in $modes) {
        $b = New-Object System.Collections.Generic.List[double]
        $a = New-Object System.Collections.Generic.List[double]
        # Interleaved: the part that is easy to skip and expensive to get wrong.
        # Any drift over the run would otherwise be charged wholly to whichever
        # variant ran second, and would look exactly like a finding.
        for ($r = 0; $r -lt $Runs; $r++) {
            Clear-ForMode $tmpBefore $m $transcriptPath $AppendBytes
            $b.Add((Invoke-Probe $Before $payloadBytes $work $tmpBefore).Ms)
            Clear-ForMode $tmpAfter $m $transcriptPath $AppendBytes
            $a.Add((Invoke-Probe $After $payloadBytes $work $tmpAfter).Ms)
        }
        $bm = Get-Median $b.ToArray()
        $am = Get-Median $a.ToArray()
        $results[$m] = [ordered]@{
            before_ms = [math]::Round($bm, 2)
            after_ms  = [math]::Round($am, 2)
            delta_ms  = [math]::Round($am - $bm, 2)
            delta_pct = [math]::Round((($am - $bm) / $bm) * 100, 1)
            before_min_ms = [math]::Round(($b | Measure-Object -Minimum).Minimum, 2)
            after_min_ms  = [math]::Round(($a | Measure-Object -Minimum).Minimum, 2)
        }
    }
}
finally {
    Restore-Env
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}

# --- report -----------------------------------------------------------------

$record = [ordered]@{
    schema          = 2
    host_class      = 'maintainer machine'
    host            = [Environment]::OSVersion.VersionString
    toolchain       = $toolchain
    before          = (Resolve-Path -LiteralPath $Before -ErrorAction SilentlyContinue).Path
    after           = (Resolve-Path -LiteralPath $After -ErrorAction SilentlyContinue).Path
    before_sha      = (& git -C $HarnessDir rev-parse --short HEAD).Trim()
    git_state       = $GitState
    payload         = $Payload
    transcript_bytes = $transcriptBytesOnDisk
    agent_bytes      = $agentBytesOnDisk
    runs            = $Runs
    modes           = $results
}

Note "host:       $($record.host) ($($record.host_class))"
Note "toolchain:  $toolchain"
Note "git state:  $GitState"
Note "transcript: $transcriptBytesOnDisk bytes"
if ($agentBytesOnDisk -gt 0) { Note "agent:      $agentBytesOnDisk bytes" }
Note "payload:    $Payload"
Note "runs:       $Runs interleaved pairs per mode"
foreach ($m in $results.Keys) {
    $r = $results[$m]
    Note ("{0,-9} before {1,8:N2} ms   after {2,8:N2} ms   delta {3,7:N2} ms ({4,5:N1}%)" -f `
            $m, $r.before_ms, $r.after_ms, $r.delta_ms, $r.delta_pct)
}
Note 'run the before binary against a copy of itself, same run count, to learn this sitting noise floor before believing a small delta'

if ($Json) {
    $record | ConvertTo-Json -Depth 6 |
        Set-Content -LiteralPath $Json -Encoding UTF8
    Note "wrote $Json"
}
