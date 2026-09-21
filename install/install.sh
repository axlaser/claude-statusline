#!/usr/bin/env bash
# Installs the claude-statusline binary into ~/.claude/bin and registers it in
# settings.json. Covers macOS and Linux from one file.
#
# No `set -e`: this script can be sourced (see CLAUDE.md), and errexit would
# leak into the caller's shell and persist after return. Failures are handled
# explicitly at each critical step instead.
#
# No bare `exit` either. The published one-liner pipes this into a shell, and
# CLAUDE.md's idiom -- `return 1 2>/dev/null || exit 1` -- returns when sourced
# and exits only when that fails, i.e. when running as a subshell. Every abort
# below uses it, and every one of them lives at top level: `return` inside a
# function would unwind the function and carry on.

REPO_SLUG="axlaser/claude-statusline"
SIGNER_WORKFLOW="$REPO_SLUG/.github/workflows/release.yml"

CLAUDE_DIR="$HOME/.claude"
BIN_DIR="$CLAUDE_DIR/bin"
BIN_PATH="$BIN_DIR/claude-statusline"
SETTINGS_PATH="$CLAUDE_DIR/settings.json"
NOTIFY_CONFIG_PATH="$CLAUDE_DIR/notify-config.json"
ICON_PATH="$CLAUDE_DIR/claude-icon.png"
STAGE_PREFIX=".claude-statusline.stage."

# --- Colors & output helpers ---
RESET=$'\033[0m'
BOLD=$'\033[1m'
DIM=$'\033[2m'
CYAN=$'\033[36m'
GREEN=$'\033[32m'
YELLOW=$'\033[33m'
RED=$'\033[31m'
GRAY=$'\033[90m'

step() { printf "  ${CYAN}${BOLD}>>>${RESET} %s\n" "$1"; }
ok()   { printf "  ${GREEN}${BOLD} +${RESET} %s\n" "$1"; }
warn() { printf "  ${YELLOW}${BOLD} !${RESET} %s\n" "$1"; }
err()  { printf "  ${RED}${BOLD} x${RESET} %s\n" "$1"; }
info() { printf "  ${DIM}   %s${RESET}\n" "$1"; }

file_bytes() { wc -c < "$1" | tr -d ' '; }
human_size() {
    local b=$1
    if (( b >= 1048576 )); then awk -v b="$b" 'BEGIN{printf "%.1f MB",b/1048576}'
    elif (( b >= 1024 )); then awk -v b="$b" 'BEGIN{printf "%.1f KB",b/1024}'
    else printf "%d B" "$b"; fi
}

# Removes the staged download. Called on every path that does not place it
# -- a staged file left behind is an unverified binary sitting in the
# install directory under a predictable-ish name.
discard_stage() {
    [[ -n ${STAGE:-} ]] && rm -f "$STAGE"
    [[ -n ${SUMS:-} ]] && rm -f "$SUMS"
    [[ -n ${BUNDLE:-} ]] && rm -f "$BUNDLE"
    return 0
}

# Puts the previous binary back after a failure that has already moved it
# aside. A binary that fails its self-check has to leave the prior
# installation untouched, and by then the new one is already in place -- so
# "untouched" has to be restored rather than merely not disturbed.
restore_previous() {
    if [[ -n ${BACKUP:-} && -e $BACKUP ]]; then
        if ! mv -f "$BACKUP" "$BIN_PATH" 2>/dev/null; then
            # Callers announce "your previous installation is untouched". When
            # the move back fails that sentence is false and the only copy is
            # sitting at $BACKUP, so say so instead of returning quietly.
            err "Could not restore the previous binary"
            info "It is still at $BACKUP -- move it back to $BIN_PATH by hand."
            BACKUP=""
            return 1
        fi
    fi
    BACKUP=""
    return 0
}

# The scripts a pre-binary installation left in ~/.claude. Removed only after
# the self-check passes *and* settings.json points at the binary: until both
# hold they are still the working installation.
LEGACY_SCRIPTS=(statusline.sh notify.sh git-refresh.sh subagent-statusline.sh)

