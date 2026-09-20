<div align="center">

# claude-statusline

**A rich, color-coded custom status line for [Claude Code](https://claude.ai/code) showing context usage, git state, costs, rate limits, and more**

[![macOS](https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white)](#macos)
[![Linux](https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black)](#linux)
[![Windows](https://img.shields.io/badge/Windows-0078D4?style=for-the-badge&logo=windows&logoColor=white)](#windows)
[![Rust](https://img.shields.io/badge/Rust-000000?style=for-the-badge&logo=rust&logoColor=white)](#installation)
[![No runtime dependencies](https://img.shields.io/badge/runtime_deps-none-success?style=for-the-badge)](#installation)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=for-the-badge)](#license)

---

Replaces Claude Code's default status bar with a detailed, color-coded dashboard
showing context usage, git state, costs, rate limits, and more — all inside a clean box frame.

![screenshot](assets/screenshot.png)

</div>

## Features

| Row | What it shows |
|-----|---------------|
| **repo** | Working directory (shortened relative to `$HOME`) and git branch with `↑ahead` / `↓behind` remote tracking, `+insertions` / `-deletions` / `~untracked`, and `⊟ stash` count |
| **agent** | Agent name with compact context % and in/out tokens (when running with `--agent` flag); each active subagent also gets its own `agent` row with context bar, `used/window` tokens, model, reasoning effort (only when explicitly set), task title, and `○ working` / `✓ done` status |
| **model** | Active model (e.g. `Opus 4.7`), reasoning effort level, and ready/working indicator |
| **context** | Color-coded context bar with percentage and token count (green < 60%, yellow < 85%, red 85%+) |
| **tokens** | Cumulative session breakdown — `in` (fresh input), `cache↑` (cache writes), `cache↓` (cache reads), `out` (output) |
| **cost** | Session cost in USD, message count, wall-clock duration, and 5-hour/7-day rate limit usage with burn-rate arrows (`⇡` over pace / `⇣` under pace) and time until reset |
| **notifications** | Sound alerts and native OS toast popups for permission requests, task completion, context compaction, rate limit warnings, and context window warnings (enable during install) |

All rows are dynamic — empty rows are automatically hidden.

---

## Highlights

### Context awareness at a glance
The context bar changes color as your conversation grows — **green** when you have plenty of room, **yellow** as you approach 85%, and **red** when you're close to the limit. No more surprise context resets mid-task.

### Burn-rate arrows on rate limits
The rate-limit segments on the cost row don't just show usage — they show **pace**. An `⇡` arrow means you're burning tokens faster than the reset rate (slow down), while `⇣` means you're under pace with time until reset. Plan your session around real data instead of guessing.

### Live working indicator
The model row shows a real-time status — `● ready` when idle, or `○ working` while Claude is generating. You always know if the model is still thinking or waiting for you.

### Compact agent view
When running with `--agent`, the agent row shows context usage as a percentage and cumulative in/out tokens in a compact inline format — all the essentials without taking up extra rows.

### Per-subagent context tracking
Every active subagent gets its own row — context bar, `used/window` tokens, model, the task's title (e.g. `Apply README review fixes`; the agent type shows when no title is available), and live status (`○ working` while active, `✓ done` for 30 seconds after completion, then the row disappears). Long titles are truncated to 39 characters plus an ellipsis. Percentages are measured against each subagent's **real** context window — fed live by Claude Code or learned per model — so a 1M-window subagent isn't judged against a 200K bar.

**Reasoning effort** shows on a subagent row only when that agent was dispatched with an explicit effort — from an agent definition's `effort:` frontmatter, for example. It uses the same wording and colours as the model row, so `low effort` on an agent row reads the same as it does above. An agent that set no effort of its own shows **no** effort segment, and that's deliberate: Claude Code reports the field only when there's an override, so absence means "this agent set nothing of its own" rather than "unknown". What it actually runs at is Claude Code's business — the status line never displays a level it wasn't told, and never guesses one.

Absence is not proof the agent set nothing, though. The segment is also missing when the live feed has gone stale and the row is rebuilt from the subagent transcript, which carries no effort — so a still-running agent can lose its segment. The **Upgrading** note below covers one more case.

> **Upgrading:** the status line and the subagent feed handler are subcommands of the same binary, so re-running the install command picks up both at once — there is no longer a partial-upgrade state where a new status line pairs with a stale handler and the row silently looks like the no-override case.

### Never miss a prompt
Sound alerts and native OS toast notifications fire on permission requests, task completion, context compaction, and rate limit warnings. Each event and channel (sound vs. visual) is independently toggleable — get pinged when Claude needs you, stay quiet when it doesn't.

---

## Installation

> **Note:** The installer will ask before overwriting any existing `statusLine` or `subagentStatusLine` configuration.
> Restart Claude Code after installing or updating.
>
> The git status segment requires git >= 2.15 (2017, when `git status --show-stash` and its porcelain `# stash` header were added). On older git the status line still works — it just renders no git segment.

---

<h3 id="macos"><img src="https://img.shields.io/badge/macOS-000000?style=for-the-badge&logo=apple&logoColor=white" alt="macOS" height="40"></h3>

**Install:**

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh | bash
```

Downloads a prebuilt, checksum-verified binary. No `jq`, no Bash version floor — nothing to install first.

**Update:**

Re-run the install command above — your other settings are preserved.

**Uninstall:**

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/uninstall.sh | bash
```

<details>
<summary><strong>Manual install</strong></summary>

Nothing here pipes a download into a shell — every step is one you can inspect before running.

1. **Download the binary** for your architecture, plus its checksum file:
   ```bash
   mkdir -p ~/.claude/bin
   # Apple Silicon
   TARGET=aarch64-apple-darwin
   # Intel: TARGET=x86_64-apple-darwin
   BASE=https://github.com/axlaser/claude-statusline/releases/latest/download
   curl -fsSL "$BASE/claude-statusline-$TARGET" -o ~/.claude/bin/claude-statusline
   curl -fsSL "$BASE/checksums.txt" -o /tmp/claude-statusline-checksums.txt
   curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/assets/claude-icon.png -o ~/.claude/claude-icon.png
   ```

2. **Verify the checksum before you run it**, then make it executable:
   ```bash
   shasum -a 256 ~/.claude/bin/claude-statusline
   grep "claude-statusline-$TARGET\$" /tmp/claude-statusline-checksums.txt
   # the two hashes must match
   chmod 700 ~/.claude/bin/claude-statusline
   ```

   Optionally verify the build provenance as well (needs the [GitHub CLI](https://cli.github.com)):
   ```bash
   curl -fsSL "$BASE/claude-statusline-$TARGET.sigstore.json" -o /tmp/claude-statusline.sigstore.json
   gh attestation verify ~/.claude/bin/claude-statusline \
     --bundle /tmp/claude-statusline.sigstore.json \
     --repo axlaser/claude-statusline \
     --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
   ```

3. **Confirm it renders**, which is the same check the installer runs:
   ```bash
   ~/.claude/bin/claude-statusline self-check && echo OK
   ```

   A non-zero exit means the binary launches but renders incorrectly — don't register it.

4. **Install terminal-notifier** (optional — for visual toast notifications):
   ```bash
   brew install terminal-notifier
   ```

5. **Create the notification config** — save as `~/.claude/notify-config.json`:
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

6. **Register it in Claude Code.** The binary edits `~/.claude/settings.json` itself, preserving everything it did not write:
   ```bash
   ~/.claude/bin/claude-statusline settings apply \
     --binary ~/.claude/bin/claude-statusline --all
   ```

   Or edit `~/.claude/settings.json` by hand — this is exactly what the command above writes:
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
           "matcher": "Edit|Write|MultiEdit|Bash|NotebookEdit",
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

7. **Remove any previous script installation.** If you are coming from a version that installed shell scripts, delete them — nothing points at them any more:
   ```bash
   rm -f ~/.claude/statusline.sh ~/.claude/notify.sh ~/.claude/git-refresh.sh ~/.claude/subagent-statusline.sh
   ```

   Leave `~/.claude/notify-config.json` alone: its format is unchanged and the binary reads it as-is.

8. **Restart Claude Code** — the status line and notifications are now active.

</details>

---

<h3 id="linux"><img src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" alt="Linux" height="40"></h3>

**Install:**

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh | bash
```

Downloads a prebuilt, checksum-verified binary, statically linked against musl — one artifact runs on any distribution, including Alpine and older glibc. No `jq`, no package manager involved.

**Update:**

Re-run the install command above — your other settings are preserved.

**Uninstall:**

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/uninstall.sh | bash
```

<details>
<summary><strong>Manual install</strong></summary>

Nothing here pipes a download into a shell — every step is one you can inspect before running.

1. **Download the binary** for your architecture, plus its checksum file. The Linux builds are statically linked against musl, so one artifact runs on any distribution including Alpine:
   ```bash
   mkdir -p ~/.claude/bin
   TARGET=x86_64-unknown-linux-musl
   # ARM: TARGET=aarch64-unknown-linux-musl
   BASE=https://github.com/axlaser/claude-statusline/releases/latest/download
   curl -fsSL "$BASE/claude-statusline-$TARGET" -o ~/.claude/bin/claude-statusline
   curl -fsSL "$BASE/checksums.txt" -o /tmp/claude-statusline-checksums.txt
   curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/master/assets/claude-icon.png -o ~/.claude/claude-icon.png
   ```

2. **Verify the checksum before you run it**, then make it executable:
   ```bash
   sha256sum ~/.claude/bin/claude-statusline
   grep "claude-statusline-$TARGET\$" /tmp/claude-statusline-checksums.txt
   # the two hashes must match
   chmod 700 ~/.claude/bin/claude-statusline
   ```

   Optionally verify the build provenance as well (needs the [GitHub CLI](https://cli.github.com)):
   ```bash
   curl -fsSL "$BASE/claude-statusline-$TARGET.sigstore.json" -o /tmp/claude-statusline.sigstore.json
   gh attestation verify ~/.claude/bin/claude-statusline \
     --bundle /tmp/claude-statusline.sigstore.json \
     --repo axlaser/claude-statusline \
     --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
   ```

3. **Confirm it renders**, which is the same check the installer runs:
   ```bash
   ~/.claude/bin/claude-statusline self-check && echo OK
   ```

   A non-zero exit means the binary launches but renders incorrectly — don't register it.

4. **Install libnotify** (optional — for visual toast notifications):
   ```bash
   sudo apt install libnotify-bin    # Debian/Ubuntu
   sudo dnf install libnotify        # Fedora/RHEL
   sudo pacman -S libnotify          # Arch
   ```

   To have a click on the toast raise the terminal window, also install `xdotool` (or `wmctrl`) on X11, or `kdotool` on KDE Wayland. Both are optional; without them the click still selects the tab or pane inside tmux, kitty, WezTerm and Konsole.

5. **Create the notification config** — save as `~/.claude/notify-config.json`:
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

6. **Register it in Claude Code.** The binary edits `~/.claude/settings.json` itself, preserving everything it did not write:
   ```bash
   ~/.claude/bin/claude-statusline settings apply \
     --binary ~/.claude/bin/claude-statusline --all
   ```

   Or edit `~/.claude/settings.json` by hand — this is exactly what the command above writes:
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
           "matcher": "Edit|Write|MultiEdit|Bash|NotebookEdit",
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

7. **Remove any previous script installation.** If you are coming from a version that installed shell scripts, delete them — nothing points at them any more:
   ```bash
   rm -f ~/.claude/statusline.sh ~/.claude/notify.sh ~/.claude/git-refresh.sh ~/.claude/subagent-statusline.sh
   ```

   Leave `~/.claude/notify-config.json` alone: its format is unchanged and the binary reads it as-is.

8. **Restart Claude Code** — the status line and notifications are now active.

</details>

---

<h3 id="windows"><img src="https://img.shields.io/badge/Windows-0078D4?style=for-the-badge&logo=windows&logoColor=white" alt="Windows" height="40"></h3>

**Install:**

```powershell
irm https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.ps1 | iex
```

Downloads a prebuilt, checksum-verified binary. PowerShell is used only to run the installer — the status line itself has no PowerShell dependency and no version floor.

**Update:**

Re-run the install command above — your other settings are preserved.

**Uninstall:**

```powershell
irm https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/uninstall.ps1 | iex
```

<details>
<summary><strong>Manual install</strong></summary>

Nothing here pipes a download into `iex` — every step is one you can inspect before running.

1. **Download the binary** for your architecture, plus its checksum file:
   ```powershell
   New-Item -ItemType Directory -Force "$env:USERPROFILE\.claude\bin" | Out-Null
   $target = "x86_64-pc-windows-msvc"
   # ARM: $target = "aarch64-pc-windows-msvc"
   $base = "https://github.com/axlaser/claude-statusline/releases/latest/download"
   $bin  = "$env:USERPROFILE\.claude\bin\claude-statusline.exe"
   $helper = "$env:USERPROFILE\.claude\bin\claude-statusline-focus.exe"
   Invoke-WebRequest -Uri "$base/claude-statusline-$target.exe" -OutFile $bin -UseBasicParsing
   Invoke-WebRequest -Uri "$base/claude-statusline-focus-$target.exe" -OutFile $helper -UseBasicParsing
   Invoke-WebRequest -Uri "$base/checksums.txt" -OutFile "$env:TEMP\claude-statusline-checksums.txt" -UseBasicParsing
   Invoke-WebRequest -Uri "https://raw.githubusercontent.com/axlaser/claude-statusline/master/assets/claude-icon.png" -OutFile "$env:USERPROFILE\.claude\claude-icon.png" -UseBasicParsing
   ```

2. **Verify the checksum before you run it:**
   ```powershell
   (Get-FileHash -Algorithm SHA256 $bin).Hash
   Select-String -Path "$env:TEMP\claude-statusline-checksums.txt" -Pattern "claude-statusline-$target.exe"
   (Get-FileHash -Algorithm SHA256 $helper).Hash
   Select-String -Path "$env:TEMP\claude-statusline-checksums.txt" -Pattern "claude-statusline-focus-$target.exe"
   # each pair of hashes must match (case aside)
   ```

   Optionally verify the build provenance as well (needs the [GitHub CLI](https://cli.github.com)):
   ```powershell
   Invoke-WebRequest -Uri "$base/claude-statusline-$target.exe.sigstore.json" -OutFile "$env:TEMP\claude-statusline.sigstore.json" -UseBasicParsing
   gh attestation verify $bin --bundle "$env:TEMP\claude-statusline.sigstore.json" `
     --repo axlaser/claude-statusline `
     --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
   ```

3. **Confirm it renders**, which is the same check the installer runs:
   ```powershell
   & $bin self-check | Out-Null; if ($LASTEXITCODE -eq 0) { "OK" }
   ```

   A non-zero exit means the binary launches but renders incorrectly — don't register it.

   The click helper is launched by the shell when you click a toast, and a downloaded file carries the Mark of the Web, so clear it now or the first click raises SmartScreen instead of your terminal:
   ```powershell
   Unblock-File $helper
   ```

4. **Install BurntToast** (optional — for visual toast notifications). Run this from **Windows PowerShell** rather than PowerShell 7: the toast is raised through Windows PowerShell 5.1, and a module installed from PowerShell 7 is only visible to it when Claude Code happens to inherit PowerShell 7's module path:
   ```powershell
   powershell.exe -Command "Install-Module -Name BurntToast -Scope CurrentUser"
   ```

5. **Create the notification config** — save as `%USERPROFILE%\.claude\notify-config.json`:
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

6. **Register it in Claude Code.** The binary edits `settings.json` itself, preserving everything it did not write — and it quotes its own path, which is what keeps a profile directory containing a space from breaking the command. The second command registers the `claude-statusline:` URI handler that makes a toast clickable; it writes only its own key under `HKCU\Software\Classes` and leaves the scheme alone if another program owns it:
   ```powershell
   & $bin settings apply --binary $bin --all
   & $bin settings protocol register --binary $bin
   ```

   Or edit `%USERPROFILE%\.claude\settings.json` by hand. Replace `YOUR_USERNAME` with your Windows username, and keep the inner quotes — this is exactly what the command above writes:

   ```json
   {
     "statusLine": {
       "type": "command",
       "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\"",
       "refreshInterval": 1
     },
     "subagentStatusLine": {
       "type": "command",
       "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" subagent"
     },
     "hooks": {
       "PostToolUse": [
         {
           "matcher": "Edit|Write|MultiEdit|Bash|NotebookEdit",
           "hooks": [{ "type": "command", "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" git-refresh", "async": true }]
         }
       ],
       "PermissionRequest": [
         {
           "hooks": [{ "type": "command", "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" notify permission", "async": true }]
         }
       ],
       "Stop": [
         {
           "hooks": [{ "type": "command", "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" notify stop", "async": true }]
         }
       ],
       "PreCompact": [
         {
           "matcher": "*",
           "hooks": [{ "type": "command", "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" notify compaction_start", "async": true }]
         }
       ],
       "PostCompact": [
         {
           "matcher": "*",
           "hooks": [{ "type": "command", "command": "\"C:/Users/YOUR_USERNAME/.claude/bin/claude-statusline.exe\" notify compaction_done", "async": true }]
         }
       ]
     }
   }
   ```

7. **Remove any previous script installation.** If you are coming from a version that installed PowerShell scripts, delete them — nothing points at them any more:
   ```powershell
   Remove-Item "$env:USERPROFILE\.claude\statusline.ps1", "$env:USERPROFILE\.claude\notify.ps1", `
     "$env:USERPROFILE\.claude\git-refresh.ps1", "$env:USERPROFILE\.claude\subagent-statusline.ps1" `
     -Force -ErrorAction SilentlyContinue
   ```

   Leave `notify-config.json` alone: its format is unchanged and the binary reads it as-is.

8. **Restart Claude Code** — the status line and notifications are now active.

</details>

---

### Prereleases

The install commands above always resolve the latest **stable** release, so a
prerelease is never installed by accident. To opt in, add `--pre`:

```bash
curl -fsSL https://raw.githubusercontent.com/axlaser/claude-statusline/dev/install/install.sh | bash -s -- --pre
```
```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/axlaser/claude-statusline/dev/install/install.ps1))) --pre
```

These fetch the installer from `dev` rather than `master`: prereleases are cut
from the integration branch, so that is where the installer matching them lives.
The stable commands above stay on `master`.

> The PowerShell form is longer than the plain one-liner because `irm | iex` has
> no way to pass arguments. If you would rather not read that, download the
> installer first and run `.\install.ps1 --pre` — see
> [Without piping to a shell](#without-piping-to-a-shell).

`--pre` installs whatever is furthest ahead, prereleases included. Once a stable
release overtakes them you get that stable release instead of an older preview,
so `--pre` is safe to leave in an update command. Everything else is unchanged:
the checksum is still verified and refusing to match still stops the install.

To go back to stable, re-run the install command without `--pre`. To pin one
exact version instead, set `CLAUDE_STATUSLINE_VERSION` to its tag:

```bash
CLAUDE_STATUSLINE_VERSION=v1.0.0 bash install.sh
```

---

### From a cloned repo

```bash
git clone https://github.com/axlaser/claude-statusline.git
cd claude-statusline
bash install/install.sh      # macOS and Linux
.\install\install.ps1        # Windows
```

These still download the published binary rather than building one — cloning saves you
piping a URL into a shell, not the download. To update, re-run the installer. To uninstall,
run `install/uninstall.sh` or `install/uninstall.ps1`.

To build and install from source instead, you need a Rust toolchain:

```bash
cargo build --release
cargo test                                     # optional but recommended
target/release/claude-statusline self-check    # must exit 0
```

Then place the binary at `~/.claude/bin/claude-statusline` and register it with
`claude-statusline settings apply --binary <that path> --all`.

### Without piping to a shell

Piping a URL into `bash` or `iex` runs code you have not read. If you would rather
not, download the installer first, read it, then run it:

```bash
curl -fsSL -O https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.sh
less install.sh          # read it
bash install.sh
```

```powershell
Invoke-WebRequest -UseBasicParsing -OutFile install.ps1 `
  -Uri https://raw.githubusercontent.com/axlaser/claude-statusline/master/install/install.ps1
Get-Content install.ps1  # read it
.\install.ps1
```

The installer downloads a prebuilt binary, verifies its SHA-256 against the
`checksums.txt` published with the release, and only then places it and sets the
execute bit. A checksum that cannot be fetched or computed stops the install —
there is no path that skips verification.

If the [GitHub CLI](https://cli.github.com) is installed, the installer also
verifies the release's build-provenance attestation. That check is skipped when
`gh` is absent, and you can demand it instead:

```bash
bash install.sh --require-attestation
```

To verify by hand at any time:

```bash
gh attestation verify ~/.claude/bin/claude-statusline \
  --repo axlaser/claude-statusline \
  --signer-workflow axlaser/claude-statusline/.github/workflows/release.yml
```

To install a specific release rather than the latest, set
`CLAUDE_STATUSLINE_VERSION` to its tag.

Both installers take `--pre` here too, selecting the prerelease channel — see
[Prereleases](#prereleases).

---

## Customization

### Refresh Interval

By default the status line updates after each assistant message. To also refresh on a timer (useful for keeping the clock and git status current), add `refreshInterval` to your settings. The installer sets this to `1` on every platform, and leaves it alone if you have already set your own:

```json
{
  "statusLine": {
    "type": "command",
    "command": "~/.claude/bin/claude-statusline",
    "refreshInterval": 1
  }
}
```

This refreshes every second, which is the minimum and is fine on every platform — the old advice to keep Windows at `2` was about PowerShell's ~124 ms startup, and there is no interpreter to start any more. Raise it if you would rather the status line moved less.

Whatever you set here survives upgrades: the installer rewrites only `type` and `command`, so `refreshInterval`, `padding`, and anything else you added to the entry are left as you left them.

### Padding

Add horizontal spacing around the status line:

```json
{
  "statusLine": {
    "type": "command",
    "command": "~/.claude/bin/claude-statusline",
    "padding": 2
  }
}
```

### Debug Logging

Logging is **off unless you ask for it**. Set `STATUSLINE_DEBUG=1` in the environment Claude Code launches with, and every subcommand appends to a single log:

| Platform | Log location |
|----------|-------------|
| macOS / Linux | `~/.claude/statusline-debug.log` |
| Windows | `%USERPROFILE%\.claude\statusline-debug.log` |

One log covers the status line, all three hooks and the click handler. Most lines carry a component prefix — `git:`, `transcript:`, `subagents:`, `model-windows:`, `notify:`, `focus:`, `git-refresh:`, `subagent-statusline:` — so you can tell which part wrote them; a few process-level entries have none. The click handler is launched by the OS without your environment, so it logs when the session that raised the toast had the variable set. Unset the variable to stop logging; the file is safe to delete at any time.

### Notifications

The installer can configure both **sound** and **visual** (native OS toast) notifications. Each channel is independently toggleable per event type.

#### Events

| Event | Trigger |
|-------|---------|
| Permission request | Claude shows a permission dialog |
| Task complete | Claude finishes responding |
| Compaction start | Context compaction begins |
| Compaction done | Context compaction completes |
| Context high | Context window usage >= 70% (configurable) |
| Rate limit | Rate limit usage >= 80% (configurable) |

#### Sound

Platform-native sounds — no additional software needed:

| Platform | Permission / Compaction start | Complete / Compaction done | Warning (rate limit / context) | Player |
|----------|-------------------------------|----------------------------|-------------------------------|--------|
| macOS | Tink | Glass | Sosumi | `afplay` |
| Linux | freedesktop bell | freedesktop complete | freedesktop dialog-warning | `paplay` / `aplay` |
| Windows | System Exclamation | System Asterisk | System Hand | Built-in (`SystemSounds`) |

#### Visual (toast notifications)

| Platform | Tool | Install |
|----------|------|---------|
| macOS | [terminal-notifier](https://github.com/julienXX/terminal-notifier) | `brew install terminal-notifier` |
| Linux | notify-send | `sudo apt install libnotify-bin` (or equivalent for your distro) |
| Windows | [BurntToast](https://github.com/Windos/BurntToast) | `Install-Module -Name BurntToast -Scope CurrentUser` |

These are optional and you install them yourself — the installer does not fetch them. If the visual tool is missing, sound notifications still work; visual silently degrades rather than failing.

Toast notifications display the Claude icon ([source](https://commons.wikimedia.org/wiki/File:Claude_AI_symbol.svg), public domain). The installer downloads it to `~/.claude/claude-icon.png` automatically, and the toast simply omits it if the file is absent.

#### Click to focus

Clicking a toast brings the terminal that runs the session to the front, and selects its tab or pane where the terminal can be driven from outside. It is on wherever `visual` is on — there is no separate switch — and nothing focuses without a click.

| Terminal | On click |
|----------|----------|
| Terminal.app, iTerm2 | the window comes forward and the session's tab is selected |
| Ghostty | the session's terminal is focused, matched by tty (or by working directory, when that is unique) |
| kitty, with `allow_remote_control` on | the window comes forward and the session's kitty window is focused |
| WezTerm | the session's pane is activated |
| Konsole | the session's tab is selected; the window is raised on X11 and on KDE Wayland |
| tmux, GNU screen, zellij | the session's pane is selected inside the multiplexer, on top of the terminal's own raise |
| Windows Terminal, the classic console, VS Code | the window comes forward (tabs cannot be selected from outside) |
| GNOME Terminal, Alacritty, Warp, anything else | the window or the application comes forward |

Per platform:

- **macOS** — terminal-notifier stores the click actions with the notification, so a click from Notification Center works after the session has ended too. The first tab selection asks for **Automation** consent (terminal-notifier controlling your terminal, under System Settings > Privacy & Security > Automation); if you deny it, clicks still bring the application forward. `tccutil reset AppleEvents` clears a wrong answer.
- **Linux** — the toast stays clickable for two minutes after it appears (the `notify` process waits that long, then exits); a later click from the notification list only dismisses it. Raising the window on X11 needs `xdotool` or `wmctrl`, and on KDE Wayland `kdotool`; GNOME on Wayland refuses activation from outside, so there the click dismisses, while tab and pane selection inside tmux, kitty, WezTerm and Konsole still work. GNOME Terminal exposes no window id, so with several GNOME Terminal windows open nothing is raised rather than the wrong one. A libnotify older than 0.7.10 (Ubuntu 22.04) rejects the action flag, and the toast is re-raised without it.
- **Windows** — a second, console-free executable, `claude-statusline-focus.exe`, handles the click through a per-user `claude-statusline:` URI handler the installer registers. No console, PowerShell or terminal window appears, including for a click from the Action Center after the session has ended. When Windows refuses to bring the window forward — an elevated terminal is the usual case — its taskbar button flashes instead. Until the handler is registered, clicking a toast keeps BurntToast's default behaviour. Window-level focus covers Windows Terminal, the classic console, VS Code and other Electron terminals, Alacritty, WezTerm, mintty and ConEmu.

The session's terminal is recorded when the toast is raised, beside the other session state (`statusline-focus-<session-id>.json` in the state directory). Once the session has ended, a click brings the application or window forward and selects nothing; the record is never used to select another session's tab.

#### Configuration

Notification settings are stored in `~/.claude/notify-config.json`:

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

Edit this file directly to toggle individual channels or adjust thresholds. The installer creates it with defaults on first run.

To enable after initial install, re-run the installer and answer **y** to the notification prompts. To disable, run the uninstaller — it removes notification hooks while preserving your other settings.

---

## Troubleshooting

<details>
<summary><strong>Status line not appearing</strong></summary>

The status line is designed to fail silently — it never writes to stderr and always exits 0, because anything else breaks Claude Code's UI. So an absent status line gives you no error to read, and these are the things to check by hand:

- Confirm the binary runs and renders: `~/.claude/bin/claude-statusline self-check`. Exit 0 means the renderer is sound; non-zero means the build is bad and you should reinstall.
- Verify the path in `settings.json` matches where the binary actually is. On Windows the stored command must keep its surrounding quotes, or a profile directory containing a space splits the command.
- macOS/Linux: confirm it is executable (`chmod +x ~/.claude/bin/claude-statusline`).
- Apple Silicon: an unsigned binary is killed on sight. Released artifacts are ad-hoc signed; if you built your own, run `codesign --sign - --force target/release/claude-statusline`.
- Restart Claude Code after changing settings.
- Set `STATUSLINE_DEBUG=1` and check the debug log.

</details>

<details>
<summary><strong>Context percentage shows 0% on first message</strong></summary>

This is normal. Claude Code doesn't report context usage until after the first API response. The bar will populate on the second refresh.

</details>

<details>
<summary><strong>Rate limits not showing</strong></summary>

Rate limit data is only available for Claude.ai Pro and Max subscribers. API users (Anthropic Console) won't see rate limit data on the cost row. The data also only appears after the first API response in a session.

</details>

<details>
<summary><strong>Notification sounds not playing</strong></summary>

- Test directly: `~/.claude/bin/claude-statusline notify stop` (should play a sound). On Windows: `& "$env:USERPROFILE\.claude\bin\claude-statusline.exe" notify stop`. From an interactive shell nothing is read from the terminal; when Claude Code runs the hook, the event's JSON arrives on stdin and names the session the toast belongs to
- Check the event is not muted in `~/.claude/notify-config.json` — `"sound": false` genuinely mutes it
- Confirm the hooks are registered — `settings.json` should carry `claude-statusline notify <event>` entries under `PermissionRequest`, `Stop`, `PreCompact` and `PostCompact`
- Linux: ensure PulseAudio/PipeWire is running (`paplay` requires it) or ALSA is available (`aplay`)
- Restart Claude Code after installation — hooks are loaded at startup

</details>

<details>
<summary><strong>Visual toast notifications not appearing</strong></summary>

**macOS:** terminal-notifier posts notifications under its own bundle ID, which macOS may silence by default. Go to **System Settings > Notifications > terminal-notifier** and enable **Allow Notifications**. If terminal-notifier doesn't appear in the list, run `terminal-notifier -title "Test" -message "Hello"` once to register it, then check again.

**Linux:** Ensure your desktop environment supports notifications (GNOME, KDE, XFCE, etc.). Test with `notify-send "Test" "Hello"`. Wayland compositors may require additional configuration.

**Windows:** BurntToast requires the Windows notification center. Test with `New-BurntToastNotification -Text "Test", "Hello"`. If notifications are suppressed, check **Settings > System > Notifications** and ensure notifications are enabled for PowerShell. The toast is raised through Windows PowerShell 5.1, whose own module paths are `Documents\WindowsPowerShell\Modules` and `Program Files\WindowsPowerShell\Modules`. A BurntToast installed from PowerShell 7 lands under `Documents\PowerShell\Modules`, which Windows PowerShell only sees when Claude Code inherited a PowerShell 7 module path from the terminal that launched it. Installing the module from a Windows PowerShell prompt (`powershell.exe -Command "Install-Module -Name BurntToast -Scope CurrentUser"`) makes it visible however Claude Code was started; `powershell.exe -Command "Get-Module -ListAvailable BurntToast"` confirms it.

**All platforms:** Set `STATUSLINE_DEBUG=1` and check `~/.claude/statusline-debug.log` for `notify:` entries to confirm the hook ran and whether the visual tool was found.

</details>

<details>
<summary><strong>Clicking a toast does nothing, or brings the wrong thing forward</strong></summary>

Set `STATUSLINE_DEBUG=1` in the environment Claude Code runs in, raise a toast, click it, and read the `focus:` lines in `~/.claude/statusline-debug.log`: they say whether a record was found, whether the session's process was still alive, and which steps ran.

- **Nothing is recorded:** the toast is raised without click handling when the state directory did not verify (the `state_dir:` line says so) — or, on Windows, when the handler is not registered. `claude-statusline settings protocol has --binary <path to claude-statusline.exe>` exits 0 when it is.
- **The session had ended:** the application or window comes forward and no tab is selected. That is deliberate; a stale record never selects another session's tab.
- **macOS, the application comes forward but the tab is not selected:** terminal-notifier needs Automation consent to drive your terminal. Look under System Settings > Privacy & Security > Automation, or run `tccutil reset AppleEvents` and click again. VS Code, Warp and Alacritty have no tab hook; they come forward as an application.
- **Linux, nothing comes forward:** the click window is two minutes; on X11 install `xdotool` or `wmctrl`, on KDE Wayland `kdotool`; on GNOME Wayland the compositor refuses activation from outside. Several GNOME Terminal windows open means none is raised, because GNOME Terminal exposes no window id.
- **Linux or macOS, the terminal you were in sets a different `TMPDIR` than your login environment:** the click handler looks for the record under the login environment's temp directory, finds nothing, and dismisses.
- **kitty:** tab selection needs `allow_remote_control yes` and a unix listen socket (`listen_on unix:/tmp/kitty`).
- **Windows, the taskbar button flashes instead:** Windows refused to bring the window forward — an elevated terminal, for instance. The flash is the fallback.

</details>

<details>
<summary><strong>Errors in the debug log</strong></summary>

Set `STATUSLINE_DEBUG=1` and check `~/.claude/statusline-debug.log`. Common causes:
- Claude Code passed unexpected JSON — a malformed payload renders `[statusline: bad JSON]` rather than an empty bar
- Permission issues writing to the per-session state files, which live in `claude-statusline-<owner>/` inside the OS temp directory (`$TMPDIR`, or `%TEMP%` on Windows). If that directory exists but is not a plain directory owned by you, the status line falls back to writing directly in the temp root rather than failing — so an unexpected pile of loose `statusline-*` files there is a signal worth checking.
- A state file rejected by its guard: symlinks and reparse points are refused deliberately, and a foreign-owned file is not written through

Because of the silent-degradation contract, a panic inside the binary is caught and logged rather than printed — so `panic caught in subcommand` in the log is the signal for a genuine bug worth reporting.

</details>

---

## How It Works

Claude Code pipes a JSON object to the binary's stdin on each update. The JSON contains session data — model info, context window usage, cost, rate limits, transcript path, and more. The binary parses this data, optionally reads the conversation transcript for additional metrics (message count, token breakdown, idle/working state), and outputs ANSI-colored text that Claude Code renders as the status bar.

It is a single multi-call binary: the status line, the notification handler, the git-refresh hook, the subagent feed handler and the click handler are all subcommands of `claude-statusline`, so an install on macOS and Linux is one file plus `settings.json` entries pointing at it. Windows adds a second file, `claude-statusline-focus.exe`: the shell launches it when a toast is clicked, and it is built without a console so the click never opens a window of its own.

Git status is cached for up to 5 seconds and invalidated as soon as `.git/index` changes (or immediately by the git-refresh hook after file-modifying tools), so it stays effectively real-time without re-running git on every refresh. The transcript is read only when its size or modification time has changed — an unchanged transcript re-displays the stored totals without opening the file, which is what keeps refreshes fast in long sessions.

Subagent rows are fed by Claude Code's `subagentStatusLine` feature. The installer registers `claude-statusline subagent` as the handler, which receives the live tasks payload — each subagent's model, context window size, status, token count, and task description — and tees it to a session-scoped state file in the status line's own directory under the OS temp directory (`claude-statusline-<owner>/statusline-tasks-<session-id>.json`). The handler prints nothing, so Claude Code's own agent panel keeps its default rendering. Per-task `model` and `contextWindowSize` require Claude Code >= v2.1.205; on older versions (or before the feed delivers data), the status line falls back to parsing subagent transcripts. Task titles come from the feed's `description` field, so with an older Claude Code, rows gracefully fall back to showing the agent type.

On the transcript fallback path, each subagent's context window is resolved by checking the session's own model first, then a learned map, then a seed table, then a 200K default. A subagent running the same model as the session inherits that session's window directly — matched on the base model id, so a variant spelling like `claude-opus-5[1m]` and a bare `claude-opus-5` count as the same model. That makes a newly released model correct on a subagent's first appearance, with no prior observation. Beyond that, the status line records each main session's model → window pair to `~/.claude/statusline-model-windows.json`, so it learns real, plan-accurate context windows automatically — new models are picked up without any repo update. The seed table covers current documented models (1M for Fable 5, Opus 4.6+, Sonnet 5, and Sonnet 4.6; 200K for Haiku 4.5, Sonnet 4.5, and Opus 4.5). The uninstaller removes the handler registration, the binary, and the learned map.

---

## License

MIT License. See [LICENSE](LICENSE) for details.
