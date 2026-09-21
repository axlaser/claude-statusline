//! The stdin payload: one tolerant reader over the JSON contract Claude Code
//! pipes in on every refresh.
//!
//! Deliberately untyped: a `#[derive(Deserialize)]` model would reject the
//! whole document when one field's type changes, which under silent
//! degradation is a blank status line. The scripts pull each field out on its
//! own, so a bad field costs exactly the row it feeds; the helpers here answer
//! "absent" for anything they cannot use, to the same effect.
//!
//! The field list came from the JSON-extraction block in `macos/statusline.sh`
//! and the accessors are named after its `J_*` variables; see `eb56345` for
//! the script's final state. `CLAUDE.md` documents the fields.
//!
//! Nothing here scrubs. [`sanitize_display`] is applied at the render sink,
//! not at ingest, because scrubbing on the way in would corrupt values that
//! never reach the screen, notably `transcript_path`.

use std::borrow::Cow;

use serde_json::Value;

/// A parsed stdin payload. Construction fails only in the two cases
/// the scripts treat as fatal (see [`Payload::parse`]); every field read
/// after that degrades instead of failing.
pub struct Payload {
    root: Value,
}

impl Payload {
    /// Parses the raw stdin bytes, or `None` for input the scripts render
    /// `[statusline: bad JSON]` for.
    ///
    /// - **Unparseable input**, including empty stdin: both scripts reach their
    ///   bad-JSON branch on it, because jq emits no rows and `ConvertFrom-Json`
    ///   has nothing to convert.
    /// - **Valid JSON that is not an object**: the bash filter's explicit `if
    ///   type != "object" then error` guard. Windows had no such check and
    ///   rendered a defaults-only line; the bash behaviour was written on
    ///   purpose, so it is the one ported. Recorded as a resolved divergence.
    pub fn parse(raw: &str) -> Option<Self> {
        match serde_json::from_str::<Value>(raw) {
            Ok(root) if root.is_object() => Some(Self { root }),
            _ => None,
        }
    }

    /// Walks a path; `.get()` on a non-object link is `None`, so a payload
    /// where `context_window` is a string simply has no `used_percentage`.
    fn at(&self, path: &[&str]) -> Option<&Value> {
        let mut cur = &self.root;
        for key in path {
            cur = cur.get(key)?;
        }
        Some(cur)
    }

    /// A string field, or `""` when it is absent, null, or any non-string type.
    ///
    /// Empty and absent collapse deliberately: every consuming site in
    /// the scripts tests `[[ -n ... ]]`, so `""` already behaved as missing.
    ///
    /// Wrong-typed values read as absent, a divergence from both scripts: jq
    /// renders an object as `{"a":1}` and PowerShell as `@{a=1}`, so the two
    /// platforms already disagreed and neither output is defensible on screen.
    pub fn text(&self, path: &[&str]) -> &str {
        self.at(path).and_then(Value::as_str).unwrap_or("")
    }

