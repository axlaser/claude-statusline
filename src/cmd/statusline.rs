//! The status line itself: gather, render, alert.
//!
//! This is the only place the four data sources meet. Everything it calls is
//! either a pure function or a guarded read, and the order is the one the
//! scripts use — the learned map is refreshed before the subagent rows resolve
//! their windows against it, and the alerts fire after the render is already a
//! string, so a slow notification cannot delay the output.
//!
//! There is no output cache. The scripts kept one because a fresh
//! interpreter cost more than the work; the binary recomputes every tick.

use std::path::{Path, PathBuf};

use crate::clock::Clock;
use crate::config::NotifyConfig;
use crate::git::GitStatus;
use crate::notify_state::{self, LatchState};
use crate::payload::Payload;
use crate::render::{self, Inputs};
use crate::session::{sanitize_session_id, state_dir, StateRoot};
use crate::subagent::{self, Row, Windows};
use crate::transcript::{self, Scan, TokenRecord};

/// The two filesystem roots every state path hangs off.
///
/// Passed rather than read from the environment at each use site. The scripts
/// had no choice — a shell reads `$HOME` wherever it stands — but ambient reads
/// make the render path untestable in-process, and a case has to pin every
/// render input, which these are. `from_env` is the production
/// construction and the only place the variables are consulted.
pub struct Roots {
    pub home: Option<PathBuf>,
    /// Where state lives, and whether this binary owns that directory. It
    /// derefs to `Path`, so every `roots.temp.join(..)` reads as before.
    pub temp: StateRoot,
}

impl Roots {
    pub fn from_env() -> Self {
        Self {
            home: crate::home_dir(),
            temp: state_dir(),
        }
    }

    fn claude_dir(&self) -> Option<PathBuf> {
        self.home.as_ref().map(|h| h.join(".claude"))
    }

    /// Where the learned model→window map lives.
    pub fn model_windows_path(&self) -> Option<PathBuf> {
        self.claude_dir()
            .map(|d| d.join("statusline-model-windows.json"))
    }

    fn notify_config_path(&self) -> Option<PathBuf> {
        self.claude_dir().map(|d| d.join("notify-config.json"))
    }
}

/// Renders one refresh and fires whatever alerts it crossed.
///
/// Returns the bytes to write to stdout. Degraded input returns the bad-JSON
/// notice and does nothing else — no state is read, written, or invalidated,
/// because a tick that could not be understood must not overwrite what the
/// last good tick recorded.
pub fn run(clock: &dyn Clock, roots: &Roots, raw: &str) -> String {
    let Some(payload) = Payload::parse(raw) else {
        // The one user-visible failure the status line has, and until now it
        // wrote nothing to the log — so the scenario README's own
        // troubleshooting section describes produced no evidence at all, even
        // with STATUSLINE_DEBUG=1. The reason is recovered by re-parsing rather
        // than plumbed out of `Payload::parse`, which keeps that function pure
        // and keeps the cost behind the flag: this runs only when logging is on.
        if crate::debug::is_enabled() {
            let n = raw.len();
            let reason = match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(_) => "valid JSON, but not an object".to_string(),
                Err(e) => e.to_string(),
            };
            crate::debug::log(move || {
                format!("statusline: {n} byte(s) of input did not parse: {reason}")
            });
        }
        return render::BAD_JSON.to_string();
    };

    let home_str = roots
        .home
        .as_deref()
        .and_then(Path::to_str)
        .map(str::to_owned);
    let session_id = payload.session_id().to_string();

    let git = git_status(clock, roots, &payload, &session_id);
    let (scan, record) = transcript_state(clock, roots, &payload, &session_id);

    // Refreshed before the rows resolve, so a session on a newly seen model
    // gives its own subagents a real denominator on the very first refresh
    // rather than one tick later.
    let learned = learn_and_load(roots, &payload);
    let windows = Windows::new(payload.model_id(), payload.context_window_size(), learned);
    let subagents = subagent_rows(clock, roots, &payload, &session_id, &windows);

    let output = render::render(&Inputs {
        payload: &payload,
        home: home_str.as_deref(),
        git: git.as_ref(),
        scan: scan.as_ref(),
        record: record.as_ref(),
        subagents: &subagents,
        now: clock.now_unix(),
    });

    fire_alerts(roots, &payload, &session_id, &output);
    output
}

fn git_status(
    clock: &dyn Clock,
    roots: &Roots,
    payload: &Payload,
    session_id: &str,
) -> Option<GitStatus> {
    let cwd = crate::git::resolve_cwd(payload.git_cwd());
    crate::git::status(clock, &roots.temp, &cwd, session_id)
}

