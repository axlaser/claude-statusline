//! The assembled box.
//!
//! Everything here is a pure function over already-gathered data; nothing
//! touches the filesystem or the clock, which lets the case table drive the
//! whole render without a process.
//!
//! SGR escapes and OSC 8 hyperlinks occupy no columns, and the box is padded
//! to a row's *visible* width, so every width question goes through
//! [`visible_width`], never `str::len`. The scripts mis-padded every row by
//! counting a 3-byte bar cell as three columns; `chars()` keeps that from
//! reappearing here.

use crate::git::GitStatus;
use crate::payload::{sanitize_display, Payload};
use crate::subagent::{normalize_model_id, Row};
use crate::transcript::{Scan, TokenRecord};

// Copied from the scripts verbatim; the captured fixtures depend on every one,
// so a change fails the case table on all three platforms by design.
pub const RESET: &str = "\x1b[0m";
pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const CYAN: &str = "\x1b[36m";
pub const MAGENTA: &str = "\x1b[35m";
pub const YELLOW: &str = "\x1b[33m";
pub const GREEN: &str = "\x1b[32m";
pub const RED: &str = "\x1b[31m";
pub const BLUE: &str = "\x1b[34m";
pub const WHITE: &str = "\x1b[37m";
pub const GRAY: &str = "\x1b[90m";
pub const BAR_EMPTY: &str = "\x1b[38;5;242m";

// The one non-SGR form this tool emits. OSC 8 opens with the URI and closes
// with an empty one; BEL terminates both, which is the spelling Claude Code's
// own status line documentation uses. `ESC \` would be equivalent to a
// terminal, but only one of the two needs a fixture.
const OSC8: &str = "\x1b]8;;";
const BEL: &str = "\x07";

/// Where the version segment points. The running version is compared against
/// this file's first heading already (see `update.rs`); the link is the rest of
/// that answer -- what actually changed.
const CHANGELOG_URL: &str = "https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md";

/// Shown instead of the box when stdin is empty or not a JSON object.
pub const BAD_JSON: &str = "\x1b[31m[statusline: bad JSON]\x1b[0m";

const LABEL_W: usize = 7;
/// The context and subagent bars are both this wide.
const BAR_WIDTH: usize = 30;
/// The box never renders narrower than this, however short its rows are.
const MIN_INNER: usize = 30;

// Colour thresholds from the scripts: product decisions, not constants to
// tune.
const CONTEXT_CRIT: i64 = 85;
const CONTEXT_WARN: i64 = 60;

/// Separator between segments within a row.
fn row_sep() -> String {
    format!(" {GRAY}·{RESET} ")
}

// ---------------------------------------------------------------------------
// Width
// ---------------------------------------------------------------------------

/// Visible terminal columns: SGR escapes contribute nothing, CJK and emoji
/// count as two cells.
///
/// The wide ranges are the scripts' list verbatim, not a Unicode width table:
/// a more correct table would disagree with the captured fixtures, so
/// widening it is a behaviour change, not a tidy-up.
pub fn visible_width(s: &str) -> usize {
    let stripped = strip_escapes(s);
    if stripped.is_ascii() {
        return stripped.len();
    }
    stripped.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let cp = c as u32;
    let wide = (0x1100..=0x115F).contains(&cp)
        || (0x2E80..=0xA4CF).contains(&cp)
        || (0xAC00..=0xD7A3).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
        || (0xFE30..=0xFE4F).contains(&cp)
        || (0xFF00..=0xFF60).contains(&cp)
        || (0xFFE0..=0xFFE6).contains(&cp)
        || cp >= 0x1F000;
    if wide {
        2
    } else {
        1
    }
}

/// Removes the two escape forms this tool emits: `ESC [ <digits and
/// semicolons> m` (SGR) and `ESC ] ... <ST>` (OSC, which here is only the
/// hyperlink pair), where `<ST>` is BEL or `ESC \`. Both contribute zero
/// columns, so both must go before a width is counted.
///
/// An unterminated sequence is left alone rather than eating the row. That
/// matters more for OSC than for SGR: OSC has no length bound, so a truncated
/// one would otherwise swallow everything after it.
fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        let consumed = match chars.peek() {
            Some('[') => sgr_len(&chars),
            Some(']') => osc_len(&chars),
            _ => None,
        };
        match consumed {
            Some(n) => {
                for _ in 0..n {
                    chars.next();
                }
            }
            None => out.push(c),
        }
    }
    out
}