# --- Options ---
REQUIRE_ATTESTATION=false
PINNED_VERSION="${CLAUDE_STATUSLINE_VERSION:-}"
ALLOW_PRERELEASE=false
DEV_CHANNEL=false
for _arg in "$@"; do
    case "$_arg" in
        --require-attestation) REQUIRE_ATTESTATION=true ;;
        --version=*)           PINNED_VERSION="${_arg#--version=}" ;;
        --pre)                 ALLOW_PRERELEASE=true ;;
        --dev)                 DEV_CHANNEL=true ;;
    esac
done

# --- Header ---
echo ""
printf "\n  ${DIM}claude-statusline installer${RESET}\n"
printf "  ${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}\n"
echo ""

# --- Platform detection ---
# First, and before anything is created, removed, or written: an unsupported
# platform must leave an existing installation exactly as it was.
step "Detecting platform"
_os=""
case "$(uname -s 2>/dev/null)" in
    Darwin) _os=apple-darwin ;;
    Linux)  _os=unknown-linux-musl ;;
esac
_arch=""
case "$(uname -m 2>/dev/null)" in
    arm64|aarch64) _arch=aarch64 ;;
    x86_64|amd64)  _arch=x86_64 ;;
esac
if [[ -z $_os || -z $_arch ]]; then
    err "Unsupported platform: $(uname -s 2>/dev/null)/$(uname -m 2>/dev/null)"
    info "Published targets: macOS and Linux on x86_64 and aarch64, Windows on x86_64 and aarch64."
    info "Nothing was changed."
    return 1 2>/dev/null || exit 1
fi
TARGET="${_arch}-${_os}"
ASSET="claude-statusline-${TARGET}"
ok "$TARGET"

for _tool in curl uname awk; do
    if ! command -v "$_tool" &>/dev/null; then
        err "$_tool is required but not installed"
        return 1 2>/dev/null || exit 1
    fi
done
echo ""

# --- Resolve the release ---
step "Resolving release"
if [[ -n $PINNED_VERSION ]]; then
    TAG="$PINNED_VERSION"
    ok "Pinned to $TAG"
elif [[ $DEV_CHANNEL == true ]]; then
    # The dev channel is one release, tagged dev-channel, whose assets the
    # release workflow replaces on every push to `dev`. There is nothing to
    # resolve: the tag is fixed, so the download URL is known before any
    # request is made, and a channel that has never been published fails at
    # the download rather than here. It outranks --pre when both are given,
    # because a user who asked for the branch head wants exactly that.
    TAG="dev-channel"
    warn "Installing the dev channel (the dev branch head, which may be unstable)"
elif [[ $ALLOW_PRERELEASE == true ]]; then
    # The releases atom feed lists every release newest-first, prereleases
    # included, over plain unauthenticated HTTPS. That is the whole reason to
    # use it rather than the API: no token, no rate limit that a shared IP can
    # exhaust for everyone behind it.
    #
    # "Newest overall" is the deliberate semantic, not "newest prerelease". A
    # user who asks for --pre wants whatever is furthest ahead; once a stable
    # release overtakes the prereleases, that is the stable one, and silently
    # installing an older prerelease instead would be the surprising answer.
    #
    # Among tagged releases, that is: only a tag that names a version, `v` and
    # a digit. The same feed carries the dev channel's `dev-*` builds, which a
    # --pre user did not ask for and which would otherwise always be newest.
    _atom=$(curl -fsSL "https://github.com/$REPO_SLUG/releases.atom" 2>/dev/null)
    _first=$(printf '%s' "$_atom" | grep -o 'releases/tag/v[0-9][^"]*' | head -n 1)
    TAG="${_first#releases/tag/}"
    if [[ -z $TAG ]]; then
        err "Could not resolve a prerelease"
        info "Set CLAUDE_STATUSLINE_VERSION=<tag> to pin a version, or check your connection."
        info "Your existing installation was left untouched."
        return 1 2>/dev/null || exit 1
    fi
    warn "Installing $TAG (prerelease channel)"
