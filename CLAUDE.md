# CLAUDE.md -- claude-statusline

Cross-platform custom status line for Claude Code, shipped as one Rust binary. Claude Code pipes JSON to stdin on each refresh; the binary parses it and renders ANSI output.

## Project Structure

```
src/                 the crate: one multi-call binary, one subcommand per former runtime script, plus the Windows click helper in src/bin/focus.rs
install/             install.sh + uninstall.sh (macOS and Linux), install.ps1 + uninstall.ps1 (Windows)
tests/equivalence.rs the single integration test file; a table of named cases
tests/fixtures/      golden captures, one directory per case
tests/harness/       fixture capture and paired measurement drivers
docs/solutions/      documented fixes and practices, by category, with YAML frontmatter (module, tags, problem_type) -- relevant when debugging or implementing in an area one of them covers
docs/performance.md  standing performance rules: cost model, measurement methodology, equivalence matrix, PR checklist -- binding for any hot-path change
```

`install/` holds the only installers. The per-OS `macos/`, `linux/` and `windows/` directories are gone: they had been reduced to shims that fetched `install/` anyway, so README now publishes `install/install.sh` and `install/install.ps1` directly and a piped install costs one fetch instead of two. `every_url_the_readme_publishes_resolves_to_a_file` asserts every published path still exists, because a 404 through `curl -fsSL … | bash` fails silently — no error, no install.

## Architecture

One binary, `claude-statusline`, dispatching on an argv token rather than `argv[0]`:

- `statusline` (the default with no subcommand) -- reads the payload on stdin, renders the box
- `notify <event>` -- sound and toast delivery, triggered by hooks on permission requests, task completion, and compaction
- `git-refresh` -- cache invalidation hook registered as PostToolUse, clears stale git status after file-modifying tools
- `subagent` -- subagentStatusLine handler, tees Claude Code's per-task feed to a session state file for the status line to read; prints nothing so the default agent panel stays intact
- `self-check` -- renders a compiled-in fixture and compares it to the compiled-in expectation; the installer's gate against a binary that launches but renders wrongly
- `focus <session>.<token>` -- the click handler: resolves the focus record a toast named and brings the session's terminal forward, selecting its tab or pane where the terminal allows it. Reached three ways: terminal-notifier's stored `-execute` action on macOS, the `notify` arm on Linux once notify-send reports the click, and `claude-statusline-focus.exe` on Windows, a second GUI-subsystem executable the shell launches through the `claude-statusline:` URI handler so no console ever appears
- `settings <apply|remove|has|has-foreign|has-legacy>` -- the installers' `settings.json` editor; `settings protocol <register|unregister|has>` owns the Windows URI handler the same way, and shares the exit-code exemption

The crate is lib+bin so the single test file can reach internal behaviour a binary-only crate cannot expose. It has two binaries -- the helper is built on every host and shipped only for Windows -- which is why `Cargo.toml` carries `default-run = "claude-statusline"`: `cargo run` cannot pick between two otherwise. The entry layers live in `src/entry.rs` so both `main`s share them verbatim. On MSVC `build.rs` delay-loads `user32.dll`, which only the click path calls, so the per-tick process does not pay for loading it.

**"The scripts", in code comments, means the three per-platform trees this binary replaced.** Roughly 150 comments explain a choice by reference to them — a threshold copied verbatim, a guard reproduced deliberately, a bug not reproduced on purpose — and every one of those is still the reason the code looks the way it does. They were deleted at `1f5acf2`; `eb56345` is their final state if you need to read one. Do not delete these comments as stale: the fixtures were captured from exactly that code, so the reasoning is what makes a fixture failure interpretable.

**Two subcommands are deliberately exempt from the exit-0 contract**: `self-check` must be able to fail, or the installer cannot tell a bad build from a good one, and `settings` must be able to fail, or an installer reports success having written nothing. Everything else exits 0 always (see Silent Degradation).

