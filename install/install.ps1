#Requires -Version 5.1
# Installs the claude-statusline binary into %USERPROFILE%\.claude\bin and
# registers it in settings.json.
#
# BOM-LESS and ASCII-ONLY, deliberately. This file is fetched with `irm <url> |
# iex`, and a BOM survives irm as a stray U+FEFF that breaks iex on the first
# token. ASCII-only keeps it safe to run from a local clone too. See
# .gitattributes -- do not "fix" the missing BOM back.
#
# No `exit` anywhere. `irm | iex` runs this in the user's live session, where
# `exit` closes their terminal. Every abort below uses `return`.

if ($PSVersionTable.PSVersion -lt [Version]'5.1') {
    Write-Host "  PowerShell 5.1+ required (current: $($PSVersionTable.PSVersion))" -ForegroundColor Red
    return
}

$repoSlug       = "axlaser/claude-statusline"
$signerWorkflow = "$repoSlug/.github/workflows/release.yml"

$claudeDir    = "$env:USERPROFILE\.claude"
$binDir       = "$claudeDir\bin"
$binPath      = "$binDir\claude-statusline.exe"
$sidecarPath  = "$binDir\claude-statusline.exe.old"
$helperPath   = "$binDir\claude-statusline-focus.exe"
$helperSidecar = "$binDir\claude-statusline-focus.exe.old"
$settingsPath = "$claudeDir\settings.json"
$configPath   = "$claudeDir\notify-config.json"
$iconPath     = "$claudeDir\claude-icon.png"
$stagePrefix  = ".claude-statusline.stage."

$ESC    = [char]27
$RESET  = "$ESC[0m"
$BOLD   = "$ESC[1m"
$DIM    = "$ESC[2m"
$CYAN   = "$ESC[36m"
$GREEN  = "$ESC[32m"
$YELLOW = "$ESC[33m"
$RED    = "$ESC[31m"
$GRAY   = "$ESC[90m"

function Step([string]$msg)  { Write-Host "  ${CYAN}${BOLD}>>>${RESET} $msg" }
function Ok([string]$msg)    { Write-Host "  ${GREEN}${BOLD} +${RESET} $msg" }
function Warn([string]$msg)  { Write-Host "  ${YELLOW}${BOLD} !${RESET} $msg" }
function Err([string]$msg)   { Write-Host "  ${RED}${BOLD} x${RESET} $msg" }
function Info([string]$msg)  { Write-Host "  ${DIM}   $msg${RESET}" }

function Format-Size([long]$bytes) {
    if ($bytes -ge 1048576) { return ("{0:N1} MB" -f ($bytes / 1048576)) }
    if ($bytes -ge 1024)    { return ("{0:N1} KB" -f ($bytes / 1024)) }
    return "$bytes B"
}

# Removes the staged download. Called on every path that does not place it
#: a staged file left behind is an unverified binary sitting in the
# install directory.
function Remove-Stage {
    foreach ($p in @($script:stagePath, $script:sumsPath, $script:bundlePath,
                     $script:helperStage, $script:helperBundle)) {
        if ($p -and (Test-Path $p)) { Remove-Item $p -Force -ErrorAction SilentlyContinue }
    }
}

# Runs a native command and reports whether it actually ran, separately from
# what it returned.
#
# $LASTEXITCODE is only written by a process that starts. When an executable
# cannot launch -- wrong architecture, a truncated download, an antivirus
# quarantine, a corrupt PE -- PowerShell raises a native error and leaves the
# PREVIOUS command's exit code in place. A bare `if ($LASTEXITCODE -ne 0)` then
# reads a stale value and treats a binary that never ran as a success.
#
# That is not hypothetical: `gh attestation verify` sets it to 0 a few steps
# above, so an unlaunchable binary used to pass the self-check gate, and the
# install went on to rewrite settings.json and delete the user's superseded
# scripts -- the exact sequence CLAUDE.md's self-check rule exists to prevent.
#
# Clearing it first and reporting Ran=$false for "did not run" is what lets each
# gate below distinguish "answered no" from "could not answer", and pick its own
# safe direction for the second case.
function Invoke-Binary {
    param([string]$Exe, [string[]]$BinArgs, [switch]$Gui)
    # A GUI-subsystem program -- the click helper -- returns to `&` the moment
    # it starts, before it has exited, so its exit code has to be read through
    # Start-Process -Wait instead. Same shape of answer: Ran, Code, Output.
    if ($Gui) {
        $proc = $null
        try {
            if ($BinArgs -and $BinArgs.Count -gt 0) {
                $proc = Start-Process -FilePath $Exe -ArgumentList $BinArgs -Wait -PassThru -WindowStyle Hidden -ErrorAction Stop
            } else {
                $proc = Start-Process -FilePath $Exe -Wait -PassThru -WindowStyle Hidden -ErrorAction Stop
            }
        } catch {
            return [PSCustomObject]@{ Ran = $false; Code = $null; Output = $_.Exception.Message }
        }
        return [PSCustomObject]@{ Ran = $true; Code = $proc.ExitCode; Output = '' }
    }
    $global:LASTEXITCODE = $null
    $output = $null
    try {
        $output = & $Exe @BinArgs 2>&1
    } catch {
        return [PSCustomObject]@{ Ran = $false; Code = $null; Output = $_.Exception.Message }
    }
    if ($null -eq $LASTEXITCODE) {
        return [PSCustomObject]@{ Ran = $false; Code = $null; Output = $output }
    }
    return [PSCustomObject]@{ Ran = $true; Code = $LASTEXITCODE; Output = $output }
}

