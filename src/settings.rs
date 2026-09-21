//! `settings.json` merge, shared by both installers.
//!
//! The installers cannot use `jq`: a fresh install has to complete on a machine
//! with no `jq` and no package manager. By then a checksum-verified binary that
//! already links a JSON implementation is on disk, so the merge lives here
//! rather than hand-rolled twice in two shell dialects against arbitrary user
//! JSON. Everything below preserves unrelated content: `settings.json` is the
//! user's file, and this code owns exactly the entries it wrote.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Default location: `~/.claude/settings.json`.
pub fn default_path() -> Option<PathBuf> {
    crate::claude_dir().map(|d| d.join("settings.json"))
}

/// Reads `settings.json`, treating an absent file as an empty object. A file
/// that exists but does not parse is an error, never an empty object: starting
/// fresh would silently discard everything the user configured.
pub fn load(path: &Path) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        // A root that parses but is not an object is refused here rather than
        // downstream: `ensure_object` would replace it with an empty map and
        // the install would report success having discarded the file.
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.is_object() => Ok(v),
            Ok(_) => Err(format!(
                "{} is valid JSON but not an object; refusing to replace it",
                path.display()
            )),
            Err(e) => Err(format!("{} is not valid JSON: {e}", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Writes `settings.json` atomically: Claude Code may read it at any moment,
/// and a partial write would be read as corrupt.
pub fn save(path: &Path, root: &Value) -> Result<(), String> {
    let mut text =
        serde_json::to_string_pretty(root).map_err(|e| format!("cannot serialise: {e}"))?;
    text.push('\n');

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }

    // Through the link, not over it: `settings.json` is often a
    // dotfiles-managed symlink, and renaming onto the link name would replace
    // it with a regular file, silently detaching the user's config.
    // `canonicalize` falls back to the given path on a first install. Not
    // `state::write_guarded`, which refuses a symlink outright: right for a
    // cache in a shared temp directory, wrong for the user's own arrangement.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    // Beside the resolved target so the rename stays on one filesystem.
    let tmp = target.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(&tmp, text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", target.display())
    })
}

/// The matcher today's PostToolUse hook registers. Kept verbatim: an install
/// has to write the same hook entries it always did.
pub const POST_TOOL_MATCHER: &str = "Edit|Write|MultiEdit|Bash|NotebookEdit";

/// Claude Code's refresh cadence for the status line, in seconds. Written only
/// when the entry does not already carry one, so a user who tuned it keeps
/// their value across upgrades.
///
/// `2` while the scripts shipped, because PowerShell's ~124 ms startup made a
/// 1-second cadence expensive on Windows and one value had to serve every
/// platform. The interpreter is gone, so the default drops to the minimum
/// the scripts already used on macOS and Linux.
pub const REFRESH_INTERVAL: u64 = 1;

/// The four scripts a pre-binary installation left in `~/.claude`, as
/// basenames: what the script installers matched on when de-duplicating their
/// own entries, so an entry any released installer wrote is recognised here.
const LEGACY_SCRIPTS: [&str; 4] = ["statusline", "notify", "git-refresh", "subagent-statusline"];

/// Both dialects, on both platforms: one list rather than a
/// platform-conditional pair the confinement rule would have to account for.
const LEGACY_EXTENSIONS: [&str; 2] = [".sh", ".ps1"];

/// Whether `command` invokes one of the scripts this binary supersedes.
///
/// The match is on `.claude/<name>` as whole segments. Matching the basename
/// alone would prune a user's own `~/.claude/hooks/notify.sh`; matching as a
/// substring would let `my-statusline.sh` count as `statusline.sh`. Deleting a
/// hook someone wrote is far worse than leaving a stale entry, so this errs
/// toward matching too little.
pub fn references_legacy_script(command: &str) -> bool {
    let command = command.trim_matches('"');
    LEGACY_SCRIPTS.iter().any(|name| {
        LEGACY_EXTENSIONS.iter().any(|ext| {
            mentions_path(command, &format!(".claude/{name}{ext}"))
                || mentions_path(command, &format!(".claude\\{name}{ext}"))
        })
    })
}