/// Characters after the `ESC` that a terminated SGR sequence occupies.
fn sgr_len(chars: &std::iter::Peekable<std::str::Chars>) -> Option<usize> {
    let mut lookahead = chars.clone();
    lookahead.next(); // the '['
    let mut consumed = 1;
    for c in lookahead {
        consumed += 1;
        if c == 'm' {
            return Some(consumed);
        }
        if !c.is_ascii_digit() && c != ';' {
            return None;
        }
    }
    None
}

/// The same for an OSC sequence, which ends at BEL or `ESC \` and admits any
/// byte before it.
fn osc_len(chars: &std::iter::Peekable<std::str::Chars>) -> Option<usize> {
    let mut lookahead = chars.clone();
    lookahead.next(); // the ']'
    let mut consumed = 1;
    while let Some(c) = lookahead.next() {
        consumed += 1;
        if c == '\x07' {
            return Some(consumed);
        }
        if c == '\x1b' && lookahead.peek() == Some(&'\\') {
            return Some(consumed + 1);
        }
    }
    None
}

/// Wraps `text` in an OSC 8 hyperlink.
///
/// Refuses a URI carrying a control character and returns the text unlinked:
/// a stray BEL or `ESC` would close the sequence early and spill the rest of
/// the URI into the row as visible text, which is the one way this can skew
/// the box.
fn hyperlink(uri: &str, text: &str) -> String {
    if uri.chars().any(char::is_control) {
        return text.to_string();
    }
    format!("{OSC8}{uri}{BEL}{text}{OSC8}{BEL}")
}

fn repeat(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

// ---------------------------------------------------------------------------
// Formatters
// ---------------------------------------------------------------------------

/// `1234567` -> `1.2M`, `84000` -> `84.0K`, `400` -> `400`.
///
/// Truncating, not rounding, in every branch: the scripts divide with integer
/// arithmetic and 999_999 renders `999.9K`, never `1000.0K`.
///
/// The `B` tier is the port's: the scripts' ladder stopped at `M`, so a count
/// past a billion rendered `1000.0M`. No `T` tier, because ~10^12 tokens in one
/// session is unreachable. `B` alone carries two decimals, because one decimal
/// there is a 100M-token bucket, whereas the same digit buys 100-token
/// resolution at `K`; `K` and `M` keep one and stay byte-identical to the
/// captures.
pub fn format_tokens(n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    if n >= 1_000_000_000 {
        // `{:02}` is load-bearing: without it 1_050_000_000 renders `1.5B`.
        format!(
            "{}.{:02}B",
            n / 1_000_000_000,
            (n % 1_000_000_000) / 10_000_000
        )
    } else if n >= 1_000_000 {
        format!("{}.{}M", n / 1_000_000, (n % 1_000_000) / 100_000)
    } else if n >= 1_000 {
        format!("{}.{}K", n / 1_000, (n % 1_000) / 100)
    } else {
        format!("{n}")
    }
}

/// A context window as a label: `1000000` -> `1M`, `200000` -> `200K`.
pub fn format_window_label(size: u64) -> String {
    let k = size / 1000;
    if k >= 1000 {
        format!("{}M", k / 1000)
    } else {
        format!("{k}K")
    }
}

pub fn pct_color(pct: i64) -> &'static str {
    if pct >= CONTEXT_CRIT {
        RED
    } else if pct >= CONTEXT_WARN {
        YELLOW
    } else {
        GREEN
    }
}

/// The whole effort segment, shared by the model row and every subagent row.
///
/// Extracted because the guard and format string were once duplicated and the
/// subagent copy has no fixture covering it, so a divergence there would land
/// unnoticed.
fn effort_segment(sep: &str, effort: &str) -> String {
    if effort.is_empty() {
        return String::new();
    }
    format!("{sep}{}{effort} effort{RESET}", effort_color(effort))
}

