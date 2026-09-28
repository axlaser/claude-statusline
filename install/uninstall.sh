#!/usr/bin/env bash
# Removes the claude-statusline binary and every settings.json entry the
# installer wrote. Covers macOS and Linux from one file.
#
# No `set -e` and no bare `exit` -- see install.sh for why both matter when the
# published one-liner pipes this into the user's live shell. One brace group,
# closed on the last line, for the same reason as install.sh's.
{

CLAUDE_DIR="$HOME/.claude"
BIN_DIR="$CLAUDE_DIR/bin"
BIN_PATH="$BIN_DIR/claude-statusline"
SETTINGS_PATH="$CLAUDE_DIR/settings.json"
NOTIFY_CONFIG_PATH="$CLAUDE_DIR/notify-config.json"
ICON_PATH="$CLAUDE_DIR/claude-icon.png"
MODEL_WINDOWS_PATH="$CLAUDE_DIR/statusline-model-windows.json"
STAGE_PREFIX=".claude-statusline.stage."

TOOLS_RECORD="$CLAUDE_DIR/statusline-installed-tools.txt"
TN_APP="$HOME/Applications/terminal-notifier.app"
BREW=/opt/homebrew/bin/brew
_me=$(id -u 2>/dev/null)

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

header "claude-statusline" "uninstaller"

# --- The notification tools the installer added ---
# Read before anything is deleted. The record is a hint, not proof: anything
# running as this user can write it, so only the fixed pairs below are
# understood, each maps to a fixed command, and nothing from the file ever
# reaches a command line. Only a regular file this user owns is read.
_pkg_present() {
    case $2 in
        brew) [[ -x $BREW ]] && HOMEBREW_NO_AUTO_UPDATE=1 "$BREW" list --versions "$1" &>/dev/null ;;
        app) [[ ! -L $TN_APP && -x $TN_APP/Contents/MacOS/terminal-notifier ]] ;;
        apt) dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q 'ok installed' ;;
        dnf | zypper) rpm -q "$1" &>/dev/null ;;
        pacman) pacman -Q "$1" &>/dev/null ;;
        apk) apk info -e "$1" &>/dev/null ;;
        *) return 1 ;;
    esac
}
_recorded=()
if [[ -f $TOOLS_RECORD && ! -L $TOOLS_RECORD && -O $TOOLS_RECORD ]]; then
    while read -r _t _m _rest; do
        [[ -z $_rest ]] || continue
        case "$_t $_m" in
            "terminal-notifier brew" | "terminal-notifier app" \
                | "libnotify-bin apt" | "libnotify-tools zypper" \
                | "libnotify dnf" | "libnotify pacman" | "libnotify apk" \
                | "xdotool apt" | "xdotool dnf" | "xdotool pacman" | "xdotool zypper" | "xdotool apk" \
                | "kdotool dnf") ;;
            *) continue ;;
        esac
        [[ " ${_recorded[*]} " == *" $_t $_m "* ]] && continue
        _pkg_present "$_t" "$_m" && _recorded+=("$_t $_m")
    done <"$TOOLS_RECORD"
fi
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
rm -rf "$BIN_DIR/$STAGE_PREFIX"* \
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
# The one question this uninstaller asks is about the notification tools
# below, which the installer, not the user, put on this machine.
step "Removing notification configuration"
if [[ -f $NOTIFY_CONFIG_PATH ]]; then
    rm -f "$NOTIFY_CONFIG_PATH" && ok "Deleted $NOTIFY_CONFIG_PATH"
else
    info "No notification config found"
fi

if [[ -f "$CLAUDE_DIR/statusline-debug.log" ]]; then
    rm -f "$CLAUDE_DIR/statusline-debug.log" && ok "Deleted $CLAUDE_DIR/statusline-debug.log"
fi

# --- Notification tools ---
# Offered with one question, and only what the installer recorded and is still
# here. Homebrew is never removed, and neither is a shared library: on dnf,
# pacman and apk notify-send ships inside libnotify itself. A package that
# other installed software depends on is not offered, because apt, dnf and
# zypper would remove those dependents with it. The package manager shows its
# own transaction and asks its own confirmation.

# Whether removing the package would remove that package alone.
_leaf() {
    case $2 in
        apt) [[ $(apt-get -s remove "$1" 2>/dev/null | grep -c '^Remv ') == 1 ]] ;;
        dnf | zypper) ! rpm -q --whatrequires "$1" &>/dev/null ;;
        pacman) pacman -Qi "$1" 2>/dev/null | grep -q '^Required By *: None' ;;
        apk) [[ -z $(apk info -r "$1" 2>/dev/null | sed 1d | grep -v '^[[:space:]]*$') ]] ;;
        *) return 0 ;;
    esac
}

# The command that removes a recorded tool, into _cmd, without sudo. `assume`
# adds the manager's yes flag, which only the override with no terminal passes.
_removal() {
    local assume=$3
    _cmd=()
    case $2 in
        brew) _cmd=(env HOMEBREW_NO_AUTO_UPDATE=1 "$BREW" uninstall "$1") ;;
        apt) _cmd=(apt-get remove ${assume:+-y} "$1") ;;
        dnf) _cmd=(dnf remove ${assume:+-y} "$1") ;;
        zypper) _cmd=(zypper ${assume:+--non-interactive} remove "$1") ;;
        pacman) _cmd=(pacman -R ${assume:+--noconfirm} "$1") ;;
        apk) _cmd=(apk del "$1") ;;
    esac
}

