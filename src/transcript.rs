//! The transcript scan: message count, token totals, and the idle/working
//! verdict, from one pass over the session's JSONL file.
//!
//! Ported from the `# 5b/5c` awk program in `macos/statusline.sh`, which the
//! PowerShell script mirrors. Three properties are load-bearing:
//!
//! - **Byte-oriented.** The awk runs under `LC_ALL=C`; a UTF-8 locale once
//!   silently zeroed the token counts of any line with a non-BMP character —
//!   `docs/solutions/logic-errors/gawk-utf8-locale-zeroes-astral-plane-extraction.md`.
//!   Everything here works on `&[u8]`: no decoding step, no decoding bug, and a
//!   transcript is not guaranteed to be valid UTF-8 anyway.
//! - **Synthetic entries do not vote.** Slash commands, meta entries and tool
//!   results are filtered out of the idle verdict, or the detector sticks on
//!   "working" after any slash command.
//! - **A line counts only if it fits inside the sampled size**, read once
//!   before the scan. A line past it is a torn or racing tail; it and
//!   everything after it are skipped, so the consumed count covers an unbroken
//!   prefix and a transcript being appended to cannot miscount.

/// What one pass over a transcript yields.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Scan {
    /// Real user messages — synthetic entries excluded.
    pub messages: u64,
    pub input_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    /// The last vote, or the caller's starting value when nothing voted.
    pub idle: bool,
    /// Bytes of the unbroken prefix consumed. Always a record boundary, so a
    /// later scan can resume from it.
    pub consumed: u64,
}

/// The record format's version tag. Not restarted at `v1`: the scripts'
/// 16-field `v2` record is an incompatible format, and a shared version number
/// is how a stale record gets read as fresh. Bump this whenever the field list
/// changes.
pub const RECORD_VERSION: &str = "v5";

/// The per-session token record, the only part of the scripts' transcript
/// cache that survives the port. A render input, not a performance cache: the
/// `(+N)` beside each bucket is this tick's total minus the stored one, and an
/// unchanged transcript re-displays the stored deltas rather than recomputing
/// them to zero, which is why mtime and size are fields and not just the key.
///
/// Dropped from the scripts' record: the incremental parser's byte offset and
/// head checksum, and `working_start_out_tokens`, which both scripts compute
/// and store only to compute again — no platform renders it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub mtime: i64,
    pub size: u64,
    /// Carried so an unchanged transcript needs no scan: everything the tokens
    /// and model rows render is reconstructable from this record.
    pub messages: u64,
    pub idle: bool,
    pub input_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub delta_in: u64,
    pub delta_cache_write: u64,
    pub delta_cache_read: u64,
    pub delta_out: u64,
    /// Where the last scan stopped, always a record boundary. `Scan.consumed`
    /// is documented as one and was previously discarded.
    pub offset: u64,
    /// FNV-1a over exactly [`HEAD_SPAN`] bytes from the start of the file.
    pub head: u64,
    /// FNV-1a over the transcript's path. A **digest**, never the path itself:
    /// the record is one pipe-separated line whose parse rejects any wrong
    /// field count, every other field is numeric or boolean, and nothing has
    /// ever needed escaping — a path containing a pipe would split into too
    /// many fields and make the record permanently unreadable, which renders
    /// full totals as one combined delta on every tick rather than once.
    pub path_digest: u64,
    /// Consecutive resumes since the last full read. See [`MAX_RESUMES`].
    pub resumes: u64,
    /// Bytes appended since the last full read. See [`MAX_RESUMED_BYTES`].
    pub grown: u64,
}

