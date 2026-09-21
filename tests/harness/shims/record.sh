#!/usr/bin/env bash
# One shim body, installed under every name the harness needs to intercept.
# Keeping it as a single file is deliberate: five near-identical stubs
# is exactly the duplication this migration exists to delete, and a shim that
# drifts from its siblings silently changes what a fixture means.
#
# Installed as: afplay, paplay, terminal-notifier, notify-send — and as the
# isolated HOME's notify.sh, which is how the status line's detached
# notification spawn gets observed.
#
# Records one line per invocation into $STATUSLINE_CAPTURE_FILE:
#
#     <name>\t<arg>\t<arg>...
#
# Arguments are backslash-escaped so a newline or tab inside one cannot forge a
# record boundary. Escaping uses parameter expansion only — a shim that forked
# sed would change the timing of the very race it exists to observe.
#
# Every invocation exits 0. These stand in for helpers whose absence the real
# scripts already tolerate, so a failing shim would look like a missing helper
# rather than a broken harness.

esc() {
    local s=$1
    s=${s//\\/\\\\}
    s=${s//$'\n'/\\n}
    s=${s//$'\r'/\\r}
    s=${s//$'\t'/\\t}
    printf '%s' "$s"
}

line="$(esc "${0##*/}")"
for arg in "$@"; do
    line="${line}	$(esc "$arg")"
done

# O_APPEND on a short write is atomic, which matters: the status line
# backgrounds notify, and notify backgrounds its sound helper, so two shims can
# be writing at once.
if [[ -n "${STATUSLINE_CAPTURE_FILE:-}" ]]; then
    printf '%s\n' "$line" >> "$STATUSLINE_CAPTURE_FILE" 2>/dev/null
fi

exit 0
