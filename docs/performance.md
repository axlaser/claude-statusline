# Performance Practices

Standing performance doctrine for `claude-statusline`. CLAUDE.md's Performance rules
section binds every hot-path change to this file; this document is the detail behind it.

**This is a living document.** Update it whenever something new is established: a cost is
measured, a hypothesis is ruled out, a behavioral divergence is accepted, a design
decision is made, or the reference numbers in §6 change. A claim in this file should
always reflect the current binary.

**Reading the older entries.** Everything dated before 2026-07-27 describes the three
per-platform script trees this binary replaced. Those entries are kept because the
reasoning still teaches — several of them are why the port is shaped the way it is — but
they are history, not rules. Where one contradicts the current model, the current model
wins, and §7 marks the entries the migration invalidated.

## 1. The cost model — what is actually expensive here

Every refresh is still a **brand-new process**, but it is a native binary. That changes
which half of the old model survives.

1. **There is no interpreter floor.** The scripts paid ~124 ms for `powershell.exe` before
   line 1 executed, and the entire caching architecture existed to dodge it. The binary
   pays process creation and nothing else, which is why the whole warm tick now costs less
   than the floor used to. Startup is no longer the thing to optimise.
2. **What remains is real work.** In rough order on Windows: subprocess `git` (**~121 ms
   for the pair as a tick actually pays it**, on the maintainer's machine, bounded by the
   5s TTL), reading and scanning the transcript (linear in its size, ~6.8 ms/MB), and the
   syscalls around the six state files. None of these are startup artefacts; each is doing
   something. The ~74 ms this line quoted until 2026-09-21 was the raw `git` pair timed on
   its own; the tick pays roughly 30 ms more to spawn and drain two children, and the
   repository has grown since that figure was taken. **The order inverts on Linux**, where
   process creation and `git` are an order of magnitude cheaper but the transcript scan
   costs the same — see §6.
3. **Work scales with session length only in the transcript**, and that scan is skipped
   entirely when `(mtime, size)` are unchanged. The stored record carries the message count
   and idle flag precisely so an unchanged transcript needs no read at all. This is the
   single most important cost decision in the port — see §7.
4. **There is no output cache, and reintroducing one is not a performance idea.** It was
   deleted because the cost it hid was the interpreter's, and that cost is gone. The
   pre-cache-check region that the script rules obsessed over does not exist.
5. **Absolute savings differ by an order of magnitude across platforms, proportions do
   not.** bash's floor was ~10 ms where PowerShell's was ~124 ms, so the same proportional
   win is a very different number of milliseconds. A conclusion measured on Windows must
   not be generalised to Linux without measuring there — the incremental-parser
   conclusion was over-generalised exactly this way and cost a 4x regression (§7).
6. **Isolated micro-costs still do not sum.** Attribute a saving with an end-to-end
   before/after median, never a sum of isolated probes. Render-path micro-optimisation in
   particular measures as zero: the work is dominated by `git` and the transcript.

## 2. Hard rules (enforced in review)

- **No new subprocess on any per-tick path.** `git` is the only one. State the
  process-count delta in the PR description for any hot-path change.
- **Do not reintroduce an output cache**, or any cache whose justification is startup cost.
  The six surviving state files are data stores and render inputs, not speed
  optimisations; `docs/performance.md` and CLAUDE.md both name them explicitly so that a
  future reader can tell the difference.
- **Do not read the transcript when `(mtime, size)` are unchanged.** The trade is
  deliberate and measured: it gives up noticing a same-length, same-mtime rewrite, and
  `an_unchanged_transcript_is_not_rescanned` makes that cost executable rather than
  described.
- **Never read a file twice in one refresh when one pass can serve.**
- **Stored record format changes bump `RECORD_VERSION`.** A field that changes meaning
  without a bump is read as the old meaning by every installed binary.
- **Parse stored records strictly.** A field that decides whether work happens — `idle`
  decides whether the transcript is read at all — must reject the whole record on anything
  it does not exactly understand, rather than defaulting.
- **Rendered output is sacred:** a performance change must be byte-identical across the
  case table (§4). If output must change, it is not a performance change — split the PR.
- **Debug logging must never evaluate expensive arguments when disabled.** `debug::log`
  takes a closure for this reason; passing an eagerly-formatted `String` defeats it.
- **Platform-conditional code stays in its four areas** (notification delivery,
  ownership checks, process and stream handling, environment spelling), enforced by
  `platform_conditional_code_stays_in_its_areas`. A `cfg!(windows)` on a hot path is a
  design smell before it is a performance one.

## 3. Measurement methodology (how numbers must be produced)

The central lesson: **warm-loop micro-benchmarks are systematically wrong for this
codebase** — they understate first-call costs by up to 100×. Production spawns a fresh
process per tick, so measurements must too.

- **One fresh process per probe.** Never loop a probe inside a warm host to average it.
- **Median of ≥ 7 runs**; state machine, OS, and toolchain version alongside numbers.
- **Isolated numbers do not sum and must not be used for claims.** Removing one first-call
  cost shifts load onto the next operation. Only **end-to-end before/after** medians of the
  real binary justify a "saves X ms" claim.
- **Isolate the environment.** Every harness run points `TEMP`/`TMP` *and*
  `USERPROFILE`/`HOME` at a scratch directory (with a `.claude` subdir) and warms the
  learned-model map before sampling — otherwise the harness fights live sessions over a
  cache-key input and "hit" samples silently measure misses. When a benchmark claims to
  measure the hit path, verify hit-ness (cache-file mtime unchanged, or the debug log's
  HIT line); see `docs/solutions/workflow-issues/` and `docs/solutions/best-practices/`.
- **Host class is part of the number.** A hosted-runner figure must never later be
  held against a bare-metal baseline, so every row in §6 carries the runner label and image
  version it came from. Rows without one are unusable as baselines.
- **Measure both the warm and the cold state.** A warm-only pair flatters whichever variant
  caches more; the script's output cache was keyed on a 5-second bucket, so a whole run
  finished inside one and most of its probes rendered nothing at all. `measure-pair.sh` /
  `measure-pair.ps1` take `--mode` / `-Mode`, which selects between `warm`, `git-miss` and
  `cold` rather than offering one switch.
- **Guard every probe with a proof of work.** A probe that silently no-opped reads as a
  spectacular speed-up: that guard is what caught a measurement of an unimplemented
  subcommand, and on 2026-09-21 it caught a `cmd.exe` quoting bug that made a 188 ms tick
  read as 14.6 ms. Assert the box rendered (`┏` and the model name), or that the handler
  wrote its file, before believing any median.
- **`tests/harness/measure-pair.sh` / `measure-pair.ps1` implement every rule above** and
  are the only measurement drivers. They pair two builds of *this crate*, stage a real
  `.git` from `states.json`, spawn the binary directly, and take the proof-of-work
  assertion as a parameter. Their `--self-test` mode is run by `cargo test`, because their
  predecessors rotted into unrunnable shape and nothing noticed for two months.
- **The host's own process floor drifts, so only deltas survive.** On 2026-09-21 the
  maintainer's machine measured a 9.6 ms bare-process floor in the morning and ~18–20 ms
  the same evening — `hostname.exe`, which does nothing, cost 20.5 ms in the second
  sitting. Absolute rows are comparable only within one sitting; an interleaved pair is
  immune, which is the whole reason interleaving is mandatory rather than advised.
- **Establish the noise floor in the same sitting, do not quote one.** Run the before
  binary against a copy of itself through the same driver, same run count. On 2026-09-21
  that control came back at +0.35 ms on the git-miss path and ±0.02 ms on the cold path
  over 21 interleaved pairs, which retired a long-standing "anything under 5 ms here is
  noise" rule of thumb that had been inherited from a by-hand probe and was costing real
  findings.
- **Give each variant its own state root when a stored format changes.** The drivers do
  this by default now, per-child rather than per-driver. Two builds sharing one `TEMP`
  reject each other's records the moment a field list or a `RECORD_VERSION` differs, so
  every probe on both sides measures a miss — it read as −41.94 ms on a *warm* tick once,
  a path where nothing had moved. See "The agent transcript is scanned backwards" in §7.
- **An identical-binary control measures the machine, not the method.** Two builds that
  differ by any code at all differ in code layout too, and under whole-crate LTO that was
  worth **+3.4 ms on the cold tick** for a change whose mechanism could not account for
  0.02 ms — see "Caching the process owner" in §7. Below about 1 ms, a paired median is
  necessary and not sufficient: price the mechanism as well, or the layout lottery will
  hand you a finding in whichever direction it feels like.
- **Bash on Git Bash: process counts are portable, milliseconds are not** (emulated fork is
  ~20–50× a real one). This applied to the scripts; it still applies to anything measured
  through an MSYS shell.
- Do not re-test the ruled-out hypotheses without new evidence. Still live: ACL-check
  removal (load-bearing), render-path micro-optimisation (measures as zero — the cost is
  `git` and the transcript). Retired with the scripts, kept only as history: temp-glob
  scaling, pre-compiled `[regex]`, `pwsh` 7 startup, script parse cost.

## 4. Equivalence verification (required for hot-path refactors)

`cargo test` is now the mechanism: `rendered_output_matches_the_captured_fixtures` replays
every case in the table below against stored golden bytes, on every published target. The
matrix is what that table exists to cover, and it is still the checklist for a case anyone
adds:

- **Git states** (scratch repo): fresh/unborn HEAD, unborn-with-staged-index, clean,
  untracked-only, dirty, stashes present, ahead-of-upstream, detached HEAD, no upstream,
  collapsed untracked dir, stash-cleared. The table runs on every published target, so
  "matches recorded behaviour" is now one statement rather than three — the per-platform
  qualifier the scripts needed is what resolving those divergences retired.
- **Payload states**: full payload, minimal payload (missing optional fields), empty stdin,
  malformed JSON (all must exit 0, no stderr), plus adversarial variants: Windows-illegal
  path characters, overlong paths, decoy field names inside string values, astral-plane
  characters in string fields.
- **Transcript states**: absent, empty, large (≥ 4 MB), unchanged since the last tick (the
  record is re-displayed and the file is never opened), torn trailing line re-read next
  tick, appended-after-sampling bytes left for the next scan, and a damaged record read as
  no record at all. Multibyte fixtures must include a 4-byte (astral-plane) character, not
  just CJK.
- **Subagent feed states**: fresh feed, stale feed, absent feed falling back to transcript
  parsing, and the done-linger window both inside and past expiry — on **both** tiers.
  That qualifier is new and was a real gap: the linger was covered on the feed tier only,
  and the fallback tier turned out not to implement it at all (`a_finished_fallback_agent_lingers_then_disappears`
  now pins it, alongside `an_unchanged_agent_transcript_is_not_rescanned` for the read skip
  that shares its record).