impl TokenRecord {
    /// Serializes to the scripts' pipe-separated shape, version first.
    pub fn to_line(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            RECORD_VERSION,
            self.mtime,
            self.size,
            self.messages,
            self.idle,
            self.input_tokens,
            self.cache_write_tokens,
            self.cache_read_tokens,
            self.output_tokens,
            self.delta_in,
            self.delta_cache_write,
            self.delta_cache_read,
            self.delta_out,
            self.offset,
            self.head,
            self.path_digest,
            self.resumes,
            self.grown
        )
    }

    /// Parses a stored record, or `None` for anything that is not exactly one.
    /// A rejection lands where an absent record does: deltas against zero, one
    /// large increment that then settles, which is the scripts' behaviour on a
    /// failed validation too. The scripts bound each digit run to 18 characters
    /// against wrapping 64-bit shell arithmetic; parsing into `u64` is the
    /// stronger form, since an over-long run fails instead of wrapping.
    pub fn parse(raw: &str) -> Option<Self> {
        let fields: Vec<&str> = raw.trim_end_matches(['\r', '\n']).split('|').collect();
        if fields.len() != 18 || fields[0] != RECORD_VERSION {
            return None;
        }
        Some(Self {
            mtime: fields[1].parse().ok()?,
            size: fields[2].parse().ok()?,
            messages: fields[3].parse().ok()?,
            // Strict, rejecting the whole record rather than defaulting: this
            // field decides whether the transcript is read at all.
            idle: match fields[4] {
                "true" => true,
                "false" => false,
                _ => return None,
            },
            input_tokens: fields[5].parse().ok()?,
            cache_write_tokens: fields[6].parse().ok()?,
            cache_read_tokens: fields[7].parse().ok()?,
            output_tokens: fields[8].parse().ok()?,
            delta_in: fields[9].parse().ok()?,
            delta_cache_write: fields[10].parse().ok()?,
            delta_cache_read: fields[11].parse().ok()?,
            delta_out: fields[12].parse().ok()?,
            offset: fields[13].parse().ok()?,
            head: fields[14].parse().ok()?,
            path_digest: fields[15].parse().ok()?,
            resumes: fields[16].parse().ok()?,
            grown: fields[17].parse().ok()?,
        })
        // An offset past the size it was taken at is not a record this code
        // could have written, so it is rejected whole rather than clamped: the
        // gates below would each have to defend against it separately.
        .filter(|r: &Self| r.offset <= r.size)
    }

    /// Combines a fresh scan with the previous record into what renders now.
    /// Unchanged transcript (same mtime and size): the stored deltas are
    /// re-displayed rather than recomputed to zero, as the scripts do on a
    /// cache hit. Changed: deltas are this scan's totals minus the stored ones,
    /// saturating at zero, because a rotated transcript can shrink and
    /// both scripts clamp rather than render a negative increment. The totals
    /// always come from the scan handed in, never from the record, so a stale
    /// record can only misstate a delta; whether the transcript is read at all
    /// is `cmd::statusline`'s `(mtime, size)` decision. The second return value
    /// is whether the record needs storing, false when nothing changed, which
    /// keeps idle ticks off the disk.
    pub fn fold(
        prev: Option<&TokenRecord>,
        scan: &Scan,
        mtime: i64,
        size: u64,
        resume: &Resume,
    ) -> (Self, bool) {
        let unchanged = prev.is_some_and(|p| p.mtime == mtime && p.size == size);
        let (prev_in, prev_cw, prev_cr, prev_out) = match prev {
            Some(p) => (
                p.input_tokens,
                p.cache_write_tokens,
                p.cache_read_tokens,
                p.output_tokens,
            ),
            None => (0, 0, 0, 0),
        };
        let record = Self {
            mtime,
            size,
            messages: scan.messages,
            idle: scan.idle,
            input_tokens: scan.input_tokens,
            cache_write_tokens: scan.cache_write_tokens,
            cache_read_tokens: scan.cache_read_tokens,
            output_tokens: scan.output_tokens,
            delta_in: match prev {
                Some(p) if unchanged => p.delta_in,
                _ => scan.input_tokens.saturating_sub(prev_in),
            },
            delta_cache_write: match prev {
                Some(p) if unchanged => p.delta_cache_write,
                _ => scan.cache_write_tokens.saturating_sub(prev_cw),
            },
            delta_cache_read: match prev {
                Some(p) if unchanged => p.delta_cache_read,
                _ => scan.cache_read_tokens.saturating_sub(prev_cr),
            },
            delta_out: match prev {
                Some(p) if unchanged => p.delta_out,
                _ => scan.output_tokens.saturating_sub(prev_out),
            },
            offset: resume.offset,
            head: resume.head,
            path_digest: resume.path_digest,
            resumes: resume.resumes,
            grown: resume.grown,
        };
        let write = prev != Some(&record);
        (record, write)
    }
}

/// The resume half of a record: where the last scan stopped, what the file
/// looked like there, and how far the resume chain has run.
///
/// Passed into [`TokenRecord::fold`] rather than filled in afterwards, so the
/// offset and the totals land in **one atomic record line**. Split across two
/// writes, a torn pair double-counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Resume {
    pub offset: u64,
    pub head: u64,
    pub path_digest: u64,
    pub resumes: u64,
    pub grown: u64,
}