/// This tick's totals, and the record that carries the per-bucket deltas.
///
/// The totals always come from this tick's scan; only the deltas come from the
/// stored record. That is what lets the head checksum the scripts needed
/// go away: they cached the totals, so a same-size rewrite was invisible.
fn transcript_state(
    clock: &dyn Clock,
    roots: &Roots,
    payload: &Payload,
    session_id: &str,
) -> (Option<Scan>, Option<TokenRecord>) {
    let path = payload.transcript_path();
    if path.is_empty() {
        return (None, None);
    }
    let path = Path::new(path);
    let Ok(meta) = std::fs::metadata(path) else {
        return (None, None);
    };
    let size = meta.len();
    let mtime = clock.mtime_unix(path).unwrap_or(0);

    let record_path = transcript::record_path(&roots.temp, session_id);
    let previous = record_path
        .as_deref()
        .and_then(crate::state::read_trusted)
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| TokenRecord::parse(&t));

    // An unchanged transcript is not read at all. Everything the tokens row and
    // the model row render is already in the record, so re-scanning reproduces
    // it byte for byte at the cost of the whole file.
    //
    // This is the one place the port keeps a *computation* cache, and it is
    // here on measured grounds rather than by symmetry with the scripts. The
    // paired measurements found the unconditional rescan costing ~50 ms on an 8 MB
    // transcript, which is invisible next to PowerShell's ~124 ms interpreter
    // floor but is four times bash's entire tick — so dropping the scripts'
    // incremental parser was right on Windows and a regression on Linux.
    // A static transcript is what a session looks like between messages, which
    // is most ticks.
    //
    // What always scanning gave up was noticing a same-size rewrite. That
    // trade is reversed here deliberately: transcripts are append-only JSONL,
    // a rewrite landing on the byte-identical length is close to unreachable,
    // and the mtime has to match as well. The scripts' version of this bug came
    // from caching totals behind a key that could go stale *and* having no
    // second signal; `(mtime, size)` together is that second signal.
    if let Some(record) = previous
        .clone()
        .filter(|p| p.mtime == mtime && p.size == size)
    {
        crate::debug::log(|| "transcript: unchanged, scan skipped".to_string());
        let scan = Scan {
            messages: record.messages,
            input_tokens: record.input_tokens,
            cache_write_tokens: record.cache_write_tokens,
            cache_read_tokens: record.cache_read_tokens,
            output_tokens: record.output_tokens,
            idle: record.idle,
            // Not stored and not rendered: `consumed` exists for the scan's own
            // torn-tail bound, and nothing downstream reads it.
            consumed: 0,
        };
        return (Some(scan), Some(record));
    }

    // A failed read must not become an empty scan. `fold` would treat its zero
    // totals as authoritative, write that record with this tick's
    // (mtime, size), and the skip above would then serve the zero record on
    // every later tick — one transient error rendering the tokens row wrong
    // until the transcript happens to change again. Degrade this tick instead
    // and leave the stored record for the next one to retry against.
    let Ok(bytes) = std::fs::read(path) else {
        crate::debug::log(|| "transcript: read failed, stored record left intact".to_string());
        return (None, None);
    };
    let scan = transcript::scan(&bytes, Some(size), true);
    let (record, needs_write) = TokenRecord::fold(previous.as_ref(), &scan, mtime, size);

    if needs_write {
        if let Some(p) = record_path.as_deref() {
            // A record that never lands means the next tick re-scans the whole
            // transcript, and the one after that, forever -- the cost the
            // (mtime, size) skip exists to avoid. Nothing else can report it.
            let outcome =
                crate::state::write_guarded_under(&roots.temp, p, record.to_line().as_bytes());
            if outcome != crate::state::WriteOutcome::Written {
                let path = p.display().to_string();
                crate::debug::log(move || {
                    format!("token record not persisted to {path}: {outcome:?}")
                });
            }
        }
    }
    (Some(scan), Some(record))
}

/// Merges this session's model→window pair into the learned map and returns
/// the map to resolve against.
///
/// The write is skipped when the entry already matches, so an unchanged pair
/// leaves the file's mtime alone — the scripts key their output cache on that
/// mtime, and churning it every tick would have defeated it.
fn learn_and_load(roots: &Roots, payload: &Payload) -> std::collections::BTreeMap<String, u64> {
    let Some(path) = roots.model_windows_path() else {
        return Default::default();
    };
    let mut map = Windows::load_learned(&path);

    let key = subagent::normalize_model_id(payload.model_id());
    let Some(window) = payload.context_window_size() else {
        return map;
    };
    if key.is_empty() || map.get(&key) == Some(&window) {
        return map;
    }

    // Re-read as the merge base rather than serialising the flattened map: a
    // concurrent session may have learned a different model since the load
    // above, and writing the stale view would drop its entry.
    let mut base = Windows::load_learned(&path);
    base.insert(key.clone(), window);
    let body = serde_json::to_string(&base).unwrap_or_default();
    if !body.is_empty() {
        // Report what actually happened. Logging "learned" unconditionally
        // claimed success on a hostile or unwritable target, which is the one
        // case where the log is the only way to find out.
        let outcome = crate::state::write_guarded(&path, format!("{body}\n").as_bytes());
        crate::debug::log(|| format!("model-windows: learned {key}={window} -> {outcome:?}"));
    }
    map.insert(key, window);
    map
}

