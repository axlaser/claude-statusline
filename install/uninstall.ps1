#Requires -Version 5.1
# Removes the claude-statusline binary and every settings.json entry the
# installer wrote.
#
# BOM-LESS and ASCII-ONLY, and no `exit` anywhere -- see install.ps1 for both.
# This file is fetched with `irm <url> | iex` and runs in the user's live
# session.

if ($PSVersionTable.PSVersion -lt [Version]'5.1') {
    Write-Host "  PowerShell 5.1+ required (current: $($PSVersionTable.PSVersion))" -ForegroundColor Red
    return
}

$claudeDir    = "$env:USERPROFILE\.claude"
$binDir       = "$claudeDir\bin"
$binPath      = "$binDir\claude-statusline.exe"
$sidecarPath  = "$binDir\claude-statusline.exe.old"
$helperPath   = "$binDir\claude-statusline-focus.exe"
$helperSidecar = "$binDir\claude-statusline-focus.exe.old"
$protocolKey  = "HKCU:\Software\Classes\claude-statusline"
$settingsPath = "$claudeDir\settings.json"
$configPath   = "$claudeDir\notify-config.json"
$iconPath     = "$claudeDir\claude-icon.png"
$modelWindows = "$claudeDir\statusline-model-windows.json"
$stagePrefix  = ".claude-statusline.stage."

# --- Output ---
# Drawn like the status line itself (src/render.rs, assemble): a gray frame and
# the status line's glyphs, each built from its code point because this file
# stays ASCII. Decided once, before the first line: plain text with ASCII
# glyphs and no frame when output is redirected, NO_COLOR is set
# (https://no-color.org) or TERM is dumb, so a log keeps each message on one
# line. The heavy frame and the check mark need Windows Terminal's fonts;
# conhost's default font lacks them, so outside it the frame is light and the
# check is a square root sign.
$plain = [Console]::IsOutputRedirected -or [bool]$env:NO_COLOR -or $env:TERM -eq 'dumb'
$ESC = [char]27
if ($plain) {
    $RESET = ''; $BOLD = ''; $DIM = ''; $CYAN = ''; $GREEN = ''; $YELLOW = ''; $RED = ''; $GRAY = ''
    $gOk = '+'; $gStep = '*'; $gDot = '-'
} else {
    $RESET  = "$ESC[0m"
    $BOLD   = "$ESC[1m"
    $DIM    = "$ESC[2m"
    $CYAN   = "$ESC[36m"
    $GREEN  = "$ESC[32m"
    $YELLOW = "$ESC[33m"
    $RED    = "$ESC[31m"
    $GRAY   = "$ESC[90m"
    if ($env:WT_SESSION) { $gOk = [string][char]0x2713 } else { $gOk = [string][char]0x221A }
    $gStep = [string][char]0x25CF
    $gDot  = [string][char]0x00B7
}
if ($env:WT_SESSION) {
    $fH = [string][char]0x2501; $fV = [string][char]0x2503
    $fTL = [string][char]0x250F; $fTR = [string][char]0x2513
    $fBL = [string][char]0x2517; $fBR = [string][char]0x251B
} else {
    $fH = [string][char]0x2500; $fV = [string][char]0x2502
    $fTL = [string][char]0x250C; $fTR = [string][char]0x2510
    $fBL = [string][char]0x2514; $fBR = [string][char]0x2518
}
# The frame's inner width, as in install.sh: every line stays within 72.
$inner = 60
$frame = $fH * $inner

function Step([string]$msg)  { Write-Host "  ${CYAN}$gStep${RESET} ${BOLD}$msg${RESET}" }
function Ok([string]$msg)    { Write-Host "    ${GREEN}$gOk${RESET} $msg" }
function Warn([string]$msg)  { Write-Host "    ${YELLOW}!${RESET} $msg" }
function Err([string]$msg)   { Write-Host "    ${RED}x${RESET} $msg" }
function Info([string]$msg)  { Write-Host "      ${DIM}$msg${RESET}" }

function Write-Header([string]$name, [string]$role) {
    Write-Host ""
    if ($plain) {
        Write-Host "  $name - $role"
    } else {
        $pad = ' ' * ($inner - 7 - $name.Length - $role.Length)
        Write-Host "  ${GRAY}$fTL$frame$fTR${RESET}"
        Write-Host "  ${GRAY}$fV${RESET} ${BOLD}$name${RESET}  ${GRAY}$gDot${RESET}  ${DIM}$role${RESET}$pad ${GRAY}$fV${RESET}"
        Write-Host "  ${GRAY}$fBL$frame$fBR${RESET}"
    }
    Write-Host ""
}

function Write-Footer([string]$msg) {
    Write-Host ""
    if (-not $plain) { Write-Host "  ${GRAY}$($fH * 42)${RESET}" }
    Write-Host "  ${GREEN}$gOk${RESET} ${BOLD}Done.${RESET} $msg"
    Write-Host ""
}

