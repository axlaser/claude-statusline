#Requires -Version 5.1
<#
.SYNOPSIS
    Fixture-capture harness for Windows.

.DESCRIPTION
    Drives the current windows\ scripts under a fully isolated USERPROFILE and
    TEMP and stores each component's observable as a golden fixture, so the Rust
    port has something to be equivalent to after the scripts are deleted. The
    functional twin of capture.sh; both read cases.json and states.json, so a
    case is defined once and captured on all three platforms.

    Windows capture runs on the maintainer's machine rather than on CI,
    because the Windows script tree is the one with no hosted equivalent of the
    developer's real environment.

    This is a development tool, not a runtime script: the silent-degradation and
    no-`exit` contracts in CLAUDE.md do not apply here, and must not. A capture
    that cannot be trusted has to fail loudly, because a harness that degrades
    silently writes a plausible fixture that everything downstream then trusts.

.EXAMPLE
    .\capture.ps1 -List
    .\capture.ps1 -Component git-refresh
    .\capture.ps1 -Case git-clean -Out C:\tmp\fixtures
    .\capture.ps1 -At 5d474a0        # regenerate a historical commit's fixtures
    .\capture.ps1 -Verify            # capture twice, require byte-identical output
#>

[CmdletBinding()]
param(
    [string] $Case,
    [string] $Component,
    [string] $At,
    [string] $Out,
    [switch] $AllowDirty,
    [switch] $List,
    [switch] $Verify
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$HarnessDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot   = (& git -C $HarnessDir rev-parse --show-toplevel).Trim() -replace '/', '\'
$Platform   = 'windows'

if (-not $Out) { $Out = Join-Path $RepoRoot 'tests\fixtures' }

# Every read in this file goes through .NET rather than Get-Content. Windows
# PowerShell 5.1 decodes a BOM-less file as Windows-1252, which would silently
# mangle the astral-plane payload -- the one input whose whole purpose is to
# carry characters outside the BMP -- and quietly corrupt every fixture taken
# from it. .NET's ReadAllText defaults to UTF-8 with BOM detection.
function Read-Utf8([string] $Path) { return [System.IO.File]::ReadAllText($Path) }
function Write-Utf8([string] $Path, [string] $Text) {
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding $false))
}

$CasesFile  = Join-Path $HarnessDir 'cases.json'
$StatesFile = Join-Path $HarnessDir 'states.json'
$Cases      = Read-Utf8 $CasesFile  | ConvertFrom-Json
$States     = Read-Utf8 $StatesFile | ConvertFrom-Json

# Resolved before the shim directory is ever prepended to PATH. The harness has
# to keep reaching the real interpreter after powershell.cmd starts shadowing
# the name for everything the scripts spawn.
$RealPowerShell = (Get-Command powershell.exe).Source
$RealHome       = $env:USERPROFILE

function Fail([string] $Message) { throw "harness: $Message" }
function Note([string] $Message) { Write-Host "  $Message" }

# git writes progress and hints to stderr on commands that succeed -- `push` and
# `worktree add` always do. Under $ErrorActionPreference = 'Stop' a native
# command's stderr becomes a terminating error record, so a normal push would
# abort the harness. Every git call in this file goes through here.
function Invoke-Git([string[]] $Arguments) {
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & git @Arguments 2>$null | Out-Null
        return $LASTEXITCODE
    } finally { $ErrorActionPreference = $saved }
}

# ---------------------------------------------------------------------------
# Source of the scripts under capture
# ---------------------------------------------------------------------------

$Worktree = $null
if ($At) {
    $SourceCommit = (& git -C $RepoRoot rev-parse --verify "$At^{commit}").Trim()
    if ($LASTEXITCODE -ne 0) { Fail "cannot resolve commit '$At'" }
    $Worktree = Join-Path ([System.IO.Path]::GetTempPath()) ("statusline-harness-src-" + [System.IO.Path]::GetRandomFileName())
    if ((Invoke-Git @('-C', $RepoRoot, 'worktree', 'add', '--detach', $Worktree, $SourceCommit)) -ne 0) {
        Fail "cannot check out $SourceCommit"
    }
    $ScriptsRoot = $Worktree
} else {
    $SourceCommit = (& git -C $RepoRoot rev-parse HEAD).Trim()
    $ScriptsRoot  = $RepoRoot
    if (-not $AllowDirty) {
        # A fixture records the commit its scripts came from. Capturing a dirty
        # tree records a commit that does not describe what actually ran, and
        # nothing downstream could tell.
        $dirty = & git -C $RepoRoot status --porcelain -- macos linux windows
        if ($dirty) {
            Write-Host "harness: the script trees are modified, so the recorded source commit"
            Write-Host "         would not describe the scripts that ran:"
            $dirty | ForEach-Object { Write-Host "         $_" }
            Fail "commit first, or pass -AllowDirty for a throwaway capture"
        }
    }
}

