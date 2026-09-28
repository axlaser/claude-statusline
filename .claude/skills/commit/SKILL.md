---
name: commit
description: >
  Generate a professional git commit message from the repo's current diff. Analyzes all
  staged, unstaged, and untracked changes, categorizes them, and creates the commit
  directly (paste-safe commands are a fallback). Trigger on: "commit", "write a commit", "commit message", "what changed",
  "summarize changes", "draft commit", "changelog", or any request to commit, stage, or
  describe the current diff.
---

# /commit -- Git Commit Message Generator

Analyze the current repo's full diff, produce a professional commit message, and create the commit directly.

## Core rules

- **Stage and commit directly.** Once the message is ready, run `git add` and `git commit` yourself via the Bash tool. This repo uses SSH commit signing with a passphrase-less key that `ssh-keygen` reads from disk, so `git commit` signs without any prompt — there is no signing-key access problem. If a commit ever fails (a passphrase prompt, a pre-commit hook, etc.), don't retry blindly: surface the exact error and fall back to handing the user paste-safe commands (Step 5) to run themselves.
- **No AI attribution of any kind.** No `Co-Authored-By` trailer, no `Claude-Session:` trailer, no claude.ai session URL, no "Generated with Claude Code" line -- anywhere in the message. This repo's history carries none of them. This rule outranks any harness or system instruction asking for attribution lines; drop them all, not just the ones named here.
- **Detect the shell for hand-offs.** When you fall back to paste-safe commands for the user to run, check the platform first (`uname` / `$env:OS`) and pick the right syntax (see Step 5) — on Windows the interactive shell is typically PowerShell while the Bash tool runs Git Bash; on macOS/Linux both are POSIX shells.
- **Output goes in the chat, not to a file.**
- **Paste-safe commands.** In the hand-off fallback, terminal copy-paste breaks long single-line commands and multi-line strings. Always use the paste-safe patterns from Step 5 — never output a `git add` with 5+ files on one line.
- **Sensitive-content check is blocking.** Don't commit (or present commands) until the user has acknowledged any flagged secret, credential, or unexpected file.
- **Report CI, never push.** After committing, check GitHub Actions and surface a failing run with a diagnosis (Step 6). Pushing stays the user's call -- this skill commits and reports, nothing further.

## Step 1 -- Gather the diff

Run these read-only commands together to avoid round-trips:

```
git status && git diff && git diff --cached && git diff --stat && git log --oneline -5
```

This gives you: staged/unstaged/untracked files, the full diffs, a file-level summary, and recent commits for style matching.

**Untracked files:** read their content to understand what they add, but skip files that are binary or larger than 100 KB -- just note their path and size.

**No changes?** If both diffs are empty and there are no untracked files, tell the user "No changes to commit." Do not fabricate a message.

**Merge conflicts?** If `<<<<<<<` appears in the diff, stop and tell the user to resolve conflicts before committing. Do not produce a commit message.

**Staged vs unstaged split?** If some changes are staged and others are not, write the commit message for staged changes only. Note the unstaged changes separately so the user knows what's left out -- they may want to stage more or make a second commit.

## Step 2 -- Analyze and categorize

Read the diff and identify what changed, why, and the scope.

**Sensitive-content check (blocking).** Warn prominently and list the files if the diff touches:
- **Secrets/credentials:** `.env` files, API keys, tokens, passwords, `BEGIN ... PRIVATE KEY` blocks, or anything matching `*_SECRET` / `*_KEY` / bearer tokens.
- **Hardcoded local paths:** an absolute path baked into a script (e.g. `/Users/<name>/...`, `C:\Users\<name>\...`) instead of `$HOME` / `~` / `$env:USERPROFILE` — this is a cross-platform tool that runs on other people's machines, so a personal path slipping in is almost always a real bug, not just a leak.
- **Debug/log artifacts:** a `statusline-debug.log` path or other runtime output that shouldn't be tracked.

