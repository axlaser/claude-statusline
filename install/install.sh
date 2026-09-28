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
#
# The whole script is one brace group, closed on the last line. Bash parses a
# group completely before running any of it, so the published pipe cannot cut
# the script short when Ctrl-C at a password prompt also stops curl.
{

REPO_SLUG="axlaser/claude-statusline"
SIGNER_WORKFLOW="$REPO_SLUG/.github/workflows/release.yml"
# The oldest gh that can verify this release's attestations; see
# gh_cannot_verify. install.ps1 pins the same floor.
GH_MIN_VERSION="2.56.0"

CLAUDE_DIR="$HOME/.claude"
BIN_DIR="$CLAUDE_DIR/bin"
BIN_PATH="$BIN_DIR/claude-statusline"
SETTINGS_PATH="$CLAUDE_DIR/settings.json"
NOTIFY_CONFIG_PATH="$CLAUDE_DIR/notify-config.json"
ICON_PATH="$CLAUDE_DIR/claude-icon.png"
STAGE_PREFIX=".claude-statusline.stage."

# --- Output ---
# Drawn like the status line itself (src/render.rs, `assemble`): a heavy gray
# frame, dim seven-wide labels and the status line's glyphs. Decided once,
# before the first line: plain text with no colour and no frame when stdout is
# not a terminal, NO_COLOR is set (https://no-color.org) or TERM is dumb, so a
# log keeps every message on one line a grep can find.
if [[ -t 1 && -z ${NO_COLOR:-} && ${TERM:-} != dumb ]]; then
    PLAIN=false
    RESET=$'\033[0m'
    BOLD=$'\033[1m'
    DIM=$'\033[2m'
    CYAN=$'\033[36m'
    GREEN=$'\033[32m'
    YELLOW=$'\033[33m'
    RED=$'\033[31m'
    GRAY=$'\033[90m'
else
    PLAIN=true
    RESET="" BOLD="" DIM="" CYAN="" GREEN="" YELLOW="" RED="" GRAY=""
fi
# The frame's inner width. Everything drawn inside it is ASCII, so ${#s} counts
# columns under any locale, and every line stays within 72.
INNER=60
_rule=$(printf '%*s' "$INNER" '')
HEAVY=${_rule// /━}
_rule=$(printf '%*s' 42 '')
RULE=${_rule// /━}

step()     { printf "  ${CYAN}●${RESET} ${BOLD}%s${RESET}\n" "$1"; }
ok()       { printf "    ${GREEN}✓${RESET} %s\n" "$1"; }
warn()     { printf "    ${YELLOW}!${RESET} %s\n" "$1"; }
err()      { printf "    ${RED}x${RESET} %s\n" "$1"; }
info()     { printf "      ${DIM}%s${RESET}\n" "$1"; }
progress() { printf "    ${GRAY}·${RESET} %s\n" "$1"; }

# header <name> <role>: the title box.
header() {
    echo ""
    if $PLAIN; then
        printf "  %s - %s\n\n" "$1" "$2"
        return 0
    fi
    printf "  ${GRAY}┏%s┓${RESET}\n" "$HEAVY"
    printf "  ${GRAY}┃${RESET} ${BOLD}%s${RESET}  ${GRAY}·${RESET}  ${DIM}%s${RESET}%*s ${GRAY}┃${RESET}\n" \
        "$1" "$2" $(( INNER - 7 - ${#1} - ${#2} )) ""
    printf "  ${GRAY}┗%s┛${RESET}\n\n" "$HEAVY"
}

# footer <message>
footer() {
    echo ""
    $PLAIN || printf "  ${GRAY}%s${RESET}\n" "$RULE"
    printf "  ${GREEN}✓${RESET} ${BOLD}Done.${RESET} %s\n\n" "$1"
}

# A card per tool: its name in the top edge, a sentence, then labelled
# details, in a light rounded frame. Everything inside is ASCII, so ${#s}
# counts columns under any locale. Plain mode prints the same lines unframed
# and unwrapped, so each stays one line a grep can find.
card_open() {
    local d
    if $PLAIN; then
        printf "    %s\n" "$1"
        return 0
    fi
    d=$(printf '%*s' $(( INNER - 3 - ${#1} )) '')
    printf "  ${GRAY}╭─${RESET} ${BOLD}${CYAN}%s${RESET} ${GRAY}%s╮${RESET}\n" "$1" "${d// /─}"
}
card_close() {
    local d
    d=$(printf '%*s' "$INNER" '')
    $PLAIN || printf "  ${GRAY}╰%s╯${RESET}\n" "${d// /─}"
    echo ""
}
card_blank() { $PLAIN || printf "  ${GRAY}│${RESET}%*s${GRAY}│${RESET}\n" "$INNER" ""; }
# card_text <sentence>, and card_row <label> <value> [colour]: wrapped at word
# boundaries, a row's continuation lines lined up under its value.
card_text() { _card_wrap "" "$1" ""; }
card_row() { _card_wrap "$1" "$2" "${3:-}"; }
_card_wrap() {
    local label=$1 colour=$3 width=56 line="" word
    local -a words
    if $PLAIN; then
        if [[ -n $label ]]; then printf "      %-10s  %s\n" "$label" "$2"; else printf "      %s\n" "$2"; fi
        return 0
    fi
    [[ -n $label ]] && width=44
    read -r -a words <<<"$2"
    for word in "${words[@]}"; do
        if [[ -n $line ]] && (( ${#line} + 1 + ${#word} > width )); then
            _card_line "$label" "$line" "$colour"
            [[ -n $label ]] && label=" "
            line=$word
        else
            line=${line:+$line }$word
        fi
    done
    _card_line "$label" "$line" "$colour"
}
_card_line() {
    if [[ -z $1 ]]; then
        printf "  ${GRAY}│${RESET}  %-56s  ${GRAY}│${RESET}\n" "$2"
    else
        printf "  ${GRAY}│${RESET}  ${DIM}%-10s${RESET}  %s%-44s${RESET}  ${GRAY}│${RESET}\n" "$1" "$3" "$2"
    fi
}

# Whether anyone can answer a question. `[ -t 0 ]` is always false under
# `curl | bash`, and `[ -e /dev/tty ]` is true on CI runners where opening the
# device fails; only opening it tells.
has_tty() { ( : </dev/tty ) 2>/dev/null; }

# ask <question>: reads a y/n into $answer from the terminal. With no terminal
# the answer is empty, which every caller reads as no.
ask() {
    answer=""
    if has_tty; then
        read -rp "    ${YELLOW}?${RESET} $1 (${GREEN}y${RESET}/${RED}n${RESET}) " answer </dev/tty
    else
        printf "    ${YELLOW}?${RESET} %s (y/n) no terminal to answer, so no\n" "$1"
    fi
}

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

# Prints why gh cannot verify provenance on this machine, or nothing when it
# can.
#
# `gh attestation verify` exits 1 both for "this is not what the release
# workflow built" and for "this gh cannot check anything", and only the first
# is a negative result. Every gh older than GH_MIN_VERSION is in the second
# case whatever it is given. Measured on 2026-09-28 against a live dev-channel
# bundle: before 2.48 there is no `attestation` command, 2.48-2.50 have no
# --signer-workflow, and 2.51-2.55 cannot parse Sigstore's current trusted root
# (its transparency-log key is Ed25519). Distribution packages sit at the old
# end -- the Ubuntu package this was diagnosed on is 2.46.0 -- so reading
# those exits as a failed verification refused every install on that machine.
#
# A version that does not parse -- a source build reports "DEV" -- gets the
# benefit of the doubt: verification runs, and still fails closed.
gh_cannot_verify() {
    local line ver maj min rest floor_maj floor_min
    command -v gh &>/dev/null || { echo "gh CLI not found"; return 0; }
    if ! line=$(gh --version 2>/dev/null); then
        echo "gh could not be run"
        return 0
    fi
    ver=$(printf '%s\n' "$line" | awk 'NR == 1 { print $3 }')
    IFS=. read -r maj min rest <<<"$ver"
    IFS=. read -r floor_maj floor_min rest <<<"$GH_MIN_VERSION"
    [[ $maj =~ ^[0-9]+$ && $min =~ ^[0-9]+$ ]] || return 0
    if (( 10#$maj < floor_maj || (10#$maj == floor_maj && 10#$min < floor_min) )); then
        echo "gh $maj.$min is too old to verify provenance (needs $GH_MIN_VERSION or later)"
    fi
    return 0
}

# The line of gh's output that says why a verification failed, made safe to
# print. Display only: nothing decides on it, so a gh that words its errors
# differently loses the line, not the verdict.
gh_failure_reason() {
    printf '%s\n' "$1" | grep -m 1 -iE '^(error|unknown|x )' | LC_ALL=C tr -cd '[:print:]' | cut -c 1-160
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
header "claude-statusline" "installer"

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
# Sweep anything a previous interrupted run left behind before adding one,
# including the directory the notification tools step unpacks into.
rm -rf "$BIN_DIR/$STAGE_PREFIX"* 2>/dev/null

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
_gh_blocker=$(gh_cannot_verify)
if [[ -n $_gh_blocker ]]; then
    warn "$_gh_blocker — provenance not verified"
elif curl -fsSL "$BASE_URL/$ASSET.sigstore.json" -o "$BUNDLE"; then
    # Verified against the downloaded bundle rather than the attestation
    # API: the API serves its bundle Snappy-compressed and needs an
    # authenticated gh, which is exactly why the release publishes the
    # bundle as an asset.
    if _gh_out=$(gh attestation verify "$STAGE" \
            --bundle "$BUNDLE" \
            --repo "$REPO_SLUG" \
            --signer-workflow "$SIGNER_WORKFLOW" 2>&1); then
        _attested=true
        ok "Provenance verified (built by $SIGNER_WORKFLOW)"
    else
        err "Attestation verification FAILED for $ASSET"
        info "The download matched its checksum but does not carry a valid"
        info "provenance attestation from this repository's release workflow."
        _gh_said=$(gh_failure_reason "$_gh_out")
        [[ -n $_gh_said ]] && info "gh: $_gh_said"
        info "Refusing to install."
        discard_stage
        return 1 2>/dev/null || exit 1
    fi
else
    warn "No attestation bundle published for this release"
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
    ask "Existing statusLine config found. Overwrite?"
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
    ask "Existing subagentStatusLine config found. Overwrite?"
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
else
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
info "Plays a sound and shows a desktop notification when Claude needs attention."
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
    ask "Enable notifications?"
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

# --- Notification tools ---
# Desktop notifications need a third-party tool: terminal-notifier on macOS,
# and on Linux notify-send plus the helper that brings the terminal forward when
# a notification is
# clicked. This step finds which are missing the way the binary looks for them
# at runtime, explains each one, and installs them with one yes, recording what
# it installed for the uninstaller. It runs last, once settings.json and the
# legacy scripts are settled, so a declined password, a held package lock or
# Ctrl-C costs only this step. It never stops the installer: whatever it does
# not install, it prints the commands for.
TOOLS_RECORD="$CLAUDE_DIR/statusline-installed-tools.txt"
TN_VERSION="3.1.0"
TN_URL="https://github.com/julienXX/terminal-notifier/releases/download/$TN_VERSION/terminal-notifier-$TN_VERSION.zip"
# Checked against the release asset on 2026-09-28. The archive holds
# terminal-notifier.app and a README beside it.
TN_SHA256="e969d4ae20287da1ba55495ae31dcedd8e9069deb8ce4eed24f6561a5fc3e4d5"
TN_APP="$HOME/Applications/terminal-notifier.app"
BREW=/opt/homebrew/bin/brew

# Owner and mode through a symlink: the runtime resolves links before its
# ownership guard (platform::trusted_tool), so this asks about the same file.
_stat_owner_l() {
    case $_STAT_DIALECT in
        gnu) stat -L -c %u "$1" 2>/dev/null ;;
        bsd) stat -L -f %u "$1" 2>/dev/null ;;
    esac
}
_stat_mode_l() {
    case $_STAT_DIALECT in
        gnu) stat -L -c %a "$1" 2>/dev/null ;;
        bsd) stat -L -f %Lp "$1" 2>/dev/null ;;
    esac
}

# Whether the runtime would run the tool found at this fixed location: the
# executable, with links followed, and the directory holding it are owned by
# this user or root and nobody else can write them.
_trusted_at() {
    local p=$1 t n=0 o m
    [[ -f $p && -x $p ]] || return 1
    while [[ -L $p ]] && (( n++ < 40 )); do
        t=$(readlink "$p") || return 1
        [[ $t == /* ]] || t=$(dirname "$p")/$t
        p=$t
    done
    for t in "$p" "$(dirname "$p")"; do
        o=$(_stat_owner_l "$t")
        m=$(_stat_mode_l "$t")
        [[ $o =~ ^[0-9]+$ && $m =~ ^[0-7]+$ ]] || return 1
        [[ $o == "$_me" || $o == 0 ]] && (( (8#$m & 8#022) == 0 )) || return 1
    done
}

# Where the binary finds terminal-notifier: PATH, then the fixed locations in
# src/platform/notify.rs. Sets _tn_path to what it found.
_tn_present() {
    _tn_path=$(command -v terminal-notifier 2>/dev/null) && return 0
    for _tn_path in "$TN_APP/Contents/MacOS/terminal-notifier" \
        /Applications/terminal-notifier.app/Contents/MacOS/terminal-notifier \
        /opt/homebrew/bin/terminal-notifier \
        /usr/local/bin/terminal-notifier \
        /opt/local/bin/terminal-notifier; do
        _trusted_at "$_tn_path" && return 0
    done
    _tn_path=""
    return 1
}

# Where the click handler finds a Linux tool: the candidate directories in
# src/platform/focus.rs, then PATH. notify-send is looked up on PATH alone.
_linux_tool_present() {
    local d
    [[ $1 == notify-send ]] && { command -v notify-send >/dev/null 2>&1; return; }
    for d in /usr/bin /usr/local/bin /bin "$HOME/.local/bin" "$HOME/.cargo/bin" /snap/bin; do
        _trusted_at "$d/$1" && return 0
    done
    command -v "$1" >/dev/null 2>&1
}

# The last line a failed command printed, made safe to show.
_last_line() {
    printf '%s\n' "$1" | grep -v '^[[:space:]]*$' | tail -n 1 | LC_ALL=C tr -cd '[:print:]' | cut -c 1-160
}

# Appends "<tool> <method>" to the record the uninstaller reads, once. Only
# through a regular file this user owns: anything else is left alone.
_record() {
    if [[ -e $TOOLS_RECORD || -L $TOOLS_RECORD ]]; then
        [[ -f $TOOLS_RECORD && ! -L $TOOLS_RECORD && -O $TOOLS_RECORD ]] || return 0
        grep -qxF "$1 $2" "$TOOLS_RECORD" 2>/dev/null && return 0
    fi
    printf '%s %s\n' "$1" "$2" >>"$TOOLS_RECORD"
}

# Prints commands below the cards, indented and without a glyph, so they copy
# cleanly. The caller spaces it from whatever came before.
_print_commands() {
    local line
    progress "To install by hand:"
    echo ""
    for line in "$@"; do
        printf '        %s\n' "$line"
    done
}

# The macOS route, picked from the machine rather than asked: Homebrew when it
# is already here on Apple Silicon and belongs to this user, otherwise the
# official release unpacked into ~/Applications, which is also the fallback
# when brew fails, since the user has already said yes. Neither needs a
# password, and Homebrew is never installed. Sets _tn_method on success.
_tn_install() {
    local zip="$BIN_DIR/${STAGE_PREFIX}$$.tn.zip" dir="$BIN_DIR/${STAGE_PREFIX}$$.tn" sum
    if [[ $(uname -m) == arm64 && $_me != 0 && -x $BREW && $(_stat_owner "$BREW") == "$_me" ]]; then
        progress "Installing terminal-notifier with Homebrew..."
        if _pm_out=$(HOMEBREW_NO_AUTO_UPDATE=1 "$BREW" install terminal-notifier </dev/null 2>&1) && _tn_present; then
            _tn_method=brew
            return 0
        fi
        warn "Homebrew could not install it: $(_last_line "$_pm_out")"
        info "Using the official release instead."
    fi
    progress "Downloading terminal-notifier $TN_VERSION..."
    rm -rf "$zip" "$dir"
    if ! curl -fsSL "$TN_URL" -o "$zip"; then
        _pm_out="the download failed: $TN_URL"
        rm -f "$zip"
        return 1
    fi
    if command -v shasum &>/dev/null; then
        sum=$(shasum -a 256 "$zip" 2>/dev/null | awk '{print $1}')
    else
        sum=$(sha256sum "$zip" 2>/dev/null | awk '{print $1}')
    fi
    if [[ $sum != "$TN_SHA256" ]]; then
        _pm_out="the download did not match its pinned checksum"
        rm -f "$zip"
        return 1
    fi
    # Unpacked beside the zip first, so only the app reaches ~/Applications
    # and a symlink planted at its destination is refused rather than followed.
    if ! (umask 022 && mkdir "$dir" && ditto -x -k "$zip" "$dir") 2>/dev/null \
        || [[ -L $dir/terminal-notifier.app || ! -f $dir/terminal-notifier.app/Contents/MacOS/terminal-notifier ]]; then
        _pm_out="the archive did not unpack to terminal-notifier.app"
        rm -rf "$zip" "$dir"
        return 1
    fi
    if [[ -L $TN_APP || -L $HOME/Applications ]]; then
        _pm_out="$TN_APP or its folder is a symlink, so nothing was placed there"
        rm -rf "$zip" "$dir"
        return 1
    fi
    # A bundle already here is one the lookup refused: its executable missing
    # after an interrupted unpack, or writable by someone else. Replaced whole.
    (umask 022 && mkdir -p "$HOME/Applications") 2>/dev/null
    [[ -e $TN_APP ]] && rm -rf "$TN_APP"
    if ! mv "$dir/terminal-notifier.app" "$TN_APP" 2>/dev/null; then
        _pm_out="could not move it into $HOME/Applications"
        rm -rf "$zip" "$dir"
        return 1
    fi
    rm -rf "$zip" "$dir"
    _tn_present || { _pm_out="it was placed but is not usable"; return 1; }
    _tn_method=app
}

# Runs one package manager command with a readable umask and no stdin, as root
# or through sudo, keeping its output for the error line. The manager goes to
# sudo by bare name, so sudo's secure_path resolves it rather than PATH.
_pm_run() {
    _pm_out=$( (umask 022 && "${_sudo[@]}" "$@" </dev/null) 2>&1 )
}

# One package per transaction, so a click helper the repositories lack never
# costs the notification itself. Branches on status codes only: sudo-rs and the
# managers word their messages differently from release to release.
_pkg_install() {
    case $_pm in
        apt-get) _pm_run env DEBIAN_FRONTEND=noninteractive apt-get -qq -o DPkg::Lock::Timeout=60 install -y "$1" ;;
        dnf)     _pm_run dnf install -y "$1" ;;
        pacman)  _pm_run pacman -S --needed --noconfirm "$1" ;;
        zypper)  _pm_run zypper --non-interactive install "$1" ;;
        apk)     _pm_run apk add "$1" ;;
    esac
}

# The package that provides a Linux tool under this package manager.
_pkg_name() {
    case $1:$_pm in
        notify-send:apt-get) echo libnotify-bin ;;
        notify-send:zypper)  echo libnotify-tools ;;
        notify-send:*)       echo libnotify ;;
        *)                   echo "$1" ;;
    esac
}

# Whether this installer can put the tool in place on this machine. kdotool is
# packaged only on Fedora; image-based systems and WSL get instructions.
_installable() {
    [[ $_kernel == Darwin ]] && return 0
    [[ -n $_pm && -z $_image ]] || return 1
    [[ $1 != kdotool || ( $_pm == dnf && $_os_id == fedora ) ]] || return 1
    [[ $_me == 0 ]] || command -v sudo &>/dev/null
}

_present() {
    if [[ $_kernel == Darwin ]]; then _tn_present; else _linux_tool_present "$1"; fi
}

# The commands that install the given tools by hand, into _cmds.
_commands_for() {
    local t s="" pkgs=()
    _cmds=()
    [[ $_me == 0 ]] || s="sudo "
    for t in "$@"; do
        case $t in
            terminal-notifier)
                if [[ -x $BREW ]]; then
                    _cmds+=("brew install terminal-notifier")
                else
                    _cmds+=("curl -fsSLO $TN_URL"
                        "shasum -a 256 terminal-notifier-$TN_VERSION.zip  # expect $TN_SHA256"
                        "mkdir -p ~/Applications && unzip -q terminal-notifier-$TN_VERSION.zip 'terminal-notifier.app/*' -d ~/Applications")
                fi
                ;;
            kdotool)
                if [[ $_pm == dnf && $_os_id == fedora ]]; then pkgs+=(kdotool); else _cmds+=("cargo install kdotool"); fi
                ;;
            *) pkgs+=("$(_pkg_name "$t")") ;;
        esac
    done
    (( ${#pkgs[@]} > 0 )) || return 0
    case $_image:$_pm in
        rpm-ostree:*) _cmds+=("${s}rpm-ostree install ${pkgs[*]}") ;;
        nixos:*|steamos:*) _cmds+=("# add through this system's own configuration: ${pkgs[*]}") ;;
        *:apt-get)    _cmds+=("${s}apt-get install ${pkgs[*]}") ;;
        *:dnf)        _cmds+=("${s}dnf install ${pkgs[*]}") ;;
        *:pacman)     _cmds+=("${s}pacman -Syu --needed ${pkgs[*]}") ;;
        *:zypper)     _cmds+=("${s}zypper install ${pkgs[*]}") ;;
        *:apk)        _cmds+=("${s}apk add ${pkgs[*]}") ;;
        *)            _cmds+=("# with your package manager: ${pkgs[*]}") ;;
    esac
}

notification_tools_offer() {
    local tools=() installable=() left=() tool pkg what why how link with
    local override="" go=false sudo_cached=false sudo_asked=false pm_name updated=false method
    local desktop=true kde=false subject
    _notify_missing=""
    _interrupted=false
    echo ""
    step "Notification tools"
    # A binary older than this installer finds terminal-notifier only on PATH,
    # so a tool placed elsewhere would install and still show nothing.
    if ! "$BIN_PATH" settings supports popup-tools &>/dev/null; then
        info "This release cannot use installed notification tools yet, so none were checked."
        return 0
    fi

    _kernel=$(uname -s)
    _pm="" _os_id="" _image=""
    if [[ $_kernel == Darwin ]]; then
        _tn_present || tools=(terminal-notifier)
    else
        case ":${XDG_CURRENT_DESKTOP:-}:" in
            *[Kk][Dd][Ee]*) kde=true ;;
        esac
        [[ -z ${DISPLAY:-} && -z ${WAYLAND_DISPLAY:-} ]] && desktop=false
        _linux_tool_present notify-send || tools+=(notify-send)
        # The click rule in src/cmd/focus.rs: Wayland raises windows only on
        # KDE, through kdotool; anything else is X11, where wmctrl will do in
        # place of xdotool.
        if [[ ${XDG_SESSION_TYPE:-} == wayland ]]; then
            if $kde && ! _linux_tool_present kdotool; then tools+=(kdotool); fi
        elif ! _linux_tool_present xdotool && ! _linux_tool_present wmctrl; then
            tools+=(xdotool)
        fi
        for method in apt-get dnf pacman zypper apk; do
            command -v "$method" &>/dev/null && { _pm=$method; break; }
        done
        _os_id=$( (. /etc/os-release && printf '%s' "${ID:-}") 2>/dev/null)
        if [[ -e /run/ostree-booted ]]; then
            _image=rpm-ostree
        elif [[ $_os_id == nixos || $_os_id == steamos ]]; then
            _image=$_os_id
        elif uname -r 2>/dev/null | grep -qi microsoft; then
            _image=wsl
        fi
    fi

    if (( ${#tools[@]} == 0 )); then
        ok "Everything desktop notifications need is installed"
        return 0
    fi

    # A click helper alone does not show notifications, so say what is missing.
    subject="Desktop notifications need"
    case " ${tools[*]} " in
        *" terminal-notifier "* | *" notify-send "*) ;;
        *) subject="Clicking a notification needs" ;;
    esac
    if (( ${#tools[@]} == 1 )); then
        printf "\n    %s a tool that isn't installed yet:\n\n" "$subject"
    else
        printf "\n    %s %d tools that aren't installed yet:\n\n" "$subject" "${#tools[@]}"
    fi
    pm_name=${_pm%-get}
    with=", with sudo"
    [[ $_me == 0 ]] && with=""
    for tool in "${tools[@]}"; do
        pkg=$(_pkg_name "$tool")
        why="notifications are sound only"
        case $tool in
            terminal-notifier)
                what="Shows Claude Code's notifications in macOS Notification Center."
                if [[ $(uname -m) == arm64 && $_me != 0 && -x $BREW && $(_stat_owner "$BREW") == "$_me" ]]; then
                    how="brew install terminal-notifier, no password needed"
                else
                    how="its official $TN_VERSION release from GitHub (400 KB, checksum-checked) into ~/Applications, no password needed"
                fi
                link="https://github.com/julienXX/terminal-notifier"
                ;;
            notify-send)
                what="Shows Claude Code's notifications on your desktop."
                how="$pm_name package $pkg$with"
                link="https://gitlab.gnome.org/GNOME/libnotify"
                ;;
            xdotool)
                what="Brings your terminal forward when you click a notification."
                why="a click only dismisses the notification"
                how="$pm_name package xdotool$with"
                link="https://github.com/jordansissel/xdotool"
                ;;
            kdotool)
                what="Brings your terminal forward when you click a notification on KDE Wayland."
                why="a click only dismisses the notification"
                how="dnf package kdotool$with"
                link="https://github.com/jinliu/kdotool"
                ;;
        esac
        if _installable "$tool"; then
            installable+=("$tool")
        elif [[ $tool == kdotool ]]; then
            how="not packaged here: cargo install kdotool, or the AUR package on Arch"
        else
            how="not automatically on this system; the commands follow"
        fi
        card_open "$tool"
        card_blank
        card_text "$what"
        card_blank
        card_row "Without it" "$why"
        card_row "Installs" "$how"
        card_row "Website" "$link" "$CYAN"
        card_blank
        card_close
    done

    case ${CLAUDE_STATUSLINE_DEPS:-} in
        yes | no) override=$CLAUDE_STATUSLINE_DEPS ;;
    esac
    if (( ${#installable[@]} > 0 )) && [[ $_kernel != Darwin ]] && ! $desktop && [[ $override != yes ]]; then
        # Over SSH, or on a server: notifications appear only inside a desktop
        # session, so installing here would change the machine for nothing.
        info "Desktop notifications appear only when Claude Code runs in a desktop"
        info "session, and this shell has none (no DISPLAY or WAYLAND_DISPLAY), so"
        info "nothing was installed."
    elif (( ${#installable[@]} > 0 )); then
        if [[ $_kernel != Darwin && $_me != 0 ]]; then
            warn "Installing packages needs administrator rights, so sudo may ask for"
            info "your password. Nothing else on the system is changed."
            echo ""
        fi
        # The override answers in advance, and says so, so a forgotten export
        # shows in the log. Anything but exactly yes or no counts as unset.
        if [[ -n $override ]]; then
            progress "CLAUDE_STATUSLINE_DEPS=$override answers this step"
            [[ $override == yes ]] && go=true
        else
            if (( ${#installable[@]} > 1 )); then ask "Install them now?"; else ask "Install it now?"; fi
            if [[ $answer =~ ^[Yy]$ ]]; then go=true; else progress "Skipped"; fi
        fi
    fi

    # Trapped rather than ignored, so a child such as sudo still dies on Ctrl-C
    # while the installer carries on without this step.
    trap '_interrupted=true' INT
    _sudo=()
    if $go && [[ $_kernel != Darwin && $_me != 0 ]]; then
        if has_tty; then
            # Noted without prompting, so only a credential this step creates
            # is dropped afterwards.
            sudo -n true 2>/dev/null && sudo_cached=true
            sudo_asked=true
            if sudo -v </dev/tty && ! $_interrupted; then
                _sudo=(sudo)
            else
                go=false
                warn "sudo did not grant administrator rights, so nothing was installed"
            fi
        else
            # No terminal, only the override: sudo may run, but never ask.
            _sudo=(sudo -n)
        fi
    fi

    if $go; then
        for tool in "${installable[@]}"; do
            $_interrupted && break
            _pm_out=""
            method=""
            if [[ $_kernel == Darwin ]]; then
                _tn_install && method=$_tn_method
                pkg=terminal-notifier
            else
                pkg=$(_pkg_name "$tool")
                progress "Installing $tool..."
                if [[ $_pm == apt-get && $updated == false ]]; then
                    _pm_run apt-get -qq -o DPkg::Lock::Timeout=60 update
                    updated=true
                fi
                _pkg_install "$pkg" && method=$pm_name
            fi
            # Recorded when it was missing before and the runtime's own lookup
            # finds it now, whatever the installer's status said.
            if [[ -n $method ]] && _present "$tool"; then
                ok "$tool installed"
                _record "$pkg" "$method"
            else
                err "$tool was not installed"
                [[ -n $_pm_out ]] && info "$(_last_line "$_pm_out")"
            fi
        done
    fi
    # On every path, success, failure and Ctrl-C alike, so the terminal is not
    # left with passwordless root it did not have before.
    if $sudo_asked && ! $sudo_cached; then
        sudo -k 2>/dev/null
    fi
    trap - INT

    for tool in "${tools[@]}"; do
        _present "$tool" || left+=("$tool")
    done
    if (( ${#left[@]} > 0 )); then
        case " ${left[*]} " in
            *" terminal-notifier "* | *" notify-send "*) _notify_missing=notifications ;;
            *) _notify_missing=clicks ;;
        esac
        $_interrupted && warn "Interrupted, so the rest were not installed"
        _commands_for "${left[@]}"
        # With nothing installable, the last card's own blank line is the gap.
        (( ${#installable[@]} > 0 )) && echo ""
        _print_commands "${_cmds[@]}"
    elif [[ $_kernel == Darwin && -n ${_tn_path:-} && -z ${SSH_CONNECTION:-} ]] && has_tty; then
        # macOS asks once whether terminal-notifier may notify. A test now puts
        # that question in front of the user while they are watching.
        "$_tn_path" -title "Claude Code" -message "Notifications are on. This is a test." >/dev/null 2>&1 &
        info "macOS may ask whether terminal-notifier can send notifications: click Allow."
        info "Missed it? Allow it in System Settings > Notifications > terminal-notifier."
    fi
}

if [[ " ${_apply_flags[*]} " == *" --notify "* ]]; then
    notification_tools_offer
fi

# --- Done ---
case ${_notify_missing:-} in
    notifications)
        echo ""
        warn "Desktop notifications stay off until those are installed."
        ;;
    clicks)
        echo ""
        warn "Clicks won't bring the terminal forward until that is installed."
        ;;
esac
footer "Restart Claude Code to turn it on."
}
