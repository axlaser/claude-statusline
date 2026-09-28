<div align="center">

# claude-statusline

**A color-coded status line for [Claude Code](https://claude.ai/code).**<br>
Context, git, tokens, cost and rate limits in one box.

[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](#install)
[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](#install)
[![Windows](https://img.shields.io/badge/Windows-0078D4?style=for-the-badge&logo=windows&logoColor=white)](#install)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=for-the-badge)](#license)

![screenshot](assets/screenshot.png)

[Install](#install) · [What it shows](#what-it-shows) · [Notifications](#notifications) · [Configuration](#configuration) · [Troubleshooting](#troubleshooting) · [Other install options](#other-install-options)

</div>

## Install

**macOS and Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh | bash
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.ps1 | iex
```

Then restart Claude Code.

The installer downloads a prebuilt binary, checks its SHA-256, confirms it renders correctly, and registers it in `~/.claude/settings.json`. It asks before replacing an existing status line, and asks whether you want notifications. If you do, it checks for the tool popups need on macOS and Linux and offers to install it (see [Popups](#popups)). Nothing else needs installing first. The git segment needs git 2.15 or later; with older git, that segment is left out.

**Update:** run the install command again. Your other settings are kept.

**Uninstall:**

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/uninstall.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/uninstall.ps1 | iex
```

The uninstaller also offers to remove the popup tools the installer added. It never removes Homebrew, or a library other software uses, and your package manager shows what it will remove and asks first.

Prefer not to pipe into a shell, or want a prerelease? See [Other install options](#other-install-options).

## What it shows

| Row | Contents |
|-----|----------|
| **repo** | Directory (shortened to `~`), branch, `↑ahead` `↓behind`, `+insertions` `-deletions` `~untracked`, and `⊟ stashes`. Labelled **project** outside a git repo. |
| **agent** | Only with `claude --agent`: the agent's name, context %, and input/output tokens. |
| **model** | Context bar with %, `used/window` tokens, model name, reasoning effort, `● ready` or `○ working`, and your Claude Code version. |
| **tokens** | Session totals for `in`, `cache↑` (writes), `cache↓` (reads) and `out`, with the latest change in brackets. |
| **agent** | One row per running subagent: context bar, `used/window`, model, effort (if set), task title, and `○ working` or `✓ done`. |
| **cost** | Cost in USD, message count, session length, and 5-hour and 7-day rate-limit usage. |

Rows with nothing to show are hidden.

**Reading it**

- **Context bar:** green below 60%, yellow below 85%, red from 85%.
- **Rate limits:** `5h 42% ⇡3% (1h)` means 42% used, 3 points ahead of an even pace, resetting in 1 hour. `⇣` means you're under pace. Rate limits only show for Pro and Max plans.
- **Version:** turns yellow with `↑` when a newer Claude Code is out. Click it to open the changelog. The check reads Claude Code's own cached changelog, so it makes no network call.
- **Model name:** `Opus 5 (1M context)` shows as `Opus 5`. The window size is already next to the bar.
- **Subagents:** each is measured against its own context window, so a 1M subagent isn't shown against a 200K bar. A finished subagent shows `✓ done` for 30 seconds, then its row goes away. Titles longer than 39 characters are cut short.
- **Subagent effort:** shown only when the subagent was launched with an explicit effort, such as `effort:` in an agent definition. It can also go missing when the live feed goes stale and the row is rebuilt from the transcript.

## Notifications

Sound and desktop notifications for the moments you'd otherwise miss. Say yes when the installer asks, or run it again later to turn them on.

| Event | When | Config key |
|-------|------|------------|
| Permission request | Claude is waiting for your approval | `permission` |
| Task complete | Claude finishes responding | `stop` |
| Compaction start / done | Context compaction begins or ends | `compaction_start`, `compaction_done` |
| Context high | Context usage reaches 70% | `context_high` |
| Rate limit | Rate-limit usage reaches 80% | `rate_limit` |

Turn each event's sound or popup on and off in `~/.claude/notify-config.json`. The installer creates it with these defaults:

```json
{
  "permission":        { "sound": true, "visual": true },
  "stop":              { "sound": true, "visual": true },
  "rate_limit":        { "sound": true, "visual": true, "threshold": 80 },
  "context_high":      { "sound": false, "visual": true, "threshold": 70 },
  "compaction_start":  { "sound": true, "visual": true },
  "compaction_done":   { "sound": true, "visual": true }
}
```

### Sounds

Built-in system sounds, nothing to install.

| Platform | Needs you | Done | Warning | Player |
|----------|-----------|------|---------|--------|
| macOS | Tink | Glass | Sosumi | `afplay` |
| Linux | freedesktop bell | freedesktop complete | freedesktop dialog-warning | `paplay`, `ffplay` or `ogg123` |
| Windows | System Exclamation | System Asterisk | System Hand | built in |

### Popups

On macOS and Linux, popups need a small third-party tool. With notifications on, the installer checks for it, shows what it is and where it comes from, and installs it if you say yes. If you say no, or it can't install it, it prints the commands instead. Without the tool, sounds still work. Windows needs nothing: popups use Windows' built-in notifications.

| Platform | Tool | What the installer does | By hand |
|----------|------|-------------------------|---------|
| macOS | [terminal-notifier](https://github.com/julienXX/terminal-notifier) | Homebrew on Apple Silicon, if you have it. Otherwise its official release, checksum-checked, into `~/Applications`. No password. | `brew install terminal-notifier` |
| Linux | `notify-send` ([libnotify](https://gitlab.gnome.org/GNOME/libnotify)) | Your package manager, through `sudo` | `sudo apt install libnotify-bin`, `sudo dnf install libnotify`, or `sudo pacman -S libnotify` |
| Windows | Nothing | Nothing | Nothing |

On Linux the installer also offers the [click-to-focus](#click-to-focus) helper your desktop uses: [xdotool](https://github.com/jordansissel/xdotool) on X11, or [kdotool](https://github.com/jinliu/kdotool) on KDE Wayland. kdotool is packaged on Fedora; elsewhere it's `cargo install kdotool`. One yes covers them all, and `sudo` may ask for your password. The installer never installs Homebrew, and installs nothing when there's no terminal to answer, or over SSH with no desktop session.

After installing terminal-notifier, the installer sends a test popup, so macOS asks for permission while you're watching. Click **Allow**.

Earlier versions used the BurntToast module on Windows. It isn't used any more, and you can remove it with `Uninstall-Module BurntToast`.

Popups use the Claude icon ([source](https://commons.wikimedia.org/wiki/File:Claude_AI_symbol.svg), public domain), which the installer saves to `~/.claude/claude-icon.png`.

### Click to focus

Clicking a popup brings the session's terminal to the front and, where the terminal allows it, selects the right tab or pane. It's on whenever popups are on.

| Terminal | What a click does |
|----------|-------------------|
| Terminal.app, iTerm2 | Raises the window and selects the tab |
| Ghostty | Focuses the session's terminal |
| kitty (with `allow_remote_control yes`) | Raises the window and focuses the kitty window |
| WezTerm | Activates the pane |
| Konsole | Selects the tab and raises the window (X11 and KDE Wayland) |
| tmux, GNU screen, zellij | Selects the pane, on top of whatever the terminal does |
| Windows Terminal, classic console, VS Code | Raises the window |
| Anything else | Raises the window or app |

Platform notes:

- **macOS:** the first tab selection asks for Automation permission. If you decline, clicks still bring the app forward. Clicks keep working from Notification Center after the session ends.
- **Linux:** a popup stays clickable for 2 minutes. Raising the window needs `xdotool` or `wmctrl` on X11, or `kdotool` on KDE Wayland; the installer offers the one your desktop uses. GNOME on Wayland doesn't allow it, but tab and pane selection in tmux, kitty, WezTerm and Konsole still works.
- **Windows:** clicks go through a small helper, `claude-statusline-focus.exe`, so no console window flashes. If Windows won't raise the window (for example an elevated terminal), the taskbar button flashes instead.

Once a session has ended, a click brings the app forward but selects no tab.

## Configuration

These go in the `statusLine` entry of `~/.claude/settings.json`. Upgrades leave them alone.

**Refresh interval.** How often, in seconds, the status line redraws on its own, on top of redrawing after each message. The installer sets `1`, the minimum. Raise it if you'd like less movement.

**Padding.** Horizontal space around the box.

```json
{
  "statusLine": {
    "type": "command",
    "command": "~/.claude/bin/claude-statusline",
    "refreshInterval": 1,
    "padding": 2
  }
}
```

**Debug log.** Off by default. Set `STATUSLINE_DEBUG=1` in the environment Claude Code starts from, and everything logs to `~/.claude/statusline-debug.log` (`%USERPROFILE%\.claude\statusline-debug.log` on Windows). Lines are prefixed by component (`git:`, `transcript:`, `notify:`, `focus:`, and so on). The file is safe to delete.

## Troubleshooting

The status line never prints errors, because anything on stderr breaks Claude Code's display. When something's wrong, the [debug log](#configuration) is where to look.

<details>
<summary><strong>The status line doesn't appear</strong></summary>

1. Run `~/.claude/bin/claude-statusline self-check`. Exit code 0 means the binary works; anything else means reinstall.
2. Check the `command` path in `settings.json` points at the binary. On Windows, keep the quotes around the path.
3. On macOS and Linux, check it's executable: `chmod +x ~/.claude/bin/claude-statusline`.
4. On Apple Silicon, a binary you built yourself must be signed: `codesign --sign - --force target/release/claude-statusline`. Released binaries already are.
5. Restart Claude Code.

</details>

<details>
<summary><strong>Context shows 0% on the first message</strong></summary>

Expected. Claude Code reports context usage only after the first response. The bar fills in on the next refresh.

</details>

<details>
<summary><strong>Rate limits don't show</strong></summary>

They're only available on Claude Pro and Max plans, not API keys, and only after the first response in a session.

</details>

<details>
<summary><strong>No notification sounds</strong></summary>

- Test it: `~/.claude/bin/claude-statusline notify stop` should play a sound. On Windows: `& "$env:USERPROFILE\.claude\bin\claude-statusline.exe" notify stop`.
- Check the event isn't set to `"sound": false` in `~/.claude/notify-config.json`.
- Check `settings.json` has `claude-statusline notify <event>` hooks under `PermissionRequest`, `Stop`, `PreCompact` and `PostCompact`.
- Linux: `paplay` needs PulseAudio or PipeWire running. Without it, `ffplay` or `ogg123` play the sound if one is installed.
- Restart Claude Code. Hooks load at startup.

</details>

<details>
<summary><strong>No popups</strong></summary>

- **Test it:** `~/.claude/bin/claude-statusline notify stop` should show a popup. On Windows: `& "$env:USERPROFILE\.claude\bin\claude-statusline.exe" notify stop`. A test popup isn't tied to a Claude Code session, so clicking it only dismisses it.
- **Tool missing:** run the installer again. It checks for the tool and offers it, or prints the commands.
- **macOS:** open **System Settings > Notifications > terminal-notifier** and turn on **Allow Notifications**. If it isn't listed, run `terminal-notifier -title Test -message Hello` once, then look again.
- **Linux:** test with `notify-send Test Hello`. Your desktop needs a notification service; some Wayland compositors need extra setup. Over SSH there's no desktop to show popups in.
- **Windows:** popups appear under the name Windows PowerShell. Check it's allowed under **Settings > System > Notifications**, and that Do not disturb is off.
- **Everywhere:** with the debug log on, `notify:` lines show whether the hook ran and whether the popup tool was found.

</details>

<details>
<summary><strong>Clicking a popup does nothing, or raises the wrong thing</strong></summary>

Turn on the debug log, raise a popup, click it, and read the `focus:` lines. They say whether a record was found, whether the session was still running, and what was tried.

- **Nothing recorded:** the state directory didn't pass its ownership check (a `state_dir:` line says so). On Windows, the click handler may not be registered: `claude-statusline settings protocol has --binary <path to claude-statusline.exe>` exits 0 if it is.
- **The session had ended:** the app comes forward and no tab is selected. That's intended.
- **macOS, app comes forward but not the tab:** terminal-notifier needs Automation permission under **System Settings > Privacy & Security > Automation**. `tccutil reset AppleEvents` lets you answer again. VS Code, Warp and Alacritty can't have tabs selected from outside.
- **Linux, nothing comes forward:** see the [platform notes](#click-to-focus). With several GNOME Terminal windows open, none is raised, because GNOME Terminal doesn't expose which window is which.
- **`TMPDIR` differs between your terminal and your login session:** the click handler looks in the wrong place and just dismisses the popup.
- **kitty:** needs `allow_remote_control yes` and a unix socket, such as `listen_on unix:/tmp/kitty`.

</details>

<details>
<summary><strong>Reading the debug log</strong></summary>

- `[statusline: bad JSON]` in the status line means Claude Code sent something unexpected.
- Session state lives in `claude-statusline-<owner>/` inside your temp directory (`$TMPDIR`, or `%TEMP%` on Windows). If that directory exists but isn't a plain directory owned by you, files are written straight into the temp directory instead. Loose `statusline-*` files there are worth a look.
- Symlinked or foreign-owned state files are refused on purpose.
- `panic caught in subcommand` is a real bug. Please [open an issue](https://github.com/axlaser/claude-statusline/issues).

</details>

## Other install options

### Read before you run

Piping a URL into a shell runs code you haven't seen. To read the installer first:

```bash
curl -fsSL -O https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh
less install.sh
bash install.sh
```

```powershell
Invoke-WebRequest -UseBasicParsing -OutFile install.ps1 `
  -Uri https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.ps1
Get-Content install.ps1
.\install.ps1
```

The installer always verifies the SHA-256 against the release's `checksums.txt`. If the checksum can't be fetched or doesn't match, it stops. If [GitHub CLI](https://cli.github.com) 2.56.0 or later is installed, it also verifies the build's provenance attestation. To make that check required, pass `--require-attestation`.

To verify an installed binary yourself:

```bash
gh attestation verify ~/.claude/bin/claude-statusline \
  --repo axlaser/claude-statusline \
  --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
```

### Pin a version

Set `CLAUDE_STATUSLINE_VERSION` to a release tag:

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh | CLAUDE_STATUSLINE_VERSION=v1.0.0 bash
```

```powershell
$env:CLAUDE_STATUSLINE_VERSION = "v1.0.0"
irm https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.ps1 | iex
```

### Prerelease and dev builds

The standard command installs the latest stable release. Two opt-in channels:

- `--pre` installs the newest tagged release, prereleases included. When a stable release overtakes it, you get that instead, so it's safe to keep using.
- `--dev` installs the latest build of the `dev` branch, published on every push. It may be broken. It wins if both flags are given.

Use the installer from the `dev` branch for either:

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/dev/install/install.sh | bash -s -- --pre   # or --dev
```

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/axlaser/claude-statusline/dev/install/install.ps1))) --pre   # or --dev
```

PowerShell needs the longer form because `irm | iex` can't pass arguments.

To go back to stable, run the standard install command. To uninstall, use the `dev` uninstaller, since it knows about everything the `dev` installer placed:

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/dev/install/uninstall.sh | bash
```

```powershell
irm https://raw.githubusercontent.com/axlaser/claude-statusline/dev/install/uninstall.ps1 | iex
```

Checksums and attestations are verified the same way on every channel.

### Answer the popup tools question in advance

`CLAUDE_STATUSLINE_DEPS=yes` installs the [popup tools](#popups) without asking, and `CLAUDE_STATUSLINE_DEPS=no` skips them. Any other value is ignored. The uninstaller reads it too, for removing them. The installer prints a line whenever the variable answers, so a forgotten export shows up.

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh | CLAUDE_STATUSLINE_DEPS=yes bash
```

With no terminal, `yes` uses `sudo` only where it needs no password; otherwise the commands are printed.

### From a clone

```bash
git clone https://github.com/axlaser/claude-statusline.git
cd claude-statusline
bash install/install.sh      # macOS and Linux
.\install\install.ps1        # Windows
```

This still downloads the published binary. It accepts the same `--pre`, `--dev` and `--require-attestation` flags. Uninstall with `install/uninstall.sh` or `install/uninstall.ps1`.

### Build from source

Needs a Rust toolchain.

```bash
cargo build --release
cargo test                                    # optional
target/release/claude-statusline self-check   # must exit 0
```

Copy the binary to `~/.claude/bin/claude-statusline` and register it:

```bash
~/.claude/bin/claude-statusline settings apply --binary ~/.claude/bin/claude-statusline --all
```

On Windows, also copy `claude-statusline-focus.exe` from the same build next to it, then run `claude-statusline.exe settings protocol register --binary <path to claude-statusline.exe>` so popups are clickable.

### Manual install: macOS and Linux

Every step can be read before you run it.

1. **Download** the binary, the checksums and the popup icon. Pick your target:

   | Machine | `TARGET` |
   |---------|----------|
   | Apple Silicon Mac | `aarch64-apple-darwin` |
   | Intel Mac | `x86_64-apple-darwin` |
   | Linux x86-64 | `x86_64-unknown-linux-musl` |
   | Linux ARM64 | `aarch64-unknown-linux-musl` |

   The Linux builds are statically linked, so they run on any distribution, Alpine included.

   ```bash
   TARGET=aarch64-apple-darwin
   BASE=https://github.com/axlaser/claude-statusline/releases/latest/download
   mkdir -p ~/.claude/bin
   curl -fsSL "$BASE/claude-statusline-$TARGET" -o ~/.claude/bin/claude-statusline
   curl -fsSL "$BASE/checksums.txt" -o /tmp/claude-statusline-checksums.txt
   curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/assets/claude-icon.png -o ~/.claude/claude-icon.png
   ```

2. **Verify** the checksum. The two hashes must match. Use `shasum -a 256` on macOS, `sha256sum` on Linux.

   ```bash
   shasum -a 256 ~/.claude/bin/claude-statusline
   grep "claude-statusline-$TARGET\$" /tmp/claude-statusline-checksums.txt
   chmod 700 ~/.claude/bin/claude-statusline
   ```

   Optionally verify provenance too (GitHub CLI 2.56.0+):

   ```bash
   curl -fsSL "$BASE/claude-statusline-$TARGET.sigstore.json" -o /tmp/claude-statusline.sigstore.json
   gh attestation verify ~/.claude/bin/claude-statusline \
     --bundle /tmp/claude-statusline.sigstore.json \
     --repo axlaser/claude-statusline \
     --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
   ```

3. **Self-check.** This is the same test the installer runs. If it fails, stop here.

   ```bash
   ~/.claude/bin/claude-statusline self-check && echo OK
   ```

4. **Notifications (optional).** Install the [popup tool](#popups) for your platform, and save the [default config](#notifications) as `~/.claude/notify-config.json`. terminal-notifier is found on `PATH`, in `~/Applications` or `/Applications`, or in a Homebrew or MacPorts prefix.

5. **Register it.** This edits `~/.claude/settings.json` and leaves everything else in it alone:

   ```bash
   ~/.claude/bin/claude-statusline settings apply --binary ~/.claude/bin/claude-statusline --all
   ```

   <details>
   <summary>Or edit <code>settings.json</code> by hand</summary>

   This is exactly what the command writes:

   ```json
   {
     "statusLine": {
       "type": "command",
       "command": "~/.claude/bin/claude-statusline",
       "refreshInterval": 1
     },
     "subagentStatusLine": {
       "type": "command",
       "command": "~/.claude/bin/claude-statusline subagent"
     },
     "hooks": {
       "PostToolUse": [
         {
           "matcher": "Edit|Write|MultiEdit|NotebookEdit",
           "hooks": [{ "type": "command", "command": "~/.claude/bin/claude-statusline git-refresh", "async": true }]
         }
       ],
       "PermissionRequest": [
         {
           "hooks": [{ "type": "command", "command": "~/.claude/bin/claude-statusline notify permission", "async": true }]
         }
       ],
       "Stop": [
         {
           "hooks": [{ "type": "command", "command": "~/.claude/bin/claude-statusline notify stop", "async": true }]
         }
       ],
       "PreCompact": [
         {
           "matcher": "*",
           "hooks": [{ "type": "command", "command": "~/.claude/bin/claude-statusline notify compaction_start", "async": true }]
         }
       ],
       "PostCompact": [
         {
           "matcher": "*",
           "hooks": [{ "type": "command", "command": "~/.claude/bin/claude-statusline notify compaction_done", "async": true }]
         }
       ]
     }
   }
   ```

   </details>

6. **Upgrading from the old shell-script version?** Delete the scripts. Keep `notify-config.json`; its format hasn't changed.

   ```bash
   rm -f ~/.claude/statusline.sh ~/.claude/notify.sh ~/.claude/git-refresh.sh ~/.claude/subagent-statusline.sh
   ```

7. **Restart Claude Code.**

### Manual install: Windows

1. **Download** the binary, the click helper, the checksums and the popup icon. On ARM, use `aarch64-pc-windows-msvc`.

   ```powershell
   $target = "x86_64-pc-windows-msvc"
   $base   = "https://github.com/axlaser/claude-statusline/releases/latest/download"
   $bin    = "$env:USERPROFILE\.claude\bin\claude-statusline.exe"
   $helper = "$env:USERPROFILE\.claude\bin\claude-statusline-focus.exe"
   New-Item -ItemType Directory -Force "$env:USERPROFILE\.claude\bin" | Out-Null
   Invoke-WebRequest -Uri "$base/claude-statusline-$target.exe" -OutFile $bin -UseBasicParsing
   Invoke-WebRequest -Uri "$base/claude-statusline-focus-$target.exe" -OutFile $helper -UseBasicParsing
   Invoke-WebRequest -Uri "$base/checksums.txt" -OutFile "$env:TEMP\claude-statusline-checksums.txt" -UseBasicParsing
   Invoke-WebRequest -Uri "https://raw.githubusercontent.com/axlaser/claude-statusline/master/assets/claude-icon.png" -OutFile "$env:USERPROFILE\.claude\claude-icon.png" -UseBasicParsing
   ```

2. **Verify** the checksums. Each pair must match (ignoring case).

   ```powershell
   (Get-FileHash -Algorithm SHA256 $bin).Hash
   Select-String -Path "$env:TEMP\claude-statusline-checksums.txt" -Pattern "claude-statusline-$target.exe"
   (Get-FileHash -Algorithm SHA256 $helper).Hash
   Select-String -Path "$env:TEMP\claude-statusline-checksums.txt" -Pattern "claude-statusline-focus-$target.exe"
   ```

   Optionally verify provenance too (GitHub CLI 2.56.0+):

   ```powershell
   Invoke-WebRequest -Uri "$base/claude-statusline-$target.exe.sigstore.json" -OutFile "$env:TEMP\claude-statusline.sigstore.json" -UseBasicParsing
   gh attestation verify $bin --bundle "$env:TEMP\claude-statusline.sigstore.json" `
     --repo axlaser/claude-statusline `
     --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
   ```

3. **Self-check**, then unblock the click helper. Downloaded files are marked as coming from the internet, and without this the first popup click opens a SmartScreen prompt instead of your terminal.

   ```powershell
   & $bin self-check | Out-Null; if ($LASTEXITCODE -eq 0) { "OK" }
   Unblock-File $helper
   ```

4. **Notifications (optional).** Save the [default config](#notifications) as `%USERPROFILE%\.claude\notify-config.json`. Popups need nothing installed.

5. **Register it.** The first command edits `settings.json`. The second registers the `claude-statusline:` link handler that makes popups clickable; it only touches its own key under `HKCU\Software\Classes`, and leaves the scheme alone if another program owns it.

   ```powershell
   & $bin settings apply --binary $bin --all
   & $bin settings protocol register --binary $bin
   ```

   To edit `settings.json` by hand instead, use the [macOS and Linux JSON](#manual-install-macos-and-linux) with every command pointing at the full, quoted path. The quotes stop a space in your profile path from breaking the command:

   ```json
   "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" subagent"
   ```

6. **Upgrading from the old PowerShell-script version?** Delete the scripts. Keep `notify-config.json`.

   ```powershell
   Remove-Item "$env:USERPROFILE\.claude\statusline.ps1", "$env:USERPROFILE\.claude\notify.ps1", `
     "$env:USERPROFILE\.claude\git-refresh.ps1", "$env:USERPROFILE\.claude\subagent-statusline.ps1" `
     -Force -ErrorAction SilentlyContinue
   ```

7. **Restart Claude Code.**

## How it works

On each refresh, Claude Code pipes a JSON description of the session to the binary, which prints the box. The same binary also handles the hooks, as subcommands:

| Subcommand | Registered as | Does |
|------------|---------------|------|
| *(none)* | `statusLine` | Draws the box |
| `subagent` | `subagentStatusLine` | Saves Claude Code's live subagent feed for the box to read. Prints nothing, so Claude Code's own agent panel is unchanged. |
| `git-refresh` | `PostToolUse` hook | Clears the cached git status after a file edit |
| `notify <event>` | `PermissionRequest`, `Stop`, `PreCompact`, `PostCompact` hooks | Plays sounds and shows popups |
| `focus` | A popup's click action | Brings the session's terminal forward |

On Windows, clicks go to `claude-statusline-focus.exe`, a second program built without a console so no window flashes.

It stays fast in long sessions because it skips repeat work. Git status is cached for up to 5 seconds and cleared as soon as `.git/index` changes. The transcript is only read again when its size or modified time changes.

**Subagent context windows.** Claude Code 2.1.205 and later reports each subagent's model and window directly. On older versions, the window is worked out in this order: the session's own model, a map learned from your past sessions (`~/.claude/statusline-model-windows.json`), a built-in table of current models, a `[1m]` marker in the model ID, and finally 200K. Task titles also come from the feed, so older versions show the agent type instead.

## License

MIT. See [LICENSE](LICENSE).
