#!/usr/bin/env bash
#
# Fixture-capture harness for macOS and Linux.
#
# Drives the *current scripts* under a fully isolated HOME and TMPDIR and stores
# each component's observable as a golden fixture, so the Rust port has
# something to be equivalent to after the scripts are deleted.
#
# This is a development tool, not a runtime script: the silent-degradation and
# no-`exit` contracts in CLAUDE.md do not apply here, and must not. A capture
# that cannot be trusted has to fail loudly — a harness that degrades silently
# writes a plausible fixture that everything downstream then treats as truth.
#
#   ./capture.sh --list
#   ./capture.sh --component git-refresh
#   ./capture.sh --case git-clean --out /tmp/fixtures
#   ./capture.sh --at 5d474a0            # regenerate a historical commit's fixtures
#   ./capture.sh --verify                # capture twice, require byte-identical output
#
# Requires bash 4+ (for mapfile), git, and jq. jq is a harness dependency only —
# the shipped binary has none, which is half the point of the migration.

set -uo pipefail

# macOS ships bash 3.2 as /bin/bash forever. Re-exec under a real one
# rather than writing the whole harness to the 2007 dialect.
if (( ${BASH_VERSINFO[0]} < 4 )); then
    for _cand in /opt/homebrew/bin/bash /usr/local/bin/bash /usr/bin/bash; do
        if [[ -x $_cand ]]; then exec "$_cand" "$0" "$@"; fi
    done
    printf 'harness: bash 4+ required, found %s\n' "${BASH_VERSION}" >&2
    exit 2
fi

HARNESS_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git -C "$HARNESS_DIR" rev-parse --show-toplevel)

case "$(uname -s)" in
    Darwin) PLATFORM=macos ;;
    Linux)  PLATFORM=linux ;;
    *)      printf 'harness: unsupported platform %s (use capture.ps1 on Windows)\n' "$(uname -s)" >&2
            exit 2 ;;
esac

OUT_DIR="$REPO_ROOT/tests/fixtures"
FILTER_CASE=""
FILTER_COMPONENT=""
AT_COMMIT=""
ALLOW_DIRTY=0
DO_LIST=0
DO_VERIFY=0

while (( $# )); do
    case "$1" in
        --case)        FILTER_CASE=${2:?}; shift 2 ;;
        --component)   FILTER_COMPONENT=${2:?}; shift 2 ;;
        --at)          AT_COMMIT=${2:?}; shift 2 ;;
        --out)         OUT_DIR=${2:?}; shift 2 ;;
        --allow-dirty) ALLOW_DIRTY=1; shift ;;
        --list)        DO_LIST=1; shift ;;
        --verify)      DO_VERIFY=1; shift ;;
        -h|--help)     sed -n '2,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *)             printf 'harness: unknown argument %s\n' "$1" >&2; exit 2 ;;
    esac
done

CASES_FILE="$HARNESS_DIR/cases.json"
STATES_FILE="$HARNESS_DIR/states.json"

fail() { printf 'harness: %s\n' "$*" >&2; exit 1; }
note() { printf '  %s\n' "$*" >&2; }

command -v jq  >/dev/null || fail "jq is required"
command -v git >/dev/null || fail "git is required"

# The locale the scripts run in is a render input, not harness hygiene.
#
# bash indexes a string by BYTE under a non-UTF-8 locale, so `get_vis` charges
# a 3-byte bar cell three terminal columns and every box row is padded to the
# wrong width. Captured under the blanket `LC_ALL=C` this file used to export,
# the macOS and Linux statusline fixtures recorded a box that no user with a
# UTF-8 terminal has ever seen -- rows between 111 and 179 columns inside one
# frame. The harness's own `sort` calls still pin `LC_ALL=C` individually, and
# the scripts pin it themselves where they need it (`format_cost`, the awk
# transcript pass), so nothing that wanted C loses it.
#
# Probed by behaviour rather than by name: the same locale is spelled `C.utf8`
# on Ubuntu, `UTF-8` on macOS, and `C.UTF-8` in neither reliably.
UTF8_LOCALE=""
for _loc in C.UTF-8 C.utf8 en_US.UTF-8 en_US.utf8 UTF-8; do
    if LC_ALL="$_loc" "${BASH:-bash}" -c '[[ ${#1} -eq 1 ]]' _ "█" 2>/dev/null; then
        UTF8_LOCALE=$_loc
        break
    fi