# --- Options ---
$requireAttestation = $false
$allowPrerelease = $false
$devChannel = $false
$pinnedVersion = $env:CLAUDE_STATUSLINE_VERSION
foreach ($a in $args) {
    if ($a -eq '--require-attestation') { $requireAttestation = $true }
    elseif ($a -eq '--pre')             { $allowPrerelease = $true }
    elseif ($a -eq '--dev')             { $devChannel = $true }
    elseif ($a -like '--version=*')     { $pinnedVersion = $a.Substring(10) }
}

Write-Host ""
Write-Host "  ${DIM}claude-statusline installer${RESET}"
Write-Host "  ${GRAY}-----------------------------------------${RESET}"
Write-Host ""

# --- Platform detection ---
# Before anything is created, removed, or written: an unsupported platform must
# leave an existing installation exactly as it was.
Step "Detecting platform"
$archRaw = $env:PROCESSOR_ARCHITECTURE
if (-not $archRaw) { $archRaw = "" }
$arch = switch -Wildcard ($archRaw.ToUpper()) {
    "AMD64" { "x86_64"; break }
    "ARM64" { "aarch64"; break }
    default { "" }
}
# A 32-bit PowerShell on 64-bit Windows reports x86 in PROCESSOR_ARCHITECTURE
# and the real value in PROCESSOR_ARCHITEW6432. Without this the installer
# refuses on a machine it fully supports.
if (-not $arch -and $env:PROCESSOR_ARCHITEW6432) {
    $arch = switch -Wildcard ($env:PROCESSOR_ARCHITEW6432.ToUpper()) {
        "AMD64" { "x86_64"; break }
        "ARM64" { "aarch64"; break }
        default { "" }
    }
}
if (-not $arch) {
    Err "Unsupported architecture: $archRaw"
    Info "Published targets: Windows, macOS and Linux on x86_64 and aarch64."
    Info "Nothing was changed."
    return
}
$target = "$arch-pc-windows-msvc"
$asset  = "claude-statusline-$target.exe"
$helperAsset = "claude-statusline-focus-$target.exe"
Ok $target
Write-Host ""