# Why a file in the install directory could not be moved: Windows' own message,
# and any claude-statusline process still running, which is what usually holds
# a file there. Claude Code may be closed while such a process lives on.
function Show-MoveFailure($Failure) {
    Info $Failure.Exception.Message
    # By name, not by path: a process running from a renamed copy can report
    # no path at all, and that is exactly the one holding the sidecar.
    foreach ($p in @(Get-Process -Name 'claude-statusline*' -ErrorAction SilentlyContinue)) {
        Info "Still running: $($p.ProcessName), PID $($p.Id), since $($p.StartTime). Stop it with: Stop-Process -Id $($p.Id)"
    }
}

# See install.ps1's copy for the full reasoning. $LASTEXITCODE is only written
# by a process that starts, so an executable that cannot launch leaves the
# previous command's code in place and a bare check reads it as success. Here
# that would report the settings entries removed when nothing ran.
function Invoke-Binary {
    param([string]$Exe, [string[]]$BinArgs)
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

Write-Header "claude-statusline" "uninstaller"

# --- The URI handler first, while the binary that owns it still exists ---
# A toast left in the Action Center carries the claude-statusline: scheme; once
# the handler is gone a click on it would open the shell's open-with dialog, so
# the notification history is cleared too.
Step "Unregistering the click handler"
$unregistered = $false
if (Test-Path $binPath) {
    $unreg = Invoke-Binary $binPath @('settings', 'protocol', 'unregister', '--binary', $binPath)
    if ($unreg.Ran -and $unreg.Code -eq 0) {
        $unregistered = $true
        if ($unreg.Output) { Info ($unreg.Output -join ' ') }
    }
}
if (-not $unregistered -and (Test-Path $protocolKey)) {
    # The binary is gone or cannot run: remove the key here, guarded on the
    # command naming our helper, so another program's scheme is never touched.
    $command = $null
    try {
        $command = (Get-ItemProperty -Path "$protocolKey\shell\open\command" -Name '(default)' -ErrorAction Stop).'(default)'
    } catch {}
    if ($command -and $command -like '*\claude-statusline-focus.exe"*') {
        Remove-Item -Path $protocolKey -Recurse -Force -ErrorAction SilentlyContinue
        $unregistered = $true
    } elseif ($command) {
        Info "The claude-statusline: scheme belongs to another program and was kept"
    }
}
if ($unregistered -or -not (Test-Path $protocolKey)) {
    Ok "No claude-statusline: URI handler remains"
} else {
    Warn "Could not remove the claude-statusline: URI handler"
    Info "Remove HKCU:\Software\Classes\claude-statusline by hand if it names claude-statusline-focus.exe"
}
# Through Windows' own notification API, under Windows PowerShell's identity,
# which is the one every toast this tool raised used: the built-in ones now and
# BurntToast's before them, so both are cleared. In a Windows PowerShell child,
# because PowerShell 7 cannot load WinRT types, and it needs nothing installed.
$ps51 = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"
$clear = Invoke-Binary $ps51 @('-NoProfile', '-NonInteractive', '-Command',
    "`$null=[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]; [Windows.UI.Notifications.ToastNotificationManager]::History.Clear('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe')")
if ($clear.Ran -and $clear.Code -eq 0) { Info "Cleared the notification history" }
Write-Host ""

# --- settings.json next, while the binary that can edit it still exists ---
# Order matters: the merge logic lives in the binary, so removing the entries
# has to happen before removing the tool that removes them.
Step "Updating Claude Code settings"
if (-not (Test-Path $settingsPath)) {
    Warn "settings.json not found"
} elseif (Test-Path $binPath) {
    $removed = Invoke-Binary $binPath @('settings', 'remove', '--binary', $binPath)
    if ($removed.Ran -and $removed.Code -eq 0) {
        Ok "Removed statusline entries and hooks from settings.json"
        Info $settingsPath
    } else {
        Warn "Failed to update settings.json"
        if ($removed.Output) { Info ($removed.Output -join ' ') }
        Info "Remove the statusLine, subagentStatusLine and claude-statusline hook entries manually"
    }
} else {
    Warn "Binary already removed - cannot edit settings.json automatically"
    Info "Remove the statusLine, subagentStatusLine and claude-statusline hook entries manually"
    Info $settingsPath
}
Write-Host ""

# --- Binary ---
# Windows will not delete a running executable but will rename it, so the
# rename-aside is what makes uninstall work while Claude Code is open. A failed
# delete of the sidecar is tolerated; the next install sweeps it.
Step "Removing the binary"
if (Test-Path $binPath) {
    $moved = $false
    try {
        Move-Item -Path $binPath -Destination $sidecarPath -Force -ErrorAction Stop
        $moved = $true
    } catch {
        Warn "Could not move $binPath aside"
        Show-MoveFailure $_
    }
    if ($moved) {
        Remove-Item $sidecarPath -Force -ErrorAction SilentlyContinue
        if (Test-Path $sidecarPath) {
            Ok "Binary disabled (a locked copy remains as claude-statusline.exe.old)"
            Info "It will be removed automatically on the next install or uninstall."
        } else {
            Ok "Deleted $binPath"
        }
    }
} else {
    Warn "Binary not found (already removed?)"
}

# The click helper, the same way: renamed aside, then deleted where Windows
# allows it.
if (Test-Path $helperPath) {
    $helperMoved = $false
    try {
        Move-Item -Path $helperPath -Destination $helperSidecar -Force -ErrorAction Stop
        $helperMoved = $true
    } catch {
        Warn "Could not move $helperPath aside"
        Show-MoveFailure $_
    }
    if ($helperMoved) {
        Remove-Item $helperSidecar -Force -ErrorAction SilentlyContinue
        if (Test-Path $helperSidecar) {
            Ok "Click helper disabled (a locked copy remains as claude-statusline-focus.exe.old)"
        } else {
            Ok "Deleted $helperPath"
        }
    }
} elseif (Test-Path $helperSidecar) {
    Remove-Item $helperSidecar -Force -ErrorAction SilentlyContinue
}

Get-ChildItem -Path $binDir -Filter "$stagePrefix*" -Force -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

# The self-check log and quarantined binary a failed install leaves behind
# for diagnosis.
Remove-Item (Join-Path $binDir "claude-statusline.self-check.txt") -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $binDir "claude-statusline.failed") -Force -ErrorAction SilentlyContinue