/// `v2.1.278`, and whether there is anything to do about it.
///
/// Grey when the running version is the newest one on disk: a number the user
/// cannot act on is a label, not an alert, and the row already carries three
/// colours that mean something. Yellow with a bare `↑` when a newer one
/// exists, which is the only state worth an eye-stop. Absent entirely on a
/// Claude Code old enough not to send `version`.
///
/// **The newer version is flagged, not named.** Naming it spent eight more
/// columns on the busiest row to answer a question the arrow already answers —
/// there is an update — with a number the user cannot do anything with in
/// place. What to do about it is one click away, on the link the segment
/// already carries in both states.
///
/// It also means the changelog's contents never reach the row: `newer` is a
/// `bool`, so the untrusted string stops at the module boundary rather than
/// being scrubbed on its way through. That is why there is no sanitiser here.
fn version_segment(sep: &str, version: &str, newer: bool) -> String {
    if version.is_empty() {
        return String::new();
    }
    // Linked in both states, but it earns the click only in the yellow one:
    // "what changed" is the question a newer version raises.
    let (colour, text) = if newer {
        (YELLOW, format!("v{version} ↑"))
    } else {
        (GRAY, format!("v{version}"))
    };
    format!("{sep}{colour}{}{RESET}", hyperlink(CHANGELOG_URL, &text))
}

/// Unknown values, including the integer form agent frontmatter allows, fall
/// through to `WHITE` rather than being rejected.
pub fn effort_color(level: &str) -> &'static str {
    match level.to_ascii_lowercase().as_str() {
        "low" => GRAY,
        "medium" => WHITE,
        "high" => CYAN,
        "xhigh" => YELLOW,
        "max" => RED,
        _ => WHITE,
    }
}

/// Filled/empty bar over [`BAR_WIDTH`] cells. `pct` is clamped, and the fill
/// count rounds to nearest.
pub fn render_bar(pct: i64, color: &str) -> String {
    let pct = pct.clamp(0, 100) as usize;
    let filled = (BAR_WIDTH * pct + 50) / 100;
    format!(
        "{color}{}{RESET}{BAR_EMPTY}{}{RESET}",
        repeat('█', filled),
        repeat('░', BAR_WIDTH - filled)
    )
}

/// `claude-sonnet-5` -> `Sonnet 5`; anything unrecognised keeps its cleaned id.
pub fn prettify_model_id(id: &str) -> String {
    let normalized = normalize_model_id(id);
    let cleaned = normalized.strip_prefix("claude-").unwrap_or(&normalized);

    for family in ["fable", "opus", "sonnet", "haiku"] {
        let Some(rest) = cleaned.strip_prefix(family) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('-') else {
            continue;
        };
        // A prefix match, so `opus-5[1m]` still resolves to `Opus 5`.
        let version: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        let version = version.trim_end_matches('-');
        if version.is_empty() || !version.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let mut head = family.chars();
        let capitalized = match head.next() {
            Some(f) => f.to_ascii_uppercase().to_string() + head.as_str(),
            None => family.to_string(),
        };
        return format!("{capitalized} {}", version.replace('-', "."));
    }
    cleaned.to_string()
}

/// `Opus 5 (1M context)` -> `Opus 5`.
///
/// Stripped by shape rather than by matching the variants that exist today:
/// Anthropic spells a variant as a trailing parenthetical, so a model that has
/// not shipped yet shortens on the same rule and this needs no table to keep
/// current. Nothing is lost from the row — the window label beside the bar
/// reads `/1M` already, and it reads the window the payload reports rather
/// than a name that claims one.
///
/// A name that is *only* a parenthetical keeps its text: emptying the segment
/// would render a model row with no model in it.
fn drop_parenthetical(name: &str) -> &str {
    let Some(head) = name.strip_suffix(')') else {
        return name;
    };
    let Some(cut) = head.rfind(" (") else {
        return name;
    };
    let trimmed = name[..cut].trim_end();
    if trimmed.is_empty() {
        name
    } else {
        trimmed
    }
}