    /// A field read as text, accepting a JSON number as its decimal spelling.
    ///
    /// The scripts interpolated the field into a string before parsing it
    /// (`"$resetsAt"` in PowerShell, `jq -r` in bash), so a number and a quoted
    /// number behaved identically on both platforms. Unlike the object case in
    /// [`Payload::text`], there is a behaviour to preserve.
    ///
    /// Used where a number is meaningful: `resets_at`, an epoch instant, and
    /// `effort.level`, which agent frontmatter may write as an integer.
    /// Dropping them silently cost the burn arrow, the countdown, the rate
    /// alert's re-arm and the whole effort segment.
    pub fn text_or_number(&self, path: &[&str]) -> Cow<'_, str> {
        match self.at(path) {
            Some(Value::String(s)) => Cow::Borrowed(s.as_str()),
            Some(Value::Number(n)) => Cow::Owned(n.to_string()),
            _ => Cow::Borrowed(""),
        }
    }

    /// A numeric field, accepting a JSON number or a numeric string.
    ///
    /// The string arm matches jq handing bash every field as text, so a quoted
    /// `"42.5"` renders identically to the unquoted form; dropping it would
    /// silently blank rows that currently render.
    ///
    /// A JSON `false` reads as absent, matching jq's `//` operator, which
    /// falls through on `false` as well as `null`. The same operator broke
    /// `notify`'s mute flags; here it is reproduced, because a numeric `false`
    /// has no sensible reading.
    pub fn number(&self, path: &[&str]) -> Option<f64> {
        match self.at(path)? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// A non-negative integer field: a JSON number or a digits-only string.
    ///
    /// The string arm is exactly bash's `^[0-9]+$` guard. An integral float
    /// (`200000.0`) is accepted because jq 1.6 prints it as `200000` and passes
    /// the guard while jq 1.7 preserves the literal and fails it, so the
    /// tolerant reading is the useful one.
    pub fn uint(&self, path: &[&str]) -> Option<u64> {
        match self.at(path)? {
            Value::Number(n) => n.as_u64().or_else(|| {
                let f = n.as_f64()?;
                (f >= 0.0 && f.fract() == 0.0).then_some(f as u64)
            }),
            Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
                s.parse().ok()
            }
            _ => None,
        }
    }

    // -- The documented fields, in the order the parity block assigns them ---

    /// `J_SESSION_ID`. Reaches a filename, so it must go through
    /// [`crate::session::sanitize_session_id`] first.
    pub fn session_id(&self) -> &str {
        self.text(&["session_id"])
    }

    /// `J_CWD` falling back to `J_CWD_FALLBACK`: `workspace.current_dir`, then
    /// the top-level `cwd`.
    pub fn cwd(&self) -> &str {
        let primary = self.text(&["workspace", "current_dir"]);
        if primary.is_empty() {
            self.text(&["cwd"])
        } else {
            primary
        }
    }

    /// `J_GIT_CWD`. The parity block does **not** give this the `cwd` fallback,
    /// so it is kept separate from [`Payload::cwd`]; the caller supplies the
    /// process working directory when it is empty.
    pub fn git_cwd(&self) -> &str {
        self.text(&["workspace", "current_dir"])
    }

    /// `J_MODEL_DISPLAY`.
    pub fn model_display_name(&self) -> &str {
        self.text(&["model", "display_name"])
    }

    /// `J_MODEL_ID`. Keys the learned model-to-window map.
    pub fn model_id(&self) -> &str {
        self.text(&["model", "id"])
    }

    /// `J_CTX_SIZE`.
    pub fn context_window_size(&self) -> Option<u64> {
        self.uint(&["context_window", "context_window_size"])
    }

    /// `J_USED_PCT`.
    pub fn used_percentage(&self) -> Option<f64> {
        self.number(&["context_window", "used_percentage"])
    }

    /// `J_TOTAL_INPUT_TOKENS`.
    pub fn total_input_tokens(&self) -> Option<u64> {
        self.uint(&["context_window", "total_input_tokens"])
    }

    /// `J_EFFORT_LEVEL`. Scrubbed at render. Read tolerantly because agent
    /// frontmatter may write an integer, which both scripts rendered as
    /// `3 effort` in white (bash through `jq -r`, PowerShell because a number
    /// is truthy and its `switch` falls to `default`);
    /// [`crate::render::effort_color`]'s catch-all arm exists for those values.
    pub fn effort_level(&self) -> Cow<'_, str> {
        self.text_or_number(&["effort", "level"])
    }

    /// `J_TOTAL_COST` falling back to the legacy top-level `total_cost_usd`.
    pub fn total_cost_usd(&self) -> Option<f64> {
        self.number(&["cost", "total_cost_usd"])
            .or_else(|| self.number(&["total_cost_usd"]))
    }

    /// `J_DURATION_MS` and its two legacy spellings, in the scripts' order.
    /// A float because bash strips the fraction textually (`${duration_ms%.*}`)
    /// before dividing, so fractional values have always been accepted.
    pub fn duration_ms(&self) -> Option<f64> {
        self.number(&["cost", "total_duration_ms"])
            .or_else(|| self.number(&["total_duration_ms"]))
            .or_else(|| self.number(&["duration_ms"]))
    }

    /// `J_TRANSCRIPT_PATH`. Opened as a file, never rendered, never scrubbed.
    pub fn transcript_path(&self) -> &str {
        self.text(&["transcript_path"])
    }

    /// `J_RATE_5H_PCT`.
    pub fn rate_five_hour_percentage(&self) -> Option<f64> {
        self.number(&["rate_limits", "five_hour", "used_percentage"])
    }

    /// `J_RATE_5H_RESETS`.
    pub fn rate_five_hour_resets_at(&self) -> Cow<'_, str> {
        self.text_or_number(&["rate_limits", "five_hour", "resets_at"])
    }

    /// `J_RATE_7D_PCT`.
    pub fn rate_seven_day_percentage(&self) -> Option<f64> {
        self.number(&["rate_limits", "seven_day", "used_percentage"])
    }

    /// `J_RATE_7D_RESETS`.
    pub fn rate_seven_day_resets_at(&self) -> Cow<'_, str> {
        self.text_or_number(&["rate_limits", "seven_day", "resets_at"])
    }

    /// `J_AGENT_NAME`. Present only in a subagent's own session; its emptiness
    /// is what selects the main-session layout.
    pub fn agent_name(&self) -> &str {
        self.text(&["agent", "name"])
    }

    /// `J_AGENT_IN`, defaulting to 0 because both scripts pin it to zero at
    /// extraction: it renders unconditionally inside the agent row.
    pub fn agent_input_tokens(&self) -> u64 {
        self.uint(&["context_window", "current_usage", "input_tokens"])
            .unwrap_or(0)
    }

    /// `J_AGENT_OUT`. Zero-defaulted like [`Payload::agent_input_tokens`].
    pub fn agent_output_tokens(&self) -> u64 {
        self.uint(&["context_window", "current_usage", "output_tokens"])
            .unwrap_or(0)
    }
}

/// The render sink's scrub for untrusted display fields, porting the scripts'
/// `sa_sanitize_title`: control bytes, DEL and `|` (a forgeable column
/// separator) become spaces, then spaces are trimmed so a field of nothing but
/// control bytes reads as empty.
///
/// The trim is space-only, matching bash's `[! ]` trim. One byte diverges:
/// bash's range starts at `\x01` because a bash string cannot hold a NUL,
/// while a JSON string can, so NUL is scrubbed here too.
pub fn sanitize_display(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c == '|' || (c as u32) < 0x20 || c as u32 == 0x7f {
                ' '
            } else {
                c
            }
        })
        .collect();
    replaced.trim_matches(' ').to_string()
}