# Only if it is now empty - the user may keep other tools here.
if ((Test-Path $binDir) -and -not (Get-ChildItem -Path $binDir -Force -ErrorAction SilentlyContinue)) {
    Remove-Item $binDir -Force -ErrorAction SilentlyContinue
    if (-not (Test-Path $binDir)) { Ok "Removed empty $binDir" }
}
Write-Host ""

# --- Notification icon ---
Step "Removing the notification icon"
if (Test-Path $iconPath) {
    Remove-Item $iconPath -Force -ErrorAction SilentlyContinue
    Ok "Deleted $iconPath"
} else {
    Info "Icon not found (not installed)"
}
Write-Host ""

# --- Data files ---
Step "Removing data files"
if (Test-Path $modelWindows) {
    Remove-Item $modelWindows -Force -ErrorAction SilentlyContinue
    Ok "Deleted $modelWindows"
} else {
    Info "No learned model-window map to remove"
}
# statusline-oc-* is kept in the list even though the binary never writes one:
# it cleans up after a script-era install that did.
foreach ($pattern in @('statusline-oc-*.txt', 'statusline-git-*.txt', 'statusline-tasks-*.json',
                       'statusline-notify-*.json', 'statusline-sa-*.txt',
                       'statusline-tokens-*.txt', 'statusline-focus-*.json')) {
    Get-ChildItem -Path $env:TEMP -Filter $pattern -Force -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}
# Current installs group the same files under claude-statusline-<owner>, where
# <owner> is a digest of this user's SID. The flat patterns above stay: a session
# upgraded mid-flight leaves its files behind in the old layout, and nothing at
# runtime ever sweeps them.
#
# Matched on a digit suffix rather than a claude-statusline-* wildcard. The test
# harness stages claude-statusline-test-* scratch roots in this same directory
# and the README's manual verification downloads claude-statusline-checksums.txt
# here; neither belongs to the uninstaller.
#
# Two further filters, both matching what the runtime already does with the same
# directory. The owner check is the Windows form of uninstall.sh's `id -u`
# scoping: %TEMP% is per-user on a default install, but a redirected or
# system-wide TEMP puts every user's state directory in one place, and none of
# the others are this uninstaller's to remove. The reparse-point check is why
# this is not a bare recursive delete: under Windows PowerShell 5.1 -- the shell
# the documented irm | iex path runs in -- Remove-Item -Recurse follows a
# junction and empties its target instead of unlinking it. Any user can plant one
# with mklink /J. verify_through_handle in src/platform/mod.rs refuses a reparse
# point at exactly this path, so such a directory is by construction never ours.
$me = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
Get-ChildItem -Path $env:TEMP -Directory -Force -ErrorAction SilentlyContinue |
    Where-Object {
        $_.Name -match '^claude-statusline-\d+$' -and
        -not ($_.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -and
        $(try { (Get-Acl $_.FullName -ErrorAction Stop).Owner -eq $me } catch { $false })
    } |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Ok "Cleared temporary session state"
Write-Host ""

# --- Notification config and debug log ---
# Both are removed unconditionally, which is what the script uninstaller did.
# Windows asks nothing here: notifications use Windows' own notification
# system, so the installer never put a tool on this machine for this
# uninstaller to offer back.
Step "Removing notification configuration"
if (Test-Path $configPath) {
    Remove-Item $configPath -Force -ErrorAction SilentlyContinue
    Ok "Deleted $configPath"
} else {
    Info "No notification config found"
}

$debugLog = "$claudeDir\statusline-debug.log"
if (Test-Path $debugLog) {
    Remove-Item $debugLog -Force -ErrorAction SilentlyContinue
    Ok "Deleted $debugLog"
}

Write-Footer "Restart Claude Code to apply."
