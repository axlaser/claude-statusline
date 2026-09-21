# Fixture-capture harness

Drives the **current** scripts and records what each one observably does, so the
Rust port has something to be equivalent to after the scripts are deleted
(R30, R31, R33; KTD9, KTD10).

Everything here is a development tool. The silent-degradation and no-`exit`
contracts in `CLAUDE.md` govern the runtime scripts and the shipped binary —
they do not apply to the harness, and must not. A capture that cannot be trusted
has to fail loudly, because a harness that degrades silently writes a plausible
fixture that everything downstream then treats as truth.

```
capture.sh      macOS and Linux driver (bash 4+, jq, git)
capture.ps1     Windows driver (PowerShell 5.1+, git) — its functional twin
measure-pair.sh   macOS and Linux paired measurement (bash 5+, jq, git)
measure-pair.ps1  Windows paired measurement — its functional twin
cases.json      the case table: what to capture, with what inputs
states.json     the docs/performance.md §4 git-state matrix, as data
payloads/       pinned stdin payloads
configs/        pinned notify-config.json inputs (R44)
inputs/         pinned state files a case supplies
shims/          recording stand-ins for the helpers a component invokes
```

Both drivers read the same `cases.json` and `states.json`. A case is defined
once and captured on all three platforms — the duplication this migration exists
to delete does not get to reappear in the harness.

`measure.sh` / `measure.ps1` used to pair a **runtime script against the binary
that replaced it**, which is what the migration had to prove. They are deleted:
the script trees went at `1f5acf2`, both drivers aborted on a missing-file
check, and `.github/workflows/measure.yml` invoked one of them and so could not
run either. The question anyone has now is "did this commit make the binary
slower", and `measure-pair.*` is the tool for it: **two builds of this crate**,
interleaved on one host, under the docs/performance.md §3 rules.

## Running it

```bash
tests/harness/capture.sh --list
tests/harness/capture.sh --component git-refresh
tests/harness/capture.sh --at 5d474a0        # regenerate a historical commit's fixtures
tests/harness/capture.sh --verify            # capture twice, require byte-identical output
```

```powershell
.\tests\harness\capture.ps1 -List
.\tests\harness\capture.ps1 -Component git-refresh
.\tests\harness\capture.ps1 -At 5d474a0
.\tests\harness\capture.ps1 -Verify
```

macOS and Linux capture runs on CI via `.github/workflows/capture-fixtures.yml`,
triggered by pushing a `capture-<component>` tag and deleting it afterwards.
Once a component's scripts are deleted (R37) its fixtures can only be
regenerated against the tree that still had them, so the tag takes an optional
commit: `capture-git-refresh@7ac72b8`. Locally that is `--at`/`-At`.
Windows capture runs on the maintainer's machine (KTD9) — the Windows script
tree is the one with no hosted equivalent of a real developer environment.

## What a capture guarantees

**Isolation.** `HOME`/`USERPROFILE` and `TMPDIR`/`TEMP` are redirected into a
throwaway root for every case, so a capture cannot read or write the real
profile and no case can see another's leftovers.

**A verified cache miss, in both directions.** The isolated temp root starts
empty, so no output cache can exist before the run. A case that renders must
leave one behind; a case marked `expect_render: false` must not. Byte-diffing
alone cannot tell a full render from a served cache or from an early exit —
all three produce the same bytes, which is exactly how the trust-check inversion
in `docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md` ran
nine days undetected.

**A recorded source commit.** Every fixture records the commit its scripts came
from. Capturing a dirty tree is refused without `--allow-dirty`, because the
recorded commit would not describe what actually ran and nothing downstream
could tell.

**No machine-local paths.** Captured output has every isolated path replaced by
a placeholder, and a capture still containing the real home path is refused
rather than written. `CLAUDE.md` forbids committing a personal absolute path,
and a fixture is a committed file.

**Deterministic git.** Identity, both dates, the branch name, and the
line-ending config are pinned in `states.json`. The detached-HEAD state renders
a short commit hash, so a fixture is only reproducible if the hash is.

## Pinning time against scripts that have no clock

R30 requires every case to pin its time source. The scripts read the real wall
clock and there is no injection point in a shell script, so the harness pins the
*outcome* instead: an intended modification time is materialised as an offset
from capture time, and recorded as an offset from the case's pinned `clock`.

Replaying in Rust pins the clock to `clock` and each state file's mtime to
`clock + mtime_offset`, which reproduces the same freshness and staleness
decisions deterministically. This is what KTD6's `Clock` trait — covering
filesystem mtimes as well as wall-clock reads — exists to make possible.

## Two drivers, one repository

`states.json` exists so a git state is described once and built identically by
both drivers. Twice that has failed silently, and in both cases the symptom was
the same: the detached-HEAD fixture — the only state that renders a commit hash
— disagreed across platforms while every other state matched.

- **bash ate the trailing newline.** `content=$(jq -r ...)` strips it, so the
  driver wrote a 12-byte `README.md` where the Windows driver wrote 13. Fixed
  with `jq -j` plus a `printf x` sentinel.