else
    # The /releases/latest redirect resolves the current stable tag without an
    # authenticated API call, and excludes prereleases -- which is what keeps
    # pipeline-verification tags from ever being installed.
    _effective=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
        "https://github.com/$REPO_SLUG/releases/latest" 2>/dev/null)
    TAG="${_effective##*/}"
    if [[ -z $TAG ]]; then
        err "Could not resolve the latest release"
        info "Set CLAUDE_STATUSLINE_VERSION=<tag> to pin a version, --pre for the prerelease"
        info "channel, --dev for the dev channel, or check your connection."
        info "Your existing installation was left untouched."
        return 1 2>/dev/null || exit 1
    fi
    # A tag shape, not merely "not the word latest". With no stable release
    # published, /releases/latest redirects to the releases index rather than to
    # a tag, so the final path segment is "releases" -- which the old guard let
    # through. The install then built a download URL from it and failed on a 404
    # reported as "Download failed", which names neither the cause nor the fix.
    case "$TAG" in
        v[0-9]*) ;;
        *)
            err "No stable release has been published yet"
            info "Install from the prerelease channel with --pre, the dev channel with"
            info "--dev, or pin a version with CLAUDE_STATUSLINE_VERSION=<tag>."
            info "Your existing installation was left untouched."
            return 1 2>/dev/null || exit 1
            ;;
    esac
    ok "$TAG"
fi
BASE_URL="https://github.com/$REPO_SLUG/releases/download/$TAG"
echo ""

# --- Verify the install directory ---
# Deliberately inverted relative to the runtime guard: at runtime an
# undeterminable owner leaves the guard passing, because failing a read closed
# kills every cache and re-fires alerts (see
# docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md). Here
# the check runs once, at install time, and a directory we cannot vouch for is
# a directory we must not place an executable into.
step "Checking the install directory"
if [[ -L $BIN_DIR ]]; then
    err "$BIN_DIR is a symlink"
    info "Refusing to install through a link. Remove it and re-run."
    return 1 2>/dev/null || exit 1
fi
if [[ ! -d $BIN_DIR ]]; then
    ( umask 077 && mkdir -p "$BIN_DIR" ) || {
        err "Cannot create $BIN_DIR"
        return 1 2>/dev/null || exit 1
    }
    ok "Created $BIN_DIR (0700)"
fi

# BSD stat takes -f, GNU stat takes -c. Probe rather than branch on uname: both
# platforms ship a `stat` and which dialect is not always what the OS suggests.
#
# The probe has to be a positive test on a known-good target, never the exit
# code of a malformed invocation. GNU reads `-f` as `--file-system`, so
# `stat -f %u DIR` treats `%u` as a missing file operand, prints DIR's
# filesystem report to stdout, and still exits non-zero -- a bare
# `stat -f ... || stat -c ...` therefore runs both halves on GNU and returns the
# report concatenated with the uid. GNU is probed first because it is the
# dialect whose wrong branch produces output rather than nothing.
if stat -c %u . >/dev/null 2>&1; then
    _STAT_DIALECT=gnu
elif stat -f %u . >/dev/null 2>&1; then
    _STAT_DIALECT=bsd
else
    _STAT_DIALECT=none
fi
_stat_owner() {
    case $_STAT_DIALECT in
        gnu) stat -c %u "$1" 2>/dev/null ;;
        bsd) stat -f %u "$1" 2>/dev/null ;;
    esac
}
_stat_mode() {
    case $_STAT_DIALECT in
        gnu) stat -c %a "$1" 2>/dev/null ;;
        bsd) stat -f %Lp "$1" 2>/dev/null ;;
    esac
}
_dir_owner=$(_stat_owner "$BIN_DIR")
_dir_mode=$(_stat_mode "$BIN_DIR")
_me=$(id -u 2>/dev/null)
# Digits-only, not merely non-empty. A dialect that answers with prose rather
# than a number must land here, where the message names the problem, instead of
# reaching the comparison below and failing as "owned by someone else".
if [[ ! $_dir_owner =~ ^[0-9]+$ || ! $_dir_mode =~ ^[0-7]+$ || ! $_me =~ ^[0-9]+$ ]]; then
    err "Cannot determine ownership or permissions of $BIN_DIR"
    info "Refusing to install where the directory cannot be vouched for."
    return 1 2>/dev/null || exit 1