# ---------------------------------------------------------------------------
# Case selection
# ---------------------------------------------------------------------------

$Selected = @($Cases.cases | Where-Object {
    (-not $Component -or $_.component -eq $Component) -and
    (-not $Case      -or $_.case      -eq $Case)
})

if ($List) {
    $Cases.cases | ForEach-Object { "$($_.component)/$($_.case)" }
    return
}
if ($Selected.Count -eq 0) { Fail 'no cases matched the filters' }

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Get-SafeSessionId([string] $PayloadRelative) {
    $path = Join-Path $HarnessDir $PayloadRelative
    if (-not (Test-Path $path) -or (Get-Item $path).Length -eq 0) { return '' }
    $text = Read-Utf8 $path
    try {
        $raw = ($text | ConvertFrom-Json).session_id
    } catch {
        # The scripts recover the session id by scanning the raw text before
        # they ever parse, which is why a malformed payload still writes to
        # session-scoped paths. Resolving it the same way here keeps the
        # cache-miss assertion applicable to exactly the cases it should be.
        $m = [regex]::Match($text, '"session_id"\s*:\s*"([^"]*)"')
        $raw = if ($m.Success) { $m.Groups[1].Value } else { '' }
    }
    if (-not $raw) { return '' }
    # The scripts' own sanitisation, applied here so a case lands where the
    # script would actually put it rather than where the author guessed.
    return ($raw -replace '[^a-zA-Z0-9_-]', '')
}

function Set-Mtime([string] $Path, [long] $Epoch) {
    (Get-Item -LiteralPath $Path).LastWriteTimeUtc =
        [DateTimeOffset]::FromUnixTimeSeconds($Epoch).UtcDateTime
}

function Wait-Capture([string] $File) {
    # The status line backgrounds its notification spawn, so a shim's record can
    # land after the foreground process has already exited. Wait for the file to
    # stop growing rather than sleeping a fixed amount and hoping.
    $stable = 0
    $prev = -1
    for ($i = 0; $i -lt 40; $i++) {
        $now = if (Test-Path $File) { (Get-Item $File).Length } else { 0 }
        if ($now -eq $prev) {
            $stable++
            if ($stable -ge 3) { return }
        } else {
            $stable = 0
        }
        $prev = $now
        Start-Sleep -Milliseconds 125
    }
}

# ---------------------------------------------------------------------------
# Git state construction (states.json)
# ---------------------------------------------------------------------------

function Invoke-HarnessGit([string] $WorkDir, [string[]] $Arguments) {
    $prefix = @('-C', $WorkDir)
    foreach ($c in $States.git_config) { $prefix += @('-c', $c) }
    $code = Invoke-Git ($prefix + $Arguments)
    if ($code -ne 0) { Fail "git $($Arguments -join ' ') failed with exit $code" }
}

function Build-GitState([string] $Name, [string] $WorkDir, [string] $RemoteDir) {
    $state = $States.git_states | Where-Object { $_.name -eq $Name }
    if (-not $state) { Fail "unknown git state '$Name'" }

    foreach ($step in $state.steps) {
        $verb = $step[0]
        switch ($verb) {
            'git' {
                Invoke-HarnessGit $WorkDir @($step[1..($step.Count - 1)])
            }
            'write' {
                $target = Join-Path $WorkDir $step[1]
                New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
                # No BOM and LF endings: the repo is created with
                # core.autocrlf=false, so a CRLF or a BOM here would change the
                # blob hash and with it the commit hash the detached-HEAD state
                # renders.
                [System.IO.File]::WriteAllText($target, ($step[2] -replace "`r`n", "`n"),
                    (New-Object System.Text.UTF8Encoding $false))
            }
            'mkdir' {
                New-Item -ItemType Directory -Force -Path (Join-Path $WorkDir $step[1]) | Out-Null
            }
            'remote-track' {
                Invoke-Git @('init', '--bare', '-b', 'main', $RemoteDir) | Out-Null
                Invoke-HarnessGit $WorkDir @('remote', 'add', 'origin', $RemoteDir)
                Invoke-HarnessGit $WorkDir @('push', '-u', 'origin', 'HEAD')
            }
            'remote-only' {
                Invoke-Git @('init', '--bare', '-b', 'main', $RemoteDir) | Out-Null
                Invoke-HarnessGit $WorkDir @('remote', 'add', 'origin', $RemoteDir)
            }
            default { Fail "unknown step verb '$verb' in state '$Name'" }
        }
    }
}