Known parsing traps this matrix exists to catch: porcelain v2 emits `# branch.oid (initial)`
on unborn HEAD, omits `# branch.ab` when no upstream, and omits `# stash` at zero; a branch
literally named `(detached)` collides with the detached sentinel.

**Byte-diffing alone cannot see a stale-input regression.** Any change touching a guarded
read, the tasks-feed freshness window, or the done-linger stamp must also assert the
observed fresh/stale outcome, not only the rendered bytes — see
`docs/solutions/best-practices/byte-diff-cannot-see-cache-hit-regressions.md`.

**Accepted divergences** (deliberate, documented — add new entries here when one is accepted):

- *2026-07-26:* a branch literally *named* `(detached)` is indistinguishable from real
  detached HEAD in porcelain v2, so it renders as the short commit hash instead of the name.
  Disambiguating would cost a subprocess on a pathological case; the sentinel collision is
  inherent to the v2 format.
- *2026-07-26:* under a UTF-8 locale, gawk's old greedy-regex token extraction silently
  zeroed the token counts of assistant lines containing an astral-plane character (emoji).
  The `LC_ALL=C` pin fixes the extraction, so token totals on emoji-bearing transcripts are
  higher — and correct — compared to earlier releases. Verified on GNU gawk 5.0; BSD awk
  unverified either way (no native macOS test hardware). See
  `docs/solutions/logic-errors/gawk-utf8-locale-zeroes-astral-plane-extraction.md`.
- *2026-07-27:* the path row's home-prefix match differs from `windows/statusline.ps1:287` on two
  spellings of a path, both resolved to the Rust port's behaviour. A `cwd` written with forward
  slashes under `$HOME` collapses to `~` (the script normalised the `cwd`'s separators but never
  `$USERPROFILE`'s own, so it fell through to `.../parent/leaf`), and the comparison is
  case-sensitive, so `c:\users\me\src` no longer collapses (the script used `OrdinalIgnoreCase`).
  Keeping the second would require a platform-conditional comparison, which the confinement rule does not allow for
  path formatting. Neither shape is reachable in practice — Claude Code supplies native backslash
  paths in canonical casing on Windows, confirmed against a live session — which is why four
  rounds of fixture captures never produced either. Asserted as literals by
  `resolved_cwd_divergences_keep_the_ports_behaviour`.
- *2026-08-02:* a user who upgrades mid-session abandons that session's flat state files,
  because state moved into `<temp>/claude-statusline-<owner>/` and nothing migrates or reads
  the old location. Three effects, all one tick and all self-correcting: an in-flight
  subagent task loses its done-linger stamp, so a finished task disappears without its
  window and the fallback tier re-reads each agent transcript once; the abandoned token
  record reads as absent, so the next tick's deltas compute against zero and the four token
  buckets render their totals as one large `(+N)`; and the abandoned notification latch
  reads as never-notified, so a session already over threshold re-fires its context or rate
  alert once. Accepted rather than migrated: a sweep would add a delete path over
  predictable names in a shared directory, which is the surface the guards exist for.
- *2026-09-21:* a user who upgrades mid-session abandons that session's token record and
  its per-agent records, because both formats gained fields — the token record is `v5` and
  the per-agent record is nine fields — and the parsers reject any record whose count or
  version differs rather than guessing at a missing one. Two effects, both one tick and both
  self-correcting, and both already on this list from the 2026-08-02 change: the token
  record reads as absent, so the next tick's deltas compute against zero and the four
  buckets render their totals as one large `(+N)`; and the fallback tier re-reads each agent
  transcript once. Rejecting is the safe direction — the alternative is reading a `v4`
  record's thirteen fields as a `v5` record's eighteen, which is a wrong number rather than
  a missing one.
- *2026-08-03:* two number formats gain a tier the scripts never had, both for the same
  reason — the scripts predate the magnitudes. `format_tokens` gains `B`, so a cumulative
  count past a billion renders `1.23B` where it used to render `1000.0M`; the ladder exists
  precisely so no unit shows a four-digit mantissa, and `M` was silently violating it at the
  top. `B` is the one tier with two decimals: a single digit there is a 100M-token bucket,
  coarse enough to sit unchanged across many refreshes, where the same digit buys 100-token
  resolution at `K`. `K` and `M` keep one decimal and stay byte-identical to the captures.
  `format_cost` gains grouping above `$999.9999`, so a four-figure session renders
  `$1,234.56` — grouped, and at two decimals rather than four, because sub-cent precision
  that carries information at `$0.0834` is noise beside a thousand dollars. Both switches are
  above every value the case table exercises, so no capture changed and
  `rendered_output_matches_the_captured_fixtures` passed untouched — which also means the
  fixtures do **not** cover these tiers. `token_and_window_labels_truncate_rather_than_round`
  and `cost_color_turns_over_fifty_cents_exactly_at_the_boundary` carry the coverage instead,
  each pinning the pair either side of its boundary. The cost row grows two columns at four
  figures (`$1.2345` -> `$1,234.56`). Nothing clips: the box sizes to its widest row via
  `max_inner`, so it widens by up to two columns and only if the cost row is already the
  longest one.
- *2026-07-28:* `"sound": false` and `"visual": false` in `notify-config.json` now genuinely
  mute an event on macOS and Linux. The bash handlers read the flag with
  `jq -r '.[$e].sound // true'`, and jq's `//` yields its right-hand side when the left is
  `false` as well as when it is null — so `false // true` was `true` and muting never worked
  on those two platforms. Windows honoured it. The port gates delivery on the flags, so
  intended behaviour wins over reproducing the bug, and an existing config that carried a
  `false` the user believed was already in effect starts behaving as written. See
  `src/config.rs:83`.

- *2026-09-20:* the model row absorbs the context row, and gains two segments the scripts
  never had. Three changes, one row, all deliberate and all visible in every capture:
  the context bar, its percentage and `used/window` now **lead** the row, ahead of the
  model name, and the separate `context` row is gone; a display name's trailing
  parenthetical is dropped, so `Opus 5 (1M context)` renders `Opus 5`; and the row ends
  with the running Claude Code version, `CC v2.1.278`, yellow and flagging a newer
  one (`CC v2.1.278 ↑`) when there is one. The parenthetical is stripped by shape rather
  than against a list of known variants, so a model that has not shipped shortens on the
  same rule — and nothing is lost, because the window label two segments to its left reads
  the window the payload reports rather than a name that claims one. The box widens by the
  bar's 30 columns, to 106 where this row is the longest; nothing clips, because
  `assemble` sizes the frame to its widest row.
  **The captures could not be recaptured** — the scripts are deleted and could not have
  produced this layout anyway — so all 49 goldens carrying both rows were rewritten
  instead: each was decomposed back into its `(section, label, content)` rows, the merge
  applied, and `assemble` re-run, so every platform keeps exactly the content it captured
  and only the frame moves. That rewrite is not taken on trust:
  `rendered_output_matches_the_captured_fixtures` re-renders each case from the port and
  compares bytes, which is what makes a wrong transformation a test failure rather than a
  new golden. `payload-empty` (linux, macos) and `payload-malformed` render the bad-JSON
  notice and have no box, so they were left untouched.
  The **version segment is not covered by the golden fixtures**: no pinned payload a case
  uses carries a `version`, exactly as the `B` and grouping tiers above sit past every
  value the table exercises. Four named tests carry that coverage instead —
  `the_model_row_opens_on_the_bar_and_closes_on_the_version` for the row,
  `a_display_names_trailing_parenthetical_is_dropped_whatever_it_says` for the name rule,
  `the_version_segment_resolves_its_changelog_through_the_real_roots` for the wiring
  through `cmd_statusline::run` (the private `Roots::changelog_path` join is reachable no
  other way, and a wrong join degrades to the same quiet row as "nothing to report"), and
  `a_hostile_changelog_target_is_refused_not_followed` for the guard, which every other
  guarded read in the crate already has.

### Accepted divergence: `Bash` no longer invalidates the git cache — 2026-09-21

The PostToolUse hook fired for `Edit`, `Write`, `MultiEdit`, `Bash` and `NotebookEdit`, and
deleted the git cache for each. Measured across 85 recent sessions and 19.8 h of active
time, **`Bash` was 2,109 of the 2,849 invalidating calls — 74% of them, and just over half
of every tool call made.** Every `ls`, every `grep`, every `cargo test` cost the next tick a
full git miss.

It is dropped from both the handler's list and the matcher an install writes. What this
changes, precisely:

- A shell command that touches `.git/index` — `git commit`, `git add`, `git checkout` — is
  **still seen on the very next tick**, because the cache is keyed on that file's mtime and
  the key does not care what moved it.
- A shell command that alters only untracked or worktree state — `touch new-file`,
  `sed -i` on a tracked file — is seen **within the 5 s TTL** rather than immediately. That
  is the divergence, and it is the same staleness bound the TTL already provides for
  `git fetch`, which nothing observes either.

Rejected alternatives: marking the cache stale instead of deleting it saves nothing,
because deletion already coalesces; and reading the payload's `tool_input.command` to tell
`git commit` from `ls` adds parsing to a path that runs after every tool call, and replaces
one guess with another.

An install made before this still carries the old matcher in its `settings.json`, so the
hook keeps running after every shell call and finds nothing to do — which is what it costs
today. The saving lands without a reinstall, because it comes from the cache not being
deleted rather than from the hook not running.

## 5. PR checklist for hot-path changes

- [ ] Subprocess delta stated. `git` is the only one that should appear.
- [ ] End-to-end before/after medians measured per §3 (fresh process, ≥ 7 runs), warm
  **and** cold, with the host class recorded. No harness script does this for you any more
  — see the note at the end of §6 — so measure the subcommand directly, and guard the
  probe: a run that silently no-ops reads as a spectacular speed-up.
- [ ] If the change can touch the git block, a row measured **with a real `.git`**. A
  scratch working directory with none never spawns `git`, and that is 121 ms of a Windows
  tick going unmeasured (§7).
- [ ] `cargo test` green, including the case table — byte-identical rendered output.
- [ ] If the change touches a guarded read, the tasks-feed freshness window, or the
  done-linger stamp: the observed fresh/stale outcome asserted, not just the bytes (§4).
- [ ] `RECORD_VERSION` bumped if any stored record format changed.
- [ ] No debug-log call site evaluates expensive arguments when logging is off.
- [ ] No new `cfg!(windows)` outside the four areas platform code is confined to.
- [ ] §6 reference numbers updated if the change moves them.

## 6. Reference numbers (update when a hot-path change moves them)

Every row carries its host class. A hosted-runner number and a bare-metal number are
not comparable, and a row that does not say which it is cannot serve as a baseline.

### Historical — the script trees (2026-07-26)

Kept for provenance and for reading §7's older entries. **These are not baselines for the
binary**; they describe software that no longer exists in this repository. Measured on the
maintainer's machine (Windows 11, Windows PowerShell 5.1, *maintainer machine* class; bash
numbers are MSYS shape-only and were never a milliseconds claim).