# --- Resolve the release ---
Step "Resolving release"
if ($pinnedVersion) {
    $tag = $pinnedVersion
    Ok "Pinned to $tag"
} elseif ($devChannel) {
    # The dev channel: the newest `dev-*` release in the same atom feed the
    # prerelease channel reads. The release workflow publishes one from every
    # push to `dev` and keeps the three newest, so the first match is the
    # branch head. It outranks --pre when both are given, because a user who
    # asked for the branch head wants exactly that.
    $tag = $null
    try {
        $atom = (Invoke-WebRequest -Uri "https://github.com/$repoSlug/releases.atom" `
            -UseBasicParsing -ErrorAction Stop).Content
        if ($atom -match 'releases/tag/dev-[^"<]+') { $tag = $Matches[0] -replace '^releases/tag/', '' }
    } catch {}
    if (-not $tag) {
        Err "Could not resolve a dev-channel release"
        Info "None may be published yet. Try --pre, set CLAUDE_STATUSLINE_VERSION=<tag>"
        Info "to pin a version, or check your connection."
        Info "Your existing installation was left untouched."
        return
    }
    Warn "Installing $tag (dev channel: the dev branch head, which may be unstable)"
} elseif ($allowPrerelease) {
    # The releases atom feed lists every release newest-first, prereleases
    # included, over plain unauthenticated HTTPS. That is the whole reason to
    # use it rather than the API: no token, no rate limit that a shared IP can
    # exhaust for everyone behind it.
    #
    # "Newest overall" is the deliberate semantic, not "newest prerelease". A
    # user who asks for --pre wants whatever is furthest ahead; once a stable
    # release overtakes the prereleases, that is the stable one, and silently
    # installing an older prerelease instead would be the surprising answer.
    #
    # Among tagged releases, that is: only a tag that names a version, `v` and
    # a digit. The same feed carries the dev channel's `dev-*` builds, which a
    # --pre user did not ask for and which would otherwise always be newest.
    $tag = $null
    try {
        $atom = (Invoke-WebRequest -Uri "https://github.com/$repoSlug/releases.atom" `
            -UseBasicParsing -ErrorAction Stop).Content
        if ($atom -match 'releases/tag/v[0-9][^"<]+') { $tag = $Matches[0] -replace '^releases/tag/', '' }
    } catch {}
    if (-not $tag) {
        Err "Could not resolve a prerelease"
        Info "Set CLAUDE_STATUSLINE_VERSION=<tag> to pin a version, or check your connection."
        Info "Your existing installation was left untouched."
        return
    }
    Warn "Installing $tag (prerelease channel)"
} else {
    # The /releases/latest redirect resolves the current stable tag without an
    # authenticated API call, and excludes prereleases -- which is what keeps
    # pipeline-verification tags from ever being installed.
    $tag = $null
    try {
        $resp = Invoke-WebRequest -Uri "https://github.com/$repoSlug/releases/latest" `
            -MaximumRedirection 5 -UseBasicParsing -ErrorAction Stop
        # Both spellings, because the two PowerShell editions expose the
        # redirected URI on different objects. Windows PowerShell 5.1 returns a
        # System.Net.HttpWebResponse, which carries ResponseUri. PowerShell 6+
        # rebuilt Invoke-WebRequest on HttpClient, so BaseResponse is a
        # System.Net.Http.HttpResponseMessage -- which has no ResponseUri at
        # all, and puts the final URI on RequestMessage.RequestUri. Reading only
        # the 5.1 spelling left $tag empty on PowerShell 7 and aborted every
        # stable install there with "Could not resolve the latest release",
        # while --pre kept working because it parses the atom feed instead.
        $final = $null
        if ($resp.BaseResponse.PSObject.Properties['ResponseUri']) {
            $final = $resp.BaseResponse.ResponseUri.AbsoluteUri
        }
        if (-not $final -and $resp.BaseResponse.PSObject.Properties['RequestMessage']) {
            $final = $resp.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
        }
        if ($final) { $tag = ($final -split '/')[-1] }
    } catch {
        try { $tag = ($_.Exception.Response.ResponseUri.AbsoluteUri -split '/')[-1] } catch {}
    }
    if (-not $tag) {
        Err "Could not resolve the latest release"
        Info "Set CLAUDE_STATUSLINE_VERSION=<tag> to pin a version, --pre for the prerelease"
        Info "channel, --dev for the dev channel, or check your connection."
        Info "Your existing installation was left untouched."
        return
    }
    # A tag shape, not merely "not the word latest". With no stable release
    # published, /releases/latest redirects to the releases index rather than to
    # a tag, so the last path segment is "releases" -- which the old guard let
    # through, producing a download 404 reported as "Download failed" instead of
    # the real reason.
    if ($tag -notmatch '^v[0-9]') {
        Err "No stable release has been published yet"
        Info "Install from the prerelease channel with --pre, the dev channel with"
        Info "--dev, or pin a version with CLAUDE_STATUSLINE_VERSION=<tag>."
        Info "Your existing installation was left untouched."
        return
    }
    Ok $tag
}
$baseUrl = "https://github.com/$repoSlug/releases/download/$tag"
Write-Host ""

# --- Verify the install directory ---
# Deliberately inverted relative to the runtime guard: at runtime an
# undeterminable owner leaves the guard passing, because failing a read closed
# kills every cache and re-fires alerts. Here the check runs once, at install
# time, and a directory we cannot vouch for is one we must not place an
# executable into.
Step "Checking the install directory"
if (Test-Path $binDir) {
    $item = Get-Item $binDir -Force
    if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        Err "$binDir is a reparse point (junction or symlink)"
        Info "Refusing to install through a link. Remove it and re-run."
        return
    }
} else {
    try {
        New-Item -ItemType Directory -Path $binDir -Force -ErrorAction Stop | Out-Null
    } catch {
        Err "Cannot create $binDir"
        return
    }
    Ok "Created $binDir"
}

$acl = $null
try { $acl = Get-Acl $binDir -ErrorAction Stop } catch {}
if (-not $acl) {
    Err "Cannot read the ACL of $binDir"
    Info "Refusing to install where the directory cannot be vouched for."
    return
}
$me = ([Security.Principal.WindowsIdentity]::GetCurrent()).User
if (-not $me) {
    Err "Cannot determine the current user"
    return
}
if ($acl.Owner) {
    $ownerSid = $null
    try {
        $ownerSid = ([Security.Principal.NTAccount]$acl.Owner).Translate([Security.Principal.SecurityIdentifier])
    } catch {
        try { $ownerSid = [Security.Principal.SecurityIdentifier]$acl.Owner } catch {}
    }
    if (-not $ownerSid) {
        Err "Cannot resolve the owner of $binDir"
        return
    }
    # Administrators owning a directory in the user's own profile is normal on
    # Windows when the profile was created by an elevated process, so an
    # admin-owned directory is accepted where a third user's would not be.
    $admins = New-Object Security.Principal.SecurityIdentifier(
        [Security.Principal.WellKnownSidType]::BuiltinAdministratorsSid, $null)
    if ($ownerSid -ne $me -and $ownerSid -ne $admins) {
        Err "$binDir is owned by $($acl.Owner), not by you"
        return
    }
} else {
    Err "Cannot determine the owner of $binDir"
    return
}

# Write access for Everyone, Users, or Authenticated Users means someone else
# can replace the binary after it is verified, which would make every check
# above decorative. Authenticated Users (S-1-5-11) is the easy one to miss:
# it is the broad principal ACL tooling grants most readily, and it covers
# every account that can log on.
$worldSids = @(
    (New-Object Security.Principal.SecurityIdentifier([Security.Principal.WellKnownSidType]::WorldSid, $null)),
    (New-Object Security.Principal.SecurityIdentifier([Security.Principal.WellKnownSidType]::BuiltinUsersSid, $null)),
    (New-Object Security.Principal.SecurityIdentifier([Security.Principal.WellKnownSidType]::AuthenticatedUserSid, $null))
)
foreach ($ace in $acl.Access) {
    if ($ace.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
    $sid = $null
    try { $sid = $ace.IdentityReference.Translate([Security.Principal.SecurityIdentifier]) } catch { continue }
    if ($worldSids -contains $sid) {
        if ($ace.FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) {
            Err "$binDir grants write access to $($ace.IdentityReference)"
            Info "Remove that permission and re-run."
            return
        }
    }
}
Ok "Owned by you, no broad write access"
Write-Host ""

# --- Sweep leftovers ---
# A sidecar with no binary at $binPath means the last run's self-check failed
# AND its restore failed after it -- the path that prints "It is still at ...
# move it back by hand". The sweep below would delete the only copy the user
# was just told to go and rescue, and re-running the installer is the first
# thing anyone does after a failed install. Put it back first. A sidecar left
# *with* the binary in place is the ordinary case the sweep is for.
if ((Test-Path $sidecarPath) -and -not (Test-Path $binPath)) {
    Move-Item -Path $sidecarPath -Destination $binPath -Force -ErrorAction SilentlyContinue
    if (Test-Path $binPath) {
        Warn "Restored the binary a failed run left at $sidecarPath"
    }
}
# Every run clears both what an interrupted download staged and what a previous
# replace renamed aside.
Get-ChildItem -Path $binDir -Filter "$stagePrefix*" -Force -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue
if (Test-Path $sidecarPath) {
    # A failed delete here is tolerated on purpose: the old binary may still be
    # running, and it will be swept on the next run instead.
    Remove-Item $sidecarPath -Force -ErrorAction SilentlyContinue
}

# --- Stage the download ---
Step "Downloading"
# Staged inside the destination directory, never in a shared temp: %TEMP%
# staging would allow a swap between verification and placement, and a
# cross-volume move would not be atomic either.
$script:stagePath  = Join-Path $binDir "$stagePrefix$PID"
$script:sumsPath   = Join-Path $binDir "$stagePrefix$PID.sums"
$script:bundlePath = Join-Path $binDir "$stagePrefix$PID.sigstore.json"
$script:helperStage  = Join-Path $binDir "$stagePrefix$PID.focus"
$script:helperBundle = Join-Path $binDir "$stagePrefix$PID.focus.sigstore.json"

$oldProgress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'
try {
    Invoke-WebRequest -Uri "$baseUrl/$asset" -OutFile $script:stagePath -UseBasicParsing -ErrorAction Stop
} catch {
    Err "Download failed: $baseUrl/$asset"
    Remove-Stage
    $ProgressPreference = $oldProgress
    return
}
$ProgressPreference = $oldProgress
Ok "$asset ($(Format-Size (Get-Item $script:stagePath).Length))"
Write-Host ""

# --- Verify the checksum ---
# Verification that cannot be performed counts as verification failure. There
# is no "proceed without checking" path: the checksum is the fail-closed gate
# for the whole transport.
Step "Verifying checksum"
try {
    Invoke-WebRequest -Uri "$baseUrl/checksums.txt" -OutFile $script:sumsPath -UseBasicParsing -ErrorAction Stop
} catch {
    Err "Could not fetch checksums.txt"
    Remove-Stage
    return
}
$expected = $null
foreach ($line in (Get-Content $script:sumsPath)) {
    $parts = $line -split '\s+', 2
    if ($parts.Count -eq 2 -and $parts[1].Trim() -eq $asset) { $expected = $parts[0].Trim(); break }
}
if (-not $expected) {
    Err "checksums.txt has no entry for $asset"
    Remove-Stage
    return
}
$actual = $null
try { $actual = (Get-FileHash -Algorithm SHA256 $script:stagePath -ErrorAction Stop).Hash } catch {}
if (-not $actual) {
    Err "Could not compute a SHA-256 hash"
    Info "Cannot verify the download, so it will not be installed."
    Remove-Stage
    return
}
if ($actual.ToLower() -ne $expected.ToLower()) {
    Err "Checksum mismatch for $asset"
    Info "expected $expected"
    Info "actual   $actual"
    Remove-Stage
    return
}
Ok "SHA-256 matches"
Write-Host ""

# --- Verify the attestation ---
# Opportunistic but fail-closed when it runs: a negative result stops the
# install with or without --require-attestation; only the inability to verify is
# tolerated, and only without the flag.
Step "Verifying build provenance"
$attested = $false
if (Get-Command gh -ErrorAction SilentlyContinue) {
    $gotBundle = $false
    try {
        Invoke-WebRequest -Uri "$baseUrl/$asset.sigstore.json" -OutFile $script:bundlePath `
            -UseBasicParsing -ErrorAction Stop
        $gotBundle = $true
    } catch {}
    if ($gotBundle) {
        # Verified against the downloaded bundle rather than the attestation
        # API: the API serves its bundle Snappy-compressed and needs an
        # authenticated gh, which is why the bundle ships as a release asset.
        $verify = Invoke-Binary 'gh' @(
            'attestation', 'verify', $script:stagePath,
            '--bundle', $script:bundlePath,
            '--repo', $repoSlug,
            '--signer-workflow', $signerWorkflow)
        if ($verify.Ran -and $verify.Code -eq 0) {
            $attested = $true
            Ok "Provenance verified (built by $signerWorkflow)"
        } elseif (-not $verify.Ran) {
            # gh was on PATH but could not be launched. That is an inability to
            # verify, not a negative result, and the policy above tolerates only
            # the former -- so warn and let the --require-attestation check below
            # decide, rather than aborting as though the signature was bad.
            Warn "gh could not be launched - provenance not verified"
        } else {
            Err "Attestation verification FAILED for $asset"
            Info "The download matched its checksum but does not carry a valid"
            Info "provenance attestation from this repository's release workflow."
            Info "Refusing to install."
            Remove-Stage
            return
        }
    } else {
        Warn "No attestation bundle published for this release"
    }
} else {
    Warn "gh CLI not found - provenance not verified"
}