/// Where this session's token record lives. `None` for a session id that
/// sanitizes to nothing, which would otherwise share one
/// `statusline-tokens-.txt` across every such session.
pub fn record_path(temp: &std::path::Path, session_id: &str) -> Option<std::path::PathBuf> {
    let safe = crate::session::sanitize_session_id(session_id);
    if safe.is_empty() {
        return None;
    }
    Some(temp.join(format!("statusline-tokens-{safe}.txt")))
}

/// Scans `bytes`, counting only records that fit within `sampled_size`, the
/// file size observed before the read. `None` disables the gate, matching
/// the script's `total < 0` path when the size could not be determined:
/// everything present is consumed. `init_idle` holds if nothing here votes.
pub fn scan(bytes: &[u8], sampled_size: Option<u64>, init_idle: bool) -> Scan {
    let mut out = Scan {
        idle: init_idle,
        ..Default::default()
    };

    // A trailing newline ends the final record rather than starting an empty
    // one, as in awk; otherwise the ungated case consumes a phantom byte.
    let body = match bytes.last() {
        Some(b'\n') => &bytes[..bytes.len() - 1],
        _ => bytes,
    };

    let mut stop = false;
    for line in body.split(|b| *b == b'\n') {
        // The record includes its newline, present or not, so a final line
        // lacking one does not fit and is left for the next scan.
        let rec = line.len() as u64 + 1;
        if let Some(total) = sampled_size {
            if stop || out.consumed + rec > total {
                stop = true;
                continue;
            }
        }
        out.consumed += rec;

        if is_typed(line, b"\"user\"")
            && !contains(line, b"\"toolUseResult\"")
            && !keyed_literal(line, b"\"isMeta\"", b"true")
            && !contains(line, b"<command-name>")
            && !contains(line, b"<local-command-stdout>")
        {
            out.messages += 1;
        }

        if is_typed(line, b"\"assistant\"") {
            out.input_tokens = out
                .input_tokens
                .saturating_add(token_value(line, b"\"input_tokens\""));
            out.cache_write_tokens = out
                .cache_write_tokens
                .saturating_add(token_value(line, b"\"cache_creation_input_tokens\""));
            out.cache_read_tokens = out
                .cache_read_tokens
                .saturating_add(token_value(line, b"\"cache_read_input_tokens\""));
            out.output_tokens = out
                .output_tokens
                .saturating_add(token_value(line, b"\"output_tokens\""));
        }

        // The idle vote's filter is deliberately not the message filter above:
        // `"isMeta"` is excluded whatever its value, and `toolUseResult` and
        // `<local-command-` are matched unquoted and unterminated. The last
        // surviving entry wins, the forward equivalent of the reverse scan.
        if !contains(line, b"\"isMeta\"")
            && !contains(line, b"<command-name>")
            && !contains(line, b"<local-command-")
            && !contains(line, b"toolUseResult")
        {
            if ordered(line, b"\"type\"", b"\"assistant\"") {
                out.idle = ordered(line, b"\"stop_reason\"", b"\"end_turn\"");
            } else if ordered(line, b"\"type\"", b"\"user\"") {
                out.idle = contains(line, b"Request interrupted by user");
            }
        }
    }

    out
}

/// `/"type"[[:space:]]*:[[:space:]]*<literal>/`, used for the counting rules.
/// Distinct from [`ordered`], which the voting rule uses and needs no colon.
/// The scripts use both, and reconciling them would change which entries count.
fn is_typed(line: &[u8], literal: &[u8]) -> bool {
    keyed_literal(line, b"\"type\"", literal)
}

/// `/<key>[[:space:]]*:[[:space:]]*<literal>/` anywhere in the line.
fn keyed_literal(line: &[u8], key: &[u8], literal: &[u8]) -> bool {
    let mut from = 0;
    while let Some(i) = find_from(line, key, from) {
        let p = skip_ws(line, i + key.len());
        if line.get(p) == Some(&b':') {
            let v = skip_ws(line, p + 1);
            if line[v..].starts_with(literal) {
                return true;
            }
        }
        from = i + 1;
    }
    false
}

/// The digits following the **last** `<key>` occurrence that is followed by a
/// colon, or 0. A faithful port of the awk `tok()` helper, two behaviours of
/// which are deliberate: an occurrence not followed by a colon leaves the
/// previous value standing, so `"input_tokens"` inside a quoted string cannot
/// wipe a real reading; one followed by a colon but not digits (`null`, a
/// float) clears it to 0. Values are read as bytes, never decoded — see the
/// module doc.
fn token_value(line: &[u8], key: &[u8]) -> u64 {
    let mut last: Option<u64> = None;
    let mut from = 0;
    while let Some(i) = find_from(line, key, from) {
        let p = skip_ws(line, i + key.len());
        if line.get(p) == Some(&b':') {
            let start = skip_ws(line, p + 1);
            let mut end = start;
            while end < line.len() && line[end].is_ascii_digit() {
                end += 1;
            }
            // Non-digits, or a run too long for u64, both read as 0: bash's
            // `^[0-9]+$` guard rejected awk's scientific-notation float too.
            last = Some(
                std::str::from_utf8(&line[start..end])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            );
        }
        from = i + 1;
    }
    last.unwrap_or(0)
}