| Path | Cost |
|---|---|
| Windows cache hit | ~326–334 ms |
| Windows miss, 13.4 MB transcript, unchanged content | 542 ms |
| Windows miss, 13.4 MB transcript, +100-line growth tick | 568 ms |
| Windows cold full rescan (once per session) | ~1470 ms |
| Windows miss, git cache expired | 614 ms |
| bash hit / miss (MSYS shape-only) | 249 / 1124 ms |
| bash external processes, hit / miss | 5 / 14 |
| bash forks, six-row subagent render | 39 |
| Interpreter floor (`powershell.exe -NoProfile`, empty script) | ~124 ms |

### Current — the binary (2026-09-21, after the per-tick cost work)

**These are the live reference.** Read them, not the 2026-07-27 pairs below, as the baseline
a change must not regress. Release build with LTO, rustc 1.97.1, git 2.48.1. Median of 21
fresh processes per probe through `tests/harness/measure-pair.*`, isolated `HOME`/`TEMP`,
one state root per variant, `STATUSLINE_DEBUG` cleared, 8 MB generated transcript, a real
`.git` in the `dirty` state, proof-of-work guard on every row.

| Tick shape | Windows 11 26200 (*maintainer machine*) | `rust:latest` container on WSL2, same box (*not bare metal, not a runner*) |
|---|---|---|
| Floor — malformed or empty input | 18.2 ms | 0.74 ms |
| Warm — nothing changed since the last tick | 19.4 ms | 1.30 ms |
| **After a new message** — the transcript grew | **25.0 ms** | **1.49 ms** |
| **Git TTL expired** — the tick after a >5 s pause | **61.1 ms** | **2.99 ms** |
| Cold — a full transcript read *and* a git miss | 116.4 ms | 59.7 ms |
| `subagent` | 19.5 ms | 0.86 ms |
| `git-refresh` | 18.3 ms | 0.75 ms |
| `self-check` | 17.8 ms | 0.74 ms |

**Read the Windows column against its own floor, not against the previous table.** A bare
`hostname.exe` — a program that prints one line — measured **22.5 ms** in this same sitting,
and `/bin/true` measured 0.56 ms in the container. The rows above were taken hours after the
morning's, on the same machine, where the same bare-process floor was 9.6 ms. Nothing in the
binary accounts for that; process creation on this host simply costs about twice as much in
the evening as it did in the morning. **Every absolute number here is only comparable within
one sitting**, which is why §3 mandates interleaving and why every claim in §7 is a paired
delta rather than a difference of two tables.

Against the morning's measurements of the same work, the two ticks the plan set out to cut:

| Tick shape | Before (2026-09-21, `3f5f919`) | After | Windows delta |
|---|---|---|---|
| Git TTL expired | 123.1 ms | 61.1 ms | **−50%** |
| After a new message, 8 MB | 76.1 ms | 25.0 ms | **−67%** |

Both measured as interleaved pairs in a single sitting — see §7 for the per-change
attribution, which is where those percentages come from. The same two on Linux, also
interleaved: the git-miss tick 22.7 → 3.0 ms, and the post-message tick 57.4 → 1.6 ms, where
removing the poll interval matters more than anywhere because the children are fast enough
that the *wait* dominated them.

**"Cold" is no longer the common case, and that is the point.** The transcript scan used to
run on every new message, because the skip held only while `(mtime, size)` were unchanged.
It now resumes from a stored offset, so the ordinary post-message tick is the 25.0 / 1.5 ms
row rather than the 116 / 60 ms one. A full read still happens: at session start, after a
rewrite, and once every 64 resumes or 4 MB of growth, which is the ceiling that keeps a
wrong offset from living forever. Transcript scan, cold, no `.git`, Windows — linear at
**~6.8 ms/MB**:

| Transcript | 0.5 MB | 1 MB | 2 MB | 4 MB | 8 MB | 16 MB | 32 MB |
|---|---|---|---|---|---|---|---|
| Scan | 5.1 ms | 9.1 ms | 16.3 ms | 30.2 ms | 56.9 ms | 110.9 ms | 219.1 ms |

Linux scans the same 8 MB in 56 ms, within noise of Windows' 57 ms: the scan is CPU work
and does not care which OS runs it, while process creation and `git` very much do. **That
inverts which cost dominates** — `git` on Windows, the transcript on Linux — so §1's
ordering describes Windows only, and it is why the transcript work was the half of this
that moved Linux.

**The measurement drivers now run.** `measure-pair.sh` and `measure-pair.ps1` pair two
builds of this crate — interleaved, one fresh process per probe, isolated profile and
temp, a real `.git` staged from `states.json`, and a parameterised proof of work. The
script-vs-binary drivers they replaced (`measure.sh`, `measure.ps1`) are deleted: they
paired against the script trees removed at `1f5acf2` and aborted on a missing-file check.
The rows above were taken by hand before those drivers existed; re-taking any of them
means re-taking all of them in one sitting, because the floor drifts (§3).

### Paired medians — script vs. binary (2026-07-27)

The Rust migration replaces each runtime script with a subcommand of one binary.
Both halves of the pair are measured end to end, one fresh
process per probe, on a single host with the runs interleaved, and recorded
before that component's scripts are deleted — after deletion the script half can
never be measured again.

Produced by the since-deleted `tests/harness/measure.sh` and `measure.ps1`: 11 interleaved pairs
per host, payload `tests/harness/payloads/tasks-feed.json`, isolated
`HOME`/`TEMP`, `STATUSLINE_DEBUG` cleared so neither variant is charged for a log
append the other skips.

**Host class is part of the number.** A hosted-runner figure must never later be
held against a bare-metal baseline, so every row carries the runner label
and image version it came from.

| Component | Host | Host class | Script | Binary | Delta |
|---|---|---|---|---|---|
| `subagent` | Windows 11 26200, Windows PowerShell 5.1.26100 | maintainer machine | 264.0 ms | 21.6 ms | −91.8% |
| `subagent` | `macos-15`, image `macos15 20260715.0234.1`, bash 5.3, jq 1.8.2 | hosted runner | 25.1 ms | 3.0 ms | −88.2% |
| `subagent` | `ubuntu-24.04`, image `ubuntu24 20260720.247.2`, bash 5.2, jq 1.7 | hosted runner | 10.4 ms | 1.1 ms | −89.1% |

Reading them:

- The Windows script figure independently corroborates §7's 2026-07-26
  measurement of the same handler (~266 ms), taken by a different harness.
- The Windows binary sits **below the ~124 ms interpreter floor**, which is the
  whole point of the migration's cost model: that floor was never the script's
  cost to avoid, it was the interpreter's cost to exist. There is no interpreter.
- Bash's own floor is ~10 ms, not ~124 ms, so the Unix saving is an order of
  magnitude smaller in absolute terms while being the same proportion. The three
  platforms were never paying the same price for the same handler.

Provenance: the binary half was built from a throwaway commit
(`d7a963ccbc975fd36b11091afc197de3173bd3d3`) that is deliberately not on the
branch. A component's port, its fixtures and its script deletion all land in
one commit, and these medians have to be recorded *before* that commit —
so the tree that was measured could not itself be a branch commit. The measured
content is what the subagent port commit lands.

#### `statusline` (2026-07-27)

Payload `tests/harness/payloads/full.json`, with the transcript and the learned
model-window map staged into the isolated `HOME` the payload points at, so both
variants parse a real session rather than an empty one.

This pair has to cover the large-transcript state. The transcript is
**generated to a target size** rather than pointed at a real session file: a
machine-local transcript is not reproducible on CI, on another machine, or next
month, and a number nobody else can reproduce is an anecdote rather than
evidence. Repeating one pinned record keeps the token totals a pure function of
the size.

All rows use a generated 8 MB transcript. Two states, because one of them alone
is misleading in each direction.

**Warm** — nothing changed since the last tick. The script's output cache (keyed
on a 5-second bucket) and its incremental parser are both working; the binary
skips its rescan on an unchanged `(mtime, size)`. This is the common tick.

| Host | Host class | Runs | Script | Binary | Delta |
|---|---|---|---|---|---|
| Windows 11 26200, Windows PowerShell 5.1.26100 | maintainer machine | 7 | 311.8 ms | 22.1 ms | **−92.9%** |
| `macos-15`, image `macos15 20260715.0234.1` | hosted runner | 11 | 70.5 ms | 5.0 ms | **−92.9%** |
| `ubuntu-24.04`, image `ubuntu24 20260720.247.2` | hosted runner | 11 | 13.2 ms | 1.3 ms | **−90.3%** |

**Cold** — every per-tick cache cleared before each probe, so both variants do
the whole job. This is the tick after the transcript grows.

| Host | Host class | Runs | Script | Binary | Delta |
|---|---|---|---|---|---|
| Windows 11 26200, Windows PowerShell 5.1.26100 | maintainer machine | 7 | 1527.1 ms | 72.4 ms | **−95.3%** |
| `macos-15`, image `macos15 20260715.0234.1` | hosted runner | 11 | 1282.0 ms | 74.4 ms | **−94.2%** |
| `ubuntu-24.04`, image `ubuntu24 20260720.247.2` | hosted runner | 11 | 299.8 ms | 62.1 ms | **−79.3%** |

The binary is faster on every platform in both states, by 79% to 95%. Three
things are worth keeping from how that number was arrived at:

- **The first version of this table had the binary 4× *slower* on Linux.** The
  port scanned the transcript unconditionally, which cost ~50 ms on 8 MB —
  invisible under PowerShell's ~124 ms interpreter floor, four times bash's
  entire tick. The porting decision had argued the scripts' incremental machinery "buys nothing
  once the interpreter is gone", measured against a *growing* transcript. That
  was the script's worst case, and the conclusion was over-generalised to
  platforms whose floor is an order of magnitude lower. The port now skips the
  rescan when `(mtime, size)` are unchanged, and Linux went from +307% to −90%.
- **A warm-only pair is not a comparison.** The script's output cache serves
  almost every probe of a short run, so the original measurement was the
  script's best case against the binary's only case — the state where it renders
  nothing at all read as 13 ms. The cold rows are the honest half, and they are
  where the script costs 0.3–1.5 seconds.