if (-not $attested) {
    if ($requireAttestation) {
        Err "--require-attestation was given but provenance could not be verified"
        Remove-Stage
        return
    }
    Info "Verify manually later with:"
    Info "  gh attestation verify `"$binPath`" --repo $repoSlug --signer-workflow $signerWorkflow"
}
Write-Host ""

# --- Place the binary ---
# Windows will not let a running executable be deleted or overwritten, but it
# will let it be renamed. Renaming aside first is what makes an upgrade work
# while Claude Code is open.
Step "Installing"
if (Test-Path $binPath) {
    try {
        Move-Item -Path $binPath -Destination $sidecarPath -Force -ErrorAction Stop
    } catch {
        Err "Could not move the existing binary aside"
        Info "Close Claude Code and re-run."
        Remove-Stage
        return
    }
}
try {
    Move-Item -Path $script:stagePath -Destination $binPath -Force -ErrorAction Stop
    $script:stagePath = $null
} catch {
    Err "Could not place the binary at $binPath"
    # Put the previous installation back rather than leaving nothing behind.
    if (Test-Path $sidecarPath) { Move-Item -Path $sidecarPath -Destination $binPath -Force -ErrorAction SilentlyContinue }
    Remove-Stage
    return
}
# checksums.txt stays until the click helper below has been checked against it.
Remove-Item $script:bundlePath -Force -ErrorAction SilentlyContinue
Ok $binPath
Info (Format-Size (Get-Item $binPath).Length)
Write-Host ""

# --- Self-check ---
# A binary can pass its checksum, launch, and still render wrongly -- a bad
# build, a corrupt fixture, an architecture that runs but misbehaves. The
# silent-degradation contract guarantees that failure would reach the user as an
# absent status line and nothing else, so this is the only place it can be
# caught. Everything destructive below is gated on it, and the sidecar stays
# where it is until it passes.
Step "Verifying the binary renders"
# The rendered output is captured, not discarded. It is the only evidence of
# what went wrong, this is a per-target failure CI cannot reproduce, and the
# binary that produced it is about to be moved out of the way.
#
# Deliberately outside $stagePrefix, unlike the staging files. The sweep above
# deletes the whole prefix before the download, and re-running the installer is
# the first thing anyone does after a failed install -- so naming these two with
# the prefix destroyed the pair the user had just been told to attach to a bug
# report, before the retry had even started. They are removed on the success
# path below instead.
$checkLog = Join-Path $binDir "claude-statusline.self-check.txt"
$check = Invoke-Binary $binPath @('self-check')
Set-Content -Path $checkLog -Value $check.Output -Encoding utf8
if (-not $check.Ran -or $check.Code -ne 0) {
    if (-not $check.Ran) {
        # Distinguished from a render mismatch on purpose: these have different
        # causes and different things worth reporting. A binary that cannot
        # start is an architecture, download-integrity or antivirus problem, and
        # its log is empty because fd 2 is redirected to the null device before
        # the subcommand is read -- so saying "what it rendered" would point the
        # user at a blank file.
        Err "The installed binary could not be launched"
        Info "It downloaded and matched its checksum but will not start on this"
        Info "machine, so it was not activated. The usual causes are a mismatched"
        Info "architecture or an antivirus product that altered the file."
        if ($check.Output) { Info "$($check.Output)" }
    } else {
        Err "The installed binary failed its self-check"
        Info "It downloaded and verified but does not render correctly, so it was"
        Info "not activated."
    }
    # Renamed aside, not deleted. Windows refuses to delete a file that is still
    # held open, and -ErrorAction SilentlyContinue swallowed exactly that -- the
    # failed binary stayed active while the script reported it gone. Renaming is
    # the operation Windows permits, and it is what this script already uses to
    # place the binary in the first place.
    $failedBin = Join-Path $binDir "claude-statusline.failed"
    try {
        Move-Item -Path $binPath -Destination $failedBin -Force -ErrorAction Stop
    } catch {
        $failedBin = $null
    }
    $hadPrevious = Test-Path $sidecarPath
    if ($hadPrevious) {
        Move-Item -Path $sidecarPath -Destination $binPath -Force -ErrorAction SilentlyContinue
        # The sidecar being gone is what proves the restore happened. Testing
        # $binPath instead read as success when the rename-aside above had also
        # failed: the failed binary was still sitting at $binPath, so the check
        # passed and the script announced an untouched previous installation
        # that had in fact never been put back.
        if (-not (Test-Path $sidecarPath)) {
            Info "Your previous installation is untouched."
        } else {
            Err "Could not restore the previous binary"
            Info "It is still at $sidecarPath -- move it back to $binPath by hand."
        }
    }
    if ($check.Ran) { Info "What it rendered: $checkLog" }
    if ($failedBin) { Info "The binary: $failedBin" }
    Info "Please attach the above when reporting this."
    Remove-Stage
    return
}
# The check passed, so this run's log and any failed binary an earlier run left
# behind are both stale: the user has a working install and nothing left to
# report. This is the only place they are removed -- see the naming note above.
Remove-Item $checkLog -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $binDir "claude-statusline.failed") -Force -ErrorAction SilentlyContinue
# Tolerated failure by design: the old binary may still be running, and the next
# run sweeps whatever is left.
if (Test-Path $sidecarPath) { Remove-Item $sidecarPath -Force -ErrorAction SilentlyContinue }
Ok "Renders correctly"
Write-Host ""

# --- The click helper ---
# A second, Windows-only executable: the shell launches it, console-free, when
# a notification toast is clicked, and it brings the session's terminal to the
# front. Placed only now, after the self-check above passed, and registered
# only after settings.json is written below. Every failure here is tolerated:
# a missing or failing helper costs click handling and nothing else, and a
# helper that fails its smoke run is deleted before anything could register it.
Step "Installing the click helper"
$helperReady = $false
$helperWhy = ""
$helperFetched = $false
$oldProgress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'
try {
    Invoke-WebRequest -Uri "$baseUrl/$helperAsset" -OutFile $script:helperStage -UseBasicParsing -ErrorAction Stop
    $helperFetched = $true
} catch {
    $helperWhy = "the helper is not published for $tag"
}
$ProgressPreference = $oldProgress
if ($helperFetched) {
    # The same gates as the binary, against the same checksums.txt.
    $helperExpected = $null
    if (Test-Path $script:sumsPath) {
        foreach ($line in (Get-Content $script:sumsPath)) {
            $parts = $line -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].Trim() -eq $helperAsset) { $helperExpected = $parts[0].Trim(); break }
        }
    }
    $helperActual = $null
    try { $helperActual = (Get-FileHash -Algorithm SHA256 $script:helperStage -ErrorAction Stop).Hash } catch {}
    if (-not $helperExpected) {
        $helperWhy = "checksums.txt has no entry for $helperAsset"
    } elseif (-not $helperActual -or $helperActual.ToLower() -ne $helperExpected.ToLower()) {
        $helperWhy = "checksum mismatch for $helperAsset"
    } elseif (Get-Command gh -ErrorAction SilentlyContinue) {
        $gotHelperBundle = $false
        try {
            Invoke-WebRequest -Uri "$baseUrl/$helperAsset.sigstore.json" -OutFile $script:helperBundle `
                -UseBasicParsing -ErrorAction Stop
            $gotHelperBundle = $true
        } catch {}
        if ($gotHelperBundle) {
            $helperVerify = Invoke-Binary 'gh' @(
                'attestation', 'verify', $script:helperStage,
                '--bundle', $script:helperBundle,
                '--repo', $repoSlug,
                '--signer-workflow', $signerWorkflow)
            if ($helperVerify.Ran -and $helperVerify.Code -ne 0) {
                $helperWhy = "attestation verification failed for $helperAsset"
            } elseif (-not $helperVerify.Ran -and $requireAttestation) {
                $helperWhy = "provenance could not be verified and --require-attestation was given"
            }
        } elseif ($requireAttestation) {
            $helperWhy = "no attestation bundle published for $helperAsset"
        }
    } elseif ($requireAttestation) {
        $helperWhy = "gh CLI not found and --require-attestation was given"
    }
}
if ($helperFetched -and -not $helperWhy) {
    # The download carries the Mark of the Web; without this the first
    # protocol launch would raise SmartScreen instead of the terminal.
    Unblock-File -Path $script:helperStage -ErrorAction SilentlyContinue
    # Smoke it as the shell will launch it: a GUI-subsystem process, waited on.
    $smoke = Invoke-Binary $script:helperStage @() -Gui
    if (-not $smoke.Ran -or $smoke.Code -ne 0) {
        $helperWhy = "the helper did not run cleanly"
    } else {
        if (Test-Path $helperPath) {
            Move-Item -Path $helperPath -Destination $helperSidecar -Force -ErrorAction SilentlyContinue
        }
        try {
            Move-Item -Path $script:helperStage -Destination $helperPath -Force -ErrorAction Stop
            $script:helperStage = $null
            $helperReady = $true
        } catch {
            $helperWhy = "could not place the helper at $helperPath"
            if (Test-Path $helperSidecar) { Move-Item -Path $helperSidecar -Destination $helperPath -Force -ErrorAction SilentlyContinue }
        }
    }
}
if ($helperReady) {
    Remove-Item $helperSidecar -Force -ErrorAction SilentlyContinue
    Ok $helperPath
} else {
    # Deleted, never left staged: an unverified or failing helper must not sit
    # in the install directory where a later run could register it.
    if ($script:helperStage) { Remove-Item $script:helperStage -Force -ErrorAction SilentlyContinue }
    Warn "Click handling not installed - $helperWhy"
}
Remove-Item $script:helperBundle, $script:sumsPath -Force -ErrorAction SilentlyContinue
Write-Host ""