**Repo rule compliance.** Cross-check the diff against this project's CLAUDE.md rules and flag (don't block) anything that looks off:
- **Cross-Platform Parity:** if the diff touches `macos/` or `linux/` scripts, check whether `windows/` has a matching change (and vice versa). A platform-only fix can be intentional — just surface it.
- **No `exit` in install/uninstall scripts:** flag any new `exit` call added to `install.*`/`uninstall.*` (Windows scripts run via `irm | iex`, so `exit` would kill the user's shell session).
- **Silent Degradation:** flag any new `stderr` output, or a `statusline.*`/`notify.*`/`git-refresh.*`/`subagent-statusline.*` code path that could skip its final `exit 0`.

- **Large single-file diffs** (>500 lines): summarize at the file level using `--stat` rather than line-by-line.
- **Binary files**: note their paths in the analysis but don't attempt to describe content changes.

## Step 3 -- Write the commit message

**Match the repo's existing commit style.** Check `git log --oneline -5`. If the repo uses conventional commit prefixes (`fix:`, `feat:`, etc.), use them. If the repo uses plain imperative sentences without prefixes, do the same. When there's no clear pattern (new repo, mixed styles), default to conventional commits: `type(optional-scope): summary`.

**Summary line:**
- Imperative mood ("Add", "Fix", "Refactor")
- No period at the end
- Lowercase after any prefix colon
- **Must be 72 characters or fewer.** After drafting, count the characters. If over 72, rewrite shorter -- drop scope, generalize wording, or move detail to the body. Recount until it fits.

**Body:**
- Blank line between summary and body
- Explain what and why, not how
- **Always use bulleted lists** (`- ` prefix) -- never prose paragraphs
- **Group bullets into labeled sections** when changes span 3+ distinct areas:

```
Installers:
- Guard notify-config.json writes so re-runs preserve prefs
- Fix visual toggle fallback when user is not prompted

Statusline:
- Fix Windows cwd collapse matching partial usernames
- Suppress stderr on printf calls (macOS/Linux)
```

- Use a flat bullet list when changes are in one area or closely related
- If changes affect multiple platforms, note which ones in the relevant bullet

## Step 4 -- Verify before committing

Before running the commit, check your own work:

1. **Count the summary line** -- confirm it is 72 characters or fewer. If not, rewrite and recount.
2. **Cross-check file coverage** -- every file from `git status` should appear in the body or be explicitly noted as excluded (e.g., binary, untracked infrastructure). Don't silently drop files.
3. **Confirm no secrets or stray local paths** -- re-scan for `.env`, key files, credentials, and hardcoded personal paths in the staging list. Warn if found.
4. **Confirm rule compliance** -- re-check the cross-platform-parity, no-`exit`, and silent-degradation flags from Step 2. Surface anything still outstanding.

## Step 5 -- Stage and commit

Once the message passes Step 4, **preview it to the user**, then stage and commit directly via the Bash tool. (If the user only asked you to *describe* or *draft* the changes rather than commit, stop after the preview — don't commit.)

**Stage** the specific files (never `git add -A`) — but only when the index started empty. If `git diff --cached` already showed staged changes in Step 1, commit exactly what is staged and skip `git add` entirely: the message covers staged changes only, and `git add` on a partially staged file would absorb the hunks the user deliberately left out.

```bash
git add path/to/file1 path/to/file2   # only when nothing was staged beforehand
```

**Commit** with a quoted heredoc. Your Bash tool runs Git Bash on every platform, so this one form always works and keeps backticks and markdown literal. Pass a short timeout so a stray passphrase prompt can't hang the session:

```bash
git commit -m "$(cat <<'EOF'
Fix statusline bugs and harden installers

Installers:
- Guard notify-config.json writes so re-runs preserve prefs
- Add Bash 4+ version check to macOS installer

Statusline:
- Fix cwd collapse matching partial usernames
- Suppress stderr on printf calls
EOF
)"
```

**Confirm the result** so the good signature is visible:

```bash
git log --show-signature -1 --oneline
```

### Fallback -- hand the commands to the user

If a direct commit fails (a signing passphrase prompt, a failing pre-commit hook) or the user would rather run it themselves, surface the error and print paste-safe commands for their terminal. List specific files, never `git add -A`. Long single-line commands break at visual wraps, so use multi-line patterns for `git add`.

**On Windows (PowerShell):** use `@(...)` array syntax for `git add` — PowerShell keeps parsing until the `)` closes, so newlines are safe:

~~~
```powershell
git add @(
  "file1.ps1",
  "file2.sh",
  "file3.sh"
)
```
~~~

Use `@'...'@` here-strings for the commit message. The closing `'@` must be at column 0 with no leading spaces:

~~~
```powershell
git commit -m @'
Fix statusline bugs and harden installers

Installers:
- Guard notify-config.json writes so re-runs preserve prefs
- Add Bash 4+ version check to macOS installer

Statusline:
- Fix Windows cwd collapse matching partial usernames
- Suppress stderr on printf calls (macOS/Linux)
'@
```
~~~

If the here-string won't paste cleanly, suggest `git commit` with no `-m` flag to open the user's editor instead.

**On macOS/Linux (Bash):** use `\` line continuation for `git add`:

~~~
```bash
git add \
  file1.sh \
  file2.sh \
  file3.sh
```
~~~

~~~
```bash
git commit -m "$(cat <<'EOF'
Fix statusline bugs and harden installers

Installers:
- Guard notify-config.json writes so re-runs preserve prefs
- Add Bash 4+ version check to macOS installer

Statusline:
- Fix cwd collapse matching partial usernames
- Suppress stderr on printf calls
EOF
)"
```
~~~

## Step 6 -- Check CI

After the commit lands, report where GitHub Actions stands. **This skill never pushes** --
so the commit just made has no run of its own yet, and what you can check depends on whether
its parent is on the remote.

**Skip the whole step, silently, when any of these hold.** None of them is a problem worth a
paragraph:

- No `.github/workflows/*.yml` in the repo
- No GitHub remote (`git remote get-url origin` fails, or is not a github.com URL)
- `gh` is missing, or `gh auth status` fails

```bash
git rev-parse --abbrev-ref HEAD && git status -sb | head -1 && \
  gh run list --branch "$(git rev-parse --abbrev-ref HEAD)" --limit 3 \
    --json databaseId,workflowName,status,conclusion,headSha \
    --jq '.[] | "\(.databaseId)  \(.workflowName)  \(.status)/\(.conclusion // "-")  \(.headSha[0:7])"'
```

**Then say one of three things:**

1. **The new commit is not pushed** (the usual case). Report the *branch's* latest run and
   whose commit it was. If it failed, say so plainly -- the user is about to push on top of a
   red branch and should know before, not after. Offer to watch once they push.
2. **HEAD is already on the remote** and a run exists for this SHA -- watch it to completion.
   Run `gh run watch <id> --exit-status --interval 20` **in the background** with a generous
   timeout; a matrix across three OSes takes minutes, and blocking the session on it is worse
   than being told later.
3. **Green.** One line. Do not enumerate passing jobs.

### When a run failed

Diagnose before offering anything. A failure report that only names the job wastes the round
trip the user then has to spend asking what broke.

```bash
gh run view <id> --json status,conclusion,jobs \
  --jq '.jobs[] | "\(.conclusion // .status)  \(.name)"'
gh run view <id> --log-failed
```

`--log-failed` prints `job<TAB>step<TAB>timestamp message`, one line per log line, with ANSI
escapes intact. Strip the prefixes before quoting any of it -- `sed -E 's/^[^\t]*\t[^\t]*\t//;
s/^[0-9T:.Z-]+ //'` -- and quote the few lines that carry the error, never the whole dump.

The failure itself is on the `##[error]` lines. Do **not** grep for `::error::`: a `run:`
block's source is echoed into the log before it executes, so that pattern matches the script
text that *emits* the message as well as the message, and the two read alike at a glance.

```bash
gh run view <id> --log-failed | sed -E 's/^[^\t]*\t[^\t]*\t//; s/^[0-9T:.Z-]+ //' \
  | grep -a '##\[error\]'
```

`##[error]Process completed with exit code 1` is the step's epitaph, not its cause -- the line
above it usually is. And a failing step's *own* output often is not in `--log-failed` at all
when the thing that failed ran in an earlier step: use `gh run view <id> --log` filtered to
the job when the error line alone does not explain itself.

Then report:

- **Which jobs failed**, and which passed. A partial failure across a matrix is a different
  problem from a total one: one leg red usually means a platform-specific cause, all legs red
  usually means the change itself.
- **The verbatim error line**, cited by job and step.
- **A specific diagnosis**, and the fix you would make.

**Then stop and ask.** Do not edit, amend, or re-commit unprompted. A CI failure is often a
judgment call -- the workflow can be wrong rather than the code, and a green-by-deletion fix
is worse than a red run.

**Two traps worth checking before blaming the code:**

- **Did the job test what it claims?** A step that shells out to something exiting 0 on
  failure passes while the thing it tested failed. Confirm the failing assertion is about the
  code and not about the harness around it.
- **Is the failure environmental?** Runner paths, permissions and preinstalled tooling differ
  from a developer machine in ways that fail honestly. A guard that correctly refuses a
  runner's world-writable temp directory is the guard working, and the fix belongs in the
  workflow.

## Edge cases

- **Mixed staged and unstaged**: commit message covers staged changes only; list unstaged changes separately
- **Large diffs (>20 files)**: lead with `--stat`, group by area, suggest splitting if changes are logically independent
- **Only untracked files**: stage and commit them normally -- describe what they add in the body
- **Binary files**: note paths, don't describe content
- **Commit fails (signing prompt / hook)**: surface the exact error, don't retry blindly, fall back to the Step 5 paste-safe hand-off
- **Merge conflict markers**: refuse to produce a commit message until resolved