- **The two floors are still the whole story on Windows.** ~124 ms of the
  script's warm 311 ms is PowerShell starting. The binary's entire warm tick is
  22 ms, well below the floor the script cannot get under by any means.

Method note: `--cold-cache` / `-ColdCache` clears both layouts from the isolated
temp root before each probe, outside the timed region — the flat `statusline-*`
the scripts wrote, and the `claude-statusline-<owner>/` directory the binary
writes since 2026-08-02. Clearing only the flat glob would leave the binary's
caches warm while the run still labelled itself cold, which reads as a
flattering median rather than as an error. The transcript
is generated to size rather than pointed at a real session file, so these
numbers are reproducible on any runner instead of tied to one machine's files.

Provenance note: unlike the `subagent` pair above, this one needed no throwaway
commit. The port landed one commit before the scripts were deleted, so a commit
carrying both the Rust statusline and the three scripts exists on the branch and
could be measured directly. That collision only bites when a single
commit has to do both.

## 7. Decision record (settled design questions — do not re-litigate without new evidence)

### Rendered-value cache key — no change (2026-07-26)

The design resolved cleanly (two-tier key: the raw-level key as tier 1, a rendered-value
key checked after display inputs are computed as tier 2; tier 2 needs no time bucket
because visible countdown labels self-invalidate; threshold notifications are safe on the
miss path because rendered percentages and configured thresholds are both integers, so
every crossing changes the key). It fails on the measured benefit bar:

- A tier-2 hit still pays everything before the render — measured 501 ms of a 538.6 ms
  forced-miss tick — because those stages produce the tier-2 key's inputs.
- The render tail (box assembly, notifications, cache write, emit) is **37.6 ms**: the
  per-hit ceiling, ~7% of tick cost, against a permanent second key tier in three
  scripts whose completeness failure mode is a silently stale display.
- Root cause: the idea was priced against pre-optimization misses (0.7–1.4 s). The git
  consolidation and incremental transcript parse moved that work behind their own caches,
  leaving the rendered-value key nothing expensive to skip.

Reopen condition: a future change that makes the render tail expensive again, or a hard
requirement to eliminate the idle 5 s re-render entirely — which additionally needs a
replacement idle recompute driver for ref-only git changes (`git fetch` touches
`FETCH_HEAD`/`packed-refs`, not `.git/index`, so no existing key probe observes it).

### Cheaper subagent tee — no change (2026-07-26)

Measured (fresh-process medians, 11+ samples): current handler ~266 ms; interpreter floor
~142 ms; raw-tee + .NET-only I/O prototype ~172 ms (−35.5%, the only variant clearing the
pre-registered ≥30% bar); cmdlet-swap-only ~249 ms (−6.9%, byte-identical output).

The bar-clearing variant does not ship, because it fails the bar's contract half:

- **Malformed-tick isolation is lost.** Today a broken payload throws in the JSON parse
  and writes nothing — the last good feed survives. A raw tee writes the garbage over it,
  silently dropping the feed tier for that session until the next good tick.
- **Raw retention feeds `tokenSamples` (an accumulating per-task array) and other
  unfiltered fields into the statusline's output-cache key** (feed content is a key
  input). Unverified tick-to-tick order/field stability risks re-introducing the
  every-tick-miss behaviour the cache work exists to eliminate.

Reopen conditions: a live multi-tick capture confirming raw field/order stability and no
idle-tick key churn, plus a structural guard (trimmed payload starts `{` and ends `}`)
re-measured to confirm the mechanism still clears 30%. The −6.9% cmdlet swap remains a
zero-risk fallback but does not meet the bar on its own.

### Git access — subprocess, not a pure-Rust library (2026-07-27)

The Rust port keeps invoking `git`. The alternative considered was `gix`, which
satisfies the same pure-Rust constraint and benchmarks faster than git itself.

Measured cost of the calls being kept — fresh-process medians, 15 runs, this
repository, git 2.48.1 on the maintainer's Windows machine. Indicative, not an
paired medians:

| Call | Median |
|---|---|
| `status --porcelain=v2 --branch --show-stash` | 36.5 ms |
| `diff --shortstat HEAD` | 37.4 ms |
| pair | 73.9 ms |

**Re-measured 2026-09-21, same machine, same argv:** 45.2 ms and 47.1 ms, pair 92.3 ms —
the repository has grown. More importantly these are the children timed *alone*; a tick
that spawns and drains both pays **~121 ms**, and that is the figure §1 and §6 now carry.
The decision below is unaffected — it was never made on cost — but do not reuse 73.9 ms as
a budget.

**Process-count delta: none for the git block itself** — 2 subprocesses per
miss, 3 on detached HEAD, 0 on a cache hit, exactly as §6 records for the
scripts. What the port removes is the interpreter around them: the bash tick
falls from 5 execs on a hit and 14 on a miss to 0 and 2–3.

The decision was not made on cost. An in-process reading means reimplementing
git's *configuration* surface — `core.autocrlf` normalisation ahead of
`--shortstat`, `.gitattributes` binary and textconv rules, rename detection,
untracked-directory collapsing. Fixtures are captured on default-config scratch
repos, so divergence in that surface passes CI and then renders a plausible
wrong number on a user's machine, where the exit-0 contract guarantees it never
announces itself. Subprocess `git` cannot diverge from `git` by construction,
and this repo treats performance as tracked rather than gated.

Reopen condition: a post-parity evaluation with the git-state fixtures in hand,
run against a configuration matrix the harness does not have today — `autocrlf`
on and off, a `.gitattributes`-marked binary file, `diff.renames` disabled.
Porcelain parsing is kept as a pure function over text so that evaluation is a
diff rather than a rewrite.

### Incremental transcript parser — deleted in the Rust port (2026-07-27)

The scripts' incremental parse (stored byte offset, head checksum over
`min(4096, size)` bytes, truncation and rewrite detection) exists because a full
rescan of a large transcript costs ~1470 ms in PowerShell. The Rust port does not
inherit that cost, so a full parse was measured before porting any of it.

Fresh-process medians, maintainer's machine (Windows 11 26200), release build,
15 runs, largest transcript available locally — 8,406,985 bytes. The plan's
13.4 MB reference transcript no longer exists on this machine, so the row is
smaller than the §6 script rows it is read against:

| Probe | Median |
|---|---|
| Process only (632-byte transcript) | 6.5 ms |
| Full parse, 8.4 MB | 46.9 ms |
| Full parse, 8.4 MB, before search optimization | 98.8 ms |

Against §6's script rows for 13.4 MB — 568 ms for an incremental growth tick and
~1470 ms for a cold full rescan — a full Rust parse is roughly an order of
magnitude cheaper than the incremental path it would be replacing, before
adjusting for the smaller file. The offset, checksum and resume machinery are
therefore deleted rather than ported.

Two things worth keeping from the measurement:

- **Substring search dominated the scan.** Replacing `windows(n).position(..)`
  with a first-byte scan followed by a compare halved the figure, 98.8 ms to
  46.9 ms, with byte-identical results. The first-byte scan vectorizes; the
  window compare does not. This is why the crate needs no search dependency.
- **The token record is not deleted with the parser.** The per-bucket `(+N)`
  deltas are this tick's totals minus the previous tick's, and an unchanged
  transcript re-displays the stored deltas rather than recomputing them to zero.
  That makes the record a render input, not a performance cache. What
  goes is the offset and checksum; what stays is mtime, size, four totals and
  four deltas.

Reopen condition: a transcript large enough that a full parse becomes visible
next to the ~124 ms interpreter floor the migration removes — on this hardware
that is somewhere north of 25 MB.

**Reopened and reversed on 2026-09-21** — see "The transcript scan resumes from a stored
offset" below. Not because the 25 MB threshold was crossed, which it was not, but because
two things this entry assumed stopped being true: the interpreter floor it measured against
no longer exists, and the scan turned out to run on every new message rather than once per
session. Read that entry, not this one, for what ships.

### The transcript scan resumes from a stored offset — 2026-09-21

Reverses "Incremental transcript parser — deleted in the Rust port" above. That entry set a
reopen condition of "a transcript large enough that a full parse becomes visible next to
the ~124 ms interpreter floor — north of 25 MB", and **that threshold was never crossed**:
at 8 MB the scan is ~57 ms. The evidence changed instead, in two ways the entry could not
have known:

- **The floor it measured against no longer exists.** The ~124 ms PowerShell startup that
  made a 57 ms scan invisible went with the scripts. Against an 11 ms warm tick the same
  scan is the tick.
- **It runs on every new message, not once per session.** The skip holds only while
  `(mtime, size)` are unchanged, and every message appends. The entry priced the cost's
  size correctly and its *frequency* not at all.

On Linux the scan is the single largest per-tick cost at any size — 56 ms at 8 MB, within
noise of Windows' 57, because it is CPU work — so this is the half of the plan that moves
the platform where `git` is cheap.

| Tick shape, 8 MB transcript | Before | After | Delta |
|---|---|---|---|
| **The tick after a new message** | 76.05 ms | 25.14 ms | **−50.91 ms (−66.9%)** |
| Warm | 19.23 ms | 19.16 ms | −0.06 ms |
| Git TTL expired | 59.85 ms | 59.61 ms | −0.24 ms |

And on Linux, where the scan is the whole tick, measured the same way in a `rust:latest`
container on WSL2 against a build with the resume gate forced off:

| Tick shape, 8 MB transcript | Before | After | Delta |
|---|---|---|---|
| **The tick after a new message** | 57.38 ms | 1.60 ms | **−55.78 ms (−97.2%)** |
| Warm | 1.36 ms | 1.35 ms | −0.01 ms |

21 interleaved pairs on each platform, against a ±0.10 ms identical-binary control. The
`append` tick shape was added to the drivers for this: every existing mode either changed
nothing or cleared the token record, and clearing the record measures the full read the
resume exists to avoid. The two ticks were being conflated.

**Three tiers where there were two.** The `(mtime, size)` skip is tested first and is
unchanged. The resume sits beneath it. Every gate that fails falls through to the full read
— a correct-but-slower recompute — never to a wrong answer, and each carries a named reason
into the debug log.

**What the design turns on, in the order it bites:**

- **Identity is size-plus-content, never inode.** The positional-read and file-index APIs
  are platform-split in std (the Windows half is still nightly), so an inode identity would
  force conditional code outside the four permitted areas; the log-shipping field moved away
  from inode identity independently, for reliability.
- **Resume only when the file *strictly* grew** — `size <= stored` falls through, not
  `size <`. A same-size rewrite has an empty tail, so a resume would re-display stale totals
  *and* rewrite the record with the new mtime, after which the cheap skip hits forever. That
  is the one case in this change that would convert a self-healing divergence into a
  permanent one.