# --- Note the superseded scripts ---
# Found here, deleted only once settings.json actually points at the binary.
# Deleting them first meant a failed 'settings apply' left a migrating user with
# neither the script integration nor a configured binary, and nothing here backs
# them up -- the binary has a sidecar, these do not.
$legacyScripts = @('statusline.ps1', 'notify.ps1', 'git-refresh.ps1', 'subagent-statusline.ps1')
$legacyFound = @($legacyScripts | Where-Object { Test-Path (Join-Path $claudeDir $_) })

# --- Configure settings.json ---
# The bare path is passed deliberately. The stored command has to be
# quoted, but quoting it here does not survive: PowerShell consumes the
# surrounding quotes of a pre-quoted argument as delimiters, so the binary would
# receive a bare path anyway and write an unquoted command. The binary adds the
# quotes on its own side, where no shell can eat them.
Step "Configuring Claude Code settings"
$applyFlags = @()

# The prompt fires only on a definite yes. A query that could not run falls to
# the else branch and writes our entry, which is the safe direction here: the
# apply below prunes a script installation's entries unconditionally, so
# skipping the write would leave a migrating user with no statusLine at all.
# Overwriting another tool's entry is recoverable; having none is the failure
# this whole gate exists to avoid.
$foreignStatusline = Invoke-Binary $binPath @('settings', 'has-foreign', '--binary', $binPath, 'statusline')
if ($foreignStatusline.Ran -and $foreignStatusline.Code -eq 0) {
    Write-Host ""
    $answer = Read-Host "  ${YELLOW}${BOLD} ?${RESET} Existing statusLine config found. Overwrite? (${GREEN}y${RESET}/${RED}n${RESET})"
    if ($answer -match '^[Yy]$') { $applyFlags += '--statusline' }
    else { Warn "Skipped statusLine update"; Info "Continuing with hook and notification setup..." }
    Write-Host ""
} else {
    $applyFlags += '--statusline'
}