- **PowerShell turned the pinned dates into local time.** `ConvertFrom-Json`
  coerces an ISO-8601 string to `[DateTime]`, which stringifies in the local
  timezone, so git recorded the *capturing machine's* offset. The instant was
  still right and the hash still changed — the fixture was reproducible only in
  the timezone it was captured in. The dates are now in git's raw
  `<seconds> <offset>` format, which neither driver mistakes for a date.

The lesson both share: a value that reaches a commit object is rendered output.
When adding a state, check the detached-HEAD hash across platforms — it is the
canary, because it is the only place a build difference becomes visible bytes.

## The locale is a render input

R30 requires every render input to be pinned. The locale is one, and pinning it
to the wrong value is not safer than leaving it loose.

bash indexes a string by **byte** unless `LC_CTYPE` names a UTF-8 locale. The
status line's `get_vis` walks a row character by character to compute its
visible width, so under `LC_ALL=C` a 3-byte `█` counts as three terminal
columns, a 2-byte `·` as two, and every row is padded to a width that is wrong
by exactly its non-ASCII byte surplus. Both drivers used to export `LC_ALL=C`
around the component under capture for determinism; the macOS and Linux
statusline fixtures it produced contained rows between 111 and 179 columns
inside one frame — a box no user with a UTF-8 terminal has ever seen.

Both drivers now probe for a working UTF-8 locale by behaviour rather than by
name (`C.utf8` on Ubuntu, `UTF-8` on macOS) and fail loudly when none is found.
Everything that genuinely wanted `C` still has it: the harness's own `sort`
calls pin it per command, and the scripts pin it themselves where a decimal
separator or a byte-wise scan depends on it.

Windows is unaffected — .NET string length counts characters — which is why
only two of the three platforms recorded the broken box, and why comparing the
platforms is what surfaced it.

## A pinned input must carry the producer's bytes, not just its data

`inputs/` files stand in for what another component wrote. Where the reader
makes assumptions about the *shape* of those bytes, an input that carries the
right data in the wrong shape is a silently wrong fixture.

`inputs/tasks-feed.json` is the live example and must stay compact single-line
JSON. `subagent-statusline` only ever emits one line, so the status line reads
the feed with a single `IFS= read -r`. Pretty-printed, bash reads `{`, fails the
object check, and falls through to the transcript tier — rendering a plausible
box with no subagent rows, on a case whose whole purpose is to show the feed
tier being used. It was captured that way once: `feed-fresh` and `feed-stale`
came back byte-identical on macOS and Linux while Windows, which reads the whole
file, correctly showed the row.

When adding an input, capture or copy the real producer's output rather than
hand-writing something equivalent-looking.

## Paired measurement

`measure-pair.sh` and `measure-pair.ps1` produce the end-to-end fresh-process
medians docs/performance.md §3 requires before any hot-path change is believed —
one median for each of two builds of this crate, taken on the same host with the
runs interleaved.

```bash
tests/harness/measure-pair.sh --before <old>/claude-statusline --after target/release/claude-statusline
```

```powershell
.\tests\harness\measure-pair.ps1 -Before <old>\claude-statusline.exe -After .\target\release\claude-statusline.exe
```

Three tick shapes, measured separately because they cost differently and are
caused differently. `warm` clears nothing: the git cache is fresh and the token
record matches, so the tick renders from stored state. `git-miss` clears only
the git caches, which is the shape any pause longer than the 5 s TTL produces
and the row §6 records at 132 ms on Windows. `cold` clears every tick cache: a
new message *and* a git miss. Pick with `--mode` / `-Mode`.

A **real `.git` is staged** from `states.json` — `dirty` by default, any state
by name. No driver did this before, and without it a change to `src/git.rs`
measures nothing at all. `{REPO}` in the payload resolves to that work tree, not
to the scratch root, which is the same mapping the Rust fixture replay uses.

The **proof-of-work assertion is a parameter**. Both variants must always render
the box and the model row; `--proof` adds a pattern both must render, and
`--proof-after-only` adds one the after variant must render and the before
variant must not. That last form is how the version-segment pair was proved —
`--payload payloads/full-with-version.json --proof-after-only 'v2[.]1[.]270'` —
without the driver *being* that one check, which is what made it refuse every
other pair.

The binary is **spawned directly**. The drivers used to time `cmd.exe /c "<exe>
< payload"`, which charged every Windows figure for a shell the status line
never spawns: measured at +15.2 ms warm and +16.2 ms cold.

`docs/performance.md` §3 governs the method and both drivers implement it
literally: one fresh process per probe, a median of at least seven runs, an
isolated `HOME`/`USERPROFILE` and `TMPDIR`/`TEMP` restored afterwards, the
scratch root removed, and `STATUSLINE_DEBUG` cleared so one variant is not
charged for a log append the other skips.