/// Whether `command` contains `path` as whole segments: after a separator or
/// the start, and before an argument separator or the end.
fn mentions_path(command: &str, path: &str) -> bool {
    let bytes = command.as_bytes();
    let mut from = 0;
    while let Some(offset) = command[from..].find(path) {
        let start = from + offset;
        let end = start + path.len();
        let after_separator = start == 0 || matches!(bytes[start - 1], b'/' | b'\\');
        let ends_the_token =
            end == bytes.len() || matches!(bytes[end], b' ' | b'\t' | b'"' | b'\'');
        if after_separator && ends_the_token {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Which parts of the integration to write.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApplySpec {
    pub statusline: bool,
    pub subagent: bool,
    pub git_refresh: bool,
    pub notify: bool,
    /// Store the binary path wrapped in quotes.
    ///
    /// Decided here rather than by the caller because the caller is a shell,
    /// and shells eat quotes: PowerShell consumed the quotes of a pre-quoted
    /// argument as delimiters, so the installer handed this code a bare path
    /// and silently wrote an unquoted command that word-split on the first
    /// space in the profile directory.
    pub quote: bool,
}

/// Whether this platform needs the stored command quoted. Windows does, or a
/// profile directory containing a space word-splits the command; Unix entries
/// are stored bare, which is what the shell installers always wrote.
pub const fn quote_for_this_platform() -> bool {
    cfg!(windows)
}

/// The notification events that get their own hook, with the hook event Claude
/// Code fires and the matcher the installer writes for it. `PermissionRequest`
/// and `Stop` carry no matcher; `PreCompact` and `PostCompact` carry `*`. The
/// asymmetry is what today's installer produces, and an install must preserve
/// today's entries, so it is reproduced rather than tidied.
const NOTIFY_HOOKS: [(&str, Option<&str>, &str); 4] = [
    ("PermissionRequest", None, "permission"),
    ("Stop", None, "stop"),
    ("PreCompact", Some("*"), "compaction_start"),
    ("PostCompact", Some("*"), "compaction_done"),
];

/// Applies the requested entries to `root`, replacing any this tool wrote
/// before and leaving everything else untouched. `binary` is the bare command
/// reference; `spec.quote` decides whether it is stored quoted.
pub fn apply(root: &mut Value, binary: &str, spec: &ApplySpec) {
    ensure_object(root);

    // Whatever this writes supersedes a script installation's entries, and by
    // now the scripts themselves are already gone. Pruning unconditionally
    // rather than behind a caller flag is deliberate: a platform whose
    // installer forgot the flag would leave `settings.json` pointing at a file
    // the same run deleted, exactly the asymmetry the parity rule existed to
    // catch. One cosmetic consequence: a `statusLine` that was a script's moves
    // to the end of the file on the migrating run, because the key is removed
    // and re-added rather than overwritten in place; the user's own keys keep
    // their positions (`existing_key_order_is_preserved`).
    //
    // Captured before `remove_legacy` deletes those entries: a migrating user's
    // own keys live in that object and should survive the migration as they
    // survive an ordinary upgrade.
    let prior_statusline = root.get(STATUS_LINE).cloned();
    let prior_subagent = root.get(SUBAGENT_STATUS_LINE).cloned();

    remove_legacy(root);

    let binary: &str = &if spec.quote && !binary.starts_with('"') {
        format!("\"{binary}\"")
    } else {
        binary.to_string()
    };

    if spec.statusline {
        merge_entry(
            root,
            STATUS_LINE,
            prior_statusline,
            binary,
            &[("refreshInterval", json!(REFRESH_INTERVAL))],
        );
    }

    if spec.subagent {
        merge_entry(
            root,
            SUBAGENT_STATUS_LINE,
            prior_subagent,
            &format!("{binary} subagent"),
            &[],
        );
    }

    if spec.git_refresh {
        // `async: true` is preserved deliberately: the entries have to match
        // what the installer always wrote, and without it the hook runs
        // synchronously inside every file-modifying tool call.
        set_hook(
            root,
            "PostToolUse",
            Some(POST_TOOL_MATCHER),
            binary,
            &format!("{binary} git-refresh"),
        );
    }

    if spec.notify {
        for (event, matcher, arg) in NOTIFY_HOOKS {
            set_hook(
                root,
                event,
                matcher,
                binary,
                &format!("{binary} notify {arg}"),
            );
        }
    }
}

/// Writes our entry at `key` over whatever was already there, keeping the
/// user's own keys. `type` and `command` are always rewritten, since an upgrade
/// has to repoint the command; `defaults` are written only when absent.
///
/// Replacing the whole object, as this used to, silently deleted a customised
/// `refreshInterval` and any `padding` on every upgrade, both of which README
/// documents as things to tune. Order is preserved on both paths:
/// `serde_json`'s `preserve_order` map overwrites an existing key in place and
/// appends a new one, so a fresh install still emits `type`, `command`,
/// `refreshInterval`.
fn merge_entry(
    root: &mut Value,
    key: &str,
    prior: Option<Value>,
    command: &str,
    defaults: &[(&str, Value)],
) {
    // A prior value that is not an object carries nothing worth keeping.
    let mut entry = match prior {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };

    entry.insert("type".to_string(), json!("command"));
    entry.insert("command".to_string(), json!(command));
    for (name, value) in defaults {
        entry
            .entry(name.to_string())
            .or_insert_with(|| value.clone());
    }

    root[key] = Value::Object(entry);
}

/// Removes every entry whose command references `binary`, and prunes the
/// containers that leaves empty. Matching is by substring with surrounding
/// quotes stripped, so a quoted Windows entry is still recognised when the
/// uninstaller passes the bare path.
pub fn remove(root: &mut Value, binary: &str) {
    remove_matching(root, &|command| references(command, binary));
}

/// Removes every entry left by a script installation. Same traversal as
/// `remove`: two hand-written walks would eventually prune different things,
/// and only one of them would have a test.
pub fn remove_legacy(root: &mut Value) {
    remove_matching(root, &references_legacy_script);
}

fn remove_matching(root: &mut Value, matches: &dyn Fn(&str) -> bool) {
    let Some(map) = root.as_object_mut() else {
        return;
    };

    for key in [STATUS_LINE, SUBAGENT_STATUS_LINE] {
        if map
            .get(key)
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            .is_some_and(matches)
        {
            map.remove(key);
        }
    }

    let Some(hooks) = map.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };

    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(entries) = hooks.get_mut(&event).and_then(Value::as_array_mut) else {
            continue;
        };
        entries.retain(|entry| !entry_matches(entry, matches));
        if entries.is_empty() {
            hooks.remove(&event);
        }
    }

    // Uninstall should leave no trace it can avoid, an empty `hooks` included.
    if hooks.is_empty() {
        map.remove("hooks");
    }
}

/// Whether an entry referencing `binary` is already present for `feature`:
/// the installer's "Already configured" path, so a re-run does not re-prompt.
pub fn has(root: &Value, binary: &str, feature: &str) -> bool {
    let present = |key: &str| {
        root.get(key)
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            .is_some_and(|c| references(c, binary))
    };

    match feature {
        "statusline" => present(STATUS_LINE),
        "subagent" => present(SUBAGENT_STATUS_LINE),
        "git-refresh" => hook_present(root, "PostToolUse", binary),
        "notify" => NOTIFY_HOOKS
            .iter()
            .any(|(event, _, _)| hook_present(root, event, binary)),
        _ => false,
    }
}

/// Whether any entry at all exists for `key`, ours or not. The installer asks
/// before overwriting a `statusLine` it did not write.
pub fn has_foreign(root: &Value, binary: &str, feature: &str) -> bool {
    let occupied = |key: &str| {
        root.get(key).is_some_and(|v| {
            let command = v.get("command").and_then(Value::as_str);
            // A script installation's entry is our own previous entry, not a
            // stranger's: prompting to overwrite it would ask the user to
            // approve replacing us with us, and a declined prompt would leave
            // `settings.json` pointing at a script the same run deletes.
            !v.is_null()
                && !command.is_some_and(|c| references(c, binary))
                && !command.is_some_and(references_legacy_script)
        })
    };
    match feature {
        "statusline" => occupied(STATUS_LINE),
        "subagent" => occupied(SUBAGENT_STATUS_LINE),
        _ => false,
    }
}

/// Whether a script installation's entry is present, for `feature` or
/// anywhere. Carries a user's existing choices across the upgrade: someone who
/// enabled notifications under the scripts is neither re-prompted nor silently
/// loses them.
pub fn has_legacy(root: &Value, feature: Option<&str>) -> bool {
    let key_is_legacy = |key: &str| {
        root.get(key)
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            .is_some_and(references_legacy_script)
    };

    match feature {
        Some("statusline") => key_is_legacy(STATUS_LINE),
        Some("subagent") => key_is_legacy(SUBAGENT_STATUS_LINE),
        Some("git-refresh") => hook_matches(root, "PostToolUse", &references_legacy_script),
        Some("notify") => NOTIFY_HOOKS
            .iter()
            .any(|(event, _, _)| hook_matches(root, event, &references_legacy_script)),
        Some(_) => false,
        // Scans every event, not only the ones we write, so an entry from an
        // installer version this one does not know about still counts.
        None => {
            key_is_legacy(STATUS_LINE)
                || key_is_legacy(SUBAGENT_STATUS_LINE)
                || root
                    .get("hooks")
                    .and_then(Value::as_object)
                    .is_some_and(|hooks| {
                        hooks.values().any(|entries| {
                            entries.as_array().is_some_and(|list| {
                                list.iter()
                                    .any(|e| entry_matches(e, &references_legacy_script))
                            })
                        })
                    })
        }
    }
}

const STATUS_LINE: &str = "statusLine";
const SUBAGENT_STATUS_LINE: &str = "subagentStatusLine";

fn ensure_object(root: &mut Value) {
    if !root.is_object() {
        // Replacing a non-object root loses user content, so this only fires
        // for input the caller has already decided to discard: `load` refuses
        // a file that is valid JSON but not an object.
        *root = Value::Object(Map::new());
    }
}

fn references(command: &str, binary: &str) -> bool {
    let needle = binary.trim_matches('"');
    !needle.is_empty() && command.trim_matches('"').contains(needle)
}

fn entry_matches(entry: &Value, matches: &dyn Fn(&str) -> bool) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(matches)
            })
        })
}