$foreignSubagent = Invoke-Binary $binPath @('settings', 'has-foreign', '--binary', $binPath, 'subagent')
if ($foreignSubagent.Ran -and $foreignSubagent.Code -eq 0) {
    Write-Host ""
    $answer = Read-Host "  ${YELLOW}${BOLD} ?${RESET} Existing subagentStatusLine config found. Overwrite? (${GREEN}y${RESET}/${RED}n${RESET})"
    if ($answer -match '^[Yy]$') { $applyFlags += '--subagent' } else { Warn "Skipped subagentStatusLine update" }
    Write-Host ""
} else {
    $applyFlags += '--subagent'
}

# Live git status. Always on: not a notification, no prompt today, and it costs
# nothing when idle.
$applyFlags += '--git-refresh'

# --- Notification configuration ---
Write-Host ""
Step "Notification configuration"
if (Test-Path $configPath) {
    Ok "Config already exists (preserving)"
    Info $configPath
} else {
    $defaultConfig = @'
{
  "permission":        { "sound": true, "visual": true },
  "stop":              { "sound": true, "visual": true },
  "rate_limit":        { "sound": true, "visual": true, "threshold": 80 },
  "context_high":      { "sound": false, "visual": true, "threshold": 70 },
  "compaction_start":  { "sound": true, "visual": true },
  "compaction_done":   { "sound": true, "visual": true }
}
'@
    [System.IO.File]::WriteAllText($configPath, $defaultConfig, (New-Object System.Text.UTF8Encoding $false))
    Ok "Created default config"
    Info $configPath
}