# ---------------------------------------------------------------------------
# One case
# ---------------------------------------------------------------------------

function Invoke-CaptureCase($Spec, [string] $OutRoot) {
    $component  = $Spec.component
    $caseName   = $Spec.case
    $observable = $Spec.observable
    $payload    = $Spec.payload
    $gitState   = if ($Spec.PSObject.Properties['git_state']) { $Spec.git_state } else { $null }
    $config     = if ($Spec.PSObject.Properties['notify_config'] -and $Spec.notify_config) { $Spec.notify_config } else { $Cases.defaults.notify_config }
    $clock      = if ($Spec.PSObject.Properties['clock'] -and $Spec.clock) { $Spec.clock } else { $Cases.defaults.clock }
    $session    = Get-SafeSessionId $payload

    $caseRoot  = Join-Path ([System.IO.Path]::GetTempPath()) ("statusline-capture-" + [System.IO.Path]::GetRandomFileName())
    $homeDir   = Join-Path $caseRoot 'home'
    $tmpDir    = Join-Path $caseRoot 'tmp'
    $workDir   = Join-Path $caseRoot 'repo\work'
    $remoteDir = Join-Path $caseRoot 'repo\remote.git'
    $shimDir   = Join-Path $caseRoot 'shims'
    $captureFile = Join-Path $caseRoot 'capture.txt'

    foreach ($d in @((Join-Path $homeDir '.claude'), $tmpDir, $workDir, $shimDir)) {
        New-Item -ItemType Directory -Force -Path $d | Out-Null
    }
    Write-Utf8 $captureFile ''

    # The shim directory. The status line spawns its notification with
    # `Start-Process -FilePath 'powershell'`, which resolves through PATH.
    Copy-Item (Join-Path $HarnessDir 'shims\record.cmd') (Join-Path $shimDir 'powershell.cmd')
    # ...and addresses the script itself by path, which PATH cannot intercept,
    # so that end is recorded where it actually looks.
    Copy-Item (Join-Path $HarnessDir 'shims\record.ps1') (Join-Path $homeDir '.claude\notify.ps1')
    Copy-Item (Join-Path $HarnessDir $config) (Join-Path $homeDir '.claude\notify-config.json')

    if ($gitState) { Build-GitState $gitState $workDir $remoteDir }

    # Supplied state files: component defaults first, then the case's own.
    $defaults = @()
    if ($Cases.defaults.inputs_by_component.PSObject.Properties[$component]) {
        $defaults = @($Cases.defaults.inputs_by_component.$component)
    }
    $own = @()
    if ($Spec.PSObject.Properties['inputs'] -and $Spec.inputs) { $own = @($Spec.inputs) }
    $inputs = @($defaults) + @($own)

    # The scripts read the real wall clock -- there is no injection point in a
    # shell script -- so an intended mtime is materialised as an offset from
    # capture time and *recorded* as an offset from the pinned clock. Replaying
    # in Rust pins the clock to `clock` and the mtimes to clock+offset, which is
    # what the Clock trait exists to make possible.
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    foreach ($supplied in $inputs) {
        # Plain string replacement, not -replace: a Windows path is not a valid
        # regex replacement string, and one containing a `$` would silently lose
        # part of itself to group substitution.
        $abs = $supplied.target.
            Replace('{HOME}',    $homeDir).
            Replace('{TMP}',     $tmpDir).
            Replace('{REPO}',    $workDir).
            Replace('{SESSION}', $session).
            Replace('/', '\')
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $abs) | Out-Null
        Copy-Item (Join-Path $HarnessDir $supplied.content) $abs -Force
        $offset = if ($supplied.PSObject.Properties['mtime_offset']) { [long]$supplied.mtime_offset } else { 0 }
        Set-Mtime $abs ($now + $offset)
    }

    # Substituted payload. Placeholders carry forward slashes on every platform:
    # the PowerShell scripts reach the filesystem through .NET path APIs, which
    # accept them, so no driver has to escape a backslash into JSON.
    $payloadFile = Join-Path $caseRoot 'stdin.json'
    $payloadSrc  = Join-Path $HarnessDir $payload
    $body = if ((Test-Path $payloadSrc) -and (Get-Item $payloadSrc).Length -gt 0) {
        (Read-Utf8 $payloadSrc).
            Replace('{HOME}', $homeDir.Replace('\', '/')).
            Replace('{TMP}',  $tmpDir.Replace('\', '/')).
            Replace('{REPO}', $workDir.Replace('\', '/'))
    } else { '' }
    Write-Utf8 $payloadFile $body

    $stdoutFile = Join-Path $caseRoot 'stdout.txt'
    $stderrFile = Join-Path $caseRoot 'stderr.txt'
    $scriptDir  = Join-Path $ScriptsRoot 'windows'

    $savedHome    = $env:USERPROFILE
    $savedTemp    = $env:TEMP
    $savedTmp     = $env:TMP
    $savedPath    = $env:PATH
    $savedCapture = $env:STATUSLINE_CAPTURE_FILE

    $before = @()
    $after  = @()
    $rc     = 0
    try {
        $env:USERPROFILE = $homeDir
        $env:TEMP = $tmpDir
        $env:TMP  = $tmpDir
        $env:PATH = "$shimDir;$env:PATH"
        $env:STATUSLINE_CAPTURE_FILE = $captureFile

        # cmd redirection rather than a PowerShell pipeline: the scripts read
        # [Console]::In.ReadToEnd(), and a pipeline would hand them a re-encoded
        # string instead of the payload's exact bytes.
        function Invoke-Script([string] $ScriptName, [string[]] $ScriptArgs) {
            $quoted = @("-NoProfile", "-File", "`"$(Join-Path $scriptDir $ScriptName)`"") + $ScriptArgs
            $line = "`"$RealPowerShell`" $($quoted -join ' ') < `"$payloadFile`" > `"$stdoutFile`" 2> `"$stderrFile`""
            & cmd /c $line
            return $LASTEXITCODE
        }

        switch ($component) {
            'statusline' {
                Push-Location $workDir
                try { $rc = Invoke-Script 'statusline.ps1' @() } finally { Pop-Location }
            }
            'git-refresh' {
                $before = @(Get-ChildItem -Path $tmpDir -Recurse -File -Force |
                            ForEach-Object { $_.FullName.Substring($tmpDir.Length + 1) } | Sort-Object)
                $rc = Invoke-Script 'git-refresh.ps1' @()
                $after = @(Get-ChildItem -Path $tmpDir -Recurse -File -Force |
                           ForEach-Object { $_.FullName.Substring($tmpDir.Length + 1) } | Sort-Object)
            }
            'subagent' {
                $rc = Invoke-Script 'subagent-statusline.ps1' @()
            }
            'notify' {
                $nargs = @()
                if ($Spec.PSObject.Properties['args'] -and $Spec.args) { $nargs = @($Spec.args) }
                $rc = Invoke-Script 'notify.ps1' $nargs
            }
            default { Fail "unknown component '$component'" }
        }

        Wait-Capture $captureFile
    } finally {
        $env:USERPROFILE = $savedHome
        $env:TEMP = $savedTemp
        $env:TMP  = $savedTmp
        $env:PATH = $savedPath
        $env:STATUSLINE_CAPTURE_FILE = $savedCapture
    }

    # The silent-degradation contract is a property of every capture, not just
    # of the degraded-input cases: a fixture taken from a run that wrote to
    # stderr would enshrine a broken status line as the expected behaviour.
    if ($rc -ne 0) { Fail "$component/$caseName`: the script exited $rc" }
    $stderrText = if (Test-Path $stderrFile) { Read-Utf8 $stderrFile } else { '' }
    if ($stderrText) { Fail "$component/$caseName`: the script wrote to stderr: $stderrText" }

    $captured = ''
    switch ($observable) {
        'stdout' {
            # Asserted in both directions. The isolated TEMP guarantees no
            # output cache existed before the run, so a case that renders must
            # leave one behind and a case that exits early must not. Both
            # outcomes produce plausible bytes, so byte-diffing alone can never
            # tell a full render from a served cache or from an early exit --
            # the shape that let the trust-check inversion run nine days.
            $ocPath = Join-Path $tmpDir "statusline-oc-$session.txt"
            $expectRender = -not ($Spec.PSObject.Properties['expect_render'] -and -not $Spec.expect_render)
            if ($session) {
                if ($expectRender -and -not (Test-Path $ocPath)) {
                    Fail "$component/$caseName`: no output-cache file was written, so this capture is not a verified cache miss"
                }
                if (-not $expectRender -and (Test-Path $ocPath)) {
                    Fail "$component/$caseName`: an output cache was written by a case that is supposed to exit before rendering"
                }
            }
            $captured = if (Test-Path $stdoutFile) { Read-Utf8 $stdoutFile } else { '' }
        }
        'deleted-paths' {
            # The observable for git-refresh is the exact set of paths that
            # disappeared from the isolated temp root. Diffing the whole root
            # rather than probing the two expected names is the point: a session
            # id that escaped sanitisation would delete something else, and only
            # a full diff can show that.
            $gone = @($before | Where-Object { $after -notcontains $_ } | Sort-Object)
            $captured = ($gone -join "`n")
            if ($captured) { $captured += "`n" }
        }
        'feed-bytes' {
            $feed = Join-Path $tmpDir "statusline-tasks-$session.json"
            $captured = if ($session -and (Test-Path $feed)) { Read-Utf8 $feed } else { '' }
            # The handler must print nothing, or Claude Code's default agent
            # panel is replaced by whatever it emitted.
            $out = if (Test-Path $stdoutFile) { Read-Utf8 $stdoutFile } else { '' }
            if ($out) { Fail "$component/$caseName`: the subagent handler wrote to stdout" }
        }
        'notify-argv' {
            # Sorted, because this observable is a *set* of invocations and not
            # a sequence. Both scripts background their sound helper, so its
            # record races the visual one: the same case captured twice really
            # does produce the two lines in either order. `deleted-paths` sorts
            # for the same reason.
            #
            # Ordinal, to match the bash driver's `LC_ALL=C sort`. A culture
            # comparison would order the two drivers differently and turn a
            # matching pair of fixtures into a divergence.
            $raw = if (Test-Path $captureFile) { Read-Utf8 $captureFile } else { '' }
            $lines = @($raw -split "`r?`n" | Where-Object { $_ -ne '' })
            if ($lines.Count -gt 1) { [Array]::Sort($lines, [System.StringComparer]::Ordinal) }
            $captured = ($lines -join "`n")
            if ($captured) { $captured += "`n" }
        }
        default { Fail "unknown observable '$observable'" }
    }

    if ($null -eq $captured) { $captured = '' }

    # Replace every machine-local path with a placeholder, then refuse to write
    # anything still carrying the real user's home. Fixtures are committed, and
    # CLAUDE.md forbids shipping a personal absolute path.
    foreach ($pair in @(
        @($workDir,     '{REPO}'),
        @($tmpDir,      '{TMP}'),
        @($homeDir,     '{HOME}'),
        @($caseRoot,    '{ROOT}'),
        @($ScriptsRoot, '{SCRIPTS}'))) {
        $captured = $captured.Replace($pair[0], $pair[1]).Replace($pair[0].Replace('\', '/'), $pair[1])
    }
    if ($RealHome -and $captured.Contains($RealHome)) {
        Fail "$component/$caseName`: captured output contains the real home path -- refusing to write a fixture"
    }

    $dest = Join-Path (Join-Path $OutRoot $component) $caseName
    New-Item -ItemType Directory -Force -Path (Join-Path $dest 'expected') | Out-Null
    [System.IO.File]::WriteAllText(
        (Join-Path $dest "expected\$Platform.txt"), $captured,
        (New-Object System.Text.UTF8Encoding $false))

    $row = [ordered]@{ platform = $Platform; expected = "expected/$Platform.txt" }
    $meta = [ordered]@{
        schema        = 1
        case          = $caseName
        component     = $component
        observable    = $observable
        source_commit = $SourceCommit
        clock         = $clock
        git_state     = $gitState
        payload       = $payload
        args          = @(if ($Spec.PSObject.Properties['args'] -and $Spec.args) { $Spec.args } else { @() })
        notify_config = $config
        session_id    = $session
        inputs        = @($inputs)
        captured      = @($row)
    }

    # Merge rather than overwrite: the other platforms' capture rows live in the
    # same file, and a Windows run must not erase what a CI run recorded.
    $metaPath = Join-Path $dest 'case.json'
    if (Test-Path $metaPath) {
        $old = Read-Utf8 $metaPath | ConvertFrom-Json
        $rows = @()
        foreach ($r in @($old.captured)) {
            if ($r.platform -ne $Platform) { $rows += [ordered]@{ platform = $r.platform; expected = $r.expected } }
        }
        $rows += $row
        $meta.captured = @($rows | Sort-Object { $_.platform })
    }
    [System.IO.File]::WriteAllText($metaPath,
        (($meta | ConvertTo-Json -Depth 8) + "`n"),
        (New-Object System.Text.UTF8Encoding $false))

    Remove-Item -Recurse -Force $caseRoot -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------
# Drive
# ---------------------------------------------------------------------------

function Invoke-All([string] $OutRoot) {
    foreach ($spec in $Selected) {
        # A case that names a platform list omitting this one has no observable
        # here. Skipping loudly beats storing an empty golden file that
        # everything downstream would then assert against.
        if ($spec.PSObject.Properties['platforms'] -and $spec.platforms -notcontains $Platform) {
            $reason = if ($spec.PSObject.Properties['platform_note']) { $spec.platform_note } else { 'no reason recorded' }
            Note "skip    $($spec.component)/$($spec.case) -- not observable on ${Platform}: $reason"
            continue
        }
        Note "capture $($spec.component)/$($spec.case)"
        Invoke-CaptureCase $spec $OutRoot
    }
}

# Pinned for the duration of the run so a second capture produces the same
# commit hashes -- the detached-HEAD state renders a short hash, which makes
# identity and both dates part of the rendered output.
$gitEnvSaved = @{}
foreach ($p in $States.git_env.PSObject.Properties) {
    $gitEnvSaved[$p.Name] = [Environment]::GetEnvironmentVariable($p.Name)
    Set-Item -Path "env:$($p.Name)" -Value $p.Value
}

try {
    Write-Host "harness: $Platform, $($Selected.Count) case(s), scripts at $($SourceCommit.Substring(0,12))"

    if ($Verify) {
        $a = Join-Path ([System.IO.Path]::GetTempPath()) ("statusline-verify-a-" + [System.IO.Path]::GetRandomFileName())
        $b = Join-Path ([System.IO.Path]::GetTempPath()) ("statusline-verify-b-" + [System.IO.Path]::GetRandomFileName())
        Invoke-All $a
        Invoke-All $b

        $differences = @()
        foreach ($fileA in Get-ChildItem -Path $a -Recurse -File) {
            $rel = $fileA.FullName.Substring($a.Length + 1)
            $fileB = Join-Path $b $rel
            if (-not (Test-Path $fileB)) { $differences += "only in run A: $rel"; continue }
            $hashA = (Get-FileHash -Algorithm SHA256 $fileA.FullName).Hash
            $hashB = (Get-FileHash -Algorithm SHA256 $fileB).Hash
            if ($hashA -ne $hashB) { $differences += "differs: $rel" }
        }
        foreach ($fileB in Get-ChildItem -Path $b -Recurse -File) {
            $rel = $fileB.FullName.Substring($b.Length + 1)
            if (-not (Test-Path (Join-Path $a $rel))) { $differences += "only in run B: $rel" }
        }

        Remove-Item -Recurse -Force $a, $b -ErrorAction SilentlyContinue
        if ($differences.Count -gt 0) {
            $differences | ForEach-Object { Write-Host "  $_" }
            Fail "NOT reproducible -- two captures of the same commit differ"
        }
        Write-Host "harness: reproducible -- two captures of $($SourceCommit.Substring(0,12)) are byte-identical"
    } else {
        Invoke-All $Out
        Write-Host "harness: wrote $($Selected.Count) fixture(s) to $Out"
    }
} finally {
    foreach ($name in $gitEnvSaved.Keys) {
        if ($null -eq $gitEnvSaved[$name]) {
            Remove-Item -Path "env:$name" -ErrorAction SilentlyContinue
        } else {
            Set-Item -Path "env:$name" -Value $gitEnvSaved[$name]
        }
    }
    if ($Worktree) {
        Invoke-Git @('-C', $RepoRoot, 'worktree', 'remove', '--force', $Worktree) | Out-Null
    }
}
