#!/usr/bin/env bash
# Interleaved before/after medians for two builds of claude-statusline.
#
# The POSIX twin of measure-pair.ps1, and the reason the Linux half of a claim
# can be measured at all. The transcript scan costs the same on Linux as on
# Windows while process creation and `git` are an order of magnitude cheaper, so
# which cost dominates inverts by platform and a Windows-only pair cannot speak
# for both. See docs/performance.md section 6.
#
# Rules carried from docs/performance.md section 3: one fresh process per probe,
# a median of at least seven runs, interleaved, isolated HOME and TMPDIR,
# STATUSLINE_DEBUG cleared, a proof of work on every variant, and a real .git
# staged from states.json.
#
# The harness is deliberately exempt from the silent-degradation contract in
# CLAUDE.md: it fails loudly, because a measurement that measured nothing reads
# as a spectacular win.
#
# Usage:
#   tests/harness/measure-pair.sh --before <exe> --after <exe> [options]
#   tests/harness/measure-pair.sh --self-test
#
# Options:
#   --runs N              samples per variant per mode (default 15, floor 7)
#   --scratch DIR         absolute scratch root (default $TMPDIR or /tmp)
#   --mode M[,M...]       warm | git-miss | cold | append (default first three)
#   --append-bytes N      the input_tokens value in the appended record (400)
#   --git-state NAME      a state from states.json (default dirty)
#   --transcript-bytes N  generated transcript size (default 8388608)
#   --agent-bytes N       stage one agent transcript this big (default 0, none)
#   --payload PATH        relative to tests/harness (default payloads/full.json)
#   --proof REGEX         both variants must render this
#   --proof-after-only R  the after variant must render it, the before must not
#   --json PATH           write the measurement record

set -uo pipefail

# EPOCHREALTIME is bash 5; macOS ships bash 3.2 as /bin/bash.
if [[ -z ${EPOCHREALTIME:-} ]]; then
    for candidate in /opt/homebrew/bin/bash /usr/local/bin/bash /usr/bin/bash; do
        if [[ -x $candidate ]] && "$candidate" -c '[[ -n ${EPOCHREALTIME:-} ]]' 2>/dev/null; then
            exec "$candidate" "$0" "$@"
        fi
    done
    printf 'measure-pair: needs bash 5 or newer for EPOCHREALTIME\n' >&2
    exit 1
fi

# For this driver own fixed-point arithmetic; the probes get a UTF-8 locale.
export LC_ALL=C

fail() {
    printf 'measure-pair: %s\n' "$1" >&2
    exit 1
}

note() { printf 'measure-pair: %s\n' "$1"; }

HARNESS_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

# --- guards and helpers, each reachable from --self-test ---------------------

# The scratch root, validated before anything is created. A relative root would
# scatter a multi-gigabyte transcript across whatever directory the driver
# happened to start in, and an empty one would resolve to the current directory
# without saying so.
resolve_scratch_root() {
    local candidate=$1
    [[ -n ${candidate//[[:space:]]/} ]] || { printf 'no scratch directory: pass --scratch an absolute path, or set TMPDIR\n' >&2; return 1; }
    [[ $candidate == /* ]] || { printf "the scratch directory must be absolute, got '%s'\n" "$candidate" >&2; return 1; }
    printf '%s/claude-statusline-measure-pair\n' "${candidate%/}"
}

# Both storage layouts. The binary groups its state under
# claude-statusline-<owner>/, and an older one wrote flat in the temp root;
# clearing one and calling the run cold leaves the other warm and reports a
# flattering median rather than an error. See
# docs/solutions/workflow-issues/isolate-profile-and-temp-when-benchmarking-statusline.md
clear_tick_caches() {
    local root=$1 glob=$2
    rm -f "$root"/$glob 2>/dev/null || true
    rm -f "$root"/claude-statusline-*/$glob 2>/dev/null || true
}

# What each mode clears before a probe. `warm` clears nothing by design.
# `append` is the odd one out: it does not clear, it *writes*. The tick after a
# new message is the shape the transcript resume exists for, and clearing the
# token record instead -- which is what `cold` does -- measures the full read
# the resume is meant to avoid.
clear_for_mode() {
    local root=$1 mode=$2 transcript=${3:-} append=${4:-0}
    case $mode in
        warm) ;;
        git-miss) clear_tick_caches "$root" 'statusline-git-*' ;;
        cold) clear_tick_caches "$root" 'statusline-*' ;;
        append)
            [[ -n $transcript ]] && printf '%s\n'                 '{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-opus-5","usage":{"input_tokens":'"$append"',"output_tokens":7}}}'                 >> "$transcript"
            ;;
        *) fail "unknown mode '$mode'" ;;
    esac
}