/// `$X.YYYY` plus whether the value exceeds the cost-warning threshold.
///
/// Four decimals up to `$999.9999`, then `$1,234.56`: grouped for legibility,
/// and two decimals because sub-cent precision is noise beside a thousand
/// dollars. The scripts had neither tier; they predate a session that could
/// bill that much.
pub fn format_cost(raw: &str) -> (String, bool) {
    let numeric = numeric_prefix(raw);
    let value: f64 = numeric.parse().unwrap_or(0.0);
    // Guards on the magnitude, so a negative four-figure value groups too.
    let formatted = if value.abs() >= 1000.0 {
        format!("${}", group_thousands(&format!("{value:.2}")))
    } else {
        format!("${value:.4}")
    };

    // Compared on the exact decimal digits rather than as a float, matching the
    // scripts: `> 0.50` means a nonzero integer part, or a fraction whose first
    // digit is 6-9, or a leading 5 followed by any nonzero digit.
    let decimal = if numeric.contains(['e', 'E']) {
        format!("{value:.10}")
    } else {
        numeric.to_string()
    };
    (formatted, decimal_exceeds_half(&decimal))
}

/// `1123.45` -> `1,123.45`; a leading sign and the fraction pass through.
///
/// Comma-grouped, not locale-aware: a locale lookup is a per-tick cost for a
/// row whose other numbers are already unlocalised, and a decimal-comma locale
/// would render `$1.123,45` beside `$0.5000` below the threshold.
fn group_thousands(s: &str) -> String {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", s),
    };
    let (int_part, frac) = match rest.split_once('.') {
        Some((int_part, frac)) => (int_part, Some(frac)),
        None => (rest, None),
    };
    let mut grouped = String::with_capacity(int_part.len() + int_part.len() / 3);
    for (i, c) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match frac {
        Some(frac) => format!("{sign}{grouped}.{frac}"),
        None => format!("{sign}{grouped}"),
    }
}

/// The longest numeric prefix, awk-style: garbage yields `0` rather than
/// reaching a formatter that would complain.
fn numeric_prefix(raw: &str) -> &str {
    let trimmed = raw.trim_start();
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < bytes.len() && bytes[i] == b'.' {
        let dot = i;
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - frac_start;
        if int_digits == 0 && frac_digits == 0 {
            i = dot;
        }
    }
    if int_digits == 0 && frac_digits == 0 {
        return "0";
    }
    // An exponent counts only when it is complete; `1e` is `1`.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mark = i;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let digits_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits_start {
            i = mark;
        }
    }
    &trimmed[..i]
}

fn decimal_exceeds_half(decimal: &str) -> bool {
    if decimal.starts_with('-') {
        return false;
    }
    let unsigned = decimal.strip_prefix('+').unwrap_or(decimal);
    let (int_part, frac) = match unsigned.split_once('.') {
        Some((i, f)) => (i, f),
        None => (unsigned, ""),
    };
    if int_part.chars().any(|c| ('1'..='9').contains(&c)) {
        return true;
    }
    let mut frac_digits = frac.chars();
    match frac_digits.next() {
        Some('6'..='9') => true,
        Some('5') => frac_digits.any(|c| ('1'..='9').contains(&c)),
        _ => false,
    }
}

/// Elapsed session time for the cost row: `45s`, `10m54s`, `2h05m`.
pub fn format_elapsed(duration_ms: f64) -> String {
    let secs = (duration_ms.trunc() as i64 / 1000).max(0);
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Time remaining in a rate-limit window: `5m`, `2h15m`, `1d3h`.
pub fn format_duration(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs - h * 3600) / 60;
        if m == 0 {
            format!("{h}h")
        } else {
            format!("{h}h{m}m")
        }
    } else {
        let d = secs / 86400;
        let h = (secs - d * 86400) / 3600;
        if h == 0 {
            format!("{d}d")
        } else {
            format!("{d}d{h}h")
        }
    }
}

/// One rate-limit window: `5h 42% ⇡3% (1h)`. The burn arrow compares usage
/// against the linear expectation for the elapsed part of the window, so an
/// old session does not look like a fast-burning one.
pub fn format_rate_window(
    label: &str,
    pct_val: Option<f64>,
    resets_at: &str,
    window_secs: i64,
    now: i64,
) -> String {
    let Some(pct_val) = pct_val else {
        return String::new();
    };
    let pct = round_pct(pct_val);
    let color = if pct >= 80 {
        RED
    } else if pct >= 50 {
        YELLOW
    } else {
        GREEN
    };

    let mut burn = String::new();
    let mut reset = String::new();
    if let Some(resets) = truncated_int(resets_at) {
        let remaining = resets - now;
        if remaining > 0 && remaining <= window_secs {
            let expected = (window_secs - remaining) * 100 / window_secs;
            let delta = pct - expected;
            let magnitude = delta.abs();
            if magnitude >= 1 {
                burn = if delta > 0 {
                    format!(" {RED}⇡{magnitude}%{RESET}")
                } else {
                    format!(" {GREEN}⇣{magnitude}%{RESET}")
                };
            }
            let label = format_duration(remaining);
            if !label.is_empty() {
                reset = format!(" {GRAY}({label}){RESET}");
            }
        }
    }
    format!("{DIM}{label}{RESET} {color}{pct}%{RESET}{burn}{reset}")
}