/// `/<first>.*<second>/`. Only the first `first` needs testing: any later one
/// that satisfies the pattern implies the first does too.
fn ordered(line: &[u8], first: &[u8], second: &[u8]) -> bool {
    match find_from(line, first, 0) {
        Some(i) => find_from(line, second, i + first.len()).is_some(),
        None => false,
    }
}

fn contains(line: &[u8], needle: &[u8]) -> bool {
    find_from(line, needle, 0).is_some()
}

/// Substring search, first-byte scan then compare. `windows(n).position(..)`
/// compares byte-at-a-time at every offset; this form lets the first-byte scan
/// vectorize. On a multi-megabyte transcript that is the difference between a
/// scan you notice and one you do not, and why no search crate is needed.
fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let (first, rest) = needle.split_first()?;
    if from > hay.len() {
        return None;
    }
    let mut i = from;
    while let Some(off) = hay[i..].iter().position(|b| b == first) {
        let start = i + off;
        match hay.get(start + 1..) {
            Some(tail) if tail.starts_with(rest) => return Some(start),
            _ => i = start + 1,
        }
    }
    None
}

/// The awk `wskip` character class, which is wider than ASCII space and tab.
fn skip_ws(line: &[u8], mut p: usize) -> usize {
    while p < line.len() && matches!(line[p], b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b) {
        p += 1;
    }
    p
}

/// How many bytes of the head the checksum covers. **Always exactly this
/// many**, never `min(4096, size)`.
///
/// A checksum over `min(4096, size)` covers a different span as a young file
/// grows past 4 KB, so a naive comparison mismatches on a pure append and
/// rescans every tick until the file passes 4 KB — a silent hit-rate bug of the
/// class byte-diffing cannot see. [`RESUME_FLOOR`] deletes that comparison
/// instead of fixing it: below the floor the file is read whole anyway, so the
/// span is never partial.
pub const HEAD_SPAN: usize = 4096;

/// Below this, always read the whole file.
///
/// The entire benefit of resuming under it is avoiding a full scan of a small
/// file, which at ~6.8 ms/MB is 0.03 ms against a ~6.5 ms process floor. The
/// floor buys the deletion of a whole bug class for nothing measurable.
pub const RESUME_FLOOR: u64 = 64 * 1024;

/// A full read is forced after this many consecutive resumes.
///
/// The entry gates reduce the probability of a poisoned offset but cannot bound
/// its lifetime, and the boundary anchor is probabilistic — in JSONL roughly one
/// byte per line is a newline. Every other degradation in this project
/// self-heals on the next tick; once an offset is stored, a wrong one is reused
/// by every later tick and the totals stay wrong with no signal and no absurd
/// number to notice. This is what restores that property. On an 8 MB transcript
/// it costs about 0.9 ms per tick amortised.
pub const MAX_RESUMES: u64 = 64;

/// A full read is also forced after this much growth since the last one, so a
/// few very large appends cannot stretch the window the count bounds.
pub const MAX_RESUMED_BYTES: u64 = 4 * 1024 * 1024;

/// FNV-1a, 64-bit.
///
/// Hand-rolled, and no dependency: nothing in std is safe to persist —
/// `DefaultHasher`'s algorithm is explicitly not stable across releases, so a
/// stored value would silently change meaning on a toolchain upgrade, an
/// unversioned format change `RECORD_VERSION` cannot catch. A hashing crate
/// would also risk linking a Windows DLL, measured at ~1.8 ms per tick when not
/// delay-loaded. FNV-1a is deterministic, byte-oriented and adequate for change
/// detection, which is the whole job.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// What the head checksum and the boundary anchor say, read only once the
/// cheap gates have passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// FNV-1a over exactly [`HEAD_SPAN`] bytes from the start.
    pub head: u64,
    /// Whether the byte immediately before the stored offset is a newline.
    pub anchor_is_newline: bool,
}