Write-Host ""
Step "Notifications"
Info "Plays a sound and shows a popup when Claude needs attention."
# The legacy check is what carries the choice across an upgrade: someone
# who enabled notifications under the scripts has hooks pointing at notify.ps1,
# which `has` does not recognise, and re-prompting them would turn a silent
# upgrade into a question they already answered.
# A query that could not run counts as "already configured", so the flag is
# carried forward rather than dropped. Losing a setting the user had is worse
# than re-applying one they already have, and this path is reached only after
# the self-check proved the binary runs.
$hasNotify = Invoke-Binary $binPath @('settings', 'has', '--binary', $binPath, 'notify')
$notifyConfigured = (-not $hasNotify.Ran) -or ($hasNotify.Code -eq 0)
if (-not $notifyConfigured) {
    $hasLegacyNotify = Invoke-Binary $binPath @('settings', 'has-legacy', '--binary', $binPath, 'notify')
    $notifyConfigured = (-not $hasLegacyNotify.Ran) -or ($hasLegacyNotify.Code -eq 0)
}
if ($notifyConfigured) {
    Ok "Already configured"
    $applyFlags += '--notify'
} else {
    Write-Host ""
    $answer = Read-Host "  ${YELLOW}${BOLD} ?${RESET} Enable notifications? (${GREEN}y${RESET}/${RED}n${RESET})"
    if ($answer -match '^[Yy]$') { $applyFlags += '--notify' }
    else { Info "Skipped - run the installer again to enable later" }
}