/// A percentage as the scripts render it: `printf '%.0f'`.
pub fn round_pct(v: f64) -> i64 {
    format!("{v:.0}").parse().unwrap_or(0)
}

/// The scripts' `${v%.*}` — everything before the first `.`, as an integer.
fn truncated_int(v: &str) -> Option<i64> {
    let head = v.split('.').next()?;
    head.parse().ok()
}

/// One token bucket: `in 2.7K (+2.7K)`, bold and coloured while it is moving.
pub fn format_bucket(
    label: &str,
    value: u64,
    delta: u64,
    idle_color: &str,
    active_color: &str,
    arrow: &str,
) -> String {
    let value_label = format_tokens(value);
    let delta_label = format_tokens(delta);
    let arrow_part = if arrow.is_empty() {
        String::new()
    } else {
        format!("{GRAY}{arrow}{RESET}")
    };
    if delta > 0 {
        format!(
            "{active_color}{BOLD}{label}{RESET}{arrow_part} \
             {active_color}{value_label}{RESET} {GREEN}(+{delta_label}){RESET}"
        )
    } else {
        format!(
            "{DIM}{label}{RESET}{arrow_part} \
             {idle_color}{value_label}{RESET} {DIM}(+{delta_label}){RESET}"
        )
    }
}

// ---------------------------------------------------------------------------
// Segments
// ---------------------------------------------------------------------------