done
[[ -n $UTF8_LOCALE ]] || fail "no UTF-8 locale found; statusline captures would record a mis-padded box"
unset _loc

# ---------------------------------------------------------------------------
# Source of the scripts under capture
# ---------------------------------------------------------------------------

WORKTREE=""
cleanup_worktree() {
    if [[ -n $WORKTREE ]]; then
        git -C "$REPO_ROOT" worktree remove --force "$WORKTREE" >/dev/null 2>&1
    fi
}
trap cleanup_worktree EXIT

if [[ -n $AT_COMMIT ]]; then
    SOURCE_COMMIT=$(git -C "$REPO_ROOT" rev-parse --verify "${AT_COMMIT}^{commit}") \
        || fail "cannot resolve commit '$AT_COMMIT'"
    WORKTREE=$(mktemp -d "${TMPDIR:-/tmp}/statusline-harness-src.XXXXXX")
    git -C "$REPO_ROOT" worktree add --detach "$WORKTREE" "$SOURCE_COMMIT" >/dev/null \
        || fail "cannot check out $SOURCE_COMMIT"
    SCRIPTS_ROOT="$WORKTREE"
else
    SOURCE_COMMIT=$(git -C "$REPO_ROOT" rev-parse HEAD)
    SCRIPTS_ROOT="$REPO_ROOT"
    # The scripts were deleted at 1f5acf2, so the working tree has nothing to
    # capture from and `--at` is no longer optional. Said here, once, because
    # the alternative is a `bash: .../statusline.sh: No such file or directory`
    # folded into the captured stderr and recorded as though it were output --
    # and the dirty-tree guard below cannot catch it, since a directory that was
    # deleted and committed reports no modification at all.
    if [[ ! -d "$SCRIPTS_ROOT/macos" && ! -d "$SCRIPTS_ROOT/linux" && ! -d "$SCRIPTS_ROOT/windows" ]]; then
        printf 'harness: no script trees in the working tree -- they were deleted at 1f5acf2.\n' >&2
        printf '         Fixtures are captured from the scripts, so this needs an explicit\n' >&2
        printf '         commit that still has them:\n\n' >&2
        printf '           %s --at eb56345 --component %s\n\n' "$0" "${FILTER_COMPONENT:-<component>}" >&2
        printf '         eb56345 is their final state. Capturing from the binary instead\n' >&2
        printf '         would make the fixture agree with the port by construction and\n' >&2
        printf '         prove nothing about parity.\n' >&2
        exit 1
    fi
    # A fixture records the commit its scripts came from. Capturing a dirty
    # tree would record a commit that does not describe what actually ran, and
    # nothing downstream could tell.
    if [[ $ALLOW_DIRTY -eq 0 ]]; then
        dirty=$(git -C "$REPO_ROOT" status --porcelain -- macos linux windows)
        if [[ -n $dirty ]]; then
            printf 'harness: the script trees are modified, so the recorded source commit\n' >&2
            printf '         would not describe the scripts that ran:\n%s\n' "$dirty" >&2
            printf '         commit first, or pass --allow-dirty for a throwaway capture.\n' >&2
            exit 1
        fi
    fi
fi

# ---------------------------------------------------------------------------
# Case selection
# ---------------------------------------------------------------------------

mapfile -t CASE_IDS < <(jq -r '.cases[] | "\(.component)/\(.case)"' "$CASES_FILE")

selected=()
for id in "${CASE_IDS[@]}"; do
    comp=${id%%/*}
    name=${id##*/}
    [[ -n $FILTER_COMPONENT && $comp != "$FILTER_COMPONENT" ]] && continue
    [[ -n $FILTER_CASE      && $name != "$FILTER_CASE"      ]] && continue
    selected+=("$id")
done