fn entry_references(entry: &Value, binary: &str) -> bool {
    entry_matches(entry, &|command| references(command, binary))
}

fn hook_matches(root: &Value, event: &str, matches: &dyn Fn(&str) -> bool) -> bool {
    root.get("hooks")
        .and_then(|h| h.get(event))
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.iter().any(|e| entry_matches(e, matches)))
}

fn hook_present(root: &Value, event: &str, binary: &str) -> bool {
    hook_matches(root, event, &|command| references(command, binary))
}

/// Replaces our entry for `event`, preserving every entry that is not ours so
/// a user's own hook on the same event survives an install.
///
/// `binary` is passed in rather than recovered by splitting `command` on its
/// first space: `quote_for_this_platform` quotes on Windows alone, so a Unix
/// `$HOME` with a space yielded a truncated key, and since `references`
/// matches on substring the retain below deleted every unrelated hook that
/// merely mentioned the home directory.
fn set_hook(root: &mut Value, event: &str, matcher: Option<&str>, binary: &str, command: &str) {
    let hooks = root
        .as_object_mut()
        .expect("root is an object by this point")
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }

    let entries = hooks
        .as_object_mut()
        .expect("hooks is an object by this point")
        .entry(event)
        .or_insert_with(|| Value::Array(Vec::new()));
    if !entries.is_array() {
        *entries = Value::Array(Vec::new());
    }

    let list = entries.as_array_mut().expect("entries is an array");
    // Drop our previous entry first, or a re-run accumulates duplicates.
    list.retain(|e| !entry_references(e, binary));

    let mut entry = Map::new();
    if let Some(m) = matcher {
        entry.insert("matcher".into(), json!(m));
    }
    entry.insert(
        "hooks".into(),
        json!([{ "type": "command", "command": command, "async": true }]),
    );
    list.push(Value::Object(entry));
}