There is no output cache. The display recomputes per tick -- the caches in the scripts existed to dodge an interpreter startup cost the migration removed. Six files survive as **data stores, not performance caches**: the learned model-to-window map, the per-task subagent done-linger stamp, the per-session notification latch, the per-session subagent tasks feed, the per-session transcript token record, and the per-session focus record that a toast's click resolves. The git status cache keeps its 5s TTL, because `git` is still a subprocess and the TTL doubles as the staleness bound for an invalidation key known to be incomplete.

### JSON Input Contract

Claude Code pipes a JSON object to stdin on each refresh. Key top-level fields:

`session_id`, `workspace.current_dir`, `cwd`, `model.display_name`, `model.id`, `context_window.context_window_size`, `context_window.used_percentage`, `context_window.total_input_tokens`, `effort.level`, `cost.total_cost_usd`, `cost.total_duration_ms`, `transcript_path`, `rate_limits.five_hour.*`, `rate_limits.seven_day.*`, `agent.name`, `context_window.current_usage.*`

Legacy spellings the accessors still accept as fallbacks: top-level `total_cost_usd`, and `total_duration_ms` / `duration_ms` for the duration.

See the accessors on `Payload` in `src/payload.rs` for the full field list. The payload is deserialized to a generic JSON value and read through tolerant per-field helpers: a typed model would fail the whole document on one field's type change, blanking the status line where per-field extraction degrades one row.

### Subagent Tasks Feed