- **A boundary anchor as well as a head checksum.** A head checksum cannot see a rewrite
  that preserves the first 4 KB and changes the middle, and unlike today's divergence that
  error never self-heals, because every later tick resumes from the poisoned record. The
  byte before the offset must be a newline; it costs one read at an offset already sought.
- **Exactly 4096 bytes of head, always, with a 64 KB floor below which nothing resumes.** A
  checksum over `min(4096, size)` covers a different span as a young file grows past 4 KB,
  so a naive comparison mismatches on a pure append and rescans every tick until the file
  passes 4 KB — a silent hit-rate bug of the class byte-diffing cannot see. Storing the
  covered length would fix it; the floor *deletes* it, and what it gives up is avoiding a
  full scan of a file under 4 KB, which at ~6.8 ms/MB is 0.03 ms.
- **Keyed on the transcript path as well as the session id.** The record path is derived
  from the id alone, so one session pointed at a new file would apply the old file's offset
  to it — and two transcripts of one session can share an identical opening, which defeats
  the head checksum exactly where it is needed. Stored as a **digest**, never the path: the
  record is one pipe-separated line, every other field is numeric or boolean, and a path
  containing a pipe would split into too many fields and make the record permanently
  unreadable.
- **A ceiling: a full read after 64 consecutive resumes or 4 MB of growth.** The entry gates
  reduce the probability of a poisoned offset but cannot bound its lifetime, and the anchor
  is probabilistic — in JSONL roughly one byte per line is a newline. Every other
  degradation in this project self-heals on the next tick; this restores that property, at
  about 0.9 ms per tick amortised on 8 MB. **The residual risk is stated rather than wished
  away:** a file truncated below the offset and regrown past the stored size, with no tick
  in between to observe the short file, is indistinguishable from an append by size alone.
  If it also opens with the same 4 KB and carries a newline at the old offset, it resumes
  across the gap and is wrong until the ceiling fires. That window is what the ceiling is
  for, and `the_resume_decision_table_holds_every_gate` has a row that says so.
- **The idle verdict is seeded from the record, not from the hardcoded default.** The
  commonest appended chunk in an active session is a single tool-result line, which does not
  vote. With the default, the model row flips to ready mid-tool-call — and the skip path
  then re-displays that wrong verdict on every following tick.
- **The checksum is hand-rolled FNV-1a, and adds no dependency.** Nothing in std is safe to
  persist: `DefaultHasher`'s algorithm is explicitly not stable across releases, so a stored
  value would silently change meaning on a toolchain upgrade — an unversioned format change
  `RECORD_VERSION` cannot catch.
- **The tail is scanned with its own length as the sampled size.** `Scan.consumed` is a
  record boundary only when that parameter equals the bytes actually handed in; passing the
  whole file's size would make the torn-tail gate unreachable and fold a partial line into
  the stored totals.

**How it is kept honest.** Every tier degrades to a correct render, so the suite cannot lean
on output. `the_resume_decision_table_holds_every_gate` drives the pure decision over
nineteen states with no filesystem at all;
`a_split_scan_equals_a_whole_scan_at_every_boundary` asserts that a head scan plus a tail
scan equals a whole scan at *every* record boundary of both pinned fixtures, including the
astral-plane one, which is the single test that catches an off-by-one in either direction;
and `each_transcript_tier_announces_itself_in_the_debug_log` runs the real binary out of
process and asserts the skip line, the resume line, and the named fall-through — the skip
had a behavioural pin since it was written but never an assertion on its line. Disabling
the resume makes two of these fail.

**`RECORD_VERSION` is `v5`,** and an upgrade costs one tick of divergence: new fields change
the field list, the parser rejects any record whose count or version differs, so a `v4`
record reads as *absent* and the next tick renders the four buckets as one large `(+N)`.
§4 already accepts this exact divergence from the 2026-08-02 change.

### Bounded git child and drained stdout — 2026-07-28

`read_from_git` stopped calling `Command::output()`. It now spawns, drains stdout on a
helper thread, polls `try_wait` against a 2 s deadline, and kills the child past it. The
change was made for correctness — `output()` blocks unbounded on the render path, and this
process is respawned every couple of seconds, so a repo on a stalled mount accumulated one
blocked process per tick — but it is a hot-path change and therefore belongs here.

Two costs added, both bounded and neither on the common path:

- **One thread per `git` invocation.** Spawned to drain the pipe, because waiting for exit
  without reading deadlocks on a status large enough to fill it. Two invocations per
  uncached tick, so at most two threads, for the lifetime of the subprocess that already
  dominates them.
- **A 5 ms poll interval.** Adds up to 5 ms of latency to detecting an exit that has
  already happened. Against a ~74 ms subprocess pair on the maintainer's machine that is
  under 7% of the cost it is measuring, and it is latency in the *wait*, not extra work.

The poll interval was removed on 2026-09-21; see "The git wait blocks on the pipe instead
of polling" below. The worst-case claim below is also corrected there and under "The status
and diff children overlap": 2.25 s is the bound per *invocation*, and the tick pays it once
per child.

`reader.join()` was replaced by a channel `recv_timeout`. Joining was unbounded in exactly
the case the deadline exists for: killing the child closes only the child's handle on the
write end, and anything `git status` spawned — a `core.fsmonitor` daemon is the ordinary
case — inherited the same piped stdout and holds it open. On Windows `TerminateProcess`
does not touch descendants at all. The drain gets the remainder of the deadline, floored at
`DRAIN_GRACE` (250 ms) so the kill path still has a moment to notice the closed pipe. Worst
case is therefore `GIT_TIMEOUT + DRAIN_GRACE` = 2.25 s, still under the 5 s cache TTL.

**Not re-measured against §6.** The paired harness cannot run: it compares the binary
against the scripts, and those were deleted at `1f5acf2`. Both drivers now refuse with that
explanation instead of failing per-probe on a missing interpreter, which previously read as
a zero-length runtime and produced a flattering median. Re-measuring needs a worktree at
`eb56345`. The §6 statusline rows predate this change and should be treated as a floor.

### The state directory — why it exists, and what it is not — 2026-08-02

Every temp-resident state file moved from the flat OS temp root into
`<temp>/claude-statusline-<owner>/`. Recorded here because all three of the
obvious justifications are wrong, and each will be re-proposed otherwise.

