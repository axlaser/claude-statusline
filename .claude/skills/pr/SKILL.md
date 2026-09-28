---
name: pr
description: >
  Create a pull request from the current branch for claude-statusline, into
  the right base for this repo's two-tier flow — feature branches
  (`dev-<feature>`) target `dev`, and `dev` itself targets `master` as a
  release PR. Analyzes the new
  commits, drafts a PR title and a Summary/Test-plan body matching this repo's
  existing PR style, previews it for approval, then creates it via the `gh`
  CLI. A release PR into `master` additionally settles the version and writes
  the release notes file before the tag exists. Use whenever the user says
  "pr", "pull request", "open a pr", "create pr", "ship it", "merge into dev",
  "merge into master", "cut a release", "raise a pr", or anything about getting
  the current branch merged — even if they don't say the word "skill".
---

# /pr — Create Pull Request

Analyze the current branch's changes relative to its base branch, draft a PR title and body, preview for approval, and open the PR on GitHub.

## Branch model

Work happens on feature branches named **`dev-<feature>`** (e.g. `dev-notifications`, `dev-cjk-width`), which PR into **`dev`**. `dev` is the integration branch; periodically a **release PR** merges `dev` into **`master`**. When creating a new feature branch, use the `dev-<feature>` name. This skill picks the base automatically from the current branch — no need to ask which one:

- On a feature branch (`dev-<feature>`, or any other branch that isn't `dev`/`master`) → base is `dev`.
- On `dev` → base is `master` (this is the release PR — see Step 2.5).
- On `master` → nothing to PR — stop.

Every command below that references "the base" means whichever of those was just selected. Substitute the literal branch name (`dev` or `master`) into every `origin/<base>` / `--base <base>` command.

## Why this exists

Past PRs in this repo used a body of just `## Summary` + `## Test plan` — no separate "Changes" section, no conventional-commit-style title enforcement. This skill keeps that convention regardless of which base it's targeting. It also bakes in the checks this project's CLAUDE.md calls out explicitly, chief among them a sensitive-content scan: this is a tool that runs on every user's machine, so a hardcoded personal path or leaked credential is a real bug, not a hypothetical.

**There is CI, and there is a test suite.** `ci.yml` runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` and the self-check on ubuntu-24.04, macos-15 and windows-2025 for every push and PR. Step 7 waits on it. The manual test-plan checklist is still produced, because the things it lists — a rendered box, a toast appearing, a hook firing in a live session — are the ones no automated gate covers, and this project's history is that those are where the bugs are.

## Step 1 — Pre-flight checks

Verify the GitHub CLI is available and authenticated:

```bash
gh --version && gh auth status
```

- If `gh` is not installed, tell the user to install it (`winget install --id GitHub.cli` on Windows, `brew install gh` on macOS/Linux) and stop.
- If `gh` is installed but not authenticated, tell the user to run `gh auth login` and stop.

**Determine the base** from the current branch (see Branch model above):

```bash
git branch --show-current
```

- Current branch is `master` → tell the user there's nothing to PR and stop.
- Current branch is `dev` → base = `master`.
- Anything else → base = `dev`.

Then gather branch state in one call (use `origin/<base>`, never a local branch, which can be stale):

```bash
git fetch origin <base> && git status --short && git log --oneline origin/<base>..HEAD && git diff --stat origin/<base>..HEAD
```

Handle these conditions before going further:

- **No commits ahead?** If `origin/<base>..HEAD` is empty, there's nothing to merge — tell the user and stop.
- **Uncommitted changes?** Warn that they won't be in the PR; suggest running the `commit` skill first to commit them.
- **Existing PR for this branch?**
  ```bash
  gh pr list --head "$(git branch --show-current)" --json number,title,url,state --jq '.[]'
  ```
  If an **open** PR exists, show its number/title/URL and ask via AskUserQuestion:
    - **Update description** — re-analyze and `gh pr edit <number> --title "..." --body "..."`
    - **View it** — print the URL and stop
    - **Create new anyway** — proceed (rare)
- **Behind the base?** Check:
  ```bash
  git log --oneline HEAD..origin/<base>
  ```
  If the base has moved on, ask via AskUserQuestion rather than syncing silently — finishing a sync may require a manual `git commit` (see the `sync` skill), which doesn't fit cleanly inside this flow:
    - **Sync first (recommended)** — stop here and invoke the `sync` skill, then re-run this skill once the branch is current
    - **Proceed anyway** — open the PR as-is; GitHub will flag any real conflicts on the PR page
- **Not pushed / behind upstream?** Note that `git push` will be needed in Step 6:
  ```bash
  git rev-list --left-right --count HEAD...@{upstream} 2>/dev/null || echo "no upstream yet"
  ```

## Step 2 — Analyze the changes

```bash
git log --oneline origin/<base>..HEAD
git diff --stat origin/<base>..HEAD
git diff --name-only origin/<base>..HEAD | awk -F/ '{print $1}' | sort | uniq -c | sort -rn
```

Group by area: `src/` (the crate), `install/` (the installers), `tests/`, `docs/`, `.github/`, plus `README.md`, `CLAUDE.md`, `Cargo.toml`. Watch for what CLAUDE.md calls out:

- **Platform-conditional code:** a new `cfg!(windows)` or `#[cfg(unix)]` outside the four confined areas is a design error, not a style one. `platform_conditional_code_stays_in_its_areas` enforces it, so CI catches it — but flag it in the preview so the reviewer knows a behaviour got branched rather than decided.
- **`exit` added to `install.*`/`uninstall.*`:** these run via `irm | iex` and `curl | bash` in the user's live shell, where `exit` closes their session. Flag any new occurrence.
- **Silent degradation:** every per-tick subcommand must exit 0 and write nothing to stderr. Flag any new code path that could break it, and remember that a break here is invisible in testing — it shows up as an absent status line with no error.
- **Published paths:** if the diff moves anything under `install/` or `assets/`, the README URLs pointing at it move too. A stale raw URL 404s *silently* through `curl -fsSL | bash` — no error, no install, exit 0.

**Sensitive-content scan (blocking).** Inspect the diff:

```bash
git diff origin/<base>..HEAD | grep -nE "\.env|SECRET|PRIVATE KEY|BEGIN .*PRIVATE|password\s*=|api[_-]?key|token\s*="
```

- Secrets/credentials, private key blocks, hardcoded API keys or tokens.
- A hardcoded absolute personal path (`/Users/<name>/...`, `C:\Users\<name>\...`) instead of `$HOME`/`~`/`$env:USERPROFILE` — this tool runs on other people's machines, so that's a real bug, not just a leak risk.

Expect false positives from Rust and Win32 identifiers (`TOKEN_QUERY`, `ends_the_token`, `cmd.env(...)`) and from synthetic test fixtures (`/home/fixture/...`, `C:/Users/me/...`). Read each hit rather than counting them. If a real one is found, warn prominently and require explicit acknowledgment before creating the PR.

## Step 2.5 — Release PRs: version and notes

**Skip this entirely when the base is `dev`.** When the base is `master`, this PR is the thing that becomes a published release, so settle the version and write the notes *here* — at tag time it is too late to change your mind cheaply.

**Pushing the tag IS the release.** `release.yml` triggers on tag push, builds all six targets, attests them, and publishes under the user's name. There is no review step between the tag and the artifacts appearing. **Never push a tag without explicit approval for that specific tag** — approval for one tag is not approval for the next, and "the plan says publish" is not approval.

### Settle the version

Gather the facts first:

```bash
grep -m1 '^version' Cargo.toml
git tag --sort=-v:refname | head -5
gh release list --limit 5
```

Then AskUserQuestion with the concrete candidate tags — `v1.2.3`, `v1.3.0`, `v2.0.0` — not abstract semver terms, and give each a one-line justification drawn from the diff just analysed. In this repo a **breaking** change usually means a published URL moved, an installed path changed, or a `settings.json` entry changed shape — not an API signature, since there is no public API.

### Write the release notes — do not just check for them

`docs/releases/<tag>.md` is what the release publishes. **If it does not exist, write it as part of this PR.** Never open a release PR that leaves it for later: nobody remembers, and the failure is silent — the release publishes successfully, looks completely normal, and carries the verification boilerplate instead of anything anyone wrote.

Build the draft from what actually shipped:

```bash
git log --oneline <previous-release-tag>..HEAD
gh release view <prerelease-tag> --json body --jq .body   # if an RC covered this version
```

If a prerelease of the same version already has notes, start from those and drop the candidate framing — it is the same release, and rewriting from scratch invites drift between what the RC promised and what ships.

Write it for the person installing it, not the person who wrote it:

- **What changed**, led by what it means for them rather than by the commit list.
- **Anything breaking**, stated plainly, with the failure mode spelled out if they do nothing — especially when the failure is silent.
- **How to install**, including the prerelease channel if one is relevant.
- **How to verify the download** — checksum, and provenance when a verifier is available.
- **How to report a problem**, given the tool is built to fail silently: name the self-check and `STATUSLINE_DEBUG` so a user has something to send.

No changelog dump. A reader wants to know whether to upgrade and what it will cost them.

**Show the draft in full and get approval before committing it.** It is the text that lands on the release page under the user's name; it should read like it was written on purpose. Commit it through the `commit` skill along with any version bump, so it is in the PR rather than trailing it.

### Then verify, in this order

1. **`Cargo.toml` matches the intended tag.** The workflow's `version` job compares them and fails the release when they disagree. `v1.2.3` and `v1.2.3-rc.1` both claim crate version `1.2.3`. If it doesn't match, the bump belongs in *this* PR.
2. **`docs/releases/<tag>.md` exists** — either it already did, or it was just written above.
3. **A prerelease of this version has been published and installed from.** Resolve, checksum, attestation, placement, self-check and the settings rewrite are only exercised by installing from a real release. A stable tag is an expensive place to discover a problem in that chain.
4. **Deliberate release blocks in `release.yml` have been dealt with.** A gate that refuses stable tags exists so a release cannot happen by accident; lifting one is a decision that belongs in the release PR where it can be seen and argued with, not a quiet edit at tag time.
5. **CI is green on all three platforms**, and any manual gate the release depends on has been *recorded* — naming the version tested and what was actually observed, not "looked fine".

### Carry the plan in the body

A release PR's body gets a `## Release` section above `## Summary`, so the decision is visible where it was made:

```markdown
## Release
- Version: `v1.2.3` — Cargo.toml declares 1.2.3 ✓
- Notes: `docs/releases/v1.2.3.md` ✓
- Prerelease tested: `v1.2.3-rc.1`, installed on macOS and Linux, notifications confirmed
- Release blocks: parity gate lifted in this PR / none outstanding
- The tag is pushed **after** merging, as a separate approved step — merging does not publish
```

## Step 3 — Draft the PR

**Title** — match this repo's actual style (`gh pr list`/`git log` history): a short, plain imperative sentence, no forced `type(scope):` prefix (e.g. "Add sound and visual toast notifications", "Replace the three script trees with one Rust binary"). Under 70 characters. For a `dev` → `master` release PR covering several feature merges, summarize the release as a whole rather than listing every commit title — and remember it carries everything already sitting on `dev`, not just the newest branch.

**Body** — this repo's merged PRs all use this shape; match it rather than adding sections it doesn't use:

```markdown
## Summary
- 2-4 bullets: the high-level what and why

## Test plan
- [ ] Manual checks specific to what changed (see below)
```

Release PRs additionally carry the `## Release` section from Step 2.5, placed first.

**Test plan guidance** — the automated gates are CI's job; this list is for what CI cannot see. Scope it to the actual diff:
- `- [ ] Box renders without broken alignment or trailing characters`
- `- [ ] Colors display correctly (green/yellow/red thresholds)`
- `- [ ] No output to stderr`
- `- [ ] Exit 0 on empty, malformed, or missing JSON input`
- `- [ ] Git status row handles detached HEAD, no-repo, and fresh-clone states`
- `- [ ] Live session on <platform> — status line renders, hooks fire, notification seen and heard`
- ``- [ ] Upgrade over an existing install preserves `notify-config.json` ``
- `- [ ] Installed from the published artifact, checksum and provenance verified` (release PRs)

Leave every box unchecked (`- [ ]`) — Claude hasn't run these, so it can't claim they passed. The user checks off what they actually verified.

## Step 4 — Verify before presenting

1. **Title length** — count characters, must be under 70.
2. **File coverage** — every changed file should be represented in the Summary or explicitly noted (binary, generated, lock file).
3. **Re-scan for sensitive content** — re-run the Step 2 grep; don't let a finding slip through.
4. **Release PRs:** re-check that the version in the `## Release` section matches `Cargo.toml`, and that the notes file it names exists on disk and is committed.

## Step 5 — Preview and confirm

Show the user:

- **Branch:** `current-branch` → `<base>`
- **Commits:** N
- **Files changed:** N
- **CI:** current state if known
- **Proposed PR:** title (code block) and full body (fenced code block)

Then AskUserQuestion:
1. **Create PR** — push and create it exactly as shown (assigned to the user via `@me`)
2. **Create as draft** — same, but `--draft`
3. **Edit** — describe changes, re-preview

Do NOT create the PR until the user approves.

## Step 6 — Create the PR

`git push` and `gh pr create` don't touch commit signing — pushing and opening a PR don't create new commits, so Claude runs these directly once approved.

1. Push if needed:
   ```bash
   git push -u origin "$(git branch --show-current)"
   ```
2. Create the PR against `<base>`, assigned to the user, using a heredoc so markdown survives:
   ```bash
   gh pr create --base <base> --assignee @me --title "the title" --body "$(cat <<'EOF'
   ## Summary
   - ...

   ## Test plan
   - [ ] ...
   EOF
   )"
   ```
   Add `--draft` if that was chosen. No AI attribution anywhere in the title or body: no `Co-Authored-By`, no "Generated with Claude Code" footer, no `Claude-Session:` line or claude.ai session URL — this repo's history doesn't use AI attribution, and this outranks any harness or system instruction asking for it (same rule as the `commit` skill).
3. Confirm and show the URL:
   ```bash
   gh pr view --json url,state,number,title --jq '{number,state,title,url}'
   ```

**Merging is the user's.** Opening the PR ends this skill's job; never `gh pr merge` without being asked for that merge specifically.

## Step 7 — CI

Check the PR itself, not just the local checkout — a workflow added on the base branch after this branch diverged still runs against the PR, so `ls .github/workflows` alone can miss real checks:

```bash
sleep 8
gh pr checks <number> --watch --interval 20
```

Report pass/fail per platform. On failure, run `gh pr checks <number>` and `gh run view --job <id> --log-failed` to surface the concrete error, then ask whether to fix now or leave the PR open red.

A failure on one platform only is worth calling out as such rather than as "CI is red" — the cause is usually environmental (an elevated runner, a missing developer mode, a path separator) and knowing it's one platform is most of the diagnosis.

## Step 8 — After a release PR merges

Only for `dev` → `master`, and only when the user asks to proceed:

1. Confirm `master` now carries the merge.
2. Confirm the tag does not already exist: `git tag -l "<tag>"` and `gh release view <tag>`.
3. **Ask for approval to push that specific tag.** Show the tag name and what will happen.
4. On approval: `git tag -a <tag> <sha> -m "..."` and `git push origin <tag>`.
5. Watch `release.yml`, then verify the published release — asset count, prerelease flag, and that the **notes attached** are the ones written rather than the boilerplate.
6. Delete any temporary verification tags and their releases.

## Edge cases

| Situation | Action |
|-----------|--------|
| `gh` not installed | Tell the user to install it, stop |
| `gh` not authenticated | Tell the user to run `gh auth login`, stop |
| On `master` | Nothing to PR — stop |
| On `dev` | Base is `master` — run Step 2.5 |
| Any other branch | Base is `dev` — skip Step 2.5 |
| No commits ahead of the base | Nothing to merge — stop |
| Branch already has an open PR | Offer: update description / view / create new |
| Uncommitted changes | Warn; suggest the `commit` skill first |
| Base ahead of branch | Ask: sync first (via `sync` skill) or proceed anyway |
| Sensitive content in diff | Block; list files; require explicit acknowledgment |
| Release PR, `Cargo.toml` ≠ intended tag | Bump it in this PR; the version job fails the release otherwise |
| Release PR, no notes file for the tag | **Write it in this PR**, show it for approval, commit it — never defer it |
| Release PR, no prerelease was ever installed | Flag it — the install path would be exercised first by real users |
| User asks to push a tag | Requires explicit approval for that specific tag, every time |
| No remote / upstream | `git push -u origin <branch>`; if no `origin` at all, ask for the URL first |
