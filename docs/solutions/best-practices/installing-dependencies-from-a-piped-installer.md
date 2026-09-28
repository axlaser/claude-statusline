---
title: Installing third-party dependencies from a piped installer
date: 2026-09-28
category: best-practices
module: installers (install.sh, uninstall.sh, install.ps1, uninstall.ps1)
problem_type: best_practice
component: installer
severity: medium
applies_when:
  - "An installer step detects, installs or removes a tool the binary uses at runtime"
  - "An installer asks the binary it just placed what it can do (settings supports)"
  - "A CI leg exercises an install route on a hosted runner"
  - "An installer or uninstaller on Windows renames a binary aside and it fails"
tags: [installer, dependencies, sudo, curl-pipe-bash, windows, macos, ci]
---

# Installing third-party dependencies from a piped installer

## Context

On 2026-09-28 the macOS and Linux installer learned to check for, explain, install and
record the tools desktop notifications need (terminal-notifier; notify-send plus xdotool,
wmctrl or kdotool), and the uninstaller to offer them back. The plan was
`docs/plans/2026-09-28-0906-feat-installer-popup-tools-plan.md`. Most of what went
wrong only showed up on real machines and hosted runners; this records those findings so
the next change to the step does not rediscover them.

## Detection must be the runtime's lookup, not an approximation

The installer decides "present" the way the binary decides it will run a tool: `PATH`
first, then the fixed locations in `src/platform/notify.rs` and `src/platform/focus.rs`,
each through the same owner-and-mode guard (`platform::trusted_tool`), links followed,
the directory checked as well as the file. A looser check reports a tool present that the
binary then refuses, and the user is never offered the install that would have fixed it.

## Hosted runners already carry a terminal-notifier

GitHub's macOS images (and `/usr/local/bin` on ubuntu-24.04) ship the `terminal-notifier`
Ruby gem, whose stub is on `PATH`. The installer is right to count it, since the runtime
would use it, so a CI leg that means to exercise an install route has to take that
directory off `PATH` first. `ci.yml`'s `notification-tools` job does, and prints what is
left, so the next image change is visible in the log instead of looking like a skip.

## Asking the binary is a two-sided contract

`claude-statusline settings supports notification-tools` lets the installer skip the step
on a binary too old to find tools outside `PATH`. Because CI installs the dev-channel
build, which is republished by a workflow running *concurrently* with CI, a rename of the
capability cannot land in one push: first make the binary answer both names, let that
build publish, then move the installer to the new name and drop the old one. A release
run that fails leaves the dev channel on the previous build, so check `gh release view
dev-channel` before the second push.

## Consent under `curl | bash`

- `[ -t 0 ]` is always false under a pipe, and `[ -e /dev/tty ]` is true on runners where
  opening it fails; only `( : </dev/tty ) 2>/dev/null` tells whether anyone can answer.
- Ctrl-C at a `sudo` prompt also kills `curl`. Wrapping the script in one brace group
  makes bash parse all of it first, and an `INT` trap (trapped, not ignored, so children
  still die) lets the installer skip only that step.
- Over SSH there is no desktop session, so the step prints commands instead of
  installing. `DISPLAY=:0` in front of `bash` exercises the install path on such a box.

## Windows pins a running executable

Renaming a running `.exe` is allowed; replacing or deleting one is not. A `notify`
process that never returned outlived Claude Code for over 25 minutes, running from the
renamed `claude-statusline.exe.old`, and every later install and uninstall failed with
"Cannot create a file when that file already exists". Two fixes followed:

- `notify` now waits on any foreground helper for at most `SPAWN_DEADLINE`, then kills
  it (`platform::notify::wait_or_kill`).
- When a move aside fails, both PowerShell installers print Windows' own message and
  every `claude-statusline*` process by **name**. Matching by path missed the culprit:
  a process running from a renamed image reported an empty `Path`.

## Things that look like bugs and are not

- A test from `claude-statusline notify stop` in a terminal carries no Claude Code
  session, so clicking it only dismisses it. Real hook notifications raise the window.
- On macOS, notifications that reach only Notification Center mean terminal-notifier's
  alert style is None or a Focus mode such as Do Not Disturb is on. Reinstalling
  terminal-notifier can reset the style; adding it to the Focus mode's allowed apps lets
  notifications through.