Second input contract beside the stdin JSON: Claude Code's `subagentStatusLine` feature pipes `{session_id, tasks: [...]}` (per-task model, context window size, status, token count, description) to `claude-statusline subagent` on each refresh tick. The handler prints nothing and tees the payload to `statusline-tasks-<session-id>.json` inside the state directory (see below); the status line reads it when fresh. Per-task `model` / `contextWindowSize` require Claude Code >= v2.1.205 -- without feed data, the status line falls back to parsing subagent transcripts and resolving the context window through five tiers, in order: this session's own model, then the learned map (`~/.claude/statusline-model-windows.json`, written from each main session's model -> window pair), then a seed table, then a `[1m]` / `-1m` marker in the model id, then a 200K default. `Windows::resolve` in `src/subagent.rs` is the authority; keep this list in step with its doc comment.

### State Directory

Every temp-resident state file is written inside `<temp>/claude-statusline-<owner>/`, where `<temp>` is `$TMPDIR` (`%TEMP%` on Windows) and `<owner>` is the uid on Unix and a digest of the token SID on Windows. Seven families live there: `statusline-git-*`, `statusline-tasks-*`, `statusline-notify-*`, `statusline-tokens-*`, `statusline-focus-*`, `statusline-sa-*-task-*`, and `statusline-sa-*-<agent-base>`. The focus record is written only when this directory verifies: on the flat fallback below, capture is skipped and the toast goes out without click handling, because a record another local user could plant must never name a window to raise. A seventh pattern, `statusline-oc-*`, is delete-only and now vestigial: the binary never writes an output cache, and the `git-refresh` unlink that cleaned up after a script-era install resolves against the state directory, where such a file can never exist. The live cleanup for it is the flat sweep both uninstallers still perform.

`session::state_dir` is the only resolver, and four call sites use it: `Roots::from_env`, the `git-refresh` and `subagent` dispatch arms, and the `notify` arm's capture of the focus record. **`temp: &Path` throughout the crate now means this directory, not the OS temp root.** `session::temp_dir` remains the escape hatch for anything that genuinely needs the root.

Two rules the design rests on, both with a test behind them:

- **Resolution creates nothing.** It runs before the payload is parsed, and a tick whose payload does not parse must leave the temp root untouched (`degraded_input_renders_the_notice_and_touches_no_state`). Creation happens on the first guarded write.
- **Only this directory is created privately.** `state::write_guarded` also serves `~/.claude`, the flat temp root on the fallback path, and the test harness's scratch roots — applying the private-directory check to those rejects every one of them, and every state write on Linux fails with it. `StateRoot` carries the distinction explicitly; it is not inferable from the path or from whether the parent exists (`guarded_creation_applies_only_to_the_state_directory`).

When the directory exists but does not verify — a symlink, a reparse point, a foreign owner — the binary falls back to writing flat in the temp root, exactly as it did before. That fallback is required by the silent-degradation contract and it means **this is not a security improvement**: an attacker who prefers the flat layout gets it by pre-creating the directory. The security boundary is still the per-file guards in `src/state.rs`. Existing flat files from an earlier version are never migrated or read; both uninstallers keep their legacy flat globs for that reason.

### Dependencies

- **Runtime:** none. `git` is required only for the git status row; without it that row is absent. No `jq`, no Bash version floor, no PowerShell version floor.
- **Build:** a Rust toolchain. Linux targets link statically against musl so one artifact runs on any distribution, including Alpine and older glibc.
- **Install:** `curl` (macOS/Linux) or `Invoke-WebRequest` (Windows), plus a SHA-256 tool -- `sha256sum`, `shasum`, or `Get-FileHash`. `gh` is optional and enables provenance verification.
- **Optional, for visual notifications:** `terminal-notifier` on macOS, `libnotify` on Linux, the `BurntToast` module on Windows. For raising the window on a toast click, Linux additionally uses `xdotool` or `wmctrl` on X11 and `kdotool` on KDE Wayland when they are installed; tab and pane selection reaches `osascript`, `kitten`, `wezterm`, `tmux`, `screen`, `zellij` and a Konsole D-Bus caller by absolute path, each optional.

## Development

### Branching

Two-tier flow: create feature branches as `dev-<feature>` (e.g. `dev-notifications`), PR them into `dev`, and periodically release `dev` into `master` via a release PR. The repo-local `pr` and `sync` skills pick the right base/source automatically from this model.

### Release Channels

Three, all served by `.github/workflows/release.yml` and resolved by the installers without a token:

- **Stable** -- a `vX.Y.Z` tag. The plain install command resolves it through `releases/latest`, which excludes prereleases.
- **Prerelease** -- any other tag shape (`v1.0.0-rc.1`, `verify-*`), published as a prerelease. `--pre` resolves the newest tagged release from the atom feed, `v` and a digit only.
- **Dev channel** -- every push to `dev`. The workflow publishes one prerelease tagged `dev-channel` and replaces its assets, title and notes in place on each push, so the Releases page carries a single entry for it. `--dev` pins that tag, resolving nothing, and outranks `--pre`. A head commit whose message carries `[skip release]` publishes nothing -- anywhere in the message, body included, so a commit message that talks about the marker must not spell it out (the commit that introduced it skipped its own run that way).

The `dev-channel` tag is created on the first run and never moved: a moving tag makes every clone's next `git fetch` fail with "would clobber existing tag". So the tag does not say which commit a build came from -- the release title and notes do, and the Sigstore attestation proves it. Pushing `dev` therefore builds all six targets on every push -- a stable or prerelease tag is still the only thing that publishes a release a plain or `--pre` install can reach.

### Testing

```
cargo test                          # the whole suite, including the fixture case table
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

All three are gates on every commit. `cargo test` includes the case table, which stages each case into isolated home and temp roots, pins the clock, renders in-process, and refuses to compare output that leaked a machine-local path.

- **One test file.** `tests/equivalence.rs` drives a table of named cases; a failure names the case and shows the diff. Fixtures live under `tests/fixtures/` as data files, never as additional test files. Do not add a second test file.
- Set `STATUSLINE_DEBUG=1` to enable debug logging to `~/.claude/statusline-debug.log`, from every subcommand.
- `claude-statusline self-check` renders the real fixture and exits non-zero on mismatch -- the fastest confirmation that a build is sound.
- Install locally via `bash install/install.sh` (or `install/install.ps1`) to test the full flow. It fetches a published release; `--dev` takes the newest dev-channel build, so a pushed `dev` can be uninstalled and reinstalled end to end without cutting a tag.

**A green Windows run does not prove the suite passed.** Five equivalence cases skip on Windows without Developer Mode because they need symlinks; they print a reason and report as passing. Others carry `#[cfg(unix)]` assertions that simply do not compile into a Windows build — the state directory's mode check is one. CI's Unix runners are what exercise both.

This is measured, not theoretical. Reintroducing the defect that guarded creation is scoped against — applying the private-directory check to every `write_guarded` parent, so `/tmp` and `~/.claude` are rejected — leaves Windows at **144 passed, 0 failed** while Linux fails **10**. Run the suite on Linux before believing a change to the state directory, the guards, or anything under `src/platform/` is green. `docker run --rm -v "<repo>:/host:ro" rust:latest bash -c 'git config --global --add safe.directory /host && git clone -q /host /src && cd /src && cargo test'` is enough, and does not need a push.

What to verify after changes:

- Box renders without broken alignment or trailing characters
- Colors display correctly (green/yellow/red thresholds)
- No output to stderr (breaks Claude Code UI)
- Exit 0 on empty, malformed, or missing JSON input
- Git status row handles detached HEAD, no-repo, and fresh-clone states

### File Encoding

Enforced by `.gitattributes` -- do not override. It governs the installers and the harness; Rust sources carry no constraint beyond git's defaults.

- `*.sh` -- LF line endings
- `*.ps1` -- CRLF line endings with UTF-8 BOM
- **Exception:** `install/install.ps1` and `install/uninstall.ps1` are **BOM-less and ASCII-only**. They run via `irm <url> | iex`, and a BOM survives `irm` as a stray U+FEFF that breaks `iex` on the first token (fixed in `762dcc0`, regressed once by re-applying the BOM rule mechanically -- do not "fix" the missing BOM back). ASCII-only keeps them safe to run from a local clone too.

`fetched_powershell_installers_are_bomless_ascii` asserts the exception, so a mechanical re-application fails the suite rather than shipping.

---

## Rules

These rules apply to every task in this project unless explicitly overridden.
Bias: caution over speed on non-trivial work.
Naming: Rust conventions throughout the crate (`snake_case` items, `SCREAMING_CASE` constants). The installers keep their dialects' conventions: Bash `snake_case`, PowerShell `PascalCase`.

### Platform-Specific Code Is Confined

There is one implementation. Platform-conditional code is confined to four areas and nowhere else:

1. **notification delivery** -- the per-OS sound and toast mechanisms, and the click side of them: capturing which terminal window a session runs in, the click transport, and the Windows URI registration,
2. **file-ownership checks** -- the uid and ACL guards,
3. **process and stream handling** -- redirecting fd 2 at entry, and creation flags on spawned children,
4. **environment spelling** -- `%USERPROFILE%` against `$HOME`, `%TEMP%` against `$TMPDIR`, and whether a stored command needs quoting.

Anything else that reaches for `cfg!(windows)` is a design error -- most often a sign that a behaviour should be resolved to one recorded answer instead of branched. Path formatting is the worked example: the port compares the home prefix case-sensitively on every platform rather than matching Windows' case-insensitive comparison, because keeping both would need a branch here (see `docs/performance.md` §4).

`platform_conditional_code_stays_in_its_areas` enforces this against the file list, in both directions -- a new branch outside the areas fails, and so does an exemption that is no longer used. One implementation grows back into three one `cfg!` at a time, which is why this is a test rather than a convention.

### Silent Degradation

Every per-tick subcommand -- `statusline`, `notify`, `git-refresh`, `subagent` -- must exit 0, even on error, and write nothing to stderr. Breaking this contract crashes the Claude Code status line for users.

Enforced at process entry in five layers, all of which must stay:

1. redirect fd 2 to the null device before anything can write to it,
2. install a no-op panic hook,
3. wrap each subcommand in `catch_unwind`,
4. flush stdout **and check the flush result**,
5. exit 0.

The flush is load-bearing: `std::process::exit` runs no destructors, so a buffered writer dropped unflushed produces an empty status line that satisfies every exit-code and stderr assertion. The release profile keeps `panic = "unwind"` so layer 3 exists in shipped builds. Never use `println!`/`eprintln!` -- they panic on a broken pipe, which is routine when the parent stops reading.

Log errors via the debug log (`STATUSLINE_DEBUG`), not to the user's terminal.

`self-check` and `settings` are the two deliberate exemptions; see Architecture.

### No `exit` in Install/Uninstall Scripts

Install and uninstall scripts must never use `exit`. Windows scripts are invoked via `irm | iex`, which runs in the user's current PowerShell session -- `exit` terminates that session and closes the terminal window.

- **Trailing `exit 0`**: Remove it. Scripts naturally return 0 when they reach the end.
- **Bash error paths**: Use `return 1 2>/dev/null || exit 1`. `return` succeeds when sourced; `exit` is the fallback for subshell invocation via `curl | bash`.
- **PowerShell error paths**: Use `return`. This exits the script scope without terminating the session.

This rule applies to `install.*` and `uninstall.*` in `install/`, which is the only place they now live. `install_scripts_never_exit_the_users_shell` asserts it.

### Installers Gate On the Self-Check

Nothing irreversible happens before `claude-statusline self-check` passes: not removing a prior installation's scripts, not rewriting `settings.json`. A binary can pass its checksum, launch, and still render wrongly, and the silent-degradation contract guarantees that failure reaches the user as an absent status line with no other signal. On a failing check, the installer restores the previous binary and leaves `settings.json` untouched.

### Performance

The hot cost is no longer an interpreter. Every refresh still spawns a fresh process, but it is a native binary with no startup floor to amortise, so the remaining costs are real work: subprocess `git`, reading the transcript, and the syscalls around state files. Full rules, cost model, and the mandatory PR checklist live in `docs/performance.md`. The non-negotiables:

- No new subprocess on a per-tick path. `git` is the only one, and it is bounded by the 5s TTL. The focus capture runs only when a visual alert fires, in the tick's alert branch or the hook, never per tick.
- Do not reintroduce an output cache. It was deleted deliberately; the cost it hid is gone.
- Do not read the transcript when `(mtime, size)` are unchanged -- everything the tokens and model rows render is reconstructable from the stored record. This is measured, not assumed: an unconditional scan cost ~50 ms on 8 MB and made the port 4x slower than the script on Linux.
- Performance changes must keep rendered output byte-identical (verified by the case table) and must be measured with fresh-process probes, never warm loops -- end-to-end before/after medians only.
- Debug-log call sites must not evaluate expensive arguments when logging is off.
- Bump `RECORD_VERSION` whenever a stored record format changes.
- `docs/performance.md` is a living document -- whenever work measures a new cost, rules out a hypothesis, accepts a rendered-output divergence, or settles a design question, update it in the same change. Its §6 reference numbers must always describe the current binary, and every row must carry the host class it was measured on.

### Repo Skills and Branching

- Every commit, sync, and pull request goes through the repo-local skills (`commit`, `sync`, `pr`) — never hand-written `git commit`, `git merge`, or `gh pr create`. The skills encode this repo's message style, two-tier flow, signing, and safety checks.
- Never create a new branch without asking the user first — use `AskUserQuestion` when available, plain chat otherwise. This applies even when a plan, skill, or workflow suggests a branch: the user decides branch creation, every time.

### Never Commit

- A hardcoded absolute personal path (`/Users/<name>/...`, `C:\Users\<name>\...`) where `$HOME` / `~` / `$env:USERPROFILE` belongs. This tool runs on other people's machines — a baked-in personal path is a shipped bug, not just a privacy leak. The case table refuses to compare output that leaked one.
- `statusline-debug.log` or any other runtime debug/log artifact.