# --- Notification icon ---
# The icon is the one download outside the release's SHA256SUMS, and it rides
# the moving master ref. Pin its hash and discard a mismatch: a missing icon
# is cosmetic, an unverified file handed to the toast stack is not.
$iconSha256 = "10497c744e9d5e489b9e9b802b964dab11ab9060d21697181218f6d3b3c648c1"
if (-not (Test-Path $iconPath)) {
    try {
        Invoke-WebRequest -Uri "https://raw.githubusercontent.com/$repoSlug/master/assets/claude-icon.png" `
            -OutFile $iconPath -UseBasicParsing -ErrorAction Stop
        $iconHash = (Get-FileHash -Path $iconPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($iconHash -eq $iconSha256) {
            Ok "Icon installed"
        } else {
            Remove-Item $iconPath -Force -ErrorAction SilentlyContinue
            Info "Icon skipped - the download did not match its pinned checksum"
        }
    } catch {}
}

# --- Apply ---
Write-Host ""
# The load-bearing one. Everything below this point deletes the user's previous
# installation, so a merge that did not run must stop here just as firmly as one
# that failed -- otherwise the scripts go and nothing replaces them.
$apply = Invoke-Binary $binPath (@('settings', 'apply', '--binary', $binPath) + $applyFlags)
if (-not $apply.Ran -or $apply.Code -ne 0) {
    Err "Failed to update settings.json"
    if ($apply.Output) { Info ($apply.Output -join ' ') }
    Info "The binary is installed at $binPath but Claude Code is not pointing at it yet."
    return
}
Ok "Updated $settingsPath"

# --- Register the click handler ---
# The binary owns the registry key the way it owns the settings.json entries:
# it writes the claude-statusline: handler when the key is absent or already
# ours, and leaves another program's registration alone and says so.
if ($helperReady) {
    $register = Invoke-Binary $binPath @('settings', 'protocol', 'register', '--binary', $binPath)
    if ($register.Ran -and $register.Code -eq 0) {
        Ok "Click handling enabled - clicking a toast brings the terminal forward"
    } else {
        Warn "Click handling not enabled"
        if ($register.Output) { Info ($register.Output -join ' ') }
    }
} else {
    Info "Click handling not enabled - $helperWhy"
}

# --- Migrate from a script installation ---
# Only now: the binary has proved it renders and settings.json points at it, so
# the scripts are genuinely superseded rather than merely replaced on disk.
# notify-config.json is deliberately not in this list: it is the user's
# configuration, its schema is unchanged, and the binary reads it as-is.
if ($legacyFound.Count -gt 0) {
    Write-Host ""
    Step "Removing the superseded scripts"
    foreach ($name in $legacyFound) {
        $path = Join-Path $claudeDir $name
        try {
            Remove-Item $path -Force -ErrorAction Stop
            Ok $name
        } catch {
            Warn "Could not remove $path"
        }
    }
    Info "Your notification settings were kept."
}

Write-Host ""
Write-Host "  ${GRAY}-----------------------------------------${RESET}"
Write-Host "  ${GREEN}${BOLD}Done!${RESET} Restart Claude Code to activate."
Write-Host ""