/// Why a tick read the whole transcript instead of resuming.
///
/// Every variant is a fall-through to a correct-but-slower recompute, never to
/// a wrong answer. They are named because a silent regression to the slow path
/// renders identically — the blind spot
/// `docs/solutions/best-practices/byte-diff-cannot-see-cache-hit-regressions.md`
/// exists to close — so the reason is an observable a test asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullReason {
    /// No record, or one this version cannot read.
    NoRecord,
    /// The record describes a different transcript file.
    PathChanged,
    /// The record is stamped later than the file it describes.
    RecordAheadOfFile,
    /// The file did not strictly grow. **`<=`, not `<`**: a same-size rewrite
    /// has an empty tail, so resuming would re-display stale totals *and*
    /// rewrite the record with the new mtime, after which the cheap skip hits
    /// forever. Today that state self-heals through a full rescan, and this is
    /// what stops the change converting a self-healing case into a permanent
    /// one.
    NotGrown,
    /// The stored offset is past the end of the file.
    OffsetPastEnd,
    /// Smaller than [`RESUME_FLOOR`].
    BelowFloor,
    /// The ceiling fired: [`MAX_RESUMES`] or [`MAX_RESUMED_BYTES`].
    CeilingReached,
    /// The first [`HEAD_SPAN`] bytes are not the ones the record was taken
    /// over, so the file was rewritten rather than appended to.
    HeadChanged,
    /// The byte before the offset is not a newline. A head checksum cannot see
    /// a rewrite that preserves the first 4 KB and changes the middle, and
    /// unlike today's divergence that error never self-heals, because every
    /// later tick resumes from the poisoned record.
    AnchorMoved,
}

impl FullReason {
    /// A short tag for the debug line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoRecord => "no record",
            Self::PathChanged => "transcript path changed",
            Self::RecordAheadOfFile => "record newer than the file",
            Self::NotGrown => "file did not grow",
            Self::OffsetPastEnd => "offset past the end",
            Self::BelowFloor => "below the resume floor",
            Self::CeilingReached => "resume ceiling reached",
            Self::HeadChanged => "head checksum changed",
            Self::AnchorMoved => "boundary anchor moved",
        }
    }
}

/// What this tick should do with the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Nothing moved. Everything the rows render is already in the record.
    Skip,
    /// Scan the bytes from `from` onwards and add them to the stored totals.
    Resume { from: u64 },
    /// Scan the whole file from zero.
    Full(FullReason),
}

/// Decides what to do, as a pure function over the record and the file's
/// identity, with the two content checks supplied lazily by `probe`.
///
/// Three tiers, tested in order: the `(mtime, size)` skip that has always been
/// here, then the resume, then the full read. Every gate that fails falls
/// through to the full read rather than to a wrong answer, and the reason comes
/// back with it. `probe` is a closure so the two reads it needs — 4 KB from the
/// head and one byte before the offset — happen only once the cheap gates have
/// passed, and so the whole state table is testable with no filesystem at all.
pub fn decide(
    record: Option<&TokenRecord>,
    path_digest: u64,
    mtime: i64,
    size: u64,
    probe: impl FnOnce() -> Option<Probe>,
) -> Decision {
    let Some(record) = record else {
        return Decision::Full(FullReason::NoRecord);
    };

    // Tier one, unchanged: everything the tokens and model rows render is in
    // the record already, so the file is not opened at all.
    if record.mtime == mtime && record.size == size {
        return Decision::Skip;
    }

    // The record is keyed on the path as well as the session id: the record
    // path is derived from the id alone, so one session pointed at a new
    // transcript would otherwise apply the old file's offset to the new one —
    // and two transcripts of one session can share an identical opening, which
    // defeats the head checksum exactly where it is needed.
    if record.path_digest != path_digest {
        return Decision::Full(FullReason::PathChanged);
    }
    if record.mtime > mtime {
        return Decision::Full(FullReason::RecordAheadOfFile);
    }
    if size <= record.size {
        return Decision::Full(FullReason::NotGrown);
    }
    if record.offset > size {
        return Decision::Full(FullReason::OffsetPastEnd);
    }
    if size < RESUME_FLOOR {
        return Decision::Full(FullReason::BelowFloor);
    }
    if record.resumes >= MAX_RESUMES || record.grown >= MAX_RESUMED_BYTES {
        return Decision::Full(FullReason::CeilingReached);
    }

    let Some(probe) = probe() else {
        return Decision::Full(FullReason::HeadChanged);
    };
    if probe.head != record.head {
        return Decision::Full(FullReason::HeadChanged);
    }
    if !probe.anchor_is_newline {
        return Decision::Full(FullReason::AnchorMoved);
    }
    Decision::Resume {
        from: record.offset,
    }
}