# The proof of work, as a predicate over what a variant actually rendered. A
# probe that silently no-opped -- a subcommand that does not exist, a payload
# contract that moved -- otherwise reads as a spectacular speed-up.
test_proof() {
    local text=$1
    shift
    local pattern
    for pattern in "$@"; do
        [[ $text =~ $pattern ]] || return 1
    done
    return 0
}

# Median of integer microseconds. Averages the two middles on an even count.
median_us() {
    local sorted
    mapfile -t sorted < <(printf '%s\n' "$@" | sort -n)
    local n=${#sorted[@]}
    (( n > 0 )) || fail 'no samples to take a median of'
    if (( n % 2 == 1 )); then
        printf '%s\n' "${sorted[(n - 1) / 2]}"
    else
        printf '%s\n' "$(( (sorted[n / 2 - 1] + sorted[n / 2]) / 2 ))"
    fi
}

ms() { awk -v us="$1" 'BEGIN { printf "%.2f", us / 1000 }'; }

# --- git state staging -------------------------------------------------------

# states.json is the single description both capture drivers already read, so a
# measurement stages the same repository a fixture does rather than inventing a
# second idea of what "dirty" means.
harness_git() {
    local cwd=$1
    shift
    local -a argv=()
    [[ -n $cwd ]] && argv+=(-C "$cwd")
    local kv
    while IFS= read -r kv; do
        [[ -n $kv ]] && argv+=(-c "$kv")
    done < <(jq -r '.git_config[]' "$HARNESS_DIR/states.json")
    git "${argv[@]}" "$@" >/dev/null 2>&1
}

build_git_state() {
    local name=$1 work=$2 remote=$3
    local states="$HARNESS_DIR/states.json"

    jq -e --arg n "$name" '.git_states[] | select(.name == $n)' "$states" >/dev/null 2>&1 \
        || fail "unknown git state '$name' in states.json"

    # The identity and both dates, so a state that commits is reproducible.
    local k v
    while IFS=$'\t' read -r k v; do
        export "$k=$v"
    done < <(jq -r '.git_env | to_entries[] | [.key, .value] | @tsv' "$states")

    local step
    while IFS= read -r step; do
        local -a parts=()
        while IFS= read -r part; do parts+=("$part"); done < <(printf '%s' "$step" | jq -r '.[]')
        local verb=${parts[0]}
        local -a rest=("${parts[@]:1}")
        case $verb in
            git)
                harness_git "$work" "${rest[@]}" || fail "git step failed in state '$name': ${rest[*]}"
                ;;
            write)
                mkdir -p "$(dirname -- "$work/${rest[0]}")"
                # Exact bytes, LF preserved: this content reaches a blob hash.
                printf '%s' "${rest[1]}" > "$work/${rest[0]}"
                ;;
            mkdir)
                mkdir -p "$work/${rest[0]}"
                ;;
            remote-track | remote-only)
                harness_git '' init --bare -b main "$remote" || fail "bare remote init failed in state '$name'"
                harness_git "$work" remote add origin "$remote" || fail "remote add failed in state '$name'"
                if [[ $verb == remote-track ]]; then
                    harness_git "$work" push -u origin HEAD || fail "push failed in state '$name'"
                fi
                ;;
            *) fail "unknown step verb '$verb' in state '$name'" ;;
        esac
    done < <(jq -c --arg n "$name" '.git_states[] | select(.name == $n) | .steps[]' "$states")

    while IFS= read -r k; do unset "$k"; done < <(jq -r '.git_env | keys[]' "$states")
}

# --- probing -----------------------------------------------------------------