/// `~/projects/thing`, or `.../parent/leaf` when it is not under home.
///
/// Separators are normalised on both sides and the home comparison is
/// case-sensitive, as in bash. Both diverge from `windows/statusline.ps1:287`,
/// which normalised only the `cwd` and compared with `OrdinalIgnoreCase`; the
/// divergences are resolved to this behaviour, recorded in the plan's Scope
/// Boundaries and `docs/performance.md` §4, and asserted by
/// `resolved_cwd_divergences_keep_the_ports_behaviour`. No payload Claude Code
/// produces reaches either shape, so no fixture covers them.
pub fn format_cwd(cwd: &str, home: Option<&str>) -> String {
    let normalized = cwd.replace('\\', "/");
    if let Some(home) = home.filter(|h| !h.is_empty()) {
        let home = home.replace('\\', "/");
        if normalized == home {
            return "~".to_string();
        }
        if let Some(tail) = normalized.strip_prefix(&format!("{home}/")) {
            return format!("~/{tail}");
        }
    }
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() > 2 {
        return format!(".../{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]);
    }
    normalized
}

/// The git segment: branch plus its ahead/behind/diff/untracked/stash counts.
pub fn format_git(status: &GitStatus) -> String {
    let branch = sanitize_display(&status.branch);
    if branch.is_empty() {
        return String::new();
    }
    let dirty = status.insertions > 0 || status.deletions > 0 || status.untracked > 0;
    let color = if dirty { YELLOW } else { GREEN };
    let mut out = format!("{color}{branch}{RESET}");
    if status.ahead > 0 {
        out += &format!(" {CYAN}↑{}{RESET}", status.ahead);
    }
    if status.behind > 0 {
        out += &format!(" {MAGENTA}↓{}{RESET}", status.behind);
    }
    if status.insertions > 0 {
        out += &format!(" {GREEN}+{}{RESET}", status.insertions);
    }
    if status.deletions > 0 {
        out += &format!(" {RED}-{}{RESET}", status.deletions);
    }
    if status.untracked > 0 {
        out += &format!(" {GRAY}~{}{RESET}", status.untracked);
    }
    if status.stash > 0 {
        // The space is deliberate: U+229F is drawn edge-to-edge in most
        // terminal fonts, so an adjacent digit reads as part of the glyph.
        out += &format!(" {DIM}⊟ {}{RESET}", status.stash);
    }
    out
}

/// One subagent row. The untrusted display fields are scrubbed here, at the
/// render sink, because every source (feed, read-back cache, transcript
/// fallback) funnels through this function.
pub fn format_subagent_row(row: &Row) -> String {
    let window = if row.window > 0 {
        row.window
    } else {
        crate::subagent::DEFAULT_WINDOW
    };
    let model = sanitize_display(&row.model);
    let display = sanitize_display(&row.display);
    // Bounded at the sink so an older version's record is capped too; no real
    // level exceeds six characters, so this never truncates a legitimate value.
    let effort: String = sanitize_display(&row.effort).chars().take(16).collect();

    let pct = ((row.used.min(u64::MAX / 100) * 100 / window) as i64).clamp(0, 100);
    let color = pct_color(pct);
    let sep = row_sep();

    let mut out = format!(
        "{} {color}{pct}%{RESET}{sep}{WHITE}{}{RESET}{GRAY}/{}{RESET}",
        render_bar(pct, color),
        format_tokens(row.used),
        format_window_label(window)
    );
    if !model.is_empty() {
        out += &format!("{sep}{MAGENTA}{}{RESET}", prettify_model_id(&model));
    }
    // Present only when the feed reported an override; absence is meaningful,
    // so nothing is inferred from the session's own effort here.
    out += &effort_segment(&sep, &effort);
    if !display.is_empty() {
        let display = if display.chars().count() > 40 {
            let head: String = display.chars().take(39).collect();
            format!("{head}…")
        } else {
            display
        };
        out += &format!("{sep}{BLUE}{display}{RESET}");
    }
    out += &if row.done {
        format!("{sep}{GREEN}✓ done{RESET}")
    } else {
        format!("{sep}{YELLOW}○ working{RESET}")
    };
    out
}

// ---------------------------------------------------------------------------
// The box
// ---------------------------------------------------------------------------

/// One labelled row, and which of the two sections it belongs to.
struct BoxRow {
    section: u8,
    label: &'static str,
    content: String,
}

/// Everything the render needs that is not already in the payload.
pub struct Inputs<'a> {
    pub payload: &'a Payload,
    pub home: Option<&'a str>,
    pub git: Option<&'a GitStatus>,
    pub scan: Option<&'a Scan>,
    pub record: Option<&'a TokenRecord>,
    pub subagents: &'a [Row],
    pub now: i64,
    /// Whether a Claude Code newer than the one running exists, as far as the
    /// changelog cache on disk knows.
    ///
    /// A `bool` and not the version: the row flags the update rather than
    /// naming it, so the string — which comes from a file this tool neither
    /// writes nor owns, fetched over the network by another program — has no
    /// reason to enter the renderer at all. Rendering it again would mean
    /// restoring a sanitiser at the sink; `update::available` still returns it
    /// for anyone who needs the number itself.
    pub update_available: bool,
}