if (( DO_LIST )); then
    printf '%s\n' "${CASE_IDS[@]}"
    exit 0
fi
(( ${#selected[@]} )) || fail "no cases matched the filters"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# BSD date takes -r <epoch>; GNU date takes -d @<epoch> and would read -r as a
# filename. Probing beats branching on uname: both runners ship a `date`, and
# which one is not always what the platform name suggests.
touch_stamp() {
    local epoch=$1
    if date -r "$epoch" +%Y%m%d%H%M.%S 2>/dev/null; then return; fi
    date -d "@$epoch" +%Y%m%d%H%M.%S
}

# Replaces every machine-local path with a placeholder, then refuses to hand
# back anything still carrying the real user's home or repo path. Fixtures are
# committed, and CLAUDE.md forbids shipping a personal absolute path.
# Replaces every machine-local path in the captured observable with a
# placeholder, reading from a file and writing the scrubbed bytes to stdout.
#
# It takes a path rather than a string because `$(cat file)` strips *every*
# trailing newline, while the Windows driver reads through .NET's ReadAllText
# and keeps them. Two drivers that disagree about a trailing byte write
# fixtures that look like a real cross-platform divergence and are not — which
# is exactly what an earlier measurement hit on the one observable whose
# captured bytes happened to end in a newline. The trailing `x` below
# survives the stripping and is removed afterwards.
scrub() {
    local text
    text=$(cat "$1"; printf x)
    text=${text%x}
    text=${text//"$WORK_DIR"/\{REPO\}}
    text=${text//"$TMP_DIR"/\{TMP\}}
    text=${text//"$HOME_DIR"/\{HOME\}}
    text=${text//"$CASE_ROOT"/\{ROOT\}}
    text=${text//"$SCRIPTS_ROOT"/\{SCRIPTS\}}
    if [[ -n ${REAL_HOME:-} && $text == *"$REAL_HOME"* ]]; then
        fail "captured output contains the real home path — refusing to write a fixture"
    fi
    printf '%s' "$text"
}

# The status line backgrounds notify, and notify backgrounds its sound helper,
# so a shim's record can land after the foreground process has already exited.
# Wait for the capture file to stop growing rather than sleeping a fixed amount
# and hoping.
settle_capture() {
    local file=$1 stable=0 prev="" now=""
    for _ in $(seq 1 40); do
        now=$(wc -c < "$file" 2>/dev/null || printf 0)
        if [[ $now == "$prev" ]]; then
            (( stable++ ))
            (( stable >= 3 )) && return 0
        else
            stable=0
        fi
        prev=$now
        sleep 0.125
    done
    return 0
}

# ---------------------------------------------------------------------------
# Git state construction (states.json)
# ---------------------------------------------------------------------------

git_env_args=()
while IFS=$'\t' read -r k v; do
    git_env_args+=("$k=$v")
done < <(jq -r '.git_env | to_entries[] | "\(.key)\t\(.value)"' "$STATES_FILE")

mapfile -t git_config_args < <(jq -r '.git_config[]' "$STATES_FILE")

run_git() {
    local args=()
    for c in "${git_config_args[@]}"; do args+=(-c "$c"); done
    env "${git_env_args[@]}" git -C "$WORK_DIR" "${args[@]}" "$@" >/dev/null 2>&1
}

build_git_state() {
    local state=$1
    local steps
    steps=$(jq -c --arg s "$state" '.git_states[] | select(.name == $s) | .steps' "$STATES_FILE")
    [[ -n $steps && $steps != null ]] || fail "unknown git state '$state'"

    local n i verb
    n=$(jq 'length' <<<"$steps")
    for (( i = 0; i < n; i++ )); do
        verb=$(jq -r --argjson i "$i" '.[$i][0]' <<<"$steps")
        case "$verb" in
            git)
                mapfile -t gargs < <(jq -r --argjson i "$i" '.[$i][1:][]' <<<"$steps")
                run_git "${gargs[@]}" || fail "git step failed in state '$state': ${gargs[*]}"
                ;;
            write)
                local rel content
                rel=$(jq -r --argjson i "$i" '.[$i][1]' <<<"$steps")
                # `-j` so jq adds no newline of its own, and the `printf x`
                # sentinel so command substitution cannot eat the one the data
                # really has. Plain `$(jq -r ...)` strips it, which wrote a
                # 12-byte README where the Windows driver wrote 13 -- a
                # different blob, a different tree, and a different commit hash
                # in the one state that renders one.
                content=$(jq -j --argjson i "$i" '.[$i][2]' <<<"$steps"; printf x)
                content=${content%x}
                mkdir -p "$(dirname -- "$WORK_DIR/$rel")"
                printf '%s' "$content" > "$WORK_DIR/$rel"
                ;;
            mkdir)
                local rel
                rel=$(jq -r --argjson i "$i" '.[$i][1]' <<<"$steps")
                mkdir -p "$WORK_DIR/$rel"
                ;;
            remote-track)
                env "${git_env_args[@]}" git init --bare -b main "$REMOTE_DIR" >/dev/null 2>&1
                run_git remote add origin "$REMOTE_DIR"
                run_git push -u origin HEAD
                ;;
            remote-only)
                env "${git_env_args[@]}" git init --bare -b main "$REMOTE_DIR" >/dev/null 2>&1
                run_git remote add origin "$REMOTE_DIR"
                ;;
            *)
                fail "unknown step verb '$verb' in state '$state'"
                ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# One case
# ---------------------------------------------------------------------------

# Saved once, restored after every case. A case exports HOME, TMPDIR and PATH
# into its isolated root; leaving any of them set would make the *next* case
# mktemp inside a directory that was just deleted, and stack another shim
# directory onto PATH each time round.
REAL_HOME=$HOME
REAL_TMPDIR=${TMPDIR:-}
REAL_PATH=$PATH

restore_env() {
    export HOME="$REAL_HOME"
    export PATH="$REAL_PATH"
    if [[ -n $REAL_TMPDIR ]]; then export TMPDIR="$REAL_TMPDIR"; else unset TMPDIR; fi
    unset STATUSLINE_CAPTURE_FILE
}

# capture_case <component> <case> <out-root>
capture_case() {
    local component=$1 case_name=$2 out_root=$3
    local spec
    spec=$(jq -c --arg c "$component" --arg n "$case_name" \
        '.cases[] | select(.component == $c and .case == $n)' "$CASES_FILE")
    [[ -n $spec ]] || fail "no such case: $component/$case_name"

    local observable payload git_state notify_config clock
    observable=$(jq -r '.observable' <<<"$spec")
    payload=$(jq -r '.payload' <<<"$spec")
    git_state=$(jq -r '.git_state // empty' <<<"$spec")
    notify_config=$(jq -r --slurpfile d "$CASES_FILE" \
        '.notify_config // $d[0].defaults.notify_config' <<<"$spec")
    clock=$(jq -r --slurpfile d "$CASES_FILE" '.clock // $d[0].defaults.clock' <<<"$spec")

    CASE_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/statusline-capture.XXXXXX")
    HOME_DIR="$CASE_ROOT/home"
    TMP_DIR="$CASE_ROOT/tmp"
    WORK_DIR="$CASE_ROOT/repo/work"
    REMOTE_DIR="$CASE_ROOT/repo/remote.git"
    local shim_dir="$CASE_ROOT/shims"
    local capture_file="$CASE_ROOT/capture.txt"
    mkdir -p "$HOME_DIR/.claude" "$TMP_DIR" "$WORK_DIR" "$shim_dir"
    : > "$capture_file"

    # The shim directory. One body, five names.
    local shim
    for shim in afplay paplay terminal-notifier notify-send; do
        cp "$HARNESS_DIR/shims/record.sh" "$shim_dir/$shim"
        chmod +x "$shim_dir/$shim"
    done
    # The status line addresses its notification spawn by path, not through
    # PATH, so that one is intercepted where it actually looks.
    cp "$HARNESS_DIR/shims/record.sh" "$HOME_DIR/.claude/notify.sh"
    chmod +x "$HOME_DIR/.claude/notify.sh"

    cp "$HARNESS_DIR/$notify_config" "$HOME_DIR/.claude/notify-config.json"

    [[ -n $git_state ]] && build_git_state "$git_state"

    # Supplied state files: component defaults first, then the case's own.
    local now inputs_json count i target content offset abs stamp
    now=$(date +%s)
    inputs_json=$(jq -c --arg c "$component" --argjson s "$spec" \
        '(.defaults.inputs_by_component[$c] // []) + ($s.inputs // [])' "$CASES_FILE")
    count=$(jq 'length' <<<"$inputs_json")

    # The scripts read the real wall clock — there is no injection point in a
    # shell script — so an intended mtime is materialised as an offset from
    # capture time and *recorded* as an offset from the pinned clock. Replaying
    # in Rust pins the clock to `clock` and the mtimes to clock+offset, which is
    # what the Clock trait exists to make possible.
    for (( i = 0; i < count; i++ )); do
        target=$(jq -r --argjson i "$i" '.[$i].target' <<<"$inputs_json")
        content=$(jq -r --argjson i "$i" '.[$i].content' <<<"$inputs_json")
        offset=$(jq -r --argjson i "$i" '.[$i].mtime_offset // 0' <<<"$inputs_json")
        abs=$target
        abs=${abs//\{HOME\}/$HOME_DIR}
        abs=${abs//\{TMP\}/$TMP_DIR}
        abs=${abs//\{REPO\}/$WORK_DIR}
        abs=${abs//\{SESSION\}/$SESSION_ID}
        mkdir -p "$(dirname -- "$abs")"
        cp "$HARNESS_DIR/$content" "$abs"
        stamp=$(touch_stamp $(( now + offset )))
        touch -t "$stamp" "$abs"
    done

    # Substituted payload. Placeholders carry forward slashes on every platform:
    # the PowerShell scripts reach the filesystem through .NET path APIs, which
    # accept them, so no driver has to escape a backslash into JSON.
    local payload_file="$CASE_ROOT/stdin"
    if [[ -s "$HARNESS_DIR/$payload" ]]; then
        local body
        body=$(cat "$HARNESS_DIR/$payload")
        body=${body//\{HOME\}/$HOME_DIR}
        body=${body//\{TMP\}/$TMP_DIR}
        body=${body//\{REPO\}/$WORK_DIR}
        printf '%s' "$body" > "$payload_file"
    else
        : > "$payload_file"
    fi

    local script="$SCRIPTS_ROOT/$PLATFORM"
    local stdout_file="$CASE_ROOT/stdout" stderr_file="$CASE_ROOT/stderr"
    local -a before after
    local rc=0
    # The observable is assembled as a file, never as a shell string, so its
    # exact bytes survive to the fixture. See `scrub`.
    local observable_file="$CASE_ROOT/observable"
    : > "$observable_file"

    export HOME="$HOME_DIR" TMPDIR="$TMP_DIR" STATUSLINE_CAPTURE_FILE="$capture_file"
    export PATH="$shim_dir:$PATH"
    export TZ=UTC LC_ALL="$UTF8_LOCALE"

    case "$component" in
        statusline)
            ( cd "$WORK_DIR" && bash "$script/statusline.sh" ) \
                < "$payload_file" > "$stdout_file" 2> "$stderr_file"
            rc=$?
            ;;
        git-refresh)
            mapfile -t before < <(cd "$TMP_DIR" && find . -type f | LC_ALL=C sort)
            bash "$script/git-refresh.sh" < "$payload_file" > "$stdout_file" 2> "$stderr_file"
            rc=$?
            mapfile -t after < <(cd "$TMP_DIR" && find . -type f | LC_ALL=C sort)
            ;;
        subagent)
            bash "$script/subagent-statusline.sh" < "$payload_file" > "$stdout_file" 2> "$stderr_file"
            rc=$?
            ;;
        notify)
            mapfile -t nargs < <(jq -r '.args[]? // empty' <<<"$spec")
            bash "$script/notify.sh" "${nargs[@]}" < "$payload_file" > "$stdout_file" 2> "$stderr_file"
            rc=$?
            ;;
        *)
            fail "unknown component '$component'"
            ;;
    esac

    settle_capture "$capture_file"
    restore_env

    # The silent-degradation contract is a property of every capture, not just
    # of the degraded-input cases: a fixture taken from a run that wrote to
    # stderr would enshrine a broken status line as the expected behaviour.
    (( rc == 0 )) || fail "$component/$case_name: the script exited $rc"
    [[ -s $stderr_file ]] && fail "$component/$case_name: the script wrote to stderr: $(cat "$stderr_file")"

    case "$observable" in
        stdout)
            # Asserted in both directions. The isolated TMPDIR guarantees
            # no output cache existed before the run, so a case that renders must
            # leave one behind and a case that exits early must not. Both
            # outcomes produce plausible bytes, so byte-diffing alone can never
            # tell a full render from a served cache or from an early exit — the
            # shape that let the trust-check inversion run nine days.
            local oc_path="$TMP_DIR/statusline-oc-$SESSION_ID.txt"
            local expect_render
            expect_render=$(jq -r 'if .expect_render == false then "no" else "yes" end' <<<"$spec")
            if [[ -n $SESSION_ID ]]; then
                if [[ $expect_render == yes && ! -f $oc_path ]]; then
                    fail "$component/$case_name: no output-cache file was written, so this capture is not a verified cache miss"
                fi
                if [[ $expect_render == no && -f $oc_path ]]; then
                    fail "$component/$case_name: an output cache was written by a case that is supposed to exit before rendering"
                fi
            fi
            cp "$stdout_file" "$observable_file"
            ;;
        deleted-paths)
            # The observable for git-refresh is the exact set of paths that
            # disappeared from the isolated temp root. Diffing the whole root
            # rather than probing the two expected names is the point: a session
            # id that escaped sanitisation would delete something else, and only
            # a full diff can show that.
            printf '%s\n' "${before[@]+"${before[@]}"}" | LC_ALL=C sort > "$CASE_ROOT/before.txt"
            printf '%s\n' "${after[@]+"${after[@]}"}"   | LC_ALL=C sort > "$CASE_ROOT/after.txt"
            comm -23 "$CASE_ROOT/before.txt" "$CASE_ROOT/after.txt" |
                sed 's|^\./||' > "$observable_file"
            ;;
        feed-bytes)
            if [[ -n $SESSION_ID && -f "$TMP_DIR/statusline-tasks-$SESSION_ID.json" ]]; then
                cp "$TMP_DIR/statusline-tasks-$SESSION_ID.json" "$observable_file"
            fi
            # The handler must print nothing, or Claude Code's default agent
            # panel is replaced by whatever it emitted.
            [[ -s $stdout_file ]] && fail "$component/$case_name: the subagent handler wrote to stdout"
            ;;
        notify-argv)
            # Sorted, because this observable is a *set* of invocations and not
            # a sequence. Both scripts background their sound helper, so its
            # record races the visual one: the same case captured twice really
            # does produce the two lines in either order. `deleted-paths` sorts
            # for the same reason.
            LC_ALL=C sort "$capture_file" > "$observable_file"
            ;;
        *)
            fail "unknown observable '$observable'"
            ;;
    esac

    local dest="$out_root/$component/$case_name"
    mkdir -p "$dest/expected"
    scrub "$observable_file" > "$dest/expected/$PLATFORM.txt"

    jq -n \
        --arg case "$case_name" \
        --arg component "$component" \
        --arg observable "$observable" \
        --arg source_commit "$SOURCE_COMMIT" \
        --arg platform "$PLATFORM" \
        --arg payload "$payload" \
        --arg notify_config "$notify_config" \
        --arg git_state "${git_state:-}" \
        --arg session "$SESSION_ID" \
        --argjson clock "$clock" \
        --argjson inputs "$inputs_json" \
        --argjson args "$(jq -c '.args // []' <<<"$spec")" \
        '{
            schema: 1,
            case: $case,
            component: $component,
            observable: $observable,
            source_commit: $source_commit,
            clock: $clock,
            git_state: (if $git_state == "" then null else $git_state end),
            payload: $payload,
            args: $args,
            notify_config: $notify_config,
            session_id: $session,
            inputs: $inputs,
            captured: [{ platform: $platform, expected: ("expected/" + $platform + ".txt") }]
        }' > "$dest/case.json.new"

    # Merge rather than overwrite: the other platforms' capture rows live in the
    # same file, and a Linux run must not erase what a macOS run recorded.
    if [[ -f "$dest/case.json" ]]; then
        jq -s '.[0] as $old | .[1] as $new
               | $new + { captured: (($old.captured // []) + $new.captured
                          | group_by(.platform) | map(.[-1]) | sort_by(.platform)) }' \
            "$dest/case.json" "$dest/case.json.new" > "$dest/case.json.merged"
        mv "$dest/case.json.merged" "$dest/case.json"
        rm -f "$dest/case.json.new"
    else
        mv "$dest/case.json.new" "$dest/case.json"
    fi

    rm -rf "$CASE_ROOT"
}

