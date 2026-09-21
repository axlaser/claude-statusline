//! The status line itself: gather, render, alert.
//!
//! The only place the four data sources meet, in the scripts' order: the
//! learned map is refreshed before the subagent rows resolve against it, and
//! alerts fire after the render is a string so a slow notification cannot
//! delay the output. There is no output cache: the scripts kept one to hide
//! interpreter startup, and the binary recomputes every tick.

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
/// Passed rather than read from the environment, unlike the scripts, because
/// ambient reads make the render path untestable in-process. `from_env` is the
/// only place the variables are consulted.
pub struct Roots {
    pub home: Option<PathBuf>,
    /// Where state lives, and whether this binary owns that directory.
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

/// Renders one refresh and fires whatever alerts it crossed. Degraded input
/// returns the bad-JSON notice and touches no state: a tick that could not be
/// understood must not overwrite what the last good tick recorded.
pub fn run(clock: &dyn Clock, roots: &Roots, raw: &str) -> String {
    let Some(payload) = Payload::parse(raw) else {
        // The reason is recovered by re-parsing rather than plumbed out of
        // `Payload::parse`, which keeps that function pure and the cost behind
        // the flag.
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

    // Refreshed before the rows resolve, so a newly seen model gives its own
    // subagents a real denominator on the first refresh, not one tick later.
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
/// Totals come from the scan and only deltas from the record, which is what
/// let the scripts' head checksum go away: they cached the totals, so a
/// same-size rewrite was invisible.
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

    // An unchanged transcript is not read at all: everything the tokens and
    // model rows render is already in the record. This is the port's one
    // computation cache, kept on measured grounds: an unconditional rescan cost
    // ~50 ms on an 8 MB transcript, invisible next to PowerShell's ~124 ms
    // interpreter floor but four times bash's entire tick, and a static
    // transcript is most ticks.
    //
    // The cost is missing a same-size rewrite. Accepted deliberately:
    // transcripts are append-only JSONL, and the mtime must match too. The
    // scripts' version of this bug cached totals behind a stale-able key with
    // no second signal; `(mtime, size)` is that second signal.
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
            // Not stored: `consumed` is the scan's own torn-tail bound and
            // nothing downstream reads it.
            consumed: 0,
        };
        return (Some(scan), Some(record));
    }

    // A failed read must not become an empty scan: `fold` would record zero
    // totals under this tick's (mtime, size) and the skip above would serve
    // them until the transcript changes again. Degrade this tick instead.
    let Ok(bytes) = std::fs::read(path) else {
        crate::debug::log(|| "transcript: read failed, stored record left intact".to_string());
        return (None, None);
    };
    let scan = transcript::scan(&bytes, Some(size), true);
    let (record, needs_write) = TokenRecord::fold(previous.as_ref(), &scan, mtime, size);

    if needs_write {
        if let Some(p) = record_path.as_deref() {
            // A record that never lands means every later tick re-scans the
            // whole transcript, and nothing else can report it.
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
/// the map to resolve against. An already-matching entry is not rewritten: the
/// scripts key their output cache on the file's mtime.
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

    // Re-read as the merge base: a concurrent session may have learned another
    // model since the load above, and writing the stale view would drop it.
    let mut base = Windows::load_learned(&path);
    base.insert(key.clone(), window);
    let body = serde_json::to_string(&base).unwrap_or_default();
    if !body.is_empty() {
        // Log the outcome, not "learned": on a hostile or unwritable target
        // the log is the only way to find out.
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
/// `rendered` is taken only so this cannot be called before the render exists:
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

    // Read once: the write below needs to know whether the latch was usable,
    // and re-reading cost a second open of the same path per crossing tick.
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
        // A muted event still latches, so unmuting mid-window does not fire for
        // a crossing the user already lived through.
        let event = config.event(alert.event);
        if event.sound || event.visual {
            // Click handling exists only for a toast; a sound-only
            // alert captures nothing. Capture happens in the tick because the
            // detached child cannot see the terminal this session runs in.
            let key = if event.visual {
                crate::focus::capture(&roots.temp, session_id, crate::debug::is_enabled())
            } else {
                None
            };
            notify_state::spawn(alert, key.as_ref());
        }
    }
    if decision.changed && !latch_unusable {
        // A latch that does not persist re-fires the same alert next tick; the
        // debug log is the only channel that can carry the failure.
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
