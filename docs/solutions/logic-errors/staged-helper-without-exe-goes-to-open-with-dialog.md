---
module: installer
date: 2026-09-20
problem_type: logic_error
component: tooling
severity: high
symptoms:
  - "A fresh Windows install raised a 'Select an app to open this .focus file' dialog mid-run"
  - "The installer then reported 'Click handling not installed - the helper did not run cleanly'"
  - "settings.json was written but the claude-statusline: URI handler was never registered"
  - "Every gate before the smoke had passed: download, checksum, attestation"
root_cause: wrong_api
resolution_type: code_fix
tags:
  - powershell
  - start-process
  - shell-execute
  - file-extension
  - installer
  - click-helper
  - windows
  - dev-channel
---

# A staged helper named `.focus` went to the shell's open-with dialog instead of running

## Problem

`install.ps1` stages every download under `$binDir` with a per-process prefix and
moves it into place only after it passes its gates. The main binary is staged as
`.claude-statusline.stage.<pid>` and the click helper as
`.claude-statusline.stage.<pid>.focus` -- names chosen so a crashed run leaves
something the next run's sweep recognises, not something that looks installed.

The helper has one gate the binary does not: it is **run in place** before it is
placed, through `Invoke-Binary -Gui`, which uses `Start-Process -Wait -PassThru`
because a GUI-subsystem process returns to `&` the moment it starts. And
`Start-Process` resolves a file the way the shell does, **by extension**. A file
ending in `.focus` has no verb, so the shell asked the user which app to open it
with, the helper never ran, the smoke reported "did not run cleanly", and the
installer -- correctly, given what it was told -- deleted the helper it had just
verified and skipped the URI registration.

## Symptoms

- A modal "Select an app to open this .focus file" dialog listing Cursor,
  Firefox, Notepad and so on, in the middle of an otherwise silent install.
- `! Click handling not installed - the helper did not run cleanly`, then at the
  end `Click handling not enabled - the helper did not run cleanly`.
- No `claude-statusline:` key under `HKCU\Software\Classes`; toasts fired without
  a launch URI.
- Dismissing the dialog leaves an `OpenWithList` entry for `.focus` under the
  user's `Explorer\FileExts`. Harmless, and a tell that this was hit.

## Why nothing caught it

Three separate verifications each covered everything except this step:

1. **The local end-to-end tests placed the helper by hand** (the README's manual
   install path), so the staged name was never executed.
2. **CI's installer job installs `--pre`**, which resolved `v1.0.0-rc.2` -- a
   release with no helper asset. The helper branch of the installer logged "not
   published for this tag" and was skipped on every green run.
3. **`tests/equivalence.rs` asserts the installer's text**: that the smoke comes
   before placement, that a failing smoke deletes the stage, that the smoke goes
   through the `-Gui` switch. All true. None of them can see what
   `Start-Process` does with a name.

The first real dev-channel install -- the first time the helper was downloaded
by the installer rather than copied beside it -- found it in one run. That is
the dev channel doing its job: the full install path, on real release assets,
without cutting a tag.

## Resolution

Stage the helper as `.claude-statusline.stage.<pid>.focus.exe`. One line in
`install.ps1`, with a comment explaining why the helper's stage name ends in
`.exe` and the binary's does not: the binary is only hashed and moved, the
helper is executed in place. Every sweep and cleanup path matches on the prefix,
so the longer name needs nothing else.

Verified by running the installer's exact smoke call --
`Start-Process -FilePath <stage> -Wait -PassThru -WindowStyle Hidden` -- against a
copy of the built helper under the new name, in Windows PowerShell 5.1 and
PowerShell 7: `Ran=True Code=0`, no dialog.

`the_installers_place_register_and_remove_the_click_helper_in_order` now asserts
the stage name ends in `.focus.exe`.

## Prevention

- **Anything executed in place must carry the extension the launcher keys on.**
  `&` and `Start-Process` differ here: `&` will run an extensionless PE, and
  `Start-Process` will not. A stage name is part of the execution contract, not
  just a cleanup convention.
- **A gate that has never seen a real subject has not been tested.** The helper
  path had checksum, attestation, smoke and placement logic, and every one of
  them ran for the first time on a user's machine. When a release asset is new,
  the first install from a build that actually contains it is the test, and the
  dev channel exists so that install can happen before a tag does.
- **The safe direction hid the defect.** Deleting a helper that "did not run
  cleanly" is the right call, and it turned a launcher mismatch into a warning
  line that a user would reasonably read as their own mistake. When a refusal
  path can be reached by a cause other than the one it was written for, the
  message should name what was observed (`Start-Process` raised, exit code N)
  rather than the conclusion drawn from it.