notification_tools_removal() {
    local entry t m offer=() kept=() override="" go=false tty=false sudo_cached=false sudo_asked=false
    local added remove s=""
    _interrupted=false
    _sudo=()
    [[ $_me == 0 ]] || s="sudo "
    echo ""
    step "Notification tools"
    printf "\n    The installer added these for desktop notifications:\n\n"
    for entry in "${_recorded[@]}"; do
        t=${entry% *}
        m=${entry#* }
        case $m in
            brew) added="Homebrew formula $t"; remove="brew uninstall $t; Homebrew itself stays" ;;
            app) added="app bundle ~/Applications/terminal-notifier.app"; remove="deletes that bundle" ;;
            *) added="$m package $t"; remove="${s}$( [[ $m == apt ]] && echo apt-get || echo "$m") remove $t, which asks you first" ;;
        esac
        if [[ $t == libnotify && $m != apt ]]; then
            remove="kept: other software uses this shared library"
        elif ! _leaf "$t" "$m"; then
            remove="kept: other installed software depends on it"
            kept+=("$entry")
        else
            offer+=("$entry")
        fi
        card_open "$t"
        card_blank
        card_row "Added" "$added"
        card_row "Removal" "$remove"
        card_blank
        card_close
    done

    if (( ${#offer[@]} > 0 )); then
        case ${CLAUDE_STATUSLINE_DEPS:-} in
            yes | no) override=$CLAUDE_STATUSLINE_DEPS ;;
        esac
        has_tty && tty=true
        if [[ -n $override ]]; then
            progress "CLAUDE_STATUSLINE_DEPS=$override answers this step"
            [[ $override == yes ]] && go=true
        else
            if (( ${#offer[@]} > 1 )); then ask "Remove them too?"; else ask "Remove it too?"; fi
            if [[ $answer =~ ^[Yy]$ ]]; then go=true; else progress "Nothing was removed"; fi
        fi
    fi

    # Trapped rather than ignored, as in install.sh: Ctrl-C stops the child and
    # skips the rest of this step, never the uninstaller.
    trap '_interrupted=true' INT
    for entry in "${offer[@]}"; do
        $go || break
        $_interrupted && break
        t=${entry% *}
        m=${entry#* }
        # sudo only for a package manager, and asked for once.
        if [[ $m != brew && $m != app && $_me != 0 && ${#_sudo[@]} -eq 0 ]]; then
            if $tty; then
                sudo -n true 2>/dev/null && sudo_cached=true
                sudo_asked=true
                if ! sudo -v </dev/tty || $_interrupted; then
                    warn "sudo did not grant administrator rights, so nothing was removed"
                    break
                fi
                _sudo=(sudo)
            else
                _sudo=(sudo -n)
            fi
        fi
        progress "Removing $t..."
        if [[ $m == app ]]; then
            [[ -L $TN_APP ]] || rm -rf "$TN_APP"
        elif $tty; then
            # From the terminal: under `curl | bash` stdin is the drained script,
            # and a manager reading its own confirmation from it would cancel.
            _removal "$t" "$m" ""
            "${_sudo[@]}" "${_cmd[@]}" </dev/tty
        else
            _removal "$t" "$m" yes
            "${_sudo[@]}" "${_cmd[@]}" </dev/null
        fi
        if _pkg_present "$t" "$m"; then
            err "$t was kept"
        else
            ok "$t removed"
        fi
    done
    if $sudo_asked && ! $sudo_cached; then
        sudo -k 2>/dev/null
    fi
    trap - INT

    # Everything offered and not removed, and everything kept for its
    # dependents, gets its command, so the choice stays the user's.
    for entry in "${offer[@]}"; do
        _pkg_present "${entry% *}" "${entry#* }" && [[ " ${kept[*]} " != *" $entry "* ]] && kept+=("$entry")
    done
    if (( ${#kept[@]} > 0 )); then
        # With nothing offered, the last card's own blank line is the gap.
        (( ${#offer[@]} > 0 )) && echo ""
        progress "To remove by hand:"
        echo ""
        for entry in "${kept[@]}"; do
            case ${entry#* } in
                app) printf '        %s\n' "rm -rf ~/Applications/terminal-notifier.app" ;;
                brew) printf '        %s\n' "brew uninstall ${entry% *}" ;;
                *)
                    _removal "${entry% *}" "${entry#* }" ""
                    printf '        %s%s\n' "$s" "${_cmd[*]}"
                    ;;
            esac
        done
    fi
}

if (( ${#_recorded[@]} > 0 )); then
    notification_tools_removal
fi
# The record goes whatever the answer, when it is ours to delete.
if [[ -f $TOOLS_RECORD && ! -L $TOOLS_RECORD && -O $TOOLS_RECORD ]]; then
    rm -f "$TOOLS_RECORD"
fi
footer "Restart Claude Code to apply."
}
