//! Whether a newer Claude Code exists, answered from a file Claude Code
//! already keeps.
//!
//! The payload carries the running `version` and nothing else about updates:
//! there is no "newer available" flag to read, and learning one first-hand
//! would cost a network call behind a subprocess, which the per-tick path does
//! not get. The one forward-looking artifact already on disk is
//! `~/.claude/cache/changelog.md`, Claude Code's cached copy of its own
//! CHANGELOG, whose first `## X.Y.Z` heading is the newest version that
//! existed when Claude Code last fetched it.
//!
//! What that buys and what it costs, stated once:
//!
//! - It is install-method independent. The npm-global, npm-local and native
//!   installers put the program itself in three different places; they all
//!   write this one cache.
//! - A stale cache under-reports. It is refreshed by the main process, not by
//!   this one, so a user who has not started Claude Code in a week sees the
//!   version that was newest a week ago. Under-reporting is the safe
//!   direction: the row stays quiet about an update rather than inventing one.
//! - It can lead the registry by minutes, because the heading lands in the
//!   CHANGELOG on `main` when the release is cut. The signal is "a newer
//!   version exists", not "an update will install right now".

use std::path::Path;

/// How much of the changelog is read. The heading sits on line 3 of the file
/// Claude Code writes; the file itself is ~700 KB, so reading it whole to find
/// a number in its first hundred bytes would make it the largest read on the
/// tick by an order of magnitude.
const PREFIX: u64 = 4096;

/// The newer version, or `None` when this session is already running it.
///
/// Every failure path is `None`: no version in the payload (a Claude Code old
/// enough not to send one), no cache, a cache that fails the guard, no heading
/// in the prefix. The segment then renders the running version alone, which is
/// what a user with nothing to act on should see.
pub fn available(changelog: Option<&Path>, running: &str) -> Option<String> {
    // A running version this comparison cannot read as a number is not one to
    // compare against: `numeric_core` normalises it to zero, every changelog
    // heading then beats it, and the module's under-report promise inverts
    // into a permanent false alarm — `v2.1.278` would advertise an update to
    // the version it is already running. The same shape guard `latest_in`
    // applies to the changelog side, and it subsumes the empty case.
    if !running.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let bytes = crate::state::read_trusted_prefix(changelog?, PREFIX)?;
    // Lossy rather than strict: a multibyte character cut in half by the
    // prefix bound must not discard a heading that parsed fine above it.
    let latest = latest_in(&String::from_utf8_lossy(&bytes))?.to_string();
    is_newer(&latest, running).then_some(latest)
}

/// The first `## X.Y.Z` heading, which is the newest release the file records.
///
/// Matched by shape, not against a list of known versions: the file is written
/// by another program and this has to keep working when its numbering moves
/// on. A heading whose text is not a digit is skipped rather than ending the
/// search, so an `## Unreleased` section above the releases costs nothing — and
/// if that program ever switches to a bracketed `## [X.Y.Z]`, every heading is
/// skipped and the row goes quiet, which is the direction to fail in.
pub fn latest_in(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("## ")?.trim();
        // Truncated to the dotted digits, not returned whole. The token is
        // only whitespace-delimited, and ESC is not whitespace, so a heading
        // reading `## 9<ESC>[2J` would otherwise hand the render sink an
        // escape sequence to print. Nothing downstream wants more than the
        // number anyway — `is_newer` cuts at the same place.
        let token = numeric_core(rest.split_whitespace().next()?);
        token
            .starts_with(|c: char| c.is_ascii_digit())
            .then_some(token)
    })
}

/// Dotted numeric comparison, component by component.
///
/// A component absent on one side counts as zero, which makes `2.2` newer than
/// `2.1.278` and equal to `2.2.0`. Numeric because the lexical comparison is
/// wrong in exactly the range this ships in: `2.1.9` sorts above `2.1.10`.
pub fn is_newer(candidate: &str, running: &str) -> bool {
    let mut a = numeric_core(candidate).split('.');
    let mut b = numeric_core(running).split('.');
    loop {
        let (left, right) = (a.next(), b.next());
        if left.is_none() && right.is_none() {
            return false;
        }
        let (left, right) = (component(left), component(right));
        if left != right {
            return left > right;
        }
    }
}

/// The leading `1.2.3` of a version, with any suffix cut away.
///
/// Cutting at the suffix rather than per component is load-bearing: a
/// prerelease must not read as *newer* than the release it precedes, and
/// `2.2.0-rc.1` split naively is four components against `2.2.0`'s three, so
/// the fourth would win a comparison the third had tied.
fn numeric_core(v: &str) -> &str {
    let end = v
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(v.len());
    &v[..end]
}

fn component(part: Option<&str>) -> u64 {
    part.unwrap_or("0").parse().unwrap_or(0)
}