**It is not a performance change.** The `disappeared_rows` scan it narrows
measures flat from 0 to 5000 temp-root entries (§7's populated-temp entry). The
change costs a little rather than saving anything, and the accounting is worth
stating exactly, because the first version of this entry undercounted it.

Resolution runs `symlink_metadata`, and then — on every tick after the first,
because the directory exists by then — the full `verify_through_handle` chain:
an `O_NOFOLLOW | O_DIRECTORY` open plus `fstat` on Unix, a `CreateFileW` plus
`GetFileInformationByHandle` plus `GetSecurityInfo` on Windows. That runs once
per process, and three processes resolve per tick.

Creation repeats the same verification **per guarded write**, not once per tick:
`create_private_dir` sits inside `write_guarded`, and one tick can write the git
cache, the token record, each per-agent state file, and each per-task done stamp.

On Windows both paths also call `trusted_owners()`, which runs
`OpenProcessToken` and two `CreateWellKnownSid` calls — and `state_dir_in` has
already computed the same owner to build the directory name, so that work is
done at least twice per tick. §2 still lists ACL-check cost as an unretired
hypothesis, so this is the part to measure first if the tick ever regresses.

Do not present any of this as an optimisation.

**It is not a security improvement.** A hostile directory at the state path
routes writes back to the flat temp root, because the silent-degradation
contract makes hard failure an absent status line with no signal. That fallback
is a downgrade oracle: an attacker who prefers the flat layout pre-creates the
directory and gets it. The directory therefore provides no property an attacker
cannot unilaterally revoke, and the security boundary remains the per-file guards
in `src/state.rs`, which are unchanged and must stay unconditional inside it.

**There is no migration, and the cited clutter does not go away.** The motivating
number — 82 of 437 entries in the maintainer's `%TEMP%` — is not reduced by
shipping this. Existing flat files are never migrated, swept, or read; they
persist until an uninstall or an OS temp cleaner removes them, which is why both
uninstallers keep their legacy flat globs permanently. The change bounds future
accumulation.

What is left is the actual reason: the tool's files are grouped rather than
interleaved, and an uninstall of a post-change install removes one directory.
That is a modest benefit and it was weighed against a real cost — the first
directory-level guard in a codebase whose every guard learning is file-scoped.

Two implementation facts worth not rediscovering:

- **`std::fs::create_dir_all` returns `Ok` on a symlink to a directory** —
  `mkdir` reports `EEXIST`, `Path::is_dir()` follows the link, and the loop
  breaks to success. Harmless while the parent was the always-present temp root;
  one level down it would have made unverified adoption the ordinary case.
- **Guarded creation applies to this directory and nothing else.**
  `state::write_guarded` also serves `~/.claude`, the flat temp root, and the
  test harness's scratch roots. Applying the private-directory check to those
  rejects all of them — `/tmp` is mode 1777 and root-owned — and every state
  write on Linux fails, invisibly, because a failed write degrades to a
  correct-but-slower recompute that renders identically. `StateRoot` carries the
  distinction explicitly; it is not inferable from the path, and not from whether
  the parent exists.

Reopen condition: none for the location. If the guard's per-write cost ever
shows above the noise floor in a §6 row, the open question is whether to verify
once per process rather than per write, not whether to move back.

### The §6 statusline rows never exercised git or a populated temp — 2026-07-28

Recorded because it changes how those numbers should be read, not because anything moved.

`measure.sh` / `measure.ps1` stage a scratch working directory with **no `.git`**, so the
official §6 "statusline" paired medians never spawn `git` and never touch the git cache —
despite §1 naming the subprocess as one of the two dominant per-tick costs. A `git.rs`
regression can pass the §5 before/after-medians gate untouched.

The same isolation hid a second cost. `disappeared_rows` calls `read_dir` over its state
directory once per tick, filtering by a `statusline-sa-<session>-task-` prefix; until
2026-08-02 that directory was the whole OS temp root. The harness empties `TMPDIR`/`TEMP`
by design — see
`docs/solutions/workflow-issues/isolate-profile-and-temp-when-benchmarking-statusline.md` —
so every recorded number reflects an empty directory, and a user's temp root is not empty.

**The temp-root half of the reopen condition is now answered, negatively.** Swept against a
populated root on the maintainer's machine (Windows 11 26200), fresh process per probe,
median of 11, 8 MB transcript, warm tick:

| Temp-root entries | Median |
|---|---|
| 0 | 7.8 ms |
| 437 (this machine's real count) | 8.1 ms |
| 2000 | 8.0 ms |
| 5000 | 7.9 ms |

Flat across three orders of magnitude, inside run-to-run noise. The scan does not matter at
any plausible temp-root size, so the alternative this entry rejected — a persisted set of
seen ids — stays rejected, now on measurement rather than on absence of it.

**The `.git` half is now measured — 2026-09-21 — and it closes this entry.** Maintainer
machine, fresh process per probe, median of 15, 8 MB transcript, against this repository's
real `.git`:

| Tick | Median |
|---|---|
| Warm, git cache hit | 11.3 ms |
| Git TTL expired, transcript unchanged | 132.4 ms |
| Cold, both expired | 187.1 ms |

The warning this entry existed to carry was correct, and the cost is large: **`git` is
121 ms of that 132 ms tick**, three orders of magnitude above the noise floor the temp-root
sweep found nothing in, and the §6 statusline pairs never saw a millisecond of it. §6 now
carries rows taken with a real `.git`, and §5 requires one from any change that can reach
the git block, so the gate no longer runs blind.

Measured in the same run with the state directory populated to 440 entries, both figures
were unchanged — 11.3 ms warm, 132.0 ms TTL-expired — which re-confirms the temp-root half
a second time, against the current binary rather than the 2026-08-02 one.

The scan itself is **kept**, deliberately. The scripts did the same thing with a shell glob
(`eb56345:linux/statusline.sh:1239`), and it is what finds task files orphaned by a session
that died without cleaning up. Replacing it with a set of previously-seen ids persisted
beside the feed would make the cost proportional to live tasks, but it changes reclamation
semantics, and no measurement yet shows the scan mattering. What is wrong today is the
claim, not the code.

Reopen condition: **met and closed, 2026-09-21.** Both halves of the conjunction have been
measured — the temp root negatively, twice, and `.git` positively — and §6 records the
result. The statusline medians no longer describe a best case.

The mechanism gap is closed too: `measure.sh` / `measure.ps1` are deleted, and their
replacements stage a real `.git` from `states.json` on every run — `dirty` by default, any
state by name. A driver that measures no git is no longer something anyone can reach for
by accident.

### Click-to-focus capture runs on the alert path only — 2026-09-20

Recorded because the feature added a state file, a second executable, a DLL import and a
subprocess lifetime, and each of those is the kind of thing §2 forbids on a per-tick path.

**Nothing joins the tick.** The focus record (`statusline-focus-<session>.json`) is
captured only when a visual alert fires: in the tick's alert branch, once per crossing,
before the detached `notify` child is spawned, and in the hook-invoked `notify` for the
permission, stop and compaction events. A tick that crosses nothing runs no capture, opens
no window enumeration, and reads no registry key; the case table asserts it writes no
record. Rendered output is byte-identical across the table.

**One import was a per-tick cost until it was delay-loaded.** The window calls the click
path needs live in `user32.dll`, which the binary never imported before. Linked normally,
the loader maps and initialises it in every process, tick included, and the first paired
measurement showed the tick median moving from 10.7 ms to 12.5 ms (minima 10.5 and 12.1)
with no other per-tick change in the diff. `build.rs` now passes `/DELAYLOAD:user32.dll`
on MSVC, so the DLL loads on the first call into it — which only a click handler or a
visual-alert capture ever makes — and the tick is back where it was (table below).

**What capture costs, per visual alert.** On Windows: one Toolhelp snapshot, one
`EnumWindows` pass, one `GetProcessTimes` per ancestor, one registry read for the URI
handler, and one guarded write. On macOS: one `proc_pidinfo` per ancestor and the write.
On Linux: one `/proc/<pid>/stat` read per ancestor and the write. Measured on the alert
path with fresh-process medians of 11, both variants interleaved, isolated profile and
temp, the toast interpreter pointed at an empty `SystemRoot` so neither variant pays
BurntToast (*maintainer machine* class, Windows 11 26200, PowerShell 7.6, release builds):

| Path | Before (6a94436) | After | Delta |
|---|---|---|---|
| `statusline` tick, minimal payload, no crossing | 10.5 ms | 10.1 ms | inside run-to-run noise |
| `notify stop` hook, visual on, capture and record | 1730.6 ms | 1736.3 ms | the alert path's whole new cost |

The alert path's absolute number is not the capture's: the stop event plays the system
sound synchronously (`PlaySoundW` with `SND_SYNC`, as the shipped handler did) on both
sides, and the PowerShell pipeline driving the probe adds its own share. With an empty
`SystemRoot` the toast spawn fails at once on both sides too. Read the delta, which is the
capture and the guarded write: about 6 ms, once per visual alert.

**Linux keeps one process alive per visual alert.** The click is observed by waiting on
`notify-send`, whose action flag implies `--wait`; GNOME does not bound that, so the
executor owns a 120-second deadline and terminates the child when it lapses. The constant
is a constant, not a config key: revisit it only with evidence from the real-desktop rows
that two minutes is the wrong window. It costs a sleeping process, no CPU, and nothing on
any tick.

**The helper is a second binary, not a second per-tick process.** `claude-statusline-focus`
runs only when the shell launches it for a click, spawns nothing, and exits within the
two-second foreground bound.

### The update signal is a read, not a check — 2026-09-20

The model row now answers "is there a newer Claude Code". The obvious way to answer it is
the one this codebase cannot have: `npm view @anthropic-ai/claude-code version` is a
network call behind a subprocess, on a path whose first hard rule is that `git` is the
only subprocess. So the question was inverted — *what already knows the answer on disk?*

Three candidates, all of them inspected in the installed 2.1.278 bundle and on a live
machine:

- `~/.claude/.last-update-result.json` — backward-looking. It records the outcome of the
  last update *attempt* (`version_from`, `version_to`), so it says what happened, never
  what is available. With `autoUpdates: false` it may not move for weeks.
- The installed `package.json` version against the payload's `version` — real, but only
  during the seconds between an in-place `npm install -g` and the session restarting, and
  the path to it differs per install method (npm-global, npm-local, native). Three
  spellings of a path to catch a window nobody is looking at the status line during.
- `~/.claude/cache/changelog.md` — Claude Code's cached copy of its own CHANGELOG, fetched
  from the repository's `main` branch. Its first `## X.Y.Z` heading is the newest version
  that existed when the main process last fetched it. One path, every install method.

The third one ships. What it costs, measured rather than assumed — produced by
`tests/harness/measure-pair.ps1`, which was added for this change and is named here
because §3 requires a paired measurement to say which tool made it. It is **not**
`measure.sh` / `measure.ps1`: those paired a runtime script against the binary that
replaced it, which is the migration's question, and they could not run at all once the
script trees were deleted; they have since been removed. Answering "did this commit make
the binary slower" needs two builds of this crate, which that pair had no mode for. The
driver takes `tests/harness/payloads/full-with-version.json` — `full.json` plus the
top-level `version` field, which is what gates the changelog read — and refuses to report
a number until it has seen the after-binary actually render the version segment. Without
that field the gate in `update::available` returns before the file is opened and the pair
would time the same code twice; no other committed payload carries it. That assertion is
now `-ProofAfterOnly`, a parameter, rather than the driver's only reason to exist.

15 interleaved pairs per mode, one fresh process per probe, isolated `USERPROFILE`/`TEMP`,
a 753 KB changelog and a 568 KB transcript staged into them, maintainer machine
(Windows 11 26200, i9-10900K, rustc 1.97.1), this tree's `--release` build against one
built from the parent commit:

| Mode | Before | After | Delta |
|---|---|---|---|
| warm | 27.7 ms / 27.2 ms | 27.6 ms / 27.5 ms | −0.11 ms / +0.27 ms |
| cold | 28.5 ms / 28.2 ms | 28.7 ms / 28.3 ms | +0.19 ms / +0.14 ms |

Two independent repetitions whose deltas straddle zero and never exceed 0.3 ms on a ~28 ms
tick. Read that as no measurable cost, not as a saving and not as a regression: a guarded
open plus a 4 KB read does not register against process creation and the transcript. This
is the same result render-path micro-optimisation gets in §1, for the same reason. An
earlier draft of this entry reported uniformly negative deltas from a throwaway probe that
never lived in the tree; the numbers were real and the probe did exercise the read, but
nobody else could re-run them, which is why the driver is committed now.

Unix has no twin yet. `measure-pair.sh` should be written the first time a hot-path change
needs a Unix pair, rather than carrying an untested one.

Three properties the read depends on, all of them deliberate:

- **Bounded.** `state::read_trusted_prefix` stops at 4 KB. `read_trusted` would pull the
  whole 753 KB file to find a number on line 3, which would make it the largest read on
  the tick by an order of magnitude. A heading pushed past the bound is simply not found.
- **Guarded.** It goes through the same symlink and foreign-owner predicate as every other
  read in `state.rs`, because "one module owns every guard" is what stopped two of them
  failing in opposite directions.
- **Gated.** No `version` in the payload means no comparison is possible, so the file is
  never opened. A Claude Code old enough not to send one pays nothing.

And one property it does not have: freshness. The cache is written by the main process, so
a user who has not started Claude Code in a week is compared against the version that was
newest a week ago. That under-reports — the row stays quiet about an update rather than
inventing one — which is the direction to fail in. It can also lead the registry by
minutes, because the heading lands on `main` when the release is cut. The segment's claim
is "a newer version exists", not "an update will install right now".

### The repo link reads `.git/config`, not `git config --get` — 2026-09-21

The path row became a clickable link to the repository's origin, which needs
`remote.origin.url`. The obvious way to get it is `git config --get remote.origin.url`,
and that is the one thing §2's first hard rule forbids: a second subprocess on a per-tick
path. `git` is the only subprocess, and it is already spent on status.

`.git/config` is read directly instead, through `state::read_trusted_prefix` with a 64 KB
bound. Nothing new had to be discovered to find it: `status()` already refuses to render a
git row unless `cwd/.git/index` is a regular file, so `cwd/.git/config` is established to
exist by the same test. The linked-worktree and submodule cases where `.git` is a file
render no git row today and therefore no link either — the same limitation, not a new one.

The value rides in the existing git cache record as a ninth field, so it is re-read only
when that cache misses. The field count is the format version: an eight-field record from
an older build fails `parse_cache_record` and is refetched, so no migration exists to get
wrong. It is re-normalised on the way *out* of the cache as well as in, because a cache
record is untrusted text and this one ends up in a terminal's link handler.

**Measured**, fresh-process probes on the maintainer's Windows machine (Windows 11,
release build), 21 ticks each, medians:

| path | baseline | with the remote read |
| --- | --- | --- |
| git cache hit | 71 ms | 71 ms (untouched by construction) |
| git cache miss (forced, new session id per tick) | 180 ms | 168 ms |

The miss-path medians overlap heavily and the "after" column is the *faster* of the two,
which is noise rather than a speedup: that path is dominated by the ~110 ms git subprocess
and a ~1 KB file read does not clear its noise floor. The cache-hit path is unchanged
without needing a measurement to say so — `read_remote` is reachable only from
`read_from_git`, which a hit never calls.

**Rendered output is not byte-identical, deliberately.** This is a feature, not a
refactor: a repository with a browsable origin now emits OSC 8 around the path. The
invariant held instead is that it costs *zero columns* —
`a_repo_remote_makes_the_path_a_link_without_costing_columns` asserts the visible text is
unchanged and every row width still agrees.

A remote URL is also somewhere credentials genuinely live: CI checkouts write
`https://x-access-token:<token>@host/...` into `.git/config`. Any URL carrying userinfo is
refused outright rather than stripped — stripping would silently produce a working link
from a file the user may not know leaks.

### A stop alert is gated on `background_tasks` — 2026-09-21

Recorded because it changes when a notification fires, and "the alert stopped firing" is
the one failure the silent-degradation contract guarantees the user cannot see.

**The defect.** `Stop` fires whenever the assistant's turn ends, including a turn that ends
while a dispatched subagent is still running. Claude Code resumes the session the moment
that agent hands back, so one user request produced two "Finished working" alerts, and the
first one was false — it announced the end of work that was still in flight. Subagent
completion itself was never the trigger: Claude Code emits `SubagentStop` for that, built
on the same branch as `Stop` (`g ? {…SubagentStop, agent_id:g} : {…Stop}`), and this repo
registers no `SubagentStop` hook.

**The gate.** `silenced_by_background_work` returns no actions for a `stop` whose payload
carries a non-empty `background_tasks`. The field is Claude Code's own register of in-flight
work, documented as distinguishing "session is done" from "session is paused waiting for
background work to wake it", and empty when nothing is in flight — so this reads a reported
fact rather than inferring one. The result is exactly one alert per request, at the point
the user can act on it.

**The fail direction is audible.** Absent, null, wrong-typed, or unparseable all mean
nothing is known to be running, and all stay audible. Only a non-empty array silences.
`session_crons` is deliberately not read: a scheduled wake-up is a later turn, not
unfinished work in this one.

**No new cost, and one avoided.** The stop hook already read its payload for the session id
the focus record needs, so the gate parses nothing that was not parsed before, and nothing
here touches a per-tick path. A silenced stop also skips the focus capture, since the
capture exists for a click on a toast that is never raised.

### The diff call rewrote the user's index — fixed 2026-09-21

Found while measuring, not while looking for it. `git diff --shortstat HEAD` calls
`refresh_index_quietly()`, which takes `index.lock` and rewrites `.git/index` whenever a
tracked file's cached stat data has gone stale — a file touched, checked out, or rewritten
with its own bytes. At a refresh every few seconds that means this tool was contending for
a lock in a repository its user was working in, and generating filesystem events their
watchers could see.

`--no-optional-locks` does **not** cover it. The flag gates `cmd_status` alone;
`builtin/diff.c` never consults it. Measured directly in a scratch clone with two
stat-dirty tracked files, index mtime before and after:

| Invocation | `.git/index` |
|---|---|
| `--no-optional-locks diff --shortstat HEAD` | rewritten |
| `GIT_OPTIONAL_LOCKS=0 diff --shortstat HEAD` | rewritten |
| `-c diff.autoRefreshIndex=false diff --shortstat HEAD` | **untouched** |

So the env-var spelling several projects adopted for this does not fix this path. The flag
ships; `-c` is override-only and writes nothing to disk. Rendered insertions and deletions
are unchanged in every state, including a genuinely dirty tree, and the cost is inside the
noise floor: **+0.66 ms on the git-miss tick (+0.5%)**, 15 interleaved pairs, maintainer
machine, against a `stat-dirty` state now in `states.json`.

`a_refresh_does_not_write_the_users_index` asserts it, and asserts the inverse in the same
test: it runs the invocation this replaced and requires that one *to* move the index, so a
staging change that stopped producing a stat-dirty tree fails rather than making the first
assertion true for the wrong reason.

### The `git` wrapper is bypassed on Windows — 2026-09-21

`PATH` on Windows resolves `git` to `C:\Program Files\Git\cmd\git.exe`, a 46 KB stub whose
only job is to set `MSYSTEM` and spawn the 4.2 MB binary in `mingw64\bin`. Every call paid
for a process and a `PATH` search that produced nothing.

Resolved once per process, by walking `PATH` for a directory named `cmd` that holds a
`git.exe` with a `mingw64\bin\git.exe` or `mingw32\bin\git.exe` beside it. The fallback to
`git` on `PATH` is unconditional: portable, MinGit, scoop and package-manager layouts
differ, and an absent absolute path must degrade to the previous behaviour, never to a
missing git row.

| Tick shape | Before | After | Delta |
|---|---|---|---|
| Warm | 19.43 ms | 19.69 ms | +0.26 ms (noise) |
| Git TTL expired | 123.72 ms | 96.26 ms | **−27.46 ms (−22.2%)** |

15 interleaved pairs per mode, one fresh process per probe, isolated `USERPROFILE`/`TEMP`,
8 MB transcript, `dirty` git state, maintainer machine (Windows 11 26200), rustc 1.97.1,
git 2.48.1. Two children per miss at ~13 ms each, which is what the children-alone probe
predicted (status 44.7 → 31.6 ms, diff 49.2 → 35.7 ms).

**Accepted constraint: the bypass path assumes Git for Windows 2.25.1 or newer.** Before
that version the real binary needed the stub's `MSYSTEM` and misbehaved without it; from
2.25.1 it detects the absence and does that work itself. The check is presence-based rather
than version-based on purpose — reading the version means running `git --version`, a new
per-tick subprocess §2 forbids outright, and inferring it from install layout is still
inferring a version from filesystem shape. Anyone on an older Git for Windows who hits this
has a `cmd\git.exe` with a `mingw64\bin\git.exe` beside it and a git row that misbehaves;
the fix is to upgrade git, and this entry is where that is written down.

### The git wait blocks on the pipe instead of polling — 2026-09-21

The bounded wait slept `POLL_INTERVAL` between `try_wait` calls, so every invocation paid
up to 5 ms of pure latency noticing an exit that had already happened — twice per uncached
tick. The 2026-07-28 entry above priced that at "under 7% of the cost it is measuring" and
left it.

The wait is now on the drain channel, which the helper thread signals when the pipe reaches
EOF — the child closing stdout, which for a healthy `git` is its own exit. The poll interval
survives as the wait's **upper bound** and must: a descendant that inherited the pipe holds
EOF back indefinitely, which is the case `join()` was replaced for, and a wait with no bound
would hand two seconds to every tick in a repository with `core.fsmonitor` enabled. After
EOF a short backoff from 100 µs covers the microseconds between the pipe closing and the
process object being signalled.

| Run | Before | After | Delta |
|---|---|---|---|
| Git TTL expired, 15 pairs | 97.25 ms | 89.40 ms | **−7.85 ms (−8.1%)** |
| Git TTL expired, 21 pairs | 96.40 ms | 90.03 ms | **−6.37 ms (−6.6%)** |
| Warm, 15 pairs | 19.48 ms | 19.39 ms | −0.09 ms |

**The plan expected this to be unmeasurable and it is not, because the noise floor was
wrong.** A sub-5 ms claim on the git-miss path was held to be unattributable — a figure
inherited from a by-hand probe. Measured directly, by running the same binary against a
copy of itself through the paired driver, 21 interleaved pairs: **+0.35 ms (0.4%)**. The
driver's interleaving is tight enough that a 6 ms delta on this path is a finding, not
noise. Anyone claiming a small delta should run that identical-binary control in the same
sitting rather than quoting either number.

Two observables carry it in the suite, because a median never could have been the whole
case: `Wait::eof_to_reap` measures from the instant the pipe reached EOF, so it prices the
latency itself rather than whatever the wait is built from, and
`a_fast_child_is_reaped_without_waiting_for_a_poll_tick` takes the worst of five — one
sample of a 5 ms poll lands under the threshold two times in five. Regressing the wait back
to sleeping makes it fail at 4.47 ms.

### The status and diff children overlap — 2026-09-21

Both `git` children now go up before either is drained, and are collected against one
shared absolute deadline. They are independent reads of the same repository, and git's own
`lockfile.h` states the contract this relies on: lockfiles block only writers, readers never
block, and the loser of an `O_CREAT|O_EXCL` race returns `-1` silently rather than creating
a file. Both commands pass flags `0`.

| Tick shape | Before | After | Delta |
|---|---|---|---|
| Git TTL expired, `dirty`, 21 pairs | 89.75 ms | 61.14 ms | **−28.62 ms (−31.9%)** |
| Git TTL expired, `clean`, 15 pairs | 88.65 ms | 60.36 ms | **−28.29 ms (−31.9%)** |
| Warm, 21 pairs | 19.53 ms | 19.46 ms | −0.07 ms |

`rev-parse` stays sequential and must. Whether it is needed is known only once status has
parsed, so hoisting it would make three children the common case — a strict increase on
every tick.

**The accepted §2 exception.** In two states the tick goes from one child to two, because
the gate that used to skip the diff is a function of *drained* status output and overlapping
the pair is precisely what removes the chance to consult it:

- **unborn HEAD with a staged index** — the branch resolves empty, so the diff used to be
  skipped. It now runs, fails (`fatal: ambiguous argument 'HEAD'`), and its result is
  discarded by the same gate as before. The row is unchanged: no git segment.
- **a status that fails or times out** — the second child is spawned into whatever is
  stalling the first, and is bounded by the same deadline.

This is the R9 exception the plan reserved, taken deliberately and on measurement rather
than on assumption. What decided it: across 85 sessions and 19.8 h of active time, the
working tree is **dirty 95.9% of the time**, so the skip-the-diff-on-a-clean-tree
alternative would have fired on 4.1% of misses for ~1.8 ms expected saving, against ~39 ms
for overlapping. `the_git_child_count_is_pinned_per_repo_state` asserts the real count —
counted, not described — for eight states including both exceptions, so nobody widens this
by accident.

**Correcting the 2026-07-28 entry's worst case.** It recorded `GIT_TIMEOUT + DRAIN_GRACE` =
2.25 s per invocation and called that "still under the 5 s cache TTL". Per invocation it is;
per *tick* it never was. Two bounded children in sequence are 4.5 s, inside the TTL, but the
three-child detached-HEAD path is 6.75 s and overruns it. Overlapping status and diff brings
that path to 4.5 s while `rev-parse` stays sequential, so the tick's worst case is now
inside the TTL in every state rather than in most of them. That is a correctness improvement
the median does not show, and it is not the two-child overrun it would be easy to claim —
that one never existed.

### Link-time optimisation, and a delay-load that measures as zero — 2026-09-21

Two changes to fixed process cost, in different files and by different mechanisms, so they
are measured separately. One paid, one did not, and both numbers are recorded because the
second is the more useful one to a future reader.

**LTO and one codegen unit: kept.** The release profile set `opt-level` and
`panic = "unwind"` and no link-time optimisation. `lto = true` with `codegen-units = 1`
(the default sixteen leaves `lto` working only across their boundaries), 21 interleaved
pairs per mode, maintainer machine, against a ±0.13 ms identical-binary control in the same
sitting:

| Tick shape | Before | After | Delta |
|---|---|---|---|
| Warm | 19.59 ms | 19.37 ms | −0.22 ms (−1.1%) |
| Git TTL expired | 60.84 ms | 60.42 ms | −0.42 ms (−0.7%) |
| Cold — new message and a git miss | 115.35 ms | 113.05 ms | **−2.31 ms (−2.0%)** |

Small but outside the floor, and largest where the transcript scan is — which is where a
whole-crate view has the most to fold. The binary also shrinks 1,170,944 → 1,062,912 bytes,
9% off every download. Release builds go from ~4 s to ~17 s; that is a maintainer's cost,
not a user's. `panic = "unwind"` is untouched and load-bearing: the third silent-degradation
layer is a no-op without it. The release binary was re-checked by hand against the entry
contract — empty and malformed input on every subcommand, plus an unknown one — all exit 0
with zero bytes on stderr, because `cargo test` exercises the dev profile and would not have
caught a release-only regression.

**Delay-loading `winmm.dll`: measures as zero, kept for its failure direction.** `PlaySoundW`
is a genuine import called only when an alert fires, so on the face of it this is the same
trade `user32.dll` already takes. It is not. Isolated with LTO on both sides, 21 pairs:
**−0.01 ms warm**, against a ±0.13 ms control. The ~1.8 ms this entry's neighbour records
for delay-loading belongs to `user32` alone — a far heavier DLL with a dependency graph
behind it — and must not be read as a per-DLL figure.

It ships anyway, but as a failure-direction change rather than a performance one: a missing
or broken `winmm` now fails at the first `PlaySoundW`, where the silent-degradation contract
turns it into one absent chime, instead of failing the whole process at load and taking the
status line down with it. Nobody should later claim milliseconds for it.

**`gdi32` is deliberately not delay-loaded.** `Win32_Graphics_Gdi` is in the feature list
only because `WNDCLASSW` carries a brush handle the crate sets to null. No gdi32 entry point
is called anywhere, so there is no import entry to defer and the linker would ignore the
directive. Confirmed against the binary's import table: `gdi32.dll` is not among its
dependents at all.

### Caching the process owner — no change, 2026-09-21

`trusted_owners()` rebuilds its list on every call, and on Windows that means opening the
process token, reading `TokenUser`, and building the Administrators SID. A warm tick
performs several guarded reads and one or two writes, three processes resolve the state
directory per tick, and the directory's own name has already computed the same answer — so
caching a successful resolution for the process lifetime looked like a free floor win.

**It is worth about 0.02 ms per tick, and was rejected on that.** Measured directly rather
than inferred, maintainer machine, 2000 iterations:

| Call | Cost |
|---|---|
| `current_owner()`, uncached — opens the token every time | 1.53 µs |
| `trusted_owners()`, served from a `OnceLock` | 158 ns |

A saving of ~1.4 µs per guarded operation, against a tick that makes perhaps ten of them.
The hypothesis was that this was milliseconds of syscall; it is microseconds.

**What made it look like more than that, and the methodological lesson.** The paired driver
first reported the cached build at −0.38 ms warm and **+3.4 ms on the cold tick**, against a
±0.02 ms identical-binary control — a reproducible regression three times larger than the
change could possibly cause in either direction. Rebuilding both halves with `lto` off
shrank it to +0.92 ms. It was code layout: **an identical-binary control measures the
machine, not the method.** Two builds that differ by any code at all differ in layout too,
and under whole-crate LTO that is worth a few milliseconds on the cold path, which is where
the 8 MB transcript scan lives. A sub-millisecond delta between two *different* binaries is
not attributable to the diff between them without a mechanism measurement beside it.

So the honest floor for a claim on this project is not one number. The identical-binary
control (±0.02 to ±0.35 ms) bounds *run-to-run* noise; attributing anything under about
1 ms to a specific change additionally needs the mechanism priced, as it is above.

The cache is not kept. It is fifteen lines and a `OnceLock` whose only subtle requirement —
never cache a failure, because `None` means "I could not ask" and caching it disarms the
owner check for the rest of the process — is a fail-direction hazard this project has
already paid nine days for once. That is not a trade worth making for 0.02 ms.

Reopen condition: a guarded operation count per tick an order of magnitude higher than
today's, or a platform where the token lookup is not a cheap local call.

### The agent transcript is scanned backwards, and keyed on size too — 2026-09-21

Two changes to the subagent fallback tier, which reads one `agent-*.jsonl` per agent and
keeps only its **last** assistant entry.

**Reverse scan.** It reached that last entry by `serde_json`-parsing every line of the file
in order and overwriting the result each time — a whole-file parse to throw all but one
line away. It now scans from the end and returns at the first assistant entry it finds,
which in the ordinary case parses one line. Unparseable lines are still skipped rather than
ending the scan, so a torn tail — routine in a file being appended to — still falls back to
the entry before it, from either direction.

| Tick shape, 4 MB agent transcript | Before | After | Delta |
|---|---|---|---|
| Re-read (the shape an agent at work produces every tick) | 105.57 ms | 71.05 ms | **−34.52 ms (−32.7%)** |
| Skip (the agent's file unchanged) | 19.23 ms | 19.08 ms | −0.15 ms |

And on Linux, same 4 MB agent transcript, against a build scanning forwards:

| Tick shape | Before | After | Delta |
|---|---|---|---|
| Re-read | 35.96 ms | 12.64 ms | **−23.32 ms (−64.9%)** |
| Skip | 1.40 ms | 1.37 ms | −0.03 ms |

15 to 21 interleaved pairs per platform, against a +0.10 ms identical-binary control.
`the_agent_reverse_scan_agrees_with_a_forward_one` pins the equivalence against a forward
reference implementation over eleven shapes — empty, no assistant entry, no trailing
newline, a torn final line, a torn line in the middle, trailing blanks, invalid UTF-8 — plus
the pinned fixture input, because "the last assistant entry" and "the first one from the
end" are the same thing only while the skipping rules stay symmetric.

**Size joined the staleness key.** It was mtime alone, which is strictly weaker than the
main transcript's `(mtime, size)` and exposed to the documented Windows behaviour where a
writer's mtime does not move until its handle closes: an agent appending through an open
handle was read once and then never again. `AgentState` gains a ninth field; an
eight-field record written by an older binary is rejected whole by the same strictness that
already rejects a torn one, at the cost of one re-scan.

**A measurement trap this unit walked into first, and the driver now prevents.** The pair
driver shared one `TEMP` between the two variants. With a record format that differs
between them — which is exactly what a field addition or a `RECORD_VERSION` bump creates —
each build rejects the other's records, so **every probe measures a miss**, on both sides.
The first run of this unit read −41.94 ms on the *warm* tick, a path where nothing should
have moved at all, because both variants were re-reading 4 MB on every probe instead of
skipping. `measure-pair.*` now give each variant its own state root and set it on the child
process rather than on the driver's own environment; the home root stays shared, because
what lives there is input rather than record. Any unit that changes a stored format must be
measured this way or the number is fiction — U8 changes `RECORD_VERSION` and would have hit
exactly this.

The drivers also learned `--agent-bytes` / `-AgentBytes`, which stages an agent transcript
beside the session's. Nothing could measure this tier before.

### The git-miss path costs ~9 ms more than it did at `eb56345` — resolved 2026-09-21

**Resolution: bisected to `d9e61f4`, and fixed.** The commit is "Bound the git child and
repair the migration failure paths" — the 2026-07-28 change recorded above, which was the
entry's own hypothesis and is now measured rather than suspected. The mechanism is the 5 ms
poll interval that change introduced: two children per uncached tick, each detected on
average half an interval after it had already exited.

Originally found by A/B-ing the binary that produced §6's original medians against
`3f5f919`, sequentially, median of 15 — recorded then as +9.0 ms cold and +9.4 ms on the
git-TTL-expired row, with the caveat that the two binaries do not render the same box and
the figure is an upper bound across two months of feature change.

Re-confirmed interleaved, 21 pairs, and then bisected over the seven commits in
`eb56345..HEAD` that touch `src/git.rs` — the search is over those, not the fifty-eight in
the range. Each step is an interleaved pair of adjacent builds, 15 runs, git-TTL-expired,
`dirty` state, maintainer machine:

| Step | Delta |
|---|---|
| `eb56345` → `1031fff` | +0.74 ms |
| `1031fff` → **`d9e61f4`** | **+5.44 ms (+4.6%)** |
| `d9e61f4` → `2f666f1` | +0.42 ms |
| `2f666f1` → `b53eaba` | −4.93 ms |
| `b53eaba` → `79a2eb3` | −0.37 ms |
| `79a2eb3` → `16eaf11` | +0.52 ms |
| `16eaf11` → `1b7e4e9` | +1.77 ms |
| `1b7e4e9` → `304d2ab` | +1.48 ms |
| `eb56345` → `304d2ab`, end to end | +6.68 ms (+5.7%) |

`d9e61f4` re-measured on its own at 21 pairs: **+4.71 ms**, against a +0.78 ms
identical-binary control in the same sitting. The remaining steps are inside that control,
and their signs do not accumulate — which is what the "several small ones, none individually
actionable" outcome would have looked like, and is not what this is.

The arithmetic agrees with the mechanism: two children, each costing on average half of a
5 ms poll, is ~5 ms. Nothing else in that commit touches the common path.

**The bounded child stays; the poll does not.** The change was made for correctness —
`output()` blocks unbounded on the render path, and a repo on a stalled mount accumulated
one blocked process per tick — so the fix could not be a revert. Removing the poll interval
in favour of blocking on the drain channel recovers it: measured at −6.37 to −7.85 ms, which
is the regression and a little more. See "The git wait blocks on the pipe instead of
polling" above.

End to end, `eb56345` against the current tree on the git-TTL-expired row, 21 interleaved
pairs: **116.46 ms → 60.35 ms, −56.11 ms (−48.2%)**.