/// The whole status line, with no trailing newline.
pub fn render(inputs: &Inputs) -> String {
    let p = inputs.payload;
    let sep = row_sep();

    // --- path -------------------------------------------------------------
    let cwd = format_cwd(p.cwd(), inputs.home);
    let git_part = inputs.git.map(format_git).unwrap_or_default();
    // The path carries the repo link rather than the branch: a branch name is
    // user text that would need percent-encoding to survive a URL, and the
    // repository root needs none.
    let repo = inputs.git.map(|g| g.remote.as_str()).unwrap_or_default();
    let mut path_row = if repo.is_empty() {
        format!("{CYAN}{cwd}{RESET}")
    } else {
        format!("{CYAN}{}{RESET}", hyperlink(repo, &cwd))
    };
    let path_label = if git_part.is_empty() {
        "project"
    } else {
        path_row += &format!("{sep}{DIM}on{RESET} {git_part}");
        "repo"
    };

    // --- model, context ---------------------------------------------------
    let display_name = sanitize_display(p.model_display_name());
    let model_short = if display_name.is_empty() {
        "unknown".to_string()
    } else {
        drop_parenthetical(
            display_name
                .strip_prefix("Claude ")
                .unwrap_or(&display_name),
        )
        .chars()
        .take(24)
        .collect()
    };

    let ctx_size = p.context_window_size();
    let used_pct = p.used_percentage();
    let pct_int = used_pct.map(round_pct);
    let pct_col = pct_int.map(pct_color).unwrap_or(WHITE);

    // Always rendered, so a fresh session shows an empty bar rather than a row
    // that opens on the model name. The bar fills from the *truncated*
    // percentage while the label shows the *rounded* one — a script quirk,
    // kept deliberately.
    let bar_pct = used_pct
        .map(|v| (v.trunc() as i64).clamp(0, 100))
        .unwrap_or(0);
    let bar_color = if pct_int.is_some() { pct_col } else { GREEN };
    let mut ctx_row = format!(
        "{} {bar_color}{}%{RESET}",
        render_bar(bar_pct, bar_color),
        pct_int.unwrap_or(0)
    );
    if let Some(size) = ctx_size {
        // total_input_tokens is preferred: used_percentage is rounded, so a
        // count derived from it jumps in 10K steps on a 1M window.
        let used = p
            .total_input_tokens()
            .unwrap_or_else(|| size * bar_pct as u64 / 100);
        ctx_row += &format!(
            "{sep}{WHITE}{}{RESET}{GRAY}/{}{RESET}",
            format_tokens(used),
            format_window_label(size)
        );
    }

    let effort = sanitize_display(&p.effort_level());
    let idle = inputs.scan.map(|s| s.idle).unwrap_or(true);
    let status_part = if idle {
        format!("{GREEN}●{RESET}  {WHITE}ready{RESET}")
    } else {
        format!("{YELLOW}○{RESET}  {YELLOW}working{RESET}")
    };
    // One row, bar first. The two were split while the bar was the row: the
    // context reading moves every tick and is what the eye goes to, so it
    // leads, and the name — which changes once a session — follows it. The
    // merge costs the box the bar's 30 columns, and that is the whole width
    // difference against the two-row form; nothing clips, because `assemble`
    // sizes the box to its widest row.
    let mut model_row = ctx_row;
    model_row += &format!("{sep}{MAGENTA}{model_short}{RESET}");
    model_row += &effort_segment(&sep, &effort);
    model_row += &format!("{sep}{status_part}");
    // The running version still gets the scrub: it comes from the payload.
    // The newer one no longer needs one, because it no longer arrives — see
    // `version_segment`.
    model_row += &version_segment(
        &sep,
        &sanitize_display(p.cli_version()),
        inputs.update_available,
    );

    // --- tokens -----------------------------------------------------------
    // Totals come from this tick's scan; only the deltas come from the stored
    // record, so an unchanged transcript re-displays them instead of zeroing.
    let scan = inputs.scan;
    let d = inputs.record;
    let tokens_row = [
        format_bucket(
            "in",
            scan.map_or(0, |s| s.input_tokens),
            d.map_or(0, |r| r.delta_in),
            CYAN,
            CYAN,
            "",
        ),
        format_bucket(
            "cache",
            scan.map_or(0, |s| s.cache_write_tokens),
            d.map_or(0, |r| r.delta_cache_write),
            GRAY,
            YELLOW,
            "↑",
        ),
        format_bucket(
            "cache",
            scan.map_or(0, |s| s.cache_read_tokens),
            d.map_or(0, |r| r.delta_cache_read),
            GRAY,
            CYAN,
            "↓",
        ),
        format_bucket(
            "out",
            scan.map_or(0, |s| s.output_tokens),
            d.map_or(0, |r| r.delta_out),
            MAGENTA,
            MAGENTA,
            "",
        ),
    ]
    .join(&sep);

    // --- agent ------------------------------------------------------------
    let agent_name = sanitize_display(p.agent_name());
    let agent_row = if agent_name.is_empty() {
        String::new()
    } else {
        let mut compact = String::new();
        if let Some(pct) = pct_int {
            compact += &format!("{pct_col}{pct}%{RESET}{sep}");
        }
        compact += &format!(
            "{DIM}in{RESET} {WHITE}{}{RESET}  {DIM}out{RESET} {WHITE}{}{RESET}",
            format_tokens(p.agent_input_tokens()),
            format_tokens(p.agent_output_tokens())
        );
        format!("{BLUE}{BOLD}{agent_name}{RESET}{sep}{compact}")
    };

    // --- cost -------------------------------------------------------------
    let mut cost_parts: Vec<String> = Vec::new();
    if let Some(cost) = p.total_cost_usd() {
        let (formatted, over) = format_cost(&format!("{cost}"));
        let color = if over { YELLOW } else { GREEN };
        cost_parts.push(format!("{color}{formatted}{RESET}"));
    }
    if let Some(messages) = scan.map(|s| s.messages).filter(|m| *m > 0) {
        let noun = if messages == 1 { "message" } else { "messages" };
        cost_parts.push(format!("{WHITE}{messages}{RESET} {DIM}{noun}{RESET}"));
    }
    if let Some(ms) = p.duration_ms() {
        cost_parts.push(format!("{WHITE}{}{RESET}", format_elapsed(ms)));
    }
    let rate_row = format_rate_row(p, inputs.now);
    if !rate_row.is_empty() {
        cost_parts.push(rate_row);
    }
    let cost_row = cost_parts.join(&sep);

    // --- assemble ---------------------------------------------------------
    let mut specs = vec![
        BoxRow {
            section: 0,
            label: path_label,
            content: path_row,
        },
        BoxRow {
            section: 0,
            label: "agent",
            content: agent_row,
        },
        BoxRow {
            section: 1,
            label: "model",
            content: model_row,
        },
        BoxRow {
            section: 1,
            label: "tokens",
            content: tokens_row,
        },
    ];
    for row in inputs.subagents {
        specs.push(BoxRow {
            section: 1,
            label: "agent",
            content: format_subagent_row(row),
        });
    }
    specs.push(BoxRow {
        section: 1,
        label: "cost",
        content: cost_row,
    });

    assemble(&specs)
}

