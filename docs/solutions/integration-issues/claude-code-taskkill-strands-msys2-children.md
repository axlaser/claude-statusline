---
title: Claude Code's tree kill strands MSYS2 children on Windows
date: 2026-10-07
category: integration-issues
module: housekeep (process reclamation and state sweep)
problem_type: integration_issue
component: tooling
severity: high
symptoms:
  - "Hundreds of claude-statusline.exe, cygwin-console-helper.exe and conhost.exe processes after a week of use"
  - "Each stranded claude-statusline.exe has one thread, zero CPU time and no live parent"
  - "Each stranded cygwin-console-helper.exe has exactly two arguments and a conhost.exe of its own"
  - "An upgrade failed with 'Could not move the existing binary aside'"
  - "Staging files (.<name>.<pid>.tmp) piling up in the state directory"
root_cause: upstream_race
resolution_type: workaround
tags:
  - windows
  - claude-code
  - msys2
  - git-for-windows
  - taskkill
  - process-leak
  - conhost
  - cygwin-console-helper
  - housekeep
---

# Claude Code's tree kill strands MSYS2 children on Windows

## Problem

Claude Code runs every status-line refresh on Windows as
`Git\bin\bash.exe -c "<command>"`: MSYS2 bash starts, and bash starts this binary. When
a newer refresh is due while one is still running, Claude Code cancels the old one with
`taskkill /PID <launcher> /T /F`, twice, 1.5 s apart, with no check that the PID still
names the same process. `/T` walks the tree from a snapshot; nothing holds the run in a
Job Object. That kill races MSYS2's process creation, and the loser is a process that
never exits.

Neither side is this project's to fix. What it can do is stop the damage accumulating.

## Symptoms

Measured on the maintainer machine (Windows 11 26200, Claude Code 2.1.292, Git for
Windows 2.55), with `refreshInterval: 1`:

- **After a week:** 116 suspended ticks, 160 console helpers and 160 console hosts. 436
  processes, about 30k handles, 760 threads and 2.4 GB of working set.
- **In an 11-minute live sample:** 993 `taskkill`s, and 14 of 119 status-line ticks (12%)
  leaked a process. No hook run and no `subagent` run leaked.
- **Upgrades failed.** A stranded tick keeps `claude-statusline.exe` mapped, so the
  installer could not move the old binary aside, and closing Claude Code does not help:
  the stranded processes have no parent left to close.
- **State piled up.** Ticks killed between write and rename had left 24 staging files in
  the state directory, and nothing ever deleted a per-session state file.

## Mechanism

Two leak shapes, one per place the kill can land.

1. **A tick created suspended and never resumed.** MSYS2 creates every native child with
   `CREATE_SUSPENDED` and resumes it a few milliseconds later (`spawn.cc`, msys2-runtime
   3.6.9). If the kill takes bash inside that window, nothing resumes the child:
   `claude-statusline.exe` with one thread, zero kernel and user CPU time, and no parent.
   It keeps its image mapped until someone terminates it.
2. **A console helper waiting for a bash that is gone.** `taskkill /T` kills the tick's
   console host before bash. Bash, now without a console, runs
   `create_invisible_console_workaround` (`fhandler/console.cc`), which starts
   `cygwin-console-helper.exe <hello> <goodbye>` with a new console host of its own. The
   helper signals the first event and waits forever on the second
   (`utils/mingw/cygwin-console-helper.cc`). If bash dies before signalling, the helper
   and its `conhost.exe` stay. CPU time says nothing here: 23 of 27 measured orphans had
   run. A three-argument helper serves a pseudo console and may outlive its creator
   legitimately; it is a different shape.

Only status-line ticks leaked in the sample, and they are the runs Claude Code cancels on
every refresh trigger; the subagent status line is never cancelled mid-start. A cadence of
one second made a cancelled start the common case: a tick took 0.5–1.3 s just to reach the binary
through Git Bash, so most were still starting when the next came due. The same kill on a
reused PID is anthropics/claude-code#96394.

On the maintainer machine an endpoint security product made it worse by adding a
path-specific delay to Git's `usr\bin\bash.exe`: an identical copy under another name
started in about 60 ms. `bash.exe -c true` against `sh.exe -c true` in the same folder
shows it, and an on-access process exclusion for that path helped. Slower starts mean a
wider window.

## What this project does

`housekeep`, a subcommand that prints nothing and exits 0, run as async hooks on
`UserPromptSubmit` and on `Notification` with matcher `idle_prompt`, and by the Windows
installers. Never per tick. Its placement and cost are in `docs/performance.md` §7.