/// What `settings protocol register` does with the open command found under
/// the scheme's key: writes when absent or already ours, rewrites a
/// stale path of ours in place, and leaves a foreign command alone and says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolRegistration {
    /// Nothing is registered: write every value.
    Write,
    /// A `claude-statusline-focus.exe` registered elsewhere: rewrite in place.
    Rewrite,
    /// Exactly this helper is registered already: change nothing.
    Unchanged,
    /// Another application owns the scheme: leave it and say so.
    Foreign(String),
}

/// True when a registered open command names a `claude-statusline-focus.exe`,
/// in any directory: our registration, possibly from an earlier install path.
pub fn names_our_helper(command: &str) -> bool {
    command
        .to_ascii_lowercase()
        .contains(&format!("\\{}\"", crate::cmd::notify::FOCUS_HELPER))
}

/// The registration decision for `register`, from the current command and the
/// helper beside the binary.
pub fn protocol_registration(current: Option<&str>, helper: &Path) -> ProtocolRegistration {
    match current {
        None => ProtocolRegistration::Write,
        Some(c) if c.trim().is_empty() => ProtocolRegistration::Write,
        Some(c) if crate::cmd::notify::handler_command_matches(c, helper) => {
            ProtocolRegistration::Unchanged
        }
        Some(c) if names_our_helper(c) => ProtocolRegistration::Rewrite,
        Some(c) => ProtocolRegistration::Foreign(c.to_string()),
    }
}

/// A helper path the registration refuses: a double quote would end the
/// quoted command early, and `%` is expanded by the shell when the command
/// runs, so either could turn the path into a different command.
pub fn helper_path_is_registrable(helper: &Path) -> bool {
    let text = helper.to_string_lossy();
    !text.contains('"') && !text.contains('%') && !text.chars().any(char::is_control)
}
