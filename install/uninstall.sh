#!/usr/bin/env bash
# Removes the claude-statusline binary and every settings.json entry the
# installer wrote. Covers macOS and Linux from one file.
#
# No `set -e` and no bare `exit` -- see install.sh for why both matter when the
# published one-liner pipes this into the user's live shell.

CLAUDE_DIR="$HOME/.claude"
BIN_DIR="$CLAUDE_DIR/bin"
BIN_PATH="$BIN_DIR/claude-statusline"
SETTINGS_PATH="$CLAUDE_DIR/settings.json"
NOTIFY_CONFIG_PATH="$CLAUDE_DIR/notify-config.json"
ICON_PATH="$CLAUDE_DIR/claude-icon.png"
MODEL_WINDOWS_PATH="$CLAUDE_DIR/statusline-model-windows.json"
STAGE_PREFIX=".claude-statusline.stage."

RESET=$'\033[0m'
BOLD=$'\033[1m'
DIM=$'\033[2m'
CYAN=$'\033[36m'
GREEN=$'\033[32m'
YELLOW=$'\033[33m'
GRAY=$'\033[90m'

step() { printf "  ${CYAN}${BOLD}>>>${RESET} %s\n" "$1"; }
ok()   { printf "  ${GREEN}${BOLD} +${RESET} %s\n" "$1"; }
warn() { printf "  ${YELLOW}${BOLD} !${RESET} %s\n" "$1"; }
info() { printf "  ${DIM}   %s${RESET}\n" "$1"; }

echo ""
printf "\n  ${DIM}claude-statusline uninstaller${RESET}\n"
printf "  ${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}\n"
echo ""

# --- settings.json first, while the binary that can edit it still exists ---
# Order matters: the merge logic lives in the binary, so removing the entries
# has to happen before removing the tool that removes them. If the binary is
# already gone the entries are reported for manual removal rather than left
# silently in place.
step "Updating Claude Code settings"
if [[ ! -f $SETTINGS_PATH ]]; then
    warn "settings.json not found"
elif [[ -x $BIN_PATH ]]; then
    if _rm_err=$("$BIN_PATH" settings remove --binary "$BIN_PATH" 2>&1); then
        ok "Removed statusline entries and hooks from settings.json"
        info "$SETTINGS_PATH"
    else
        warn "Failed to update settings.json"
        [[ -n $_rm_err ]] && info "$_rm_err"
        info "Remove the \"statusLine\", \"subagentStatusLine\" and claude-statusline hook entries manually"
    fi
else
    warn "Binary already removed — cannot edit settings.json automatically"
    info "Remove the \"statusLine\", \"subagentStatusLine\" and claude-statusline hook entries manually"
    info "$SETTINGS_PATH"
fi
echo ""

# --- Binary ---
step "Removing the binary"
if [[ -f $BIN_PATH ]]; then
    rm -f "$BIN_PATH" && ok "Deleted $BIN_PATH" || warn "Could not delete $BIN_PATH"
else
    warn "Binary not found (already removed?)"
fi

# Anything an interrupted install left staged, plus the self-check log and
# quarantined binary a failed install leaves behind for diagnosis.
rm -f "$BIN_DIR/$STAGE_PREFIX"* \
    "$BIN_DIR/claude-statusline.self-check.txt" \
    "$BIN_DIR/claude-statusline.failed" 2>/dev/null

# Only if we created it and it is now empty -- the user may keep other tools here.
if [[ -d $BIN_DIR ]] && [[ -z "$(ls -A "$BIN_DIR" 2>/dev/null)" ]]; then
    rmdir "$BIN_DIR" 2>/dev/null && ok "Removed empty $BIN_DIR"
fi
echo ""

# --- Notification icon ---
step "Removing the notification icon"
if [[ -f $ICON_PATH ]]; then
    rm -f "$ICON_PATH" && ok "Deleted $ICON_PATH"
else
    info "Icon not found (not installed)"
fi
echo ""

# --- Data stores ---
# The learned model-to-window map is data this tool created, so it goes.
step "Removing data files"
if [[ -f $MODEL_WINDOWS_PATH ]]; then
    rm -f "$MODEL_WINDOWS_PATH" && ok "Deleted $MODEL_WINDOWS_PATH"
else
    info "No learned model-window map to remove"
fi

# Per-session state lives in the temp directory and is keyed by session id.
_tmpdir="${TMPDIR:-/tmp}"
_tmpdir="${_tmpdir%/}"
# statusline-oc-* is kept in the list even though the binary never writes one:
# it cleans up after a script-era install that did.
rm -f "$_tmpdir"/statusline-oc-*.txt "$_tmpdir"/statusline-git-*.txt \
      "$_tmpdir"/statusline-tasks-*.json "$_tmpdir"/statusline-notify-*.json \
      "$_tmpdir"/statusline-sa-*.txt "$_tmpdir"/statusline-tokens-*.txt       "$_tmpdir"/statusline-focus-*.json 2>/dev/null
# Current installs group the same files under claude-statusline-<uid>. The flat
# globs above stay: a session upgraded mid-flight leaves its files behind in the
# old layout, and nothing at runtime ever sweeps them.
#
# Scoped to this user's own id rather than a claude-statusline-* glob. The test
# harness stages claude-statusline-test-* scratch roots in this same directory,
# the README's manual verification downloads claude-statusline-checksums.txt
# here, and on a shared /tmp another user's state directory matches the prefix
# too -- none of which this uninstaller may remove.
_uid="$(id -u 2>/dev/null || true)"
if [[ -n $_uid && -d "$_tmpdir/claude-statusline-$_uid" ]]; then
    rm -rf "$_tmpdir/claude-statusline-$_uid" 2>/dev/null
fi
ok "Cleared temporary session state"
echo ""

# --- Notification config and debug log ---
# Both are removed unconditionally, which is what the script uninstaller did.
# The uninstaller preserves today's prompts, and today there is none here --
# adding one would be a UX change smuggled in under a port.
step "Removing notification configuration"
if [[ -f $NOTIFY_CONFIG_PATH ]]; then
    rm -f "$NOTIFY_CONFIG_PATH" && ok "Deleted $NOTIFY_CONFIG_PATH"
else
    info "No notification config found"
fi

if [[ -f "$CLAUDE_DIR/statusline-debug.log" ]]; then
    rm -f "$CLAUDE_DIR/statusline-debug.log" && ok "Deleted $CLAUDE_DIR/statusline-debug.log"
fi

echo ""
printf "  ${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}\n"
printf "  ${GREEN}${BOLD}Done!${RESET} Restart Claude Code to apply.\n"
echo ""