/// One tier per refresh, never merged: the feed when it is fresh, else the
/// per-agent transcripts.
fn subagent_rows(
    clock: &dyn Clock,
    roots: &Roots,
    payload: &Payload,
    session_id: &str,
    windows: &Windows,
) -> Vec<Row> {
    let safe = sanitize_session_id(session_id);
    if !safe.is_empty() {
        let feed = crate::cmd::subagent::feed_path(&roots.temp, &safe);
        if subagent::feed_is_fresh(clock, &feed) {
            let json = crate::state::read_trusted(&feed)
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            if let Some(rows) =
                subagent::rows_from_feed(clock, &roots.temp, session_id, &json, windows)
            {
                return rows;
            }
        }
    }
    subagent::rows_from_transcripts(
        clock,
        &roots.temp,
        session_id,
        payload.transcript_path(),
        windows,
    )
}

/// Reads the latch, decides the edges, spawns what crossed, stores the result.
///
/// `rendered` is taken only so this cannot be called before the render exists —
/// the alert must never sit between the work and the output.
fn fire_alerts(roots: &Roots, payload: &Payload, session_id: &str, rendered: &str) {
    let _ = rendered;
    let Some(path) = notify_state::latch_path(&roots.temp, session_id) else {
        return;
    };
    let config = roots
        .notify_config_path()
        .map(|p| NotifyConfig::load(&p))
        .unwrap_or_default();

    let ctx_pct = payload
        .used_percentage()
        .map(render::round_pct)
        .unwrap_or(0);
    let (rate_max, resets_now) = rate_inputs(payload);

    // Read once. The write below needs to know whether the latch was usable,
    // and re-reading the file to find out cost a second open/read of the same
    // path on every tick that crossed a threshold. Matching a fieldless variant
    // binds nothing, so this does not move the value out from under `decide`.
    let latch = notify_state::read_latch(&path);
    let latch_unusable = matches!(latch, LatchState::Unusable);

    let decision = notify_state::decide(
        latch,
        ctx_pct,
        config.threshold("context_high"),
        rate_max,
        config.threshold("rate_limit"),
        &resets_now,
    );

    for alert in &decision.alerts {
        // the per-event flags gate delivery. A muted event still latches,
        // so unmuting mid-window does not immediately fire for a crossing the
        // user already lived through.
        let event = config.event(alert.event);
        if event.sound || event.visual {
            // Click handling exists only for a toast (KD4, R5): a sound-only
            // alert captures nothing, so the alert path stays as cheap as it
            // was. Capture happens here, in the tick, because the detached
            // child cannot see the terminal this session runs in.
            let key = if event.visual {
                crate::focus::capture(&roots.temp, session_id, crate::debug::is_enabled())
            } else {
                None
            };
            notify_state::spawn(alert, key.as_ref());
        }
    }
    if decision.changed && !latch_unusable {
        // A latch that does not persist re-fires the same alert on the next
        // tick, so a failure here is worth a line in the debug log -- it is the
        // only channel that can carry it.
        let outcome = crate::state::write_guarded_under(
            &roots.temp,
            &path,
            notify_state::latch_json(&decision.latch).as_bytes(),
        );
        if outcome != crate::state::WriteOutcome::Written {
            let path = path.display().to_string();
            crate::debug::log(move || format!("notify latch not persisted to {path}: {outcome:?}"));
        }
    }
}

/// The higher of the two rate windows, and the `resets_at` that belongs to it.
fn rate_inputs(payload: &Payload) -> (i64, String) {
    let five = payload.rate_five_hour_percentage();
    let seven = payload.rate_seven_day_percentage();
    let five_int = five.map(|v| v.trunc() as i64);
    let seven_int = seven.map(|v| v.trunc() as i64);

    let max = five_int.unwrap_or(0).max(seven_int.unwrap_or(0));
    let resets = match (five_int, seven_int) {
        (Some(f), Some(s)) if s > f => payload.rate_seven_day_resets_at(),
        (None, Some(_)) => payload.rate_seven_day_resets_at(),
        _ => payload.rate_five_hour_resets_at(),
    };
    (max, resets.to_string())
}