**Reclamation (Windows).** One Toolhelp snapshot, then each process whose creation name
is one of the two images is opened once with
`PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | SYNCHRONIZE`, judged, and
terminated on that same handle. Holding the handle pins the process object, so a PID
freed and reused between judging and killing cannot be hit. The decision is a pure
function (`housekeep::judge`) over the facts read, table-tested on every OS. Every
condition must hold:

| | Stranded tick | Stranded helper |
|---|---|---|
| Image | `claude-statusline.exe` by creation name, so a file since renamed to `.old` still matches, in this binary's directory (case-insensitive, `\\?\` stripped) | `cygwin-console-helper.exe` under a path ending `\usr\bin\cygwin-console-helper.exe` |
| Shape | one thread, zero CPU time | exactly two arguments, read with `NtQueryInformationProcess(ProcessCommandLineInformation)` on the same handle |
| Parent | gone, or its PID now held by a process created after the child (focus capture's rule) | same |
| Age | created before the snapshot, at least 60 s old | same |
| Owner | this user | same |

A fact that cannot be read means skip, never kill
(`docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`). The 60 s
floor is what keeps the rules off a detached `notify` child, which legitimately runs with
a dead parent; `the_age_floor_outlasts_every_bounded_wait` holds it above every bounded
wait such a child makes. Other programs' orphaned two-argument helpers are reclaimed too,
because a helper whose creator is gone can never be signalled. After the terminations,
one shared wait of at most 1 s lets the images unmap.

**Sweep (all platforms).** Staging files older than 10 minutes and state files of the
seven families older than 7 days are deleted from the private state directory, and
only there: never in the flat fallback, regular files only, never through a link.

**The cadence stays at one second.** Slowing the default refresh to 10 s would cut
cancellations, and was not adopted, to keep per-second redraw; cancelled ticks still leak at the measured
rate, and these passes are what reclaim them (`docs/performance.md` §7).

**Upgrades survive a lock.** `install.ps1` renames a locked stale `.old` aside instead of
failing, runs `housekeep` once the new binary has passed its self-check, then removes
`.old`. `uninstall.ps1` runs `housekeep` before `settings remove`.

**Not a fix for this, but shipped beside it.** Hooks launch the binary without a shell
from Claude Code 2.1.139, which skips Git Bash on every hook run. Hooks never leaked, so
this saves processes rather than preventing leaks. The status line cannot follow: its
schema accepts no `shell` and no `args`. Separately, a detached `notify` child had held
the tick's output pipes open for the life of a toast, so Claude Code saw alert ticks run
about 5 s; that is fixed at entry (`docs/performance.md` §7, "A detached child held the
tick's output pipes").

## Verification

- `cargo test` never terminates a process: every test that launches `housekeep` sets
  `STATUSLINE_SKIP_PROCESS_RECLAIM`.
- An integration test creates a suspended copy of the binary and confirms it reads one
  thread and zero CPU, still matches by creation name after a rename to `.old`, and can
  be deleted once terminated. A process the test cannot open reads as unknown.
- A read-only probe of the policy on the maintainer machine matched 48 orphaned
  two-argument helpers and 17 stranded ticks from the installed binary's directory.
- Count by hand: `Get-Process claude-statusline, cygwin-console-helper -ErrorAction
  SilentlyContinue`, before and after
  `& "$env:USERPROFILE\.claude\bin\claude-statusline.exe" housekeep`. With
  `STATUSLINE_DEBUG=1`, each pass logs one line:
  `housekeep: candidates=N terminated=N refused=N not_exited=[...] skipped={...} files_removed=N`.

## What only upstream can fix

- **Claude Code** kills by tree snapshot with no Job Object, kills the console host
  before its children, and repeats the kill without a liveness check. Tracked as
  anthropics/claude-code#98976 (this leak) and #96394 (the second `taskkill` hitting
  reused PIDs); #96476 is the related hook-timeout case, where only the launcher dies.
- **MSYS2** creates native children suspended. Cygwin `28122c60fa` removes the console
  helper on Windows build 26100 and later, but Git for Windows 2.55 does not include it.
  Once it ships, the helper shape should disappear on those builds; the suspended-spawn
  window remains.

## Prevention

- **When a host kills your process tree, assume a child can be stranded mid-creation.**
  Anything that runs through a shell the host cancels needs an owner for what the kill
  leaves behind.
- **A kill rule needs several independent facts, all read on one handle.** Name alone,
  age alone or a dead parent alone each matches something alive: a live tick, a `notify`
  child, a helper serving a terminal.
- **Count leaks per launch type before choosing a fix.** 12% of ticks against zero hooks
  is what showed the leaks come from the status-line cadence, not the hooks, so
  direct-launch hooks could not have been the fix.