fn format_rate_row(p: &Payload, now: i64) -> String {
    let five = p.rate_five_hour_percentage();
    let seven = p.rate_seven_day_percentage();
    if five.is_none() && seven.is_none() {
        return String::new();
    }
    let five_part = format_rate_window("5h", five, &p.rate_five_hour_resets_at(), 18_000, now);
    let seven_part = format_rate_window("7d", seven, &p.rate_seven_day_resets_at(), 604_800, now);
    match (five_part.is_empty(), seven_part.is_empty()) {
        (true, true) => String::new(),
        (false, true) => five_part,
        (true, false) => seven_part,
        (false, false) => format!("{five_part} {GRAY}·{RESET} {seven_part}"),
    }
}

/// Frames the rows: two sections split by a heavy divider, thin dividers
/// between rows within a section.
fn assemble(specs: &[BoxRow]) -> String {
    let mut inners: Vec<(u8, String, usize)> = Vec::new();
    for spec in specs {
        if spec.content.is_empty() {
            continue;
        }
        let label = format!("{:<LABEL_W$}", spec.label);
        let inner = format!(" {DIM}{label}{RESET} {GRAY}│{RESET}  {} ", spec.content);
        let width = visible_width(&inner);
        inners.push((spec.section, inner, width));
    }

    let max_inner = inners
        .iter()
        .map(|(_, _, w)| *w)
        .max()
        .unwrap_or(MIN_INNER)
        .max(MIN_INNER);

    let heavy = repeat('━', max_inner);
    let left = repeat('─', LABEL_W + 1);
    let right = repeat('─', max_inner.saturating_sub(LABEL_W + 4).max(1));
    let row_divider = format!(
        "{GRAY}┃{RESET} {GRAY}{left}{RESET}{GRAY}┼{RESET}{GRAY}{right}{RESET} {GRAY}┃{RESET}"
    );
    let section_divider = format!("{GRAY}┣{heavy}┫{RESET}");

    let mut out = format!("{GRAY}┏{heavy}┓{RESET}");
    let mut previous: Option<u8> = None;
    for (section, inner, width) in &inners {
        if let Some(previous) = previous {
            out.push('\n');
            out += if previous == *section {
                &row_divider
            } else {
                &section_divider
            };
        }
        previous = Some(*section);
        let padding = repeat(' ', max_inner.saturating_sub(*width));
        out += &format!("\n{GRAY}┃{RESET}{inner}{padding}{GRAY}┃{RESET}");
    }
    out += &format!("\n{GRAY}┗{heavy}┛{RESET}");
    out
}