# ---------------------------------------------------------------------------
# Drive
# ---------------------------------------------------------------------------

resolve_session() {
    local payload=$1 raw=""
    if [[ -s "$HARNESS_DIR/$payload" ]]; then
        raw=$(jq -r '.session_id // empty' "$HARNESS_DIR/$payload" 2>/dev/null)
        if [[ -z $raw ]]; then
            # The scripts recover the session id by scanning the raw text before
            # they ever parse, which is why a malformed payload still writes to
            # session-scoped paths. Resolving it the same way here keeps the
            # cache-miss assertion applicable to exactly the cases it should be.
            raw=$(sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
                  "$HARNESS_DIR/$payload" | head -n 1)
        fi
    fi
    # The scripts' own sanitisation, applied here so a case lands where the
    # script would actually put it rather than where the author guessed.
    printf '%s' "${raw//[^a-zA-Z0-9_-]/}"
}

run_all() {
    local out_root=$1 id comp name payload spec platforms reason
    for id in "${selected[@]}"; do
        comp=${id%%/*}
        name=${id##*/}
        spec=$(jq -c --arg c "$comp" --arg n "$name" \
            '.cases[] | select(.component == $c and .case == $n)' "$CASES_FILE")

        # A case that names a platform list omitting this one has no observable
        # here. Skipping loudly beats storing an empty golden file that
        # everything downstream would then assert against.
        platforms=$(jq -r '.platforms // empty | join(" ")' <<<"$spec")
        if [[ -n $platforms && " $platforms " != *" $PLATFORM "* ]]; then
            reason=$(jq -r '.platform_note // "no reason recorded"' <<<"$spec")
            note "skip    $comp/$name -- not observable on $PLATFORM: $reason"
            continue
        fi

        payload=$(jq -r '.payload' <<<"$spec")
        SESSION_ID=$(resolve_session "$payload")
        note "capture $comp/$name"
        capture_case "$comp" "$name" "$out_root"
    done
}

printf 'harness: %s, %d case(s), scripts at %s\n' "$PLATFORM" "${#selected[@]}" "${SOURCE_COMMIT:0:12}" >&2

if (( DO_VERIFY )); then
    a=$(mktemp -d "${TMPDIR:-/tmp}/statusline-verify-a.XXXXXX")
    b=$(mktemp -d "${TMPDIR:-/tmp}/statusline-verify-b.XXXXXX")
    run_all "$a"
    run_all "$b"
    if diff -r "$a" "$b" >/dev/null 2>&1; then
        printf 'harness: reproducible — two captures of %s are byte-identical\n' "${SOURCE_COMMIT:0:12}" >&2
        rm -rf "$a" "$b"
    else
        printf 'harness: NOT reproducible — the two captures differ:\n' >&2
        diff -r "$a" "$b" >&2
        rm -rf "$a" "$b"
        exit 1
    fi
else
    run_all "$OUT_DIR"
    printf 'harness: wrote %d fixture(s) to %s\n' "${#selected[@]}" "$OUT_DIR" >&2
fi