fi
if [[ $_dir_owner != "$_me" ]]; then
    err "$BIN_DIR is owned by uid $_dir_owner, not by you (uid $_me)"
    return 1 2>/dev/null || exit 1
fi
# Group- or world-writable means someone else can replace the binary after it
# is verified, which would make every check above decorative.
if (( (8#$_dir_mode & 8#022) != 0 )); then
    err "$BIN_DIR is group- or world-writable (mode $_dir_mode)"
    info "Fix with: chmod go-w \"$BIN_DIR\""
    return 1 2>/dev/null || exit 1
fi
ok "Owned by you, mode $_dir_mode"
echo ""

# --- Stage the download ---
step "Downloading"
# A `.previous` with no binary at $BIN_PATH means the last run's self-check
# failed AND its restore failed after it -- the path that prints "It is still
# at ... move it back by hand". The sweep below would delete the only copy the
# user was just told to go and rescue, and re-running the installer is the
# first thing anyone does after a failed install. Put it back before sweeping.
# An interrupted run that left a `.previous` *with* the binary in place is the
# ordinary case the sweep is for, and is untouched by this.
if [[ ! -e $BIN_PATH ]]; then
    for _orphan in "$BIN_DIR/$STAGE_PREFIX"*.previous; do
        [[ -e $_orphan ]] || continue
        if mv -f "$_orphan" "$BIN_PATH" 2>/dev/null; then
            chmod 700 "$BIN_PATH" 2>/dev/null
            warn "Restored the binary a failed run left at $(basename "$_orphan")"
        fi
        break
    done
fi
# Sweep anything a previous interrupted run left behind before adding one.
rm -f "$BIN_DIR/$STAGE_PREFIX"* 2>/dev/null

# Staged inside the destination directory, never in a shared world-writable
# temp: /tmp staging would let another user swap the file between verification
# and placement, and a cross-filesystem move would not be atomic either.
umask 077
STAGE="$BIN_DIR/${STAGE_PREFIX}$$"
SUMS="$BIN_DIR/${STAGE_PREFIX}$$.sums"
BUNDLE="$BIN_DIR/${STAGE_PREFIX}$$.sigstore.json"

if ! curl -fsSL "$BASE_URL/$ASSET" -o "$STAGE"; then
    err "Download failed: $BASE_URL/$ASSET"
    discard_stage
    return 1 2>/dev/null || exit 1
fi
# No execute bit yet. Between here and verification the file must not be
# runnable, by us or by anything else that finds it.
chmod 600 "$STAGE" 2>/dev/null
ok "$ASSET ($(human_size "$(file_bytes "$STAGE")"))"
echo ""

# --- Verify the checksum ---
# Verification that cannot be performed counts as verification failure. There is
# no "proceed without checking" path here: the checksum is the fail-closed gate
# for the whole transport.
step "Verifying checksum"
if ! curl -fsSL "$BASE_URL/checksums.txt" -o "$SUMS"; then
    err "Could not fetch checksums.txt"
    discard_stage
    return 1 2>/dev/null || exit 1
fi
_expected=$(awk -v f="$ASSET" '$2 == f { print $1 }' "$SUMS" | head -n 1)
if [[ -z $_expected ]]; then
    err "checksums.txt has no entry for $ASSET"
    discard_stage
    return 1 2>/dev/null || exit 1
fi
if command -v sha256sum &>/dev/null; then
    _actual=$(sha256sum "$STAGE" 2>/dev/null | awk '{print $1}')
elif command -v shasum &>/dev/null; then
    _actual=$(shasum -a 256 "$STAGE" 2>/dev/null | awk '{print $1}')
else
    err "No SHA-256 tool found (sha256sum or shasum)"
    info "Cannot verify the download, so it will not be installed."
    discard_stage
    return 1 2>/dev/null || exit 1
fi
if [[ -z $_actual || $_actual != "$_expected" ]]; then
    err "Checksum mismatch for $ASSET"
    info "expected $_expected"
    info "actual   ${_actual:-<none>}"
    discard_stage
    return 1 2>/dev/null || exit 1
fi
ok "SHA-256 matches"
echo ""

# --- Verify the attestation ---
# Opportunistic but fail-closed when it runs: a negative result stops the
# install with or without --require-attestation; only the inability to verify is
# tolerated, and only without the flag.
step "Verifying build provenance"
_attested=false
if command -v gh &>/dev/null; then
    if curl -fsSL "$BASE_URL/$ASSET.sigstore.json" -o "$BUNDLE"; then
        # Verified against the downloaded bundle rather than the attestation
        # API: the API serves its bundle Snappy-compressed and needs an
        # authenticated gh, which is exactly why the release publishes the
        # bundle as an asset.
        if gh attestation verify "$STAGE" \
                --bundle "$BUNDLE" \
                --repo "$REPO_SLUG" \
                --signer-workflow "$SIGNER_WORKFLOW" &>/dev/null; then
            _attested=true
            ok "Provenance verified (built by $SIGNER_WORKFLOW)"
        else
            err "Attestation verification FAILED for $ASSET"
            info "The download matched its checksum but does not carry a valid"
            info "provenance attestation from this repository's release workflow."
            info "Refusing to install."
            discard_stage
            return 1 2>/dev/null || exit 1
        fi
    else
        warn "No attestation bundle published for this release"
    fi
else
    warn "gh CLI not found — provenance not verified"
fi

if [[ $_attested != true ]]; then
    if [[ $REQUIRE_ATTESTATION == true ]]; then
        err "--require-attestation was given but provenance could not be verified"
        discard_stage
        return 1 2>/dev/null || exit 1
    fi
    info "Verify manually later with:"
    info "  gh attestation verify \"$BIN_PATH\" --repo $REPO_SLUG \\"
    info "     --signer-workflow $SIGNER_WORKFLOW"
fi
echo ""

# --- Place the binary ---
step "Installing"
# Move any existing binary aside rather than overwriting it, so the self-check
# below has something to roll back to. The stage prefix is deliberate:
# a run interrupted between here and the self-check leaves the backup where
# the next run's sweep will find it.
BACKUP=""
if [[ -e $BIN_PATH ]]; then
    BACKUP="$BIN_DIR/${STAGE_PREFIX}$$.previous"
    if ! mv -f "$BIN_PATH" "$BACKUP"; then
        err "Could not move the existing binary aside"
        BACKUP=""
        discard_stage
        return 1 2>/dev/null || exit 1
    fi
fi
if ! mv -f "$STAGE" "$BIN_PATH"; then
    err "Could not place the binary at $BIN_PATH"
    discard_stage
    restore_previous
    return 1 2>/dev/null || exit 1
fi
STAGE=""
# The execute bit goes on only now, after both gates have passed. 0700 also
# leaves it writable only by this user.
chmod 700 "$BIN_PATH" 2>/dev/null || warn "Could not set permissions on $BIN_PATH"
rm -f "$SUMS" "$BUNDLE" 2>/dev/null
ok "$BIN_PATH"
info "$(human_size "$(file_bytes "$BIN_PATH")")"
echo ""

# --- Self-check ---
# A binary can pass its checksum, launch, and still render wrongly -- a bad
# build, a corrupt fixture, an architecture that runs but misbehaves. The
# silent-degradation contract guarantees that failure would reach the user as
# an absent status line and nothing else, so this is the only place it can be
# caught. Everything destructive below is gated on it.
step "Verifying the binary renders"
# The rendered output is captured, not discarded. It is the only evidence of
# what went wrong, this is a per-target failure CI cannot reproduce, and the
# binary that produced it is about to be moved out of the way.
#
# Deliberately *outside* $STAGE_PREFIX, unlike everything else this script
# writes into $BIN_DIR. The sweep above deletes the whole prefix before the
# download, and re-running the installer is the first thing anyone does after a
# failed install -- so naming these two with the prefix destroyed the pair the
# user had just been told to attach to a bug report, before the retry had even
# started. They are removed on the success path below instead.
_CHECK_LOG="$BIN_DIR/claude-statusline.self-check.txt"
if ! "$BIN_PATH" self-check >"$_CHECK_LOG" 2>&1; then
    err "The installed binary failed its self-check"
    info "It downloaded and verified but does not render correctly, so it was"
    info "not activated."
    _FAILED_BIN="$BIN_DIR/claude-statusline.failed"
    mv -f "$BIN_PATH" "$_FAILED_BIN" 2>/dev/null || { rm -f "$BIN_PATH"; _FAILED_BIN=""; }
    if restore_previous; then
        info "Your previous installation is untouched."
    fi
    info "What it rendered: $_CHECK_LOG"
    [[ -n $_FAILED_BIN ]] && info "The binary it rendered with: $_FAILED_BIN"
    info "Please attach both when reporting this."
    return 1 2>/dev/null || exit 1
fi
# The check passed, so this run's log and any failed binary an earlier run left
# behind are both stale: the user has a working install and nothing left to
# report. This is the only place they are removed -- see the naming note above.
rm -f "$_CHECK_LOG" "$BIN_DIR/claude-statusline.failed" 2>/dev/null
[[ -n $BACKUP ]] && rm -f "$BACKUP"
BACKUP=""
ok "Renders correctly"
echo ""

# --- Note the superseded scripts ---
# Found here, deleted only once settings.json actually points at the binary.
# Deleting them first meant a failed `settings apply` left a migrating user with
# neither the script integration nor a configured binary, and nothing here backs
# them up -- the binary has a sidecar, these do not.
_legacy_found=()
for _script in "${LEGACY_SCRIPTS[@]}"; do
    [[ -e "$CLAUDE_DIR/$_script" ]] && _legacy_found+=("$_script")
done

# --- Configure settings.json ---
# The merge runs through the binary just placed. It is the only JSON
# implementation on hand: a fresh install has to work with no jq and no package
# manager, and hand-rolling a JSON merge in shell against the user's own
# settings is not something to do twice in two dialects.
step "Configuring Claude Code settings"
_apply_flags=()

if "$BIN_PATH" settings has-foreign --binary "$BIN_PATH" statusline &>/dev/null; then
    echo ""
    read -rp "  ${YELLOW}${BOLD} ?${RESET} Existing statusLine config found. Overwrite? (${GREEN}y${RESET}/${RED}n${RESET}) " answer </dev/tty
    if [[ "$answer" =~ ^[Yy]$ ]]; then
        _apply_flags+=(--statusline)
    else
        warn "Skipped statusLine update"
        info "Continuing with hook and notification setup..."
    fi
    echo ""
else
    _apply_flags+=(--statusline)
fi

if "$BIN_PATH" settings has-foreign --binary "$BIN_PATH" subagent &>/dev/null; then
    echo ""
    read -rp "  ${YELLOW}${BOLD} ?${RESET} Existing subagentStatusLine config found. Overwrite? (${GREEN}y${RESET}/${RED}n${RESET}) " answer </dev/tty
    [[ "$answer" =~ ^[Yy]$ ]] && _apply_flags+=(--subagent) || warn "Skipped subagentStatusLine update"
    echo ""
else
    _apply_flags+=(--subagent)
fi

# Live git status. Always on: it is not a notification, it has no prompt today,
# and it costs nothing when idle.
_apply_flags+=(--git-refresh)

# --- Notification configuration ---
echo ""
step "Notification configuration"
if [[ -f $NOTIFY_CONFIG_PATH ]]; then
    ok "Config already exists (preserving)"
    info "$NOTIFY_CONFIG_PATH"
    _config_was_new=false
else
    _config_was_new=true
    cat > "$NOTIFY_CONFIG_PATH" <<'NCEOF'
{
  "permission":        { "sound": true, "visual": true },
  "stop":              { "sound": true, "visual": true },
  "rate_limit":        { "sound": true, "visual": true, "threshold": 80 },
  "context_high":      { "sound": false, "visual": true, "threshold": 70 },
  "compaction_start":  { "sound": true, "visual": true },
  "compaction_done":   { "sound": true, "visual": true }
}
NCEOF
    ok "Created default config"
    info "$NOTIFY_CONFIG_PATH"
fi

echo ""
step "Notifications"
info "Plays a sound and shows a popup when Claude needs attention."
# The legacy check is what carries the choice across an upgrade: someone
# who enabled notifications under the scripts has hooks pointing at notify.sh,
# which `has` does not recognise, and re-prompting them would turn a silent
# upgrade into a question they already answered.
if "$BIN_PATH" settings has --binary "$BIN_PATH" notify &>/dev/null \
    || "$BIN_PATH" settings has-legacy --binary "$BIN_PATH" notify &>/dev/null; then
    ok "Already configured"
    _apply_flags+=(--notify)
else
    echo ""
    read -rp "  ${YELLOW}${BOLD} ?${RESET} Enable notifications? (${GREEN}y${RESET}/${RED}n${RESET}) " answer </dev/tty
    if [[ "$answer" =~ ^[Yy]$ ]]; then
        _apply_flags+=(--notify)
    else
        info "Skipped — run the installer again to enable later"
    fi
fi

# --- Notification icon ---
# The icon is the one download outside the release's SHA256SUMS, and it rides
# the moving master ref. Pin its hash and discard a mismatch: a missing icon
# is cosmetic, an unverified file handed to the toast stack is not.
ICON_SHA256="10497c744e9d5e489b9e9b802b964dab11ab9060d21697181218f6d3b3c648c1"
if [[ ! -f $ICON_PATH ]]; then
    if curl -fsSL "https://raw.githubusercontent.com/$REPO_SLUG/master/assets/claude-icon.png" \
        -o "$ICON_PATH" 2>/dev/null; then
        # Same tool fallback as the binary's checksum; the installer has
        # already aborted by this point if neither exists.
        if command -v sha256sum &>/dev/null; then
            _icon_actual=$(sha256sum "$ICON_PATH" 2>/dev/null | awk '{print $1}')
        else
            _icon_actual=$(shasum -a 256 "$ICON_PATH" 2>/dev/null | awk '{print $1}')
        fi
        if [[ $_icon_actual == "$ICON_SHA256" ]]; then
            ok "Icon installed"
        else
            rm -f "$ICON_PATH"
            info "Icon skipped — the download did not match its pinned checksum"
        fi
    fi
fi

# --- Apply ---
echo ""
if ! _apply_err=$("$BIN_PATH" settings apply --binary "$BIN_PATH" "${_apply_flags[@]}" 2>&1); then
    err "Failed to update settings.json"
    [[ -n $_apply_err ]] && info "$_apply_err"
    info "The binary is installed at $BIN_PATH but Claude Code is not pointing at it yet."
    return 1 2>/dev/null || exit 1
fi
ok "Updated $SETTINGS_PATH"

# --- Migrate from a script installation ---
# Only now: the binary has proved it renders *and* settings.json points at it,
# so the scripts are genuinely superseded rather than merely replaced on disk.
# notify-config.json is deliberately not in this list: it is the user's
# configuration, its schema is unchanged, and the binary reads it as-is.
if (( ${#_legacy_found[@]} > 0 )); then
    echo ""
    step "Removing the superseded scripts"
    for _script in "${_legacy_found[@]}"; do
        if rm -f "$CLAUDE_DIR/$_script"; then
            ok "$_script"
        else
            warn "Could not remove $CLAUDE_DIR/$_script"
        fi
    done
    info "Your notification settings were kept."
fi

# --- Done ---
echo ""
printf "  ${GRAY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}\n"
printf "  ${GREEN}${BOLD}Done!${RESET} Restart Claude Code to activate.\n"
echo ""
