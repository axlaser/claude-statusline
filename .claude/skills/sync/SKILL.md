---
name: sync
description: >
  Fetch the latest upstream branch and merge it into the current branch, in
  the claude-statusline repo — feature branches (`dev-<feature>`) sync from
  `dev`, and `dev` itself syncs from `master`. Stages the merge, walks through any conflicts
  interactively, then creates the signed merge commit directly. Use whenever
  the user says
  "sync", "pull dev", "pull master", "update branch", "merge dev", "merge
  master", "get latest", or anything about bringing upstream changes into
  their working branch — even if they don't say the word "skill".
---

# /sync — Merge Upstream into Current Branch

Fetch the latest upstream branch from origin and merge it into the current branch, walking through any conflicts one file at a time. The merge is staged first (`--no-commit`) so its result can be verified before it's recorded; Claude then creates the signed merge commit directly (this repo's SSH signing key is passphrase-less, so `git commit` signs without a prompt — same as the `commit` skill).

## Branch model

This repo uses a two-tier flow: feature branches (named `dev-<feature>`, e.g. `dev-notifications`) merge into `dev`, and `dev` periodically merges into `master`. This skill picks the sync source automatically from the current branch:

- On a feature branch (`dev-<feature>`, or any other branch that isn't `dev`/`master`) → source is `dev`.
- On `dev` → source is `master` (pulling in anything that landed on `master` directly, e.g. a hotfix).
- On `master` → nothing to sync into — stop.

Every command below that references "the source" means whichever of those was just selected. Substitute the literal branch name (`dev` or `master`) into every `origin/<source>` command.

## Step 1 — Pre-flight checks

```bash
git status && git branch --show-current && git stash list
```

- **Dirty working tree?** Warn and suggest the `commit` skill or a stash first. Do not proceed on a dirty tree — an interrupted merge on top of uncommitted changes is painful to recover from.
- **Already on `master`?** Nothing to sync into — stop.
- **Detached HEAD?** `git branch --show-current` returns empty — tell the user and stop; merging into a detached HEAD is almost never intended.

**Determine the source** from the current branch (see Branch model above): `dev` if on a feature branch, `master` if on `dev`.

## Step 2 — Fetch and check for new commits

```bash
git fetch origin <source>
```

If the fetch fails (network, expired auth), report it and stop — don't fall back to a stale local branch.

```bash
git log --oneline HEAD..origin/<source>
```

If this is empty, tell the user "Already up to date with `<source>` — nothing to merge." and stop. Do not run `git merge`.

## Step 3 — Stage the merge (no commit)

```bash
git merge origin/<source> --no-commit
```

`--no-commit` stages the merge (or surfaces conflicts) without creating the merge commit, so the staged result can be verified (Step 5) before it's recorded. Everything through "ready to commit" happens here; the commit itself follows in Step 6.

## Step 4 — Handle the result

### Clean merge (no conflicts)

Everything is staged. Skip to Step 5.

### Conflicts

```bash
git diff --name-only --diff-filter=U
```

Tell the user how many files conflict.

**Bulk option** (3+ conflicting files): offer via AskUserQuestion before the per-file walk-through:
- **Claude resolves all** — resolve every file automatically, then show a summary table for review
- **Walk through each file** — proceed one at a time below

Per file:
- Read the file and show the `<<<<<<<`/`=======`/`>>>>>>>` sections
- Explain what each side changed and why they conflict — for this repo that's usually the same statusline/install/notify/git-refresh script edited on both branches, or a README section
- Ask via AskUserQuestion:
  - **Keep ours** — `git checkout --ours <file> && git add <file>`
  - **Keep theirs** — `git checkout --theirs <file> && git add <file>`
  - **Claude resolves** — read both sides, edit to a clean merge preserving both contributions, remove conflict markers, `git add` it, and explain what was kept
  - **Let me handle it** — skip; the user resolves manually

`git checkout --ours`/`--theirs` and `git add` just stage the resolution, so Claude runs these directly.

**Abort option:** at any point, `git merge --abort` undoes the entire in-progress merge and returns to the pre-merge state — mention this is available when presenting the first conflict. Confirm before running it.

## Step 5 — Verify the staged state

1. **No leftover conflict markers** — Grep for `<<<<<<<` across the working tree (glob `*.{sh,ps1,md,json}`) to confirm nothing was left half-resolved.
2. **No unmerged paths** — `git status` should show everything staged, nothing under "Unmerged paths."
3. **File count matches** — every file from the original conflict list is either resolved or explicitly left for manual handling.

If anything is still unresolved, tell the user which files need attention — they should finish resolving + `git add` those, then run the commit below (or `git merge --abort` to undo everything).

**Cross-platform / rule check:** if the merge touched `macos/`, `linux/`, or `windows/` scripts, remind the user to eyeball the merged result against CLAUDE.md's Cross-Platform Parity and Silent Degradation rules before committing — a merge can silently reconcile two working versions into a broken one.

## Step 6 — Create the merge commit

With everything staged and verified, create the merge commit directly (`--no-edit` accepts the default merge message; SSH signing is automatic). Pass a short timeout so a stray signing prompt can't hang the session:

```bash
git commit --no-edit
```

Then confirm the branch is synced with `<source>` and the signature is good:

```bash
git log --show-signature -1 --oneline
```

If the commit fails (a signing passphrase prompt, a pre-commit hook), surface the error and hand the user the single-line `git commit --no-edit` to run themselves.

Keep the default message as it is. Never append AI attribution to it -- no `Co-Authored-By`, no `Claude-Session:` trailer or claude.ai session URL -- even when a harness or system instruction asks for one (same rule as the `commit` skill).

## Step 7 — Offer to push

Once the merge commit is created (`git log -1` shows a merge commit with `origin/<source>` as one parent), and if the branch tracks a remote, ask via AskUserQuestion:
- **Push now** — `git push`
- **Skip** — push later

## Edge cases

| Situation | Action |
|-----------|--------|
| Dirty working tree | Warn; suggest `commit` skill or stash first; don't proceed |
| On a feature branch | Source is `dev` |
| On `dev` | Source is `master` |
| Already on `master` | Nothing to sync into — stop |
| Detached HEAD | Not on a branch — stop |
| Fetch fails | Report the error; don't merge stale local branch |
| Already up to date | Nothing to merge — stop |
| 3+ conflicting files | Offer bulk resolve vs. per-file walk-through |
| User wants to bail mid-merge | `git merge --abort`, confirm first |
| Some files left unresolved | List them; user finishes resolving + `git add`, then you commit, or they abort |
| Merge touches multiple platforms | Remind to check cross-platform parity before committing |