# One fresh process, timed end to end, exec'd directly -- no shell between the
# stopwatch and the binary.
probe_us() {
    local exe=$1 payload=$2 cwd=$3 temp=$4
    local t0 t1 s0 f0 s1 f1
    t0=$EPOCHREALTIME
    ( cd -- "$cwd" && TMPDIR="$temp" "$exe" < "$payload" >/dev/null ) || fail "a probe of $exe exited non-zero"
    t1=$EPOCHREALTIME
    s0=${t0%%.*}; f0=${t0##*.}
    s1=${t1%%.*}; f1=${t1##*.}
    printf '%s\n' "$(( (s1 - s0) * 1000000 + 10#$f1 - 10#$f0 ))"
}

probe_out() {
    local exe=$1 payload=$2 cwd=$3 temp=$4
    ( cd -- "$cwd" && TMPDIR="$temp" LC_ALL="$UTF8_LOCALE" "$exe" < "$payload" 2>/dev/null )
}

assert_work() {
    local exe=$1 label=$2 payload=$3 cwd=$4 temp=$5 forbidden=$6
    shift 6
    local text
    text=$(probe_out "$exe" "$payload" "$cwd" "$temp")
    test_proof "$text" "$@" \
        || fail "the $label variant did not render what this pair claims to measure -- refusing to report a measurement of nothing"
    if [[ -n $forbidden && $text =~ $forbidden ]]; then
        fail "the $label variant rendered '$forbidden', which it was asserted not to know about"
    fi
}

# A locale that actually decodes UTF-8, found by behaviour rather than by name:
# the box-drawing characters in the proof of work are one character only under
# one, and a C locale would fail every variant identically and silently.
find_utf8_locale() {
    local candidate
    for candidate in C.UTF-8 C.utf8 en_US.UTF-8 en_US.utf8 UTF-8; do
        if [[ $(LC_ALL=$candidate bash -c 'printf %s "${#1}"' _ "$(printf '█')" 2>/dev/null) == 1 ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

# --- self-test ---------------------------------------------------------------

# Exercises the guards that make a number evidence, without needing either
# binary. `cargo test` runs this, because the previous drivers rotted into
# unrunnable shape without anything noticing for two months.
self_test() {
    local failures=0
    check() {
        if [[ $1 == ok ]]; then printf '  ok   %s\n' "$2"
        else printf '  FAIL %s -- %s\n' "$2" "$3"; failures=$((failures + 1)); fi
    }

    local bad
    for bad in '' '   ' 'relative-scratch'; do
        if resolve_scratch_root "$bad" >/dev/null 2>&1; then
            check bad "scratch-refused('$bad')" 'the driver accepted a path it must refuse'
        else
            check ok "scratch-refused('$bad')" ''
        fi
    done
    [[ -e relative-scratch ]] \
        && check bad 'scratch-refusal-creates-nothing' 'a refused path was created anyway' \
        || check ok 'scratch-refusal-creates-nothing' ''
    local rooted
    rooted=$(resolve_scratch_root "${TMPDIR:-/tmp}") || rooted=''
    [[ $rooted == /* ]] && check ok 'scratch-accepted-absolute' '' \
        || check bad 'scratch-accepted-absolute' "resolved to '$rooted'"

    local t
    t=$(mktemp -d "${TMPDIR:-/tmp}/measure-pair-selftest-XXXXXX")
    mkdir -p "$t/claude-statusline-1000"
    : > "$t/statusline-git-abc.txt"
    : > "$t/claude-statusline-1000/statusline-git-abc.txt"
    : > "$t/claude-statusline-1000/statusline-tokens-abc.txt"
    clear_for_mode "$t" git-miss
    [[ -e "$t/statusline-git-abc.txt" ]] \
        && check bad 'git-miss-clears-flat' 'the flat git cache survived' \
        || check ok 'git-miss-clears-flat' ''
    [[ -e "$t/claude-statusline-1000/statusline-git-abc.txt" ]] \
        && check bad 'git-miss-clears-nested' 'the state-directory git cache survived' \
        || check ok 'git-miss-clears-nested' ''
    [[ -e "$t/claude-statusline-1000/statusline-tokens-abc.txt" ]] \
        && check ok 'git-miss-keeps-the-record' '' \
        || check bad 'git-miss-keeps-the-record' 'the token record was cleared by a git-only mode'
    clear_for_mode "$t" cold
    [[ -e "$t/claude-statusline-1000/statusline-tokens-abc.txt" ]] \
        && check bad 'cold-clears-the-record' 'cold mode left a tick cache warm' \
        || check ok 'cold-clears-the-record' ''
    rm -rf "$t"

    # The proof-of-work guard fires on a binary that renders nothing. `cat` is
    # the stub: it reads the payload, exits 0, and renders no box.
    local stubdir
    stubdir=$(mktemp -d "${TMPDIR:-/tmp}/measure-pair-stub-XXXXXX")
    printf '{}' > "$stubdir/payload.json"
    if ( assert_work "$(command -v cat)" stub "$stubdir/payload.json" "$stubdir" "$stubdir" '' "$(printf '┏')" 'Opus 5' ) >/dev/null 2>&1; then
        check bad 'proof-of-work-refuses-a-silent-probe' 'a binary that rendered no box was accepted'
    else
        check ok 'proof-of-work-refuses-a-silent-probe' ''
    fi
    rm -rf "$stubdir"
    test_proof "$(printf '┏') Opus 5" "$(printf '┏')" 'Opus 5' \
        && check ok 'proof-of-work-accepts-a-real-render' '' \
        || check bad 'proof-of-work-accepts-a-real-render' 'a real render failed its own guard'

    # HOME and TMPDIR are this process own, so the isolation a run applies dies
    # with it -- but the scratch root must not outlive the run.
    local probe
    probe=$(resolve_scratch_root "${TMPDIR:-/tmp}")
    mkdir -p "$probe"
    rm -rf "$probe"
    [[ -e $probe ]] \
        && check bad 'scratch-root-removed' 'the scratch root survived its own cleanup' \
        || check ok 'scratch-root-removed' ''

    grep -q 'requires at least 7 runs' "${BASH_SOURCE[0]}" \
        && check ok 'runs-floor-is-enforced' '' \
        || check bad 'runs-floor-is-enforced' 'the section 3 sample floor is not enforced'

    (( failures == 0 )) || fail "self-test failures: $failures"
    note 'self-test passed'
}

# --- arguments ---------------------------------------------------------------

BEFORE='' AFTER='' RUNS=15 SCRATCH=${TMPDIR:-/tmp} GIT_STATE=dirty
TRANSCRIPT_BYTES=8388608 AGENT_BYTES=0 APPEND_BYTES=400 PAYLOAD='payloads/full.json' PROOF='' PROOF_AFTER_ONLY=''
JSON_OUT='' SELF_TEST=0
MODES='warm git-miss cold'

while (( $# > 0 )); do
    case $1 in
        --before) BEFORE=$2; shift 2 ;;
        --after) AFTER=$2; shift 2 ;;
        --runs) RUNS=$2; shift 2 ;;
        --scratch) SCRATCH=$2; shift 2 ;;
        --mode) MODES=${2//,/ }; shift 2 ;;
        --git-state) GIT_STATE=$2; shift 2 ;;
        --transcript-bytes) TRANSCRIPT_BYTES=$2; shift 2 ;;
        --agent-bytes) AGENT_BYTES=$2; shift 2 ;;
        --append-bytes) APPEND_BYTES=$2; shift 2 ;;
        --payload) PAYLOAD=$2; shift 2 ;;
        --proof) PROOF=$2; shift 2 ;;
        --proof-after-only) PROOF_AFTER_ONLY=$2; shift 2 ;;
        --json) JSON_OUT=$2; shift 2 ;;
        --self-test) SELF_TEST=1; shift ;;
        -h | --help) sed -n '2,32p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) fail "unknown argument '$1'" ;;
    esac
done

if (( SELF_TEST )); then
    self_test
    exit 0
fi

# --- setup -------------------------------------------------------------------

[[ -n $BEFORE && -n $AFTER ]] || fail 'both --before and --after are required'
(( RUNS >= 7 )) || fail "docs/performance.md section 3 requires at least 7 runs; got $RUNS"
for exe in "$BEFORE" "$AFTER"; do
    [[ -x $exe ]] || fail "no executable binary at $exe"
done
command -v jq >/dev/null 2>&1 || fail 'the git-state staging needs jq'
command -v git >/dev/null 2>&1 || fail 'a real .git needs git'

UTF8_LOCALE=$(find_utf8_locale) || fail 'no UTF-8 locale on this host; the proof of work cannot decode the box'

TOOLCHAIN=$( cd -- "$HARNESS_DIR" && rustc --version 2>/dev/null )
[[ -n $TOOLCHAIN ]] || fail 'rustc --version did not report a toolchain; the number needs one beside it'

ROOT=$(resolve_scratch_root "$SCRATCH") || exit 1
rm -rf "$ROOT"
HOME2="$ROOT/home"
# The driver own TMPDIR, so nothing it does lands in the real one. The
# probes never see it: they get $TMP_BEFORE or $TMP_AFTER.
TMP2="$ROOT/tmp"
WORK="$ROOT/repo/work"
REMOTE="$ROOT/repo/remote.git"
# One state root per variant. The home root stays shared: what lives there is
# input -- the transcript, the changelog, the warmed model map -- while every
# per-tick record lives under TMPDIR. The two builds must not read each other's
# stored records: when a record format changes between them, each rejects the
# other's and every probe measures a miss, which looks exactly like a finding
# on whichever path the record was meant to skip.
TMP_BEFORE="$ROOT/tmp-before"
TMP_AFTER="$ROOT/tmp-after"
mkdir -p "$HOME2/.claude/cache" "$HOME2/.claude/projects/fixtures" "$TMP2" "$TMP_BEFORE" "$TMP_AFTER" "$WORK"

cleanup() { rm -rf "$ROOT"; }
trap cleanup EXIT

build_git_state "$GIT_STATE" "$WORK" "$REMOTE"

# The learned model map, warmed before sampling: without it the first probes
# measure a miss the rest do not.
cp "$HARNESS_DIR/inputs/model-windows.json" "$HOME2/.claude/statusline-model-windows.json"

# Claude Code's own cached changelog, read only when the payload carries a
# version. Staged regardless so --payload payloads/full-with-version.json works.
{
    printf '# Changelog\n\n## 2.1.270\n\n'
    for _ in $(seq 900); do printf -- '- a changelog entry that is about this long, give or take\n'; done
} > "$HOME2/.claude/cache/changelog.md"

# A transcript of the requested size, built from the pinned input's records so
# the token counts are the ones every other fixture reads.
TRANSCRIPT="$HOME2/.claude/projects/fixtures/transcript.jsonl"
: > "$TRANSCRIPT"
while (( $(wc -c < "$TRANSCRIPT") < TRANSCRIPT_BYTES )); do
    for _ in $(seq 64); do cat "$HARNESS_DIR/inputs/transcript.jsonl"; done >> "$TRANSCRIPT"
done
TRANSCRIPT_ON_DISK=$(wc -c < "$TRANSCRIPT")

# The subagent fallback tier reads `<transcript dir>/<stem>/subagents/agent-*.jsonl`
# and re-parses one whole file whenever its (mtime, size) move -- which is every
# tick an agent is working. Nothing could measure that before.
AGENT_ON_DISK=0
if (( AGENT_BYTES > 0 )); then
    AGENT_DIR="$HOME2/.claude/projects/fixtures/transcript/subagents"
    mkdir -p "$AGENT_DIR"
    AGENT="$AGENT_DIR/agent-probe.jsonl"
    : > "$AGENT"
    while (( $(wc -c < "$AGENT") < AGENT_BYTES )); do
        for _ in $(seq 256); do
            printf '%s\n' '{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-opus-5","usage":{"input_tokens":1200,"cache_creation_input_tokens":400,"cache_read_input_tokens":41000}}}'
        done >> "$AGENT"
    done
    AGENT_ON_DISK=$(wc -c < "$AGENT")
fi

# The payload, with the same placeholders the fixture replay substitutes.
# {REPO} is the git work tree, not the scratch root: it is what reaches
# workspace.current_dir, and a directory with no .git measures no git at all.
PAYLOAD_SRC="$HARNESS_DIR/$PAYLOAD"
[[ -f $PAYLOAD_SRC ]] || fail "no payload at $PAYLOAD_SRC"
PROBE_PAYLOAD="$ROOT/payload.json"
# There is no single {TMP} any more: each variant has its own state root, so a
# payload naming one would point at neither. Refused rather than substituted,
# because the wrong root is a silently flattering measurement.
grep -q "{TMP}" "$PAYLOAD_SRC" && fail "$PAYLOAD uses {TMP}, which no longer has one value: each variant measures against its own state root"
sed -e "s#{REPO}#$WORK#g" -e "s#{HOME}#$HOME2#g" \
    -e "s#{SESSION}#fixture-session-0001#g" "$PAYLOAD_SRC" > "$PROBE_PAYLOAD"

export HOME="$HOME2"
export TMPDIR="$TMP2"
unset STATUSLINE_DEBUG

BOX=$(printf '┏')
declare -a PATTERNS=("$BOX" 'Opus 5')
[[ -n $PROOF ]] && PATTERNS+=("$PROOF")

# Warm each variant once so the state files it reads exist, then prove the work.
probe_us "$BEFORE" "$PROBE_PAYLOAD" "$WORK" "$TMP_BEFORE" >/dev/null
probe_us "$AFTER" "$PROBE_PAYLOAD" "$WORK" "$TMP_AFTER" >/dev/null
assert_work "$BEFORE" before "$PROBE_PAYLOAD" "$WORK" "$TMP_BEFORE" "$PROOF_AFTER_ONLY" "${PATTERNS[@]}"
if [[ -n $PROOF_AFTER_ONLY ]]; then
    assert_work "$AFTER" after "$PROBE_PAYLOAD" "$WORK" "$TMP_AFTER" '' "${PATTERNS[@]}" "$PROOF_AFTER_ONLY"
else
    assert_work "$AFTER" after "$PROBE_PAYLOAD" "$WORK" "$TMP_AFTER" '' "${PATTERNS[@]}"
fi

MODE_JSON=''
for mode in $MODES; do
    declare -a bs=() as=()
    # Interleaved: the part that is easy to skip and expensive to get wrong. Any
    # drift over the run would otherwise be charged wholly to whichever variant
    # ran second, and would look exactly like a finding.
    for (( r = 0; r < RUNS; r++ )); do
        clear_for_mode "$TMP_BEFORE" "$mode" "$TRANSCRIPT" "$APPEND_BYTES"
        bs+=("$(probe_us "$BEFORE" "$PROBE_PAYLOAD" "$WORK" "$TMP_BEFORE")")
        clear_for_mode "$TMP_AFTER" "$mode" "$TRANSCRIPT" "$APPEND_BYTES"
        as+=("$(probe_us "$AFTER" "$PROBE_PAYLOAD" "$WORK" "$TMP_AFTER")")
    done
    bmed=$(median_us "${bs[@]}")
    amed=$(median_us "${as[@]}")
    bmin=$(printf '%s\n' "${bs[@]}" | sort -n | head -1)
    amin=$(printf '%s\n' "${as[@]}" | sort -n | head -1)
    delta=$(awk -v a="$amed" -v b="$bmed" 'BEGIN { printf "%.2f", (a - b) / 1000 }')
    pct=$(awk -v a="$amed" -v b="$bmed" 'BEGIN { printf "%.1f", ((a - b) / b) * 100 }')
    note "$(printf '%-9s before %8s ms   after %8s ms   delta %7s ms (%5s%%)' \
        "$mode" "$(ms "$bmed")" "$(ms "$amed")" "$delta" "$pct")"
    MODE_JSON="$MODE_JSON$(jq -n --arg m "$mode" --arg b "$(ms "$bmed")" --arg a "$(ms "$amed")" \
        --arg d "$delta" --arg p "$pct" --arg bn "$(ms "$bmin")" --arg an "$(ms "$amin")" \
        '{($m): {before_ms: ($b|tonumber), after_ms: ($a|tonumber), delta_ms: ($d|tonumber),
                 delta_pct: ($p|tonumber), before_min_ms: ($bn|tonumber), after_min_ms: ($an|tonumber)}}')"
done

note "host:       $(uname -srm) (${MEASURE_HOST_CLASS:-unlabelled host: record the class beside the number})"
note "toolchain:  $TOOLCHAIN"
note "git state:  $GIT_STATE"
note "transcript: $TRANSCRIPT_ON_DISK bytes"
(( AGENT_ON_DISK > 0 )) && note "agent:      $AGENT_ON_DISK bytes"
note "payload:    $PAYLOAD"
note "runs:       $RUNS interleaved pairs per mode"
note 'run the before binary against a copy of itself, same run count, to learn this sitting noise floor before believing a small delta'

if [[ -n $JSON_OUT ]]; then
    printf '%s' "$MODE_JSON" | jq -s 'add' | jq \
        --arg schema 2 \
        --arg host "$(uname -srm)" \
        --arg host_class "${MEASURE_HOST_CLASS:-unlabelled}" \
        --arg toolchain "$TOOLCHAIN" \
        --arg before "$BEFORE" --arg after "$AFTER" \
        --arg git_state "$GIT_STATE" --arg payload "$PAYLOAD" \
        --argjson transcript_bytes "$TRANSCRIPT_ON_DISK" --argjson agent_bytes "$AGENT_ON_DISK" \
        --argjson runs "$RUNS" \
        '{schema: ($schema|tonumber), host_class: $host_class, host: $host, toolchain: $toolchain,
          before: $before, after: $after, git_state: $git_state, payload: $payload,
          transcript_bytes: $transcript_bytes, agent_bytes: $agent_bytes, runs: $runs, modes: .}' > "$JSON_OUT"
    note "wrote $JSON_OUT"
fi