Interleaving is the part that is easy to skip and expensive to get wrong. A
machine that gets busier halfway through a run would charge the whole drift to
whichever variant was measured second, and the result would look exactly like a
finding. It is also not hypothetical here: this host's bare process-creation
floor was measured at ~9.6 ms and, hours later the same day, at ~18–20 ms, with
`hostname.exe` alone costing 20.5 ms. **Absolute rows are only comparable within
one sitting; deltas survive.**

**Learn the noise floor, do not quote one.** Run the before binary against a copy
of itself, same run count, in the same sitting. On 2026-09-21 that control came
back at +0.35 ms on the git-miss path over 21 interleaved pairs, which retired a
long-standing "anything under 5 ms here is noise" rule of thumb inherited from a
by-hand probe — it had been costing real findings.

Both drivers **prove each variant does its work before timing anything**. A
probe that silently no-opped — a subcommand that does not exist, a payload
contract that moved — would otherwise be reported as a spectacular speed-up.

Both carry a `--self-test` / `-SelfTest` mode that exercises the guards without
needing either binary: the scratch-path refusal, the two cold-cache layouts, the
proof-of-work guard against a stub that renders nothing, and the environment
restore. `harness_measure_drivers_pass_their_own_self_test` in
`tests/equivalence.rs` runs it, because the previous drivers rotted into
unrunnable shape and nothing noticed for two months.

macOS and Linux pairs come from `.github/workflows/measure.yml`, one job per
platform so both halves of a pair share a runner; it builds both refs from one
checkout and records the runner label and image version beside the numbers.
Windows pairs are measured on the maintainer's machine, for the same reason
KTD9 keeps Windows capture off CI.

## Observables (R31)

| Component | Observable | How it is captured |
|---|---|---|
| `statusline` | rendered bytes | stdout |
| `subagent` | the exact bytes written to the tasks feed | the feed file; stdout must stay empty |
| `git-refresh` | the exact set of paths deleted | the isolated temp root, diffed before and after |
| `notify` | the command and arguments invoked | the PATH shims |

The `git-refresh` observable is the whole temp-root diff rather than a probe of
the two expected names. That is the point: a session id that escaped
sanitisation would delete something else, and only a full diff can show it.

## Shims

`shims/record.sh` is one body installed under every name that needs
intercepting — `afplay`, `paplay`, `terminal-notifier`, `notify-send`, and the
isolated `HOME`'s `notify.sh`. Five near-identical stubs would be the same
duplication problem in miniature, and a shim that drifted from its siblings
would silently change what a fixture means.

Each records one line per invocation into `$STATUSLINE_CAPTURE_FILE`, with
arguments backslash-escaped so a newline or tab inside one cannot forge a record
boundary. The status line backgrounds its notification spawn and `notify`
backgrounds its sound helper, so both drivers wait for the capture file to stop
growing rather than sleeping a fixed amount and hoping.

On Windows the spawn is intercepted twice: `powershell.cmd` on `PATH` (the
status line spawns `Start-Process -FilePath 'powershell'`, which resolves
through `PATH`) and `record.ps1` installed at the isolated `HOME`'s
`.claude\notify.ps1` (the script addresses its own notifier by path, which
`PATH` cannot intercept).

## Known gaps

**Windows `notify` delivery has no external observable — resolved at U7.** The
Windows *script* delivers entirely in-process (`System.Media.SoundPlayer`,
`SystemSounds`, the BurntToast module), so it spawns nothing a `PATH` shim can
see. Both `notify` cases keep `platforms: ["macos", "linux"]` and are skipped
with a printed reason on Windows rather than stored as empty fixtures, because
an empty golden file is worse than a missing one: everything downstream then
asserts against nothing.

U7 resolved the question rather than closing the gap. The port cannot call
BurntToast in-process, so it raises the toast by invoking `powershell.exe` at
its absolute path under `%SystemRoot%` — which gives Windows an external
observable for the first time, but one with no script counterpart to be
captured from. It is asserted against a literal in `tests/equivalence.rs`
instead, which is R20's mechanism. Windows sound stays in-process, through
`winmm`, and remains unobservable by design: spawning a player to make it
visible to the harness would be a real behaviour change made for the test's
convenience.

**One notify fixture records a bug rather than a target.**
`muted-sound-for-event` captures a sound helper being invoked with
`"sound": false` set, because both bash scripts read the flag as
`jq -r '.[$e].sound // true'` and jq's `//` yields its right-hand side when the
left is `false` as well as when it is null. R44 makes the flags gate delivery,
so the port mutes correctly and the fixture is kept as the record of what that
deliberately breaks. `tests/equivalence.rs` names it in `DIVERGENT_FIXTURES`.

**Reproducibility is per environment, not across machines.** The status line
renders a truncated working directory, so the length of the temp root reaches
the rendered bytes. Within one environment that length is constant and captures
are byte-identical; a different machine or a changed runner image can shift it.
Each platform's fixtures are captured in one place — CI for macOS and Linux, the
maintainer's machine for Windows — so this is a constraint to know about, not a
failure mode in normal use.
