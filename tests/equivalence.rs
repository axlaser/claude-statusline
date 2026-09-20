//! The single integration test file for claude-statusline.
//!
//! Cases live in tables. Every failure names the case, so a red run points at
//! the exact scenario without a second lookup. Fixtures live under
//! `tests/fixtures/` as data files, never as additional test files.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use claude_statusline::clock::{Clock, TestClock};
use claude_statusline::cmd::statusline as cmd_statusline;
use claude_statusline::debug;
use claude_statusline::focus::{self, Anchor, CaptureOutcome, Identity, Key, Observation, Record};
use claude_statusline::git::{self, GitStatus, Porcelain};
use claude_statusline::notify_state::{self, decide, Latch, LatchState};
use claude_statusline::payload::{sanitize_display, Payload};
use claude_statusline::platform;
use claude_statusline::render;
use claude_statusline::settings;
use claude_statusline::state::{self, WriteOutcome};
use claude_statusline::subagent::{self, Row, Windows};
use claude_statusline::transcript::{self, Scan, TokenRecord};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

const BIN: &str = env!("CARGO_BIN_EXE_claude-statusline");

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the built binary as a fresh process, which is the only way to observe
/// the entry contract: exit code and stderr emptiness are process-level facts.
fn run_bin(args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Run {
    run_exe(BIN, args, stdin, env)
}

/// `run_bin` for a named executable: the entry contract covers two binaries
/// once the click helper exists, and they share one driver.
fn run_exe(bin: &str, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("failed to spawn the binary under test");
    // A broken pipe here is the child behaving correctly, not a failure: most
    // subcommands exit without ever draining stdin, and the parent's write
    // races that exit. Linux and Windows lose the race often enough to fail the
    // suite; macOS mostly wins it, which is what kept this hidden. Taking the
    // handle also closes it on drop, so a subcommand that *does* read stdin
    // still sees EOF.
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("failed to collect output");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn scratch_dir(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("claude-statusline-test-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("failed to create the scratch directory");
    dir
}

/// A state root the binary does not own — the shape every case that stages
/// files directly into its scratch directory needs.
///
/// This is not a convenience wrapper: it is the unguarded half of the contract.
/// `write_guarded` must keep behaving exactly as it did for a parent this binary
/// did not create, which is the flat temp root on the fallback path, `~/.claude`,
/// and these scratch roots. Every case below that passes `inherited(...)` is
/// asserting that half.
fn inherited(path: &std::path::Path) -> claude_statusline::session::StateRoot {
    claude_statusline::session::StateRoot::inherited(path.to_path_buf())
}

/// Collects failures so one run reports every broken case rather than stopping
/// at the first.
#[derive(Default)]
struct Failures(Vec<String>);

impl Failures {
    fn check(&mut self, case: &str, ok: bool, detail: impl FnOnce() -> String) {
        if !ok {
            self.0.push(format!("  [{case}] {}", detail()));
        }
    }

    fn assert_empty(self, what: &str) {
        assert!(
            self.0.is_empty(),
            "{what} failed for {} case(s):\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

// ---------------------------------------------------------------------------
// Entry contract
// ---------------------------------------------------------------------------

struct EntryCase {
    name: &'static str,
    /// Which executable is under test: the main binary for every case here,
    /// the click helper for the rows the focus section adds.
    bin: &'static str,
    args: &'static [&'static str],
    stdin: &'static str,
}

/// Every subcommand exits 0 and writes nothing to stderr, whatever it is fed.
/// This is the contract `CLAUDE.md` calls Silent Degradation: breaking it
/// crashes the Claude Code status line for users.
#[test]
fn entry_contract_always_exits_zero_and_silent() {
    let cases = [
        EntryCase {
            bin: BIN,
            name: "empty-stdin",
            args: &[],
            stdin: "",
        },
        EntryCase {
            bin: BIN,
            name: "malformed-json",
            args: &[],
            stdin: "{not json",
        },
        EntryCase {
            bin: BIN,
            name: "truncated-json",
            args: &[],
            stdin: "{\"session_id\":",
        },
        EntryCase {
            bin: BIN,
            name: "json-not-object",
            args: &[],
            stdin: "[1,2,3]",
        },
        EntryCase {
            bin: BIN,
            name: "whitespace-only",
            args: &[],
            stdin: "   \n\t ",
        },
        EntryCase {
            bin: BIN,
            name: "unknown-subcommand",
            args: &["no-such-subcommand"],
            stdin: "",
        },
        EntryCase {
            bin: BIN,
            name: "notify-no-event",
            args: &["notify"],
            stdin: "",
        },
        EntryCase {
            bin: BIN,
            name: "git-refresh-empty",
            args: &["git-refresh"],
            stdin: "",
        },
        EntryCase {
            bin: BIN,
            name: "subagent-empty",
            args: &["subagent"],
            stdin: "",
        },
        // an unwinding panic must not escape as stderr or a non-zero code.
        EntryCase {
            bin: BIN,
            name: "forced-panic",
            args: &["__panic-probe"],
            stdin: "",
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let run = run_exe(c.bin, c.args, c.stdin, &[]);
        failures.check(c.name, run.code == Some(0), || {
            format!("expected exit 0, got {:?}", run.code)
        });
        failures.check(c.name, run.stderr.is_empty(), || {
            format!("expected empty stderr, got {:?}", run.stderr)
        });
    }
    failures.assert_empty("entry contract");
}

/// `std::process::exit` runs no destructors, so a buffered writer dropped
/// unflushed produces empty output that still satisfies every exit-code and
/// stderr assertion above. This is the case that catches that.
#[test]
fn buffered_output_reaches_stdout_before_exit() {
    let run = run_bin(&["self-check"], "", &[]);
    assert_eq!(
        run.code,
        Some(0),
        "self-check should succeed on an unmodified binary"
    );
    assert!(
        !run.stdout.is_empty(),
        "self-check produced no stdout — output was buffered and lost at exit"
    );
    assert!(
        run.stderr.is_empty(),
        "self-check wrote to stderr: {:?}",
        run.stderr
    );
}

/// The self-check is the installer's only guard against placing a binary
/// that launches but renders wrongly, so it must be able to fail.
#[test]
fn self_check_reports_failure_with_nonzero_exit() {
    let run = run_bin(
        &["self-check"],
        "",
        &[("STATUSLINE_FORCE_SELFCHECK_MISMATCH", "1")],
    );
    assert_eq!(
        run.code,
        Some(1),
        "a forced self-check mismatch must exit non-zero; it is exempt from the exit-0 catch"
    );
}

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// The clock covers filesystem timestamps as well as wall-clock reads: feed freshness
/// and transcript staleness are both `now - mtime`, so pinning only the clock
/// would leave those comparisons reading real file times.
#[test]
fn test_clock_drives_both_now_and_mtime() {
    let dir = scratch_dir("clock");
    let file = dir.join("state.json");
    std::fs::write(&file, b"{}").unwrap();

    let clock = TestClock::at(1_000_000).with_mtime(&file, 999_000);

    assert_eq!(
        clock.now_unix(),
        1_000_000,
        "now() must come from the injected clock"
    );
    assert_eq!(
        clock.mtime_unix(&file),
        Some(999_000),
        "mtime() must come from the injected clock, not the filesystem"
    );
    assert_eq!(
        clock.age_secs(&file),
        Some(1_000),
        "age must be computed from the injected pair"
    );
}

#[test]
fn test_clock_reports_missing_mtime_for_unknown_path() {
    let clock = TestClock::at(500);
    assert_eq!(clock.mtime_unix(Path::new("/nonexistent/path")), None);
    assert_eq!(clock.age_secs(Path::new("/nonexistent/path")), None);
}

// ---------------------------------------------------------------------------
// State-file guards
// ---------------------------------------------------------------------------

/// The guard's fail direction, and the nine-day incident in
/// `docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`.
/// An owner that cannot be determined must NOT fail the read closed: that
/// inversion silently killed every read-side cache and re-fired the context
/// alert every two seconds for nine days. The symlink rejection is the
/// load-bearing guard; the owner check is defense-in-depth.
#[test]
fn owner_check_fail_direction_matches_the_shipped_fix() {
    struct Case {
        name: &'static str,
        owner: Option<u64>,
        expected: bool,
        why: &'static str,
    }

    let me = 4242u64;
    // On Windows the set also carries the Administrators group, because an
    // elevated process creates files owned by it rather than by the user.
    let admins = 777u64;
    let trusted = [me, admins];
    let cases = [
        Case {
            name: "owned-by-me",
            owner: Some(me),
            expected: true,
            why: "our own file must be trusted",
        },
        Case {
            name: "owned-by-other",
            owner: Some(9999),
            expected: false,
            why: "a foreign-owned file must be rejected",
        },
        Case {
            name: "owner-unresolvable",
            owner: None,
            expected: true,
            why: "an undeterminable owner must degrade to the symlink guard, not fail closed",
        },
        Case {
            name: "owned-by-administrators",
            owner: Some(admins),
            expected: true,
            why: "an elevated process creates files owned by Administrators; rejecting them \
                  leaves every state read failing, silently and all at once",
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let got = state::owner_check_passes(c.owner, &trusted);
        failures.check(c.name, got == c.expected, || {
            format!("expected {}, got {} — {}", c.expected, got, c.why)
        });
    }
    failures.assert_empty("owner-check fail direction");
}

/// A state file that exists but cannot be parsed reads as its conservative
/// value. For the notification latch that means "already notified", so a
/// corrupt latch suppresses a repeat alert rather than re-firing it every tick.
#[test]
fn unreadable_state_reads_as_its_conservative_value() {
    let dir = scratch_dir("latch");
    let latch = dir.join("statusline-notify-abc.json");
    std::fs::write(&latch, b"\x00\x01 not json at all").unwrap();

    assert!(
        state::latch_reads_as_notified(&latch),
        "an unparseable latch must suppress, not re-fire"
    );
}

/// A hostile target that cannot be removed must abort the write rather
/// than following the link. On a sticky directory the unlink fails, so a guard
/// that removes-then-writes without re-checking would write through the
/// attacker's symlink into a victim-owned file.
#[test]
fn write_to_hostile_target_is_skipped_not_followed() {
    let dir = scratch_dir("hostile");
    let victim = dir.join("victim.txt");
    let link = dir.join("statusline-state.json");
    std::fs::write(&victim, b"original").unwrap();

    if !make_symlink(&victim, &link) {
        skipped_for_want_of_symlinks();
        return;
    }

    let outcome = state::write_guarded(&link, b"attacker-controlled");
    assert!(
        matches!(
            outcome,
            WriteOutcome::Written | WriteOutcome::SkippedHostile
        ),
        "unexpected outcome: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"original",
        "the write followed the symlink and clobbered the victim file"
    );
}

#[test]
fn write_to_a_normal_path_succeeds() {
    let dir = scratch_dir("normal-write");
    let target = dir.join("statusline-state.json");
    let outcome = state::write_guarded(&target, b"{\"ok\":true}");
    assert!(
        matches!(outcome, WriteOutcome::Written),
        "unexpected outcome: {outcome:?}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"ok\":true}");
}

/// A crashed run can leave a stale regular file at the pid-derived staging
/// name. The exclusive create that keeps a re-planted link from being followed
/// refuses anything that already exists, so the unlink ahead of it is what
/// keeps that safety from costing every later write in the session.
#[test]
fn a_stale_staging_file_does_not_block_the_write() {
    let dir = scratch_dir("stale-staging");
    let target = dir.join("statusline-state.json");
    let stale = dir.join(format!(".statusline-state.json.{}.tmp", std::process::id()));
    std::fs::write(&stale, b"left by a crashed run").unwrap();

    let outcome = state::write_guarded(&target, b"{\"ok\":true}");
    assert!(
        matches!(outcome, WriteOutcome::Written),
        "a stale staging leftover blocked the write: {outcome:?}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"ok\":true}");
}

/// The ownership FFI compiling proves nothing about what it returns. This repo
/// lost nine days to a trust check that answered "untrusted" for every file
/// because its dependency was unavailable in the spawned child process — a
/// failure that was 100% reproducible there and 0% reproducible from a shell.
/// Exercise the real calls, in the real process.
#[test]
fn ownership_resolution_works_in_this_process() {
    let dir = scratch_dir("owner");
    let file = dir.join("mine.json");
    std::fs::write(&file, b"{}").unwrap();

    assert!(
        platform::current_owner().is_some(),
        "current_owner() could not resolve our own identity — the guard would \
         degrade to the symlink check everywhere"
    );
    assert!(
        platform::file_owner(&file).is_some(),
        "file_owner() returned None for a file we just created — the ownership \
         half of the guard is inert"
    );
}

/// The behaviour that actually matters: a file we own round-trips through the
/// guard. If ownership resolution is subtly wrong, this fails while the two
/// `is_some()` assertions above still pass.
#[test]
fn our_own_state_file_round_trips_through_the_guard() {
    let dir = scratch_dir("round-trip");
    let target = dir.join("statusline-state.json");

    let outcome = state::write_guarded(&target, b"{\"notified_context_high\":true}");
    assert!(
        matches!(outcome, WriteOutcome::Written),
        "writing our own file was refused: {outcome:?}"
    );

    let back = state::read_trusted(&target);
    assert!(
        back.is_some(),
        "a file we just wrote read back as untrusted — this is the shape of the \
         nine-day cache-death incident. trusted owners: {:?}, file owner: {:?}",
        platform::trusted_owners(),
        platform::file_owner(&target),
    );
    assert!(
        state::latch_reads_as_notified(&target),
        "a latch we wrote with notified_context_high=true did not read as notified"
    );
}

/// `settings` at the process boundary, which is the only place its contract
/// exists.
///
/// `settings_cli` lives in `src/main.rs`, private to the bin crate, so this
/// file cannot call it — and nothing spawned the binary to reach it either, so
/// the subcommand had no coverage at all. That matters more here than anywhere
/// else in the tree: `settings` is *exempt* from the exit-0 contract precisely
/// so an installer can branch on its exit code, and both installers do. An
/// exit code that lied would be invisible everywhere except a user's
/// half-configured machine.
#[test]
fn the_settings_subcommand_reports_through_its_exit_code() {
    let dir = scratch_dir("settings-cli");
    let path = dir.join("settings.json");
    let settings = path.to_str().expect("the scratch path is UTF-8");
    let binary = "/home/someone/.claude/bin/claude-statusline";

    let settings_run = |args: &[&str]| {
        let mut full = vec!["settings"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--binary", binary, "--settings", settings]);
        run_bin(&full, "", &[])
    };

    // Applying to an absent file creates it and reports success.
    let applied = settings_run(&["apply", "--all", "--no-quote"]);
    assert_eq!(applied.code, Some(0), "apply --all should succeed");
    let body = std::fs::read_to_string(&path).expect("apply wrote no settings.json");
    for key in ["statusLine", "subagentStatusLine", "git-refresh", "notify"] {
        assert!(body.contains(key), "apply --all omitted {key}:\n{body}");
    }

    // The query forms answer through the exit code, which is what the
    // installers read; 0 is yes and 1 is no.
    assert_eq!(settings_run(&["has", "statusline"]).code, Some(0));
    assert_eq!(settings_run(&["has", "subagent"]).code, Some(0));
    assert_eq!(
        settings_run(&["has-foreign", "statusline"]).code,
        Some(1),
        "our own entry is not foreign"
    );
    assert_eq!(
        settings_run(&["has-legacy"]).code,
        Some(1),
        "a binary installation is not a script installation"
    );

    // `remove` is `apply`'s inverse, and says so.
    assert_eq!(settings_run(&["remove"]).code, Some(0));
    let body = std::fs::read_to_string(&path).expect("remove deleted settings.json");
    assert!(
        !body.contains("claude-statusline"),
        "remove left entries behind:\n{body}"
    );
    assert_eq!(
        settings_run(&["has", "statusline"]).code,
        Some(1),
        "has must report the removal"
    );

    // Failure directions. Each of these used to exit 0 or be silently dropped,
    // which is the exact outcome the exit-0 exemption exists to prevent: an
    // installer reporting success having configured nothing.
    assert_eq!(
        run_bin(&["settings", "apply", "--settings", settings], "", &[]).code,
        Some(1),
        "--binary is required"
    );
    assert_eq!(
        settings_run(&["apply", "--subagnet"]).code,
        Some(1),
        "a mistyped flag must not be dropped into positionals and reported as success"
    );
    assert_eq!(
        settings_run(&["frobnicate"]).code,
        Some(1),
        "an unknown action is a failure"
    );
    assert_eq!(settings_run(&["has"]).code, Some(1), "has needs a feature");

    // A settings.json that is valid JSON but not an object must be refused, not
    // replaced -- replacing it discards everything the user configured while
    // reporting success.
    std::fs::write(&path, b"[1, 2, 3]").expect("failed to stage a non-object settings.json");
    let refused = settings_run(&["apply", "--all"]);
    assert_eq!(
        refused.code,
        Some(1),
        "a non-object root must be refused: {}",
        refused.stdout
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file was removed"),
        "[1, 2, 3]",
        "the refused file must be left exactly as it was"
    );

    // Diagnostics go to stdout, because fd 2 is redirected to the null device
    // before any subcommand runs -- anything written there would vanish and
    // leave a failing installer with nothing to show.
    assert!(
        !refused.stdout.is_empty(),
        "a failure must say why on stdout"
    );
    assert!(refused.stderr.is_empty(), "nothing may reach stderr");
}

/// Announces a symlink case that could not run, loudly enough to survive
/// `cargo test`'s output capture.
///
/// The five cases that call this cover the load-bearing half of the file- and
/// directory-level trust guards, and they skip on Windows without Developer
/// Mode. Keep the count here, in `CLAUDE.md`, and the call sites in step — it
/// has drifted twice, and it is the number a reader uses to judge how much a
/// green Windows run proves. They used to say so with
/// `eprintln!`, which `cargo test` captures and shows only on failure — so a
/// skipped case printed nothing at all on a green run, and CLAUDE.md's claim
/// that they "print a reason but report as passing" was true only under
/// `--nocapture`. The maintainer develops on Windows, which is exactly where
/// that silence is most expensive.
///
/// A marker file makes the skip legible after the fact regardless of capture;
/// `CI=true` turns it into a hard failure, because CI's Unix runners have no
/// excuse for lacking symlinks and a silent skip there would retire the
/// coverage entirely.
fn skipped_for_want_of_symlinks() {
    let reason = "this platform/session cannot create symlinks unprivileged";
    if std::env::var("CI").is_ok_and(|v| !v.is_empty() && v != "0") {
        panic!("symlink cases cannot be skipped in CI: {reason}");
    }
    eprintln!("skipped: {reason}");
    let marker = std::env::temp_dir().join("statusline-skipped-symlink-cases.txt");
    let _ = std::fs::write(&marker, format!("{reason}\n"));
    println!(
        "SKIPPED: a symlink case did not run ({reason}). \
         A green local run is not evidence these passed; see {}",
        marker.display()
    );
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) -> bool {
    // Requires Developer Mode or elevation; the caller skips when this fails.
    std::os::windows::fs::symlink_file(target, link).is_ok()
}

/// A *directory* symlink, which is a different call from the file one on
/// Windows and the same one on Unix.
///
/// The state-directory guard is only ever asked about directories, so a file
/// symlink would not exercise it: `symlink_dir` is what a squatter plants.
#[cfg(unix)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(windows)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}

// ---------------------------------------------------------------------------
// State directory
// ---------------------------------------------------------------------------

/// The directory guard's fail directions, asserted one at a time.
///
/// These mirror `state::is_hostile`'s table one level up. The pairing is the
/// point: two guards over the same question that disagree about the
/// unavailable-dependency case is the exact defect
/// `docs/solutions/logic-errors/get-acl-unavailable-inverts-trust-check.md`
/// records, and it cost this project nine days.
#[test]
fn the_state_directory_guard_matches_its_fail_directions() {
    use claude_statusline::platform::{create_private_dir, dir_verdict, DirVerdict};

    let dir = scratch_dir("state-dir-guard");
    let mut failures = Failures::default();

    let absent = dir.join("not-there");
    failures.check("absent", dir_verdict(&absent) == DirVerdict::Absent, || {
        format!("expected Absent, got {:?}", dir_verdict(&absent))
    });

    // Created through the production path, so the mode it sets is the mode
    // under test rather than one the case chose.
    let fresh = dir.join("fresh");
    failures.check(
        "create",
        create_private_dir(&fresh) == DirVerdict::Private,
        || format!("expected Private, got {:?}", create_private_dir(&fresh)),
    );
    failures.check(
        "create-verdict",
        dir_verdict(&fresh) == DirVerdict::Private,
        || format!("expected Private, got {:?}", dir_verdict(&fresh)),
    );
    // Two of the three per-tick processes can reach this concurrently, so
    // `AlreadyExists` has to be a success rather than a race that loses state.
    failures.check(
        "create-twice",
        create_private_dir(&fresh) == DirVerdict::Private,
        || "a second create on an existing private directory did not succeed".to_string(),
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(&fresh).expect("fresh dir");
        failures.check(
            "create-mode",
            md.permissions().mode() & 0o777 == 0o700,
            || {
                format!(
                    "expected mode 0700, got {:o}",
                    md.permissions().mode() & 0o777
                )
            },
        );

        // Anyone else with search permission can plant inside it, which is the
        // whole property being verified.
        let loose = dir.join("loose");
        std::fs::create_dir(&loose).expect("loose dir");
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        failures.check(
            "mode-0755",
            dir_verdict(&loose) == DirVerdict::Hostile,
            || format!("expected Hostile for 0755, got {:?}", dir_verdict(&loose)),
        );
    }

    let file = dir.join("a-file");
    std::fs::write(&file, b"x").expect("write");
    failures.check(
        "regular-file",
        dir_verdict(&file) == DirVerdict::Hostile,
        || format!("expected Hostile for a file, got {:?}", dir_verdict(&file)),
    );

    let target = dir.join("target");
    std::fs::create_dir(&target).expect("target");
    let link = dir.join("planted");
    if make_dir_symlink(&target, &link) {
        failures.check("symlink", dir_verdict(&link) == DirVerdict::Hostile, || {
            format!(
                "expected Hostile for a symlink, got {:?}",
                dir_verdict(&link)
            )
        });
        // The load-bearing half: creation must refuse too, or the read-only
        // verdict is advice nobody takes.
        failures.check(
            "symlink-create",
            create_private_dir(&link) == DirVerdict::Hostile,
            || "create_private_dir adopted a symlinked directory".to_string(),
        );
        failures.check(
            "symlink-no-write-through",
            std::fs::read_dir(&target)
                .expect("target readable")
                .next()
                .is_none(),
            || "something was written through the planted link".to_string(),
        );
    } else {
        skipped_for_want_of_symlinks();
    }

    failures.assert_empty("state directory guard");
}

/// The render path writes its state inside the guarded directory, not beside it.
///
/// Every other case in this file builds `Roots` with `StateRoot::inherited`,
/// which exercises the *unguarded* branch — the one that behaves exactly as it
/// did before the state directory existed. That is deliberate and load-bearing
/// (`guarded_creation_applies_only_to_the_state_directory` depends on it), but
/// it leaves the production shape untested: `Roots::from_env` builds a guarded
/// root, and nothing else here does.
///
/// So this is the one case that runs the real render against the real
/// resolution. Without it a regression that quietly resolved to the flat root
/// would render byte-identically, pass the whole fixture table, and ship.
#[test]
fn the_render_path_writes_inside_the_guarded_state_directory() {
    let dir = scratch_dir("render-guarded-root");
    let home = dir.join("home");
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    std::fs::create_dir_all(&tmp).expect("tmp");
    let mut failures = Failures::default();

    let resolved = claude_statusline::session::state_dir_in(&tmp);
    failures.check("guarded", resolved.is_guarded(), || {
        "a clean temp root did not resolve to a guarded state directory".to_string()
    });

    let session = "render-guarded-1";
    let payload = format!(
        r#"{{"session_id":"{session}","cwd":"{cwd}","workspace":{{"current_dir":"{cwd}"}},"model":{{"display_name":"Opus 5","id":"claude-opus-5"}},"context_window":{{"context_window_size":200000,"used_percentage":91.5}},"rate_limits":{{"five_hour":{{"used_percentage":95}}}}}}"#,
        cwd = slashed(&dir),
    );

    let roots = cmd_statusline::Roots {
        home: Some(home.clone()),
        temp: resolved.clone(),
    };
    let rendered = cmd_statusline::run(&TestClock::at(1_000), &roots, &payload);
    failures.check("renders", rendered.contains('\u{250f}'), || {
        "the guarded-root render produced no box".to_string()
    });

    // The token record and the notify latch are the two state files a payload
    // with no transcript and a crossed threshold still produces. Both must land
    // under the state directory.
    let inside: Vec<String> = std::fs::read_dir(resolved.path())
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    failures.check("wrote-inside", !inside.is_empty(), || {
        "the render wrote no state inside the guarded directory".to_string()
    });

    // And nothing may land flat beside it. A resolver that silently fell back
    // would still satisfy the check above only if it wrote nowhere at all, so
    // this is the half that catches a partial regression.
    let stray: Vec<String> = std::fs::read_dir(&tmp)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("statusline-"))
                .collect()
        })
        .unwrap_or_default();
    failures.check("no-stray", stray.is_empty(), || {
        format!("state landed flat in the temp root: {stray:?}")
    });

    failures.assert_empty("guarded-root render path");
}

/// Resolution answers where state lives and creates nothing doing it.
///
/// The creates-nothing half is not decoration. `Roots::from_env` runs before the
/// payload is parsed, so a tick whose payload is garbage would otherwise leave a
/// directory behind — and
/// `degraded_input_renders_the_notice_and_touches_no_state` asserts the temp
/// root is empty after exactly that.
#[test]
fn the_state_root_resolves_without_creating_anything() {
    use claude_statusline::session::state_dir_in;

    let dir = scratch_dir("state-root-resolve");
    let mut failures = Failures::default();

    let resolved = state_dir_in(&dir);
    failures.check("guarded", resolved.is_guarded(), || {
        "a clean temp root did not resolve to the guarded state directory".to_string()
    });
    failures.check("under-root", resolved.path().starts_with(&dir), || {
        format!("{} is not under the temp root", resolved.path().display())
    });
    failures.check("named", resolved.path() != dir, || {
        "the state directory is the temp root itself".to_string()
    });
    failures.check(
        "creates-nothing",
        std::fs::read_dir(&dir)
            .expect("scratch readable")
            .next()
            .is_none(),
        || "resolution created something in the temp root".to_string(),
    );

    // Same owner, same answer. A suffix that varied between ticks would write
    // state under one name and read it under another, and every cache would
    // miss forever while rendering correctly.
    failures.check(
        "stable",
        state_dir_in(&dir).path() == resolved.path(),
        || "two resolutions of the same root disagreed".to_string(),
    );

    // A hostile directory routes to the flat root, which is today's behaviour.
    let hostile_root = scratch_dir("state-root-hostile");
    let target = hostile_root.join("elsewhere");
    std::fs::create_dir(&target).expect("target");
    let candidate = state_dir_in(&hostile_root).path().to_path_buf();
    std::fs::remove_dir_all(&candidate).ok();
    if make_dir_symlink(&target, &candidate) {
        let fallen_back = state_dir_in(&hostile_root);
        failures.check("fallback-path", fallen_back.path() == hostile_root, || {
            format!(
                "expected the flat root, got {}",
                fallen_back.path().display()
            )
        });
        failures.check("fallback-unguarded", !fallen_back.is_guarded(), || {
            "the fallback root is still marked guarded".to_string()
        });
    } else {
        skipped_for_want_of_symlinks();
    }

    failures.assert_empty("state root resolution");
}

/// Guarded creation applies to the state directory and to nothing else.
///
/// This is a regression test for a defect that would have shipped: applying the
/// private-directory check to every `write_guarded` parent rejects `/tmp` (mode
/// 1777, root-owned), `~/.claude` (0755), and the harness's own scratch roots,
/// so every state write fails on Linux and the learned model-window map stops
/// persisting on all Unix. It is invisible on Windows and invisible in rendered
/// output, because a failed write degrades to a correct-but-slower recompute.
#[test]
fn guarded_creation_applies_only_to_the_state_directory() {
    use claude_statusline::session::{state_dir_in, StateRoot};
    use claude_statusline::state::{write_guarded, write_guarded_under, WriteOutcome};

    let dir = scratch_dir("guarded-creation");
    let mut failures = Failures::default();

    // The guarded path: the parent does not exist yet and this binary owns it.
    let guarded = state_dir_in(&dir);
    let target = guarded.join("statusline-git-case.txt");
    failures.check(
        "guarded-write",
        write_guarded_under(&guarded, &target, b"x") == WriteOutcome::Written,
        || "a write into the state directory did not land".to_string(),
    );
    failures.check("guarded-location", target.is_file(), || {
        "the file did not land inside the state directory".to_string()
    });
    failures.check(
        "no-staging-left",
        std::fs::read_dir(guarded.path())
            .expect("state dir readable")
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with('.')),
        || "a staging file was left behind in the state directory".to_string(),
    );

    // The inherited path, three ways. Each of these is a real parent the binary
    // writes to and does not own.
    let scratch_parent = dir.join("inherited-scratch");
    std::fs::create_dir(&scratch_parent).expect("scratch parent");
    let cases: [(&str, PathBuf); 2] = [
        ("existing-parent", scratch_parent.join("f.txt")),
        // Absent parent on the *unguarded* side: inferring "guarded" from
        // parent-absence would send this through the private check.
        (
            "absent-parent",
            dir.join("made-by-create-dir-all").join("f.txt"),
        ),
    ];
    for (name, path) in cases {
        let root = StateRoot::inherited(dir.clone());
        failures.check(
            name,
            write_guarded_under(&root, &path, b"x") == WriteOutcome::Written,
            || {
                format!(
                    "an inherited-parent write at {} did not land",
                    path.display()
                )
            },
        );
        failures.check(name, path.is_file(), || {
            format!("{} was not created", path.display())
        });
    }

    // The bare entry point keeps today's behaviour for callers that never see a
    // StateRoot at all — the learned model-window map under ~/.claude.
    let bare = dir.join("bare").join("f.txt");
    failures.check(
        "bare-write_guarded",
        write_guarded(&bare, b"x") == WriteOutcome::Written,
        || "write_guarded no longer creates an ordinary parent".to_string(),
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The `/tmp` shape specifically: world-writable and not ours to judge.
        let shared = dir.join("shared-1777");
        std::fs::create_dir(&shared).expect("shared");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).expect("chmod");
        let root = StateRoot::inherited(shared.clone());
        let path = shared.join("statusline-git-case.txt");
        failures.check(
            "world-writable-parent",
            write_guarded_under(&root, &path, b"x") == WriteOutcome::Written,
            || "a fallback write into a /tmp-shaped parent was refused".to_string(),
        );
    }

    // A planted parent yields SkippedHostile, not Failed. The two are different
    // answers and `cmd::subagent` maps them to different ticks and log lines.
    let hostile_root = scratch_dir("guarded-creation-hostile");
    let elsewhere = hostile_root.join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("elsewhere");
    let guarded_hostile = state_dir_in(&hostile_root);
    let candidate = guarded_hostile.path().to_path_buf();
    std::fs::remove_dir_all(&candidate).ok();
    if make_dir_symlink(&elsewhere, &candidate) {
        // Resolution would normally route this to the flat root; the case
        // constructs the guarded root directly so the creation path itself is
        // what gets tested.
        let forced = state_dir_in(&hostile_root);
        let outcome = if forced.is_guarded() {
            write_guarded_under(&forced, &candidate.join("statusline-git-case.txt"), b"x")
        } else {
            // Resolution already refused, which is the outer defence. Assert the
            // creation primitive directly instead.
            match claude_statusline::platform::create_private_dir(&candidate) {
                claude_statusline::platform::DirVerdict::Hostile => WriteOutcome::SkippedHostile,
                other => panic!("expected Hostile from create_private_dir, got {other:?}"),
            }
        };
        failures.check(
            "hostile-parent",
            outcome == WriteOutcome::SkippedHostile,
            || format!("expected SkippedHostile, got {outcome:?}"),
        );
        failures.check(
            "hostile-no-write-through",
            std::fs::read_dir(&elsewhere)
                .expect("elsewhere readable")
                .next()
                .is_none(),
            || "the write went through the planted link".to_string(),
        );
    } else {
        skipped_for_want_of_symlinks();
    }

    failures.assert_empty("guarded creation scope");
}

// ---------------------------------------------------------------------------
// Debug log
// ---------------------------------------------------------------------------

/// `CLAUDE.md`'s Silent Degradation rule has two halves: never write to stderr,
/// and log errors via `STATUSLINE_DEBUG`. Silencing without logging would make
/// a field failure indistinguishable from no failure.
#[test]
fn debug_log_writes_only_when_enabled() {
    let dir = scratch_dir("debug");
    let log = dir.join("statusline-debug.log");

    debug::log_to(&log, false, || "should not appear".to_string());
    assert!(
        !log.exists(),
        "the log was created while STATUSLINE_DEBUG was unset"
    );

    debug::log_to(&log, true, || "hello from the probe".to_string());
    let body = std::fs::read_to_string(&log).expect("the log should exist once enabled");
    assert!(
        body.contains("hello from the probe"),
        "log body was {body:?}"
    );
}

// ---------------------------------------------------------------------------
// Release workflow contract
// ---------------------------------------------------------------------------
//
// A release workflow is only exercised by pushing a tag, which is a slow and
// irreversible way to learn that an action reference went unpinned or that the
// arm runner label drifted back to an alias. These cases assert the properties
// that are decidable from the file itself, so the tag push only has to prove
// the parts that genuinely need a runner.

fn repo_file(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Reads a repository file with line endings normalised to LF.
///
/// These cases assert structure, not bytes, and a Windows checkout hands them
/// CRLF. Without this a multi-line pattern match silently means something
/// different depending on which platform ran the test.
fn read_repo_file(rel: &str) -> String {
    std::fs::read_to_string(repo_file(rel))
        .unwrap_or_else(|e| panic!("could not read {rel}: {e}"))
        .replace("\r\n", "\n")
}

const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";

/// The six published targets, each with the artifact suffix its family carries.
const PUBLISHED_TARGETS: [&str; 6] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
];

/// Every published target builds, tests, and is attested. A target that is
/// silently absent ships an installer that resolves a URL returning 404.
#[test]
fn release_workflow_covers_every_published_target() {
    let wf = read_repo_file(RELEASE_WORKFLOW);
    let mut failures = Failures::default();

    for target in PUBLISHED_TARGETS {
        failures.check(target, wf.contains(&format!("target: {target}")), || {
            "missing from the build matrix".to_string()
        });
        failures.check(target, wf.contains(&format!("Attest {target}")), || {
            "has no attestation step, so the release would ship it unattested".to_string()
        });
    }
    failures.assert_empty("published targets");
}

/// A mutable tag reference is a supply-chain hole: the SHA a release was built
/// with must be the SHA the reference names.
#[test]
fn every_action_reference_is_pinned_to_a_full_sha() {
    let wf = read_repo_file(RELEASE_WORKFLOW);
    let mut failures = Failures::default();
    let mut seen = 0usize;

    for (n, line) in wf.lines().enumerate() {
        let Some((_, reference)) = line.trim().split_once("uses:") else {
            continue;
        };
        let reference = reference.trim();
        seen += 1;
        // Strip the trailing `# v5` comment the pin carries for readability.
        let pin = reference.split('#').next().unwrap_or("").trim();
        let sha = pin.rsplit('@').next().unwrap_or("");
        let pinned = sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit());
        failures.check(&format!("line {}", n + 1), pinned, || {
            format!("`{pin}` is not pinned to a full 40-character commit SHA")
        });
    }

    assert!(seen > 0, "no `uses:` references found — did the file move?");
    failures.assert_empty("action pinning");
}

/// Approach item 7: alias labels move under you. `windows-latest` silently
/// became a different image more than once, and neither arm label has an alias
/// that resolves to arm at all.
#[test]
fn runner_labels_are_explicit_not_aliases() {
    let wf = read_repo_file(RELEASE_WORKFLOW);
    let mut failures = Failures::default();

    for (n, line) in wf.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("runner:") && !trimmed.starts_with("runs-on:") {
            continue;
        }
        failures.check(
            &format!("line {}", n + 1),
            !trimmed.contains("-latest"),
            || format!("`{trimmed}` uses a moving alias"),
        );
    }

    for label in ["windows-11-arm", "ubuntu-24.04-arm"] {
        failures.check(label, wf.contains(label), || {
            "the explicit arm runner label is missing".to_string()
        });
    }
    failures.assert_empty("runner labels");
}

/// The workflow grants nothing beyond read at its own scope, and the write
/// grants appear exactly once — on the single attesting and publishing job.
#[test]
fn write_permissions_are_confined_to_the_publishing_job() {
    let wf = read_repo_file(RELEASE_WORKFLOW);

    let scope = wf
        .find("\npermissions:\n")
        .expect("the workflow declares no top-level `permissions:` block");
    let scope_block: String = wf[scope + 1..]
        .lines()
        .take(2)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        scope_block.contains("contents: read"),
        "workflow scope must be read-only, got:\n{scope_block}"
    );

    let mut failures = Failures::default();
    for grant in [
        "contents: write",
        "id-token: write",
        "attestations: write",
        "artifact-metadata: write",
    ] {
        let count = wf.matches(grant).count();
        failures.check(grant, count == 1, || {
            format!("appears {count} times; it belongs to the publishing job alone")
        });
    }
    failures.assert_empty("elevated permissions");
}

/// Anything that is not exactly `vX.Y.Z` publishes as a prerelease, and the
/// installer's default resolution skips those.
///
/// A step used to refuse stable tags outright, until the port had proved parity
/// with the scripts it replaced. That was satisfied and the step is gone — so
/// this is what remains between a verification tag and a user who just ran the
/// one-liner. The classification failing open toward "prerelease" is the
/// deliberate direction: an unexpected tag shape stays out of `releases/latest`
/// rather than shipping to everyone.
#[test]
fn anything_but_a_release_tag_publishes_as_a_prerelease() {
    let wf = read_repo_file(RELEASE_WORKFLOW);
    assert!(
        wf.contains("--prerelease"),
        "nothing marks non-release tags as prereleases, so the installer would pick them up"
    );
    assert!(
        wf.contains("prerelease=true"),
        "the tag classification is gone, so nothing decides what is a prerelease"
    );
    // The regex is the whole classification: a tag that fails to match this is
    // a prerelease. Loosening it to something that also matches `v1.0.0-rc.1`
    // would publish every candidate as stable.
    assert!(
        wf.contains(r"^v[0-9]+\.[0-9]+\.[0-9]+$"),
        "the stable-tag pattern is no longer anchored, so a candidate could classify as stable"
    );
}

/// A tag and a `Cargo.toml` that disagree publish happily, and the mismatch
/// surfaces later as an artifact that reports the wrong version. The gate is
/// cheap; the thing that makes it worth a test is that it can be disabled
/// without being deleted — drop `needs: version` and every later job runs
/// regardless of what the check said.
#[test]
fn the_release_tag_must_match_the_crate_version() {
    let wf = read_repo_file(RELEASE_WORKFLOW);
    assert!(
        wf.contains("name: The tag and the crate version agree"),
        "the version gate is gone — a tag can now claim any version"
    );
    assert!(
        wf.contains("needs: version"),
        "nothing depends on the version gate, so its result cannot block a release"
    );
}

/// A release publishes the notes written for its tag, and every tag that has
/// notes must actually have them reach the release.
///
/// The wiring is easy to unhook without noticing: drop `--notes-file` and
/// `gh release create` still succeeds, just with whatever it was given instead.
/// The release would look fine and say the wrong thing.
#[test]
fn a_tagged_release_publishes_the_notes_written_for_it() {
    let wf = read_repo_file(RELEASE_WORKFLOW);
    assert!(
        wf.contains("--notes-file"),
        "the release no longer publishes a notes file"
    );
    assert!(
        wf.contains("docs/releases/${GITHUB_REF_NAME}.md"),
        "nothing resolves notes by tag name, so a tag's notes cannot reach its release"
    );
    assert!(
        wf.contains("name: release-notes"),
        "the notes are not handed to the publishing job, which has no checkout of its own"
    );

    // Every notes file has to be named for a tag this workflow would accept,
    // or it silently never publishes.
    let dir = repo_file("docs/releases");
    let entries = std::fs::read_dir(&dir).expect("docs/releases exists");
    let mut seen = 0;
    let mut failures = Failures::default();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(tag) = name.strip_suffix(".md") else {
            continue;
        };
        seen += 1;
        let versioned = tag.starts_with('v')
            && tag[1..].starts_with(|c: char| c.is_ascii_digit())
            && tag.matches('.').count() >= 2;
        failures.check(&name, versioned, || {
            "is not named for a tag the release workflow triggers on".to_string()
        });
    }
    assert!(seen > 0, "no release notes found, so this asserts nothing");
    failures.assert_empty("release notes naming");
}

// ---------------------------------------------------------------------------
// git-refresh
// ---------------------------------------------------------------------------
//
// The pilot component. Its observable is the exact set of paths deleted, so
// these cases assert on that set rather than on side effects — a port that
// deleted the right files plus one more would pass any "the cache is gone"
// check.

use claude_statusline::cmd::git_refresh;

/// Characters are removed, not replaced. `../../a/b` becomes `ab`, not
/// `______a_b`: a port that substituted would derive a different filename for
/// the same session and silently stop invalidating anything.
#[test]
fn session_ids_are_sanitised_by_removal() {
    struct Case {
        name: &'static str,
        raw: &'static str,
        want: &'static str,
    }

    let cases = [
        Case {
            name: "plain",
            raw: "fixture-session-0001",
            want: "fixture-session-0001",
        },
        Case {
            name: "traversal",
            raw: "../../fixture/escape",
            want: "fixtureescape",
        },
        Case {
            name: "windows-separators",
            raw: "..\\..\\evil",
            want: "evil",
        },
        Case {
            name: "absolute",
            raw: "/etc/passwd",
            want: "etcpasswd",
        },
        Case {
            name: "underscores-and-dashes-kept",
            raw: "a_b-c",
            want: "a_b-c",
        },
        Case {
            name: "all-stripped",
            raw: "../..",
            want: "",
        },
        Case {
            name: "nul-and-newline",
            raw: "abc\0def\nghi",
            want: "abcdefghi",
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let got = claude_statusline::session::sanitize_session_id(c.raw);
        failures.check(c.name, got == c.want, || {
            format!("{:?} -> {:?}, expected {:?}", c.raw, got, c.want)
        });
    }
    failures.assert_empty("session id sanitisation");
}

/// The session id reaches a filename, so a separator surviving sanitisation
/// would let a hook delete outside the temp directory. This asserts the
/// property directly rather than trusting the sanitiser's unit test.
#[test]
fn no_payload_can_produce_a_path_outside_the_temp_root() {
    let temp = scratch_dir("git-refresh-escape");
    let hostile = [
        "../../../../etc/passwd",
        "..\\..\\..\\Windows\\System32",
        "/absolute/path",
        "C:\\Windows",
        "a/../../b",
        "....//....//x",
    ];

    let mut failures = Failures::default();
    for raw in hostile {
        let payload = serde_json::json!({ "tool_name": "Edit", "session_id": raw }).to_string();
        for path in git_refresh::targets(&payload, &temp) {
            failures.check(raw, path.starts_with(&temp), || {
                format!("escaped the temp root: {}", path.display())
            });
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            failures.check(raw, !name.contains(".."), || {
                format!("filename still carries a traversal: {name}")
            });
        }
    }
    failures.assert_empty("path traversal");
}

/// Only the two performance caches. The tasks feed and the notification latch
/// are data stores, not caches — deleting them here would drop subagent rows and
/// re-fire the context alert on every edit.
#[test]
fn only_the_git_and_output_caches_are_invalidated() {
    let temp = scratch_dir("git-refresh-scope");
    let session = "fixture-session-0001";

    let files = [
        format!("statusline-git-{session}.txt"),
        format!("statusline-oc-{session}.txt"),
        format!("statusline-tasks-{session}.json"),
        format!("statusline-notify-{session}.json"),
        format!("statusline-focus-{session}.json"),
        format!("statusline-sa-{session}-task-0001.txt"),
        "unrelated.txt".to_string(),
    ];
    for f in &files {
        std::fs::write(temp.join(f), b"x").unwrap();
    }

    let payload = serde_json::json!({ "tool_name": "Edit", "session_id": session }).to_string();
    git_refresh::run(&payload, &temp);

    let mut failures = Failures::default();
    for f in &files {
        let gone = !temp.join(f).exists();
        let should_go = f.starts_with("statusline-git-") || f.starts_with("statusline-oc-");
        failures.check(f, gone == should_go, || {
            if should_go {
                "should have been deleted but survived".to_string()
            } else {
                "was deleted but is a data store, not a cache".to_string()
            }
        });
    }
    failures.assert_empty("invalidation scope");
}

/// Every degraded input is a no-op, and a tool that cannot change files is too.
#[test]
fn only_file_modifying_tools_invalidate_anything() {
    let temp = scratch_dir("git-refresh-tools");

    struct Case {
        name: &'static str,
        payload: String,
        expect: usize,
    }
    let session = "fixture-session-0001";
    let with =
        |tool: &str| serde_json::json!({ "tool_name": tool, "session_id": session }).to_string();

    let cases = [
        Case {
            name: "Edit",
            payload: with("Edit"),
            expect: 2,
        },
        Case {
            name: "Write",
            payload: with("Write"),
            expect: 2,
        },
        Case {
            name: "MultiEdit",
            payload: with("MultiEdit"),
            expect: 2,
        },
        Case {
            name: "Bash",
            payload: with("Bash"),
            expect: 2,
        },
        Case {
            name: "NotebookEdit",
            payload: with("NotebookEdit"),
            expect: 2,
        },
        Case {
            name: "Read",
            payload: with("Read"),
            expect: 0,
        },
        Case {
            name: "Glob",
            payload: with("Glob"),
            expect: 0,
        },
        Case {
            name: "empty-stdin",
            payload: String::new(),
            expect: 0,
        },
        Case {
            name: "malformed",
            payload: "{not json".to_string(),
            expect: 0,
        },
        Case {
            name: "not-an-object",
            payload: "[1,2,3]".to_string(),
            expect: 0,
        },
        Case {
            name: "no-session-id",
            payload: r#"{"tool_name":"Edit"}"#.to_string(),
            expect: 0,
        },
        Case {
            name: "session-id-sanitises-to-empty",
            payload: r#"{"tool_name":"Edit","session_id":"../.."}"#.to_string(),
            expect: 0,
        },
        Case {
            name: "tool-name-wrong-type",
            payload: r#"{"tool_name":123,"session_id":"abc"}"#.to_string(),
            expect: 0,
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let got = git_refresh::targets(&c.payload, &temp).len();
        failures.check(c.name, got == c.expect, || {
            format!("expected {} target(s), got {got}", c.expect)
        });
    }
    failures.assert_empty("tool matcher");
}

/// A missing cache file is the common case — the status line may not have
/// rendered since the last edit — and must not be an error.
#[test]
fn missing_cache_files_are_a_no_op() {
    let temp = scratch_dir("git-refresh-missing");
    let payload =
        serde_json::json!({ "tool_name": "Edit", "session_id": "nothing-here" }).to_string();
    assert!(
        git_refresh::run(&payload, &temp).is_empty(),
        "reported deleting files that were never there"
    );
}

/// AE-style equivalence against the captured fixture: the set of paths
/// the Rust port deletes must equal the set the script deleted, for the same
/// payload. This is the check the whole harness exists to make possible.
#[test]
fn deleted_paths_match_the_captured_fixtures() {
    let root = repo_file("tests/fixtures/git-refresh");
    let Ok(entries) = std::fs::read_dir(&root) else {
        println!("no git-refresh fixtures captured yet");
        return;
    };

    let mut failures = Failures::default();
    let mut checked = 0usize;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let case = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let meta = std::fs::read_to_string(dir.join("case.json")).unwrap_or_default();
        let payload_rel = json_string_field(&meta, "payload").unwrap_or("");
        let payload = std::fs::read_to_string(repo_file(&format!("tests/harness/{payload_rel}")))
            .unwrap_or_default();

        // Every platform that has been captured must agree with the port. A
        // fixture recorded on a platform this test is not running on is still
        // asserted: the deleted-path set is platform-independent, which is
        // exactly the claim a resolved divergence has to make about rendered output.
        for platform in ["macos", "linux", "windows"] {
            let expected_path = dir.join("expected").join(format!("{platform}.txt"));
            let Ok(expected_raw) = std::fs::read_to_string(&expected_path) else {
                continue;
            };
            checked += 1;

            let mut expected: Vec<String> = expected_raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            expected.sort();

            let temp = scratch_dir(&format!("gr-fixture-{case}-{platform}"));
            // Recreate what the harness supplied, so the port has the same
            // files available to delete that the script did.
            for name in &expected {
                std::fs::write(temp.join(name), b"cache").unwrap();
            }

            let mut got: Vec<String> = git_refresh::run(&payload, &temp)
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            got.sort();

            failures.check(&format!("{case}/{platform}"), got == expected, || {
                format!("script deleted {expected:?}, port deleted {got:?}")
            });
        }
    }

    println!("compared {checked} captured platform fixture(s)");
    // Asserted, not merely printed. Discovery that silently matches nothing
    // leaves `failures` empty too, and the pass then reads as coverage.
    assert!(checked > 0, "no git-refresh fixtures were compared");
    failures.assert_empty("git-refresh fixture equivalence");
}

// ---------------------------------------------------------------------------
// subagent tasks feed
// ---------------------------------------------------------------------------
//
// The handler's observable is the exact bytes it writes to the tasks feed, and
// its second contract is that it writes nothing to stdout — output there
// replaces Claude Code's default agent panel rather than adding to it, so an
// accidental byte does not degrade the display, it deletes it.
//
// Field order is part of the observable. The feed's bytes are an input to the
// status line's output-cache key, so a reordering would miss the cache on every
// tick while rendering identically.

use claude_statusline::cmd::subagent as cmd_subagent;

struct ProjectionCase {
    name: &'static str,
    payload: &'static str,
    /// The exact bytes written, or `None` when the tick must be skipped and the
    /// previous feed left in place.
    want: Option<&'static str>,
}

/// Every state of the tasks-feed contract, resolved to one answer each.
///
/// Two of these record a resolution rather than a port: the shell handlers
/// disagree, and a single behaviour had to be chosen. Both are stored here as
/// literals rather than captured from a script run, which is the mechanism a resolved divergence
/// prescribes for exactly this situation.
#[test]
fn the_projection_resolves_every_tasks_feed_state() {
    let cases = [
        ProjectionCase {
            name: "drops-absent-and-null-fields",
            payload: r#"{"session_id":"s1","tasks":[{"id":"a","effort":null,"status":"running"}]}"#,
            // `effort` is the field that makes this load-bearing: Claude Code
            // reports it only when the task carries an explicit override, so
            // presence is the signal to render the segment at all. An empty
            // string would render an override that does not exist.
            want: Some(r#"{"tasks":[{"id":"a","status":"running"}]}"#),
        },
        ProjectionCase {
            name: "keeps-falsy-values",
            payload: r#"{"session_id":"s1","tasks":[{"id":"a","tokenCount":0,"description":""}]}"#,
            want: Some(r#"{"tasks":[{"id":"a","description":"","tokenCount":0}]}"#),
        },
        ProjectionCase {
            name: "emits-fields-in-reader-order",
            payload: r#"{"session_id":"s1","tasks":[{"tokenCount":7,"id":"a","status":"x","name":"n"}]}"#,
            want: Some(r#"{"tasks":[{"id":"a","name":"n","status":"x","tokenCount":7}]}"#),
        },
        ProjectionCase {
            name: "drops-fields-the-reader-does-not-consume",
            payload: r#"{"session_id":"s1","tasks":[{"id":"a","tokenSamples":[1,2],"extra":"x"}]}"#,
            want: Some(r#"{"tasks":[{"id":"a"}]}"#),
        },
        // RESOLVED DIVERGENCE. jq's `select(type == "object")` drops a
        // non-object task; the PowerShell handler emits `{}` for it, which
        // reaches the reader as a task with no id. Resolved to the bash
        // behaviour on both platforms: `{}` is not a task.
        ProjectionCase {
            name: "non-object-task-is-dropped-not-emitted-as-empty",
            payload: r#"{"session_id":"s1","tasks":[{"id":"a"},"nope",42,null]}"#,
            want: Some(r#"{"tasks":[{"id":"a"}]}"#),
        },
        // RESOLVED DIVERGENCE. Windows PowerShell 5.1 escapes `'`, `<`, `>` and
        // every non-ASCII character as \uXXXX; jq and PowerShell 7 emit them
        // raw. The Windows handler therefore has no single byte-exact
        // behaviour of its own — it depends on which interpreter the user runs.
        // Resolved to the minimal-escaping form, which matches jq, matches
        // PowerShell 7, and is what the only consumer — a JSON parser in the
        // status line — reads identically either way.
        ProjectionCase {
            name: "escapes-minimally-like-jq-not-like-powershell-51",
            payload: r#"{"session_id":"s1","tasks":[{"id":"a","description":"the user's <tag> & ✅"}]}"#,
            want: Some(r#"{"tasks":[{"id":"a","description":"the user's <tag> & ✅"}]}"#),
        },
        ProjectionCase {
            name: "absent-tasks-writes-an-empty-list",
            payload: r#"{"session_id":"s1"}"#,
            want: Some(r#"{"tasks":[]}"#),
        },
        ProjectionCase {
            name: "empty-tasks-writes-an-empty-list",
            payload: r#"{"session_id":"s1","tasks":[]}"#,
            want: Some(r#"{"tasks":[]}"#),
        },
        // Everything below leaves the previous feed alone. A tee that
        // overwrote on garbage would silently drop the subagent rows until the
        // next good tick — the contract half that got the raw-tee prototype
        // rejected in docs/performance.md §7.
        ProjectionCase {
            name: "tasks-of-the-wrong-type-is-a-malformed-tick",
            payload: r#"{"session_id":"s1","tasks":"nope"}"#,
            want: None,
        },
        ProjectionCase {
            name: "unparseable-payload",
            payload: "{not json",
            want: None,
        },
        ProjectionCase {
            name: "payload-is-not-an-object",
            payload: "[1,2,3]",
            want: None,
        },
        ProjectionCase {
            name: "no-session-id",
            payload: r#"{"tasks":[{"id":"a"}]}"#,
            want: None,
        },
        ProjectionCase {
            name: "session-id-sanitises-to-nothing",
            payload: r#"{"session_id":"../..","tasks":[{"id":"a"}]}"#,
            want: None,
        },
        ProjectionCase {
            name: "empty-payload",
            payload: "",
            want: None,
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let got = cmd_subagent::project(c.payload).map(|(_, bytes)| bytes);
        failures.check(c.name, got.as_deref() == c.want, || {
            format!("got {got:?}, expected {:?}", c.want)
        });
    }
    failures.assert_empty("tasks-feed projection");
}

/// The session id reaches a filename, so a separator surviving sanitisation
/// would let the handler write outside the temp directory — and unlike
/// `git-refresh`, this component *creates* files, so an escape plants content
/// rather than deleting it.
#[test]
fn no_payload_can_tee_outside_the_temp_root() {
    let temp = scratch_dir("subagent-escape");
    let hostile = [
        "../../../../etc/cron.d/x",
        "..\\..\\..\\Windows\\System32\\x",
        "/absolute/path",
        "C:\\Windows",
        "a/../../b",
    ];

    let mut failures = Failures::default();
    for raw in hostile {
        let payload = serde_json::json!({ "session_id": raw, "tasks": [] }).to_string();
        let Some((safe_id, _)) = cmd_subagent::project(&payload) else {
            continue;
        };
        let path = cmd_subagent::feed_path(&temp, &safe_id);
        failures.check(raw, path.starts_with(&temp), || {
            format!("escaped the temp root: {}", path.display())
        });
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        failures.check(raw, !name.contains(".."), || {
            format!("filename still carries a traversal: {name}")
        });
    }
    failures.assert_empty("tasks-feed path traversal");
}

/// The feed path is entirely predictable from the session id, and on a shared
/// `/tmp` that is the difference between a state file and an arbitrary-write
/// primitive. Both shell handlers refuse rather than follow; so must this.
#[test]
fn a_hostile_feed_target_is_refused_not_followed() {
    let dir = scratch_dir("subagent-hostile");
    let victim = dir.join("victim.txt");
    let link = cmd_subagent::feed_path(&dir, "s1");
    std::fs::write(&victim, b"original").unwrap();

    if !make_symlink(&victim, &link) {
        skipped_for_want_of_symlinks();
        return;
    }

    let payload = r#"{"session_id":"s1","tasks":[{"id":"a"}]}"#;
    let outcome = cmd_subagent::run(payload, &inherited(&dir));
    assert!(
        matches!(
            outcome,
            cmd_subagent::Tick::Wrote(_) | cmd_subagent::Tick::Hostile
        ),
        "unexpected outcome: {outcome:?}"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"original",
        "the tee followed the symlink and clobbered the victim file"
    );
}

/// Anything on stdout replaces Claude Code's default agent panel. This runs the
/// real binary because stdout emptiness is a process-level fact, and it asserts
/// the feed was written in the same breath — otherwise "prints nothing" would
/// be satisfied by a handler that does nothing.
#[test]
fn the_subagent_handler_prints_nothing_while_still_teeing() {
    let dir = scratch_dir("subagent-silent");
    let root = dir.to_string_lossy().into_owned();
    let env: &[(&str, &str)] = &[("TMPDIR", &root), ("TEMP", &root), ("TMP", &root)];

    let valid = r#"{"session_id":"silent-1","tasks":[{"id":"a","status":"running"}]}"#;
    let mut failures = Failures::default();

    for (name, stdin) in [
        ("valid", valid),
        ("malformed", "{not json"),
        ("empty", ""),
        ("not-an-object", "[1,2,3]"),
    ] {
        let run = run_bin(&["subagent"], stdin, env);
        failures.check(name, run.code == Some(0), || {
            format!("expected exit 0, got {:?}", run.code)
        });
        failures.check(name, run.stdout.is_empty(), || {
            format!(
                "wrote to stdout, which replaces the agent panel: {:?}",
                run.stdout
            )
        });
        failures.check(name, run.stderr.is_empty(), || {
            format!("wrote to stderr: {:?}", run.stderr)
        });
    }

    // Composed through the same resolver the binary used, not from `dir`
    // directly. The feed now lands one level down, and hardcoding the flat path
    // here would report a working handler as broken — or, worse, keep passing
    // against a stale flat file if the move were ever reverted.
    let resolved = claude_statusline::session::state_dir_in(&dir);
    failures.check("valid", resolved.is_guarded(), || {
        "the resolver fell back to the flat root, so this case is not exercising \
         the state directory at all"
            .to_string()
    });
    let feed =
        std::fs::read_to_string(cmd_subagent::feed_path(&resolved, "silent-1")).unwrap_or_default();
    failures.check("valid", !feed.is_empty(), || {
        "the valid payload wrote no feed, so silence here proves nothing".to_string()
    });
    // The flat path must be empty: a handler that wrote both would satisfy the
    // check above while leaving the clutter this change exists to remove.
    failures.check(
        "valid",
        !cmd_subagent::feed_path(&dir, "silent-1").exists(),
        || "the feed was also written flat in the temp root".to_string(),
    );
    failures.assert_empty("subagent stdout contract");
}

/// Equivalence against the captured fixtures: the bytes the port writes to
/// the feed must equal the bytes each platform's script wrote, for the same
/// payload and the same supplied state.
#[test]
fn feed_bytes_match_the_captured_fixtures() {
    let root = repo_file("tests/fixtures/subagent");
    let Ok(entries) = std::fs::read_dir(&root) else {
        println!("no subagent fixtures captured yet");
        return;
    };

    let mut failures = Failures::default();
    let mut checked = 0usize;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let case = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("case.json")).unwrap_or_default(),
        )
        .unwrap_or(serde_json::Value::Null);
        let payload_rel = meta["payload"].as_str().unwrap_or("");
        let payload = std::fs::read_to_string(repo_file(&format!("tests/harness/{payload_rel}")))
            .unwrap_or_default();
        let session = meta["session_id"].as_str().unwrap_or("");

        // Every platform that has been captured is asserted, including ones
        // this test is not running on: the feed's bytes are platform
        // independent, which is the claim the fixtures exist to prove.
        for platform in ["macos", "linux", "windows"] {
            let expected_path = dir.join("expected").join(format!("{platform}.txt"));
            let Ok(expected) = std::fs::read_to_string(&expected_path) else {
                continue;
            };
            checked += 1;
            let label = format!("{case}/{platform}");

            let temp = scratch_dir(&format!("subagent-fixture-{case}-{platform}"));
            // Recreate what the harness supplied, so the port starts from the
            // same state the script did — the malformed case is only meaningful
            // if a previous feed is actually there to survive.
            for input in meta["inputs"].as_array().into_iter().flatten() {
                let target = input["target"].as_str().unwrap_or("");
                let content = input["content"].as_str().unwrap_or("");
                let Some(rel) = target.strip_prefix("{TMP}/") else {
                    failures.check(&label, false, || {
                        format!("unsupported input target `{target}`: this replay only stages {{TMP}} files")
                    });
                    continue;
                };
                let bytes = std::fs::read(repo_file(&format!("tests/harness/{content}")))
                    .unwrap_or_default();
                std::fs::write(temp.join(rel.replace("{SESSION}", session)), bytes).unwrap();
            }

            cmd_subagent::run(&payload, &inherited(&temp));

            let got = std::fs::read_to_string(cmd_subagent::feed_path(&temp, session))
                .unwrap_or_default();
            failures.check(&label, got == expected, || {
                format!("script wrote {expected:?}, port wrote {got:?}")
            });

            // An empty expectation means no feed exists at all, which a plain
            // string compare cannot distinguish from an empty file.
            if expected.is_empty() {
                let stray: Vec<String> = std::fs::read_dir(&temp)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                failures.check(&label, stray.is_empty(), || {
                    format!("expected no feed file, found {stray:?}")
                });
            }
        }
    }

    println!("compared {checked} captured platform fixture(s)");
    assert!(checked > 0, "no subagent fixtures were compared");
    failures.assert_empty("subagent fixture equivalence");
}

// ---------------------------------------------------------------------------
// notify
// ---------------------------------------------------------------------------
//
// The observable here is the command and arguments notify invokes, so every
// case asserts the plan rather than the effect: nothing is spawned, no toast
// appears on the developer's desktop, and the assertions are the same on every
// host because `plan` takes the platform as a parameter.

use claude_statusline::cmd::notify::{self, Action, Env, Platform};
use claude_statusline::config::NotifyConfig;

/// Fixtures whose captured bytes are a record of what the scripts do, not a
/// target for the port.
///
/// `muted-sound-for-event` is the only one. Both bash scripts read the config
/// flag as `jq -r '.[$e].sound // true'`, and jq's `//` yields its right-hand
/// side when the left is `false` as well as when it is null — so `false // true`
/// is `true`, and `"sound": false` has never muted anything on macOS or Linux.
/// The flags gate delivery, so the port mutes correctly and
/// deliberately breaks the current behaviour of both bash platforms. Windows
/// already behaved correctly. `muting_is_honoured_on_every_platform` is the
/// literal that replaces the capture.
const DIVERGENT_FIXTURES: [&str; 1] = ["muted-sound-for-event"];

/// An environment with every helper and asset present, so a case that wants to
/// exercise the absent ones removes them explicitly rather than depending on
/// what the test machine happens to have installed.
fn full_env() -> Env {
    let home = PathBuf::from("/home/fixture");
    let mut files: std::collections::BTreeSet<PathBuf> =
        ["bell.oga", "complete.oga", "dialog-warning.oga"]
            .iter()
            .map(|f| PathBuf::from(format!("/usr/share/sounds/freedesktop/stereo/{f}")))
            .collect();
    // Spelled with literal backslashes rather than `PathBuf::join`, which would
    // use the host's separator and stop matching what the planner emits when
    // these tests run on Unix.
    for f in [
        "Windows Exclamation.wav",
        "chimes.wav",
        "Windows Battery Low.wav",
        "Windows Battery Critical.wav",
    ] {
        files.insert(PathBuf::from(format!("C:\\Windows\\Media\\{f}")));
    }
    Env {
        home: home.clone(),
        cwd: PathBuf::from("/repo/work"),
        system_root: PathBuf::from("C:\\Windows"),
        programs: [
            "terminal-notifier",
            "notify-send",
            "paplay",
            "ffplay",
            "ogg123",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        files,
    }
}

/// Collapses a plan into the shim's record format, so a planned invocation and
/// a captured one can be compared directly.
fn as_records(actions: &[Action]) -> Vec<String> {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    }
    let mut out: Vec<String> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Spawn { program, args, .. } => {
                let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
                let mut line = esc(name);
                for arg in args {
                    line.push('\t');
                    line.push_str(&esc(arg));
                }
                Some(line)
            }
            // In-process Windows audio leaves nothing for a shim to record.
            Action::PlayWav(_) | Action::Beep => None,
        })
        .collect();
    // The observable is a set: the sound helper is backgrounded, so its record
    // races the visual one. Both capture drivers sort for the same reason.
    out.sort();
    out
}

struct NotifyCase {
    name: &'static str,
    platform: Platform,
    event: &'static str,
    value: &'static str,
    stdin: &'static str,
    config: &'static str,
    want: &'static [&'static str],
}

const PERMISSION_PAYLOAD: &str =
    r#"{"tool_name":"Bash","session_id":"s1","tool_input":{"command":"git status --porcelain"}}"#;

/// Every event on every platform, plus the states that change what is invoked.
#[test]
fn every_event_invokes_what_the_scripts_invoked() {
    let cases = [
        NotifyCase {
            name: "macos-permission",
            platform: Platform::Macos,
            event: "permission",
            value: "",
            stdin: PERMISSION_PAYLOAD,
            config: "{}",
            want: &[
                "afplay\t/System/Library/Sounds/Tink.aiff",
                "terminal-notifier\t-title\tClaude Code\t-message\tBash: git status --porcelain",
            ],
        },
        NotifyCase {
            name: "linux-permission",
            platform: Platform::Linux,
            event: "permission",
            value: "",
            stdin: PERMISSION_PAYLOAD,
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tBash: git status --porcelain\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        NotifyCase {
            name: "macos-stop",
            platform: Platform::Macos,
            event: "stop",
            value: "",
            stdin: "",
            config: "{}",
            want: &[
                "afplay\t/System/Library/Sounds/Glass.aiff",
                "terminal-notifier\t-title\tClaude Code\t-message\tFinished working",
            ],
        },
        NotifyCase {
            name: "linux-rate-limit-carries-its-value",
            platform: Platform::Linux,
            event: "rate_limit",
            value: "82",
            stdin: "",
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tRate limit at 82%\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/dialog-warning.oga",
            ],
        },
        NotifyCase {
            name: "macos-context-high-carries-its-value",
            platform: Platform::Macos,
            event: "context_high",
            value: "71",
            stdin: "",
            config: "{}",
            want: &[
                "afplay\t/System/Library/Sounds/Sosumi.aiff",
                "terminal-notifier\t-title\tClaude Code\t-message\tContext window at 71%",
            ],
        },
        NotifyCase {
            name: "linux-compaction-start",
            platform: Platform::Linux,
            event: "compaction_start",
            value: "",
            stdin: "",
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tCompacting context...\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        NotifyCase {
            name: "linux-compaction-done",
            platform: Platform::Linux,
            event: "compaction_done",
            value: "",
            stdin: "",
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tContext compacted\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/complete.oga",
            ],
        },
        // An unknown event has no message and no sound, so it invokes nothing
        // rather than raising a blank notification.
        NotifyCase {
            name: "unknown-event-invokes-nothing",
            platform: Platform::Macos,
            event: "not-an-event",
            value: "",
            stdin: "",
            config: "{}",
            want: &[],
        },
        NotifyCase {
            name: "empty-event-invokes-nothing",
            platform: Platform::Linux,
            event: "",
            value: "",
            stdin: "",
            config: "{}",
            want: &[],
        },
        // A permission payload that says nothing useful still notifies: the
        // user needs to know something is waiting even if we cannot say what.
        NotifyCase {
            name: "unparseable-payload-still-prompts",
            platform: Platform::Linux,
            event: "permission",
            value: "",
            stdin: "{not json",
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tWaiting for permission\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        NotifyCase {
            name: "tool-with-no-detail-names-the-tool",
            platform: Platform::Linux,
            event: "permission",
            value: "",
            stdin: r#"{"tool_name":"WebFetch"}"#,
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tWebFetch\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        // A file path under the working directory is shown relative to it, so
        // the notification is not mostly the user's home directory.
        NotifyCase {
            name: "file-path-is-relative-to-cwd",
            platform: Platform::Linux,
            event: "permission",
            value: "",
            stdin: r#"{"tool_name":"Edit","tool_input":{"file_path":"/repo/work/src/main.rs"}}"#,
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tEdit: src/main.rs\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        NotifyCase {
            name: "file-path-outside-cwd-is-left-whole",
            platform: Platform::Linux,
            event: "permission",
            value: "",
            stdin: r#"{"tool_name":"Read","tool_input":{"file_path":"/etc/hosts"}}"#,
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tRead: /etc/hosts\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        // Every metacharacter that would matter to a shell is delivered
        // literally, because argv is a list and no shell ever sees it.
        NotifyCase {
            name: "metacharacters-are-delivered-literally",
            platform: Platform::Linux,
            event: "permission",
            value: "",
            stdin: r#"{"tool_name":"Bash","tool_input":{"command":"echo \"hi\"; rm -rf /; $(id) `id` && x\ny"}}"#,
            config: "{}",
            want: &[
                "notify-send\tClaude Code\tBash: echo \"hi\"; rm -rf /; $(id) `id` && x\\ny\t--urgency=normal",
                "paplay\t/usr/share/sounds/freedesktop/stereo/bell.oga",
            ],
        },
        NotifyCase {
            name: "visual-muted-leaves-only-sound",
            platform: Platform::Macos,
            event: "stop",
            value: "",
            stdin: "",
            config: r#"{"stop":{"visual":false}}"#,
            want: &["afplay\t/System/Library/Sounds/Glass.aiff"],
        },
        NotifyCase {
            name: "a-non-boolean-flag-does-not-mute",
            platform: Platform::Macos,
            event: "stop",
            value: "",
            stdin: "",
            config: r#"{"stop":{"sound":"false","visual":null}}"#,
            want: &[
                "afplay\t/System/Library/Sounds/Glass.aiff",
                "terminal-notifier\t-title\tClaude Code\t-message\tFinished working",
            ],
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let cfg = NotifyConfig::parse(c.config);
        let got = as_records(&notify::plan(
            c.platform,
            c.event,
            c.value,
            c.stdin,
            &cfg,
            &full_env(),
            None,
        ));
        let want: Vec<String> = c.want.iter().map(|s| s.to_string()).collect();
        failures.check(c.name, got == want, || {
            format!("got {got:?}, want {want:?}")
        });
    }
    failures.assert_empty("notify invocations");
}

/// The resolved divergence. `sound: false` must actually mute, on
/// every platform — which is a deliberate break from what both bash scripts do
/// today. See `DIVERGENT_FIXTURES`.
#[test]
fn muting_is_honoured_on_every_platform() {
    let cfg = NotifyConfig::parse(r#"{"permission":{"sound":false,"visual":true}}"#);
    let env = full_env();
    let mut failures = Failures::default();

    for (name, platform, want) in [
        (
            "macos",
            Platform::Macos,
            vec!["terminal-notifier\t-title\tClaude Code\t-message\tBash: git status --porcelain"],
        ),
        (
            "linux",
            Platform::Linux,
            vec!["notify-send\tClaude Code\tBash: git status --porcelain\t--urgency=normal"],
        ),
    ] {
        let got = as_records(&notify::plan(
            platform,
            "permission",
            "",
            PERMISSION_PAYLOAD,
            &cfg,
            &env,
            None,
        ));
        let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        failures.check(name, got == want, || {
            format!("a muted event still invoked a sound helper: got {got:?}")
        });
    }

    // Windows sound is in-process, so muting is asserted on the plan itself
    // rather than on an invocation.
    let plan = notify::plan(
        Platform::Windows,
        "permission",
        "",
        PERMISSION_PAYLOAD,
        &cfg,
        &env,
        None,
    );
    failures.check(
        "windows",
        !plan
            .iter()
            .any(|a| matches!(a, Action::PlayWav(_) | Action::Beep)),
        || format!("a muted event still planned audio: {plan:?}"),
    );
    failures.assert_empty("muted delivery");
}

/// A helper that is not installed is skipped, and the rest of the notification
/// still goes out. The scripts tolerate every one of these being absent.
#[test]
fn a_missing_helper_degrades_rather_than_dropping_the_notification() {
    let mut env = full_env();
    env.programs.remove("notify-send");
    env.programs.remove("paplay");

    let cfg = NotifyConfig::default();
    let got = as_records(&notify::plan(
        Platform::Linux,
        "stop",
        "",
        "",
        &cfg,
        &env,
        None,
    ));
    assert_eq!(
        got,
        vec!["ffplay\t-nodisp\t-autoexit\t-loglevel\tquiet\t/usr/share/sounds/freedesktop/stereo/complete.oga"],
        "the next available player should have been used and the visual skipped"
    );

    // Every player gone: sound is dropped, the visual survives.
    env.programs.remove("ffplay");
    env.programs.remove("ogg123");
    env.programs.insert("notify-send".to_string());
    let got = as_records(&notify::plan(
        Platform::Linux,
        "stop",
        "",
        "",
        &cfg,
        &env,
        None,
    ));
    assert_eq!(
        got,
        vec!["notify-send\tClaude Code\tFinished working\t--urgency=normal"]
    );

    // The sound asset missing is the other half: Linux checks, macOS does not.
    let mut env = full_env();
    env.files.clear();
    let got = as_records(&notify::plan(
        Platform::Linux,
        "stop",
        "",
        "",
        &cfg,
        &env,
        None,
    ));
    assert_eq!(
        got,
        vec!["notify-send\tClaude Code\tFinished working\t--urgency=normal"],
        "a missing sound asset should skip the player, not the notification"
    );
}

/// The icon is added only when it is actually on disk, because both helpers
/// treat a missing icon path as an error rather than ignoring it.
#[test]
fn the_icon_is_attached_only_when_it_exists() {
    let cfg = NotifyConfig::default();
    let mut env = full_env();
    env.files
        .insert(PathBuf::from("/home/fixture/.claude/claude-icon.png"));

    let macos = as_records(&notify::plan(
        Platform::Macos,
        "stop",
        "",
        "",
        &cfg,
        &env,
        None,
    ));
    assert_eq!(
        macos,
        vec![
            "afplay\t/System/Library/Sounds/Glass.aiff",
            "terminal-notifier\t-title\tClaude Code\t-message\tFinished working\t-appIcon\t/home/fixture/.claude/claude-icon.png\t-contentImage\t/home/fixture/.claude/claude-icon.png",
        ]
    );

    let linux = as_records(&notify::plan(
        Platform::Linux,
        "stop",
        "",
        "",
        &cfg,
        &env,
        None,
    ));
    assert!(
        linux
            .iter()
            .any(|l| l.contains("--icon=/home/fixture/.claude/claude-icon.png")),
        "the icon flag is missing: {linux:?}"
    );
}

/// Asserted as an invariant rather than by inspecting escapes.
///
/// The permission message is `tool_input.command` — whatever the model was
/// about to run — so it is attacker-influenceable. On Windows it crosses into a
/// second interpreter, and the only safe way to do that is as data. This checks
/// that no byte of it ever appears in the program path or in any argument,
/// which is a stronger claim than "the quoting looks right".
#[test]
fn the_windows_toast_never_carries_the_message_in_its_argv() {
    let hostile = "'; Remove-Item C:\\ -Recurse; $(whoami) `id` \"quoted\"";
    let stdin = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": hostile },
    })
    .to_string();

    let plan = notify::plan(
        Platform::Windows,
        "permission",
        "",
        &stdin,
        &NotifyConfig::default(),
        &full_env(),
        None,
    );

    let Some(Action::Spawn {
        program,
        args,
        stdin: payload,
        ..
    }) = plan.first()
    else {
        panic!("the Windows plan did not start with a spawn: {plan:?}");
    };

    // Absolute path under %SystemRoot%: a powershell.exe planted on PATH or in
    // the working directory must never be what raises a notification.
    assert_eq!(
        program,
        "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
    );
    assert!(
        args.contains(&"-NoProfile".to_string()),
        "a user profile could otherwise redefine what the script means"
    );

    // The distinctive fragments of the hostile string must appear nowhere in
    // argv — not escaped, not quoted, not at all.
    for fragment in ["Remove-Item", "whoami", "quoted"] {
        assert!(
            !program.contains(fragment),
            "the program path carries the message"
        );
        for arg in args {
            assert!(
                !arg.contains(fragment),
                "an argument carries the message, which is exactly what this forbids: {arg:?}"
            );
        }
    }

    // It does reach the child — as data, on stdin.
    let payload = payload.as_deref().unwrap_or("");
    assert!(
        payload.contains("Remove-Item"),
        "the message never reached the child at all: {payload:?}"
    );
    let parsed: serde_json::Value = serde_json::from_str(payload).expect("stdin is not JSON");
    assert_eq!(
        parsed["message"],
        serde_json::Value::String(format!("Bash: {hostile}"))
    );

    // And the script body is a constant that cannot be influenced.
    assert!(
        !notify::WINDOWS_TOAST_SCRIPT.contains('"'),
        "a double quote in the body would be re-encoded by the Windows command-line rules"
    );
}

/// The configured thresholds, which the status line reads to decide whether to fire at
/// all. A non-integer must fall back rather than disable the alert.
#[test]
fn thresholds_default_when_absent_or_unusable() {
    struct Case {
        name: &'static str,
        json: &'static str,
        event: &'static str,
        want: i64,
    }

    let cases = [
        Case {
            name: "absent-config",
            json: "{}",
            event: "context_high",
            want: 70,
        },
        Case {
            name: "absent-rate-limit",
            json: "{}",
            event: "rate_limit",
            want: 80,
        },
        Case {
            name: "configured",
            json: r#"{"context_high":{"threshold":55}}"#,
            event: "context_high",
            want: 55,
        },
        Case {
            name: "non-integer",
            json: r#"{"context_high":{"threshold":"high"}}"#,
            event: "context_high",
            want: 70,
        },
        Case {
            name: "unparseable-file",
            json: "{not json",
            event: "rate_limit",
            want: 80,
        },
        Case {
            name: "json-not-an-object",
            json: "[1,2,3]",
            event: "context_high",
            want: 70,
        },
    ];

    let mut failures = Failures::default();
    for c in cases {
        let got = NotifyConfig::parse(c.json).threshold(c.event);
        failures.check(c.name, got == c.want, || {
            format!("threshold for {} was {got}, want {}", c.event, c.want)
        });
    }
    failures.assert_empty("notify thresholds");
}

/// Equivalence against the captured fixtures, for the cases where the
/// scripts and the port are supposed to agree.
#[test]
fn notify_invocations_match_the_captured_fixtures() {
    let root = repo_file("tests/fixtures/notify");
    let Ok(entries) = std::fs::read_dir(&root) else {
        println!("no notify fixtures captured yet");
        return;
    };

    let mut failures = Failures::default();
    let mut checked = 0usize;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let case = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if DIVERGENT_FIXTURES.contains(&case.as_str()) {
            println!("skipping {case}: a recorded divergence, asserted as a literal instead");
            continue;
        }

        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("case.json")).unwrap_or_default(),
        )
        .unwrap_or(serde_json::Value::Null);
        let payload_rel = meta["payload"].as_str().unwrap_or("");
        let payload = std::fs::read_to_string(repo_file(&format!("tests/harness/{payload_rel}")))
            .unwrap_or_default();
        let config_rel = meta["notify_config"].as_str().unwrap_or("");
        let cfg = NotifyConfig::load(&repo_file(&format!("tests/harness/{config_rel}")));
        let event = meta["args"][0].as_str().unwrap_or("");
        let value = meta["args"][1].as_str().unwrap_or("");

        for (platform, name) in [(Platform::Macos, "macos"), (Platform::Linux, "linux")] {
            let Ok(expected_raw) =
                std::fs::read_to_string(dir.join("expected").join(format!("{name}.txt")))
            else {
                continue;
            };
            checked += 1;
            let label = format!("{case}/{name}");

            // The harness supplies its own isolated home and working directory,
            // and the capture scrubbed both back to placeholders. Replaying
            // with the same placeholders is what makes the comparison possible.
            let mut env = full_env();
            env.home = PathBuf::from("{HOME}");
            env.cwd = PathBuf::from("{REPO}");

            let expected: Vec<String> = expected_raw
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            let got = as_records(&notify::plan(
                platform, event, value, &payload, &cfg, &env, None,
            ));

            failures.check(&label, got == expected, || {
                format!("script invoked {expected:?}, port planned {got:?}")
            });
        }
    }

    println!("compared {checked} captured platform fixture(s)");
    assert!(checked > 0, "no notify fixtures were compared");
    failures.assert_empty("notify fixture equivalence");
}

// ---------------------------------------------------------------------------
// Installer contract
// ---------------------------------------------------------------------------
//
// These rules are invisible until they are violated, and each one has already
// cost this project something: an `exit` closes the terminal of anyone who ran
// the published one-liner, a BOM breaks `iex` on the first token, and staging
// in a shared temp reopens the window between verification and placement.

/// Every shell installer. These are exactly what a user can pipe into their
/// shell from the published one-liner URLs, so the rules below apply to all of
/// them.
const INSTALL_SH: [&str; 2] = ["install/install.sh", "install/uninstall.sh"];
const INSTALL_PS1: [&str; 2] = ["install/install.ps1", "install/uninstall.ps1"];

fn code_lines(body: &str, comment: char) -> impl Iterator<Item = (usize, &str)> {
    body.lines()
        .enumerate()
        .map(|(n, l)| (n + 1, l.trim()))
        .filter(move |(_, l)| !l.is_empty() && !l.starts_with(comment))
}

/// Every file allowed to hold platform-conditional code, and the area that
/// earns it the exemption.
///
/// One implementation replaced three, and the way that erodes is one
/// `cfg!(windows)` at a time until the file is three implementations again.
/// These four areas are where a platform difference is irreducible; a branch
/// anywhere else is a behaviour that should be resolved to one recorded answer
/// instead of forked.
const PLATFORM_CONDITIONAL: [(&str, &str); 9] = [
    (
        "src/platform/mod.rs",
        "file-ownership checks and process-entry stream handling",
    ),
    ("src/platform/notify.rs", "notification delivery"),
    (
        "src/platform/focus.rs",
        "notification delivery: click capture, transport and URI registration",
    ),
    ("src/cmd/notify.rs", "notification delivery"),
    (
        "src/notify_state.rs",
        "notification delivery: creation flags on the spawned notifier",
    ),
    (
        "src/git.rs",
        "process creation flags: no console flash on the git child",
    ),
    (
        "src/lib.rs",
        "environment spelling: %USERPROFILE% against $HOME",
    ),
    (
        "src/session.rs",
        "environment spelling: %TEMP% against $TMPDIR",
    ),
    (
        "src/settings.rs",
        "whether a stored command needs quoting, which only Windows does",
    ),
];

/// Every `.rs` file under `src/`, repo-relative and slash-separated.
fn crate_sources() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&repo_file("src"), &mut found);
    let base = repo_file("");
    let mut rel: Vec<String> = found
        .iter()
        .filter_map(|p| p.strip_prefix(&base).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    rel.sort();
    rel
}

/// Platform-conditional code stays where it is unavoidable.
///
/// The interesting direction is a *new* file appearing: a `cfg!(windows)` in
/// the renderer or the payload reader means a behaviour got branched instead of
/// decided, which is how one implementation grows back into three.
#[test]
fn platform_conditional_code_stays_in_its_areas() {
    const MARKERS: [&str; 6] = [
        "cfg!(windows)",
        "cfg!(unix)",
        "#[cfg(windows)]",
        "#[cfg(unix)]",
        "#[cfg(target_os",
        "#[cfg(target_family",
    ];

    let allowed: std::collections::BTreeMap<&str, &str> =
        PLATFORM_CONDITIONAL.iter().copied().collect();
    let mut failures = Failures::default();

    for rel in crate_sources() {
        let body = read_repo_file(&rel);
        let branches = MARKERS.iter().any(|m| body.contains(m));
        match (branches, allowed.get(rel.as_str())) {
            (true, None) => failures.check(&rel, false, || {
                "branches on the platform but is not one of the areas where that is \
                 allowed — resolve the behaviour to one answer, or widen the list \
                 deliberately"
                    .to_string()
            }),
            // A file that stops branching is fine, but the list must not keep
            // claiming it does: a stale exemption quietly permits a new branch.
            (false, Some(area)) => failures.check(&rel, false, || {
                format!("is listed for `{area}` but no longer branches on the platform")
            }),
            _ => {}
        }
    }
    failures.assert_empty("platform-conditional confinement");
}

/// `irm | iex` and `curl | bash` both run these in the user's live shell,
/// where `exit` terminates their session and closes the window. CLAUDE.md's
/// idiom is `return 1 2>/dev/null || exit 1`: `return` succeeds when sourced,
/// and the `exit` fallback only ever runs in a subshell.
#[test]
fn install_scripts_never_exit_the_users_shell() {
    let mut failures = Failures::default();

    for rel in INSTALL_SH {
        let body = read_repo_file(rel);
        for (n, line) in code_lines(&body, '#') {
            if line.contains("exit") && line != "return 1 2>/dev/null || exit 1" {
                failures.check(&format!("{rel}:{n}"), false, || {
                    format!("bare `exit` outside CLAUDE.md's guarded idiom: {line}")
                });
            }
        }
    }

    for rel in INSTALL_PS1 {
        let body = read_repo_file(rel);
        for (n, line) in code_lines(&body, '#') {
            // Case-insensitively, because PowerShell keywords are. The gate
            // compared `w == "exit"` for a while, which `Exit` and `EXIT` walked
            // straight through while still closing the user's session.
            let has_exit = line
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .any(|w| w.eq_ignore_ascii_case("exit"));
            // The two spellings that do not contain the bare word: both end the
            // session just as hard.
            let lowered = line.to_ascii_lowercase();
            let has_call = lowered.contains("::exit(") || lowered.contains("setshouldexit");
            if has_exit || has_call {
                failures.check(&format!("{rel}:{n}"), false, || {
                    format!("`exit` would close the user's PowerShell session: {line}")
                });
            }
        }
    }
    failures.assert_empty("no-exit rule");
}

/// `$LASTEXITCODE` is only written by a process that actually starts. An
/// executable that cannot launch — wrong architecture, a truncated download, an
/// antivirus quarantine — leaves the *previous* command's code in place, so a
/// bare `if ($LASTEXITCODE -ne 0)` reads a stale value and treats a binary that
/// never ran as a success.
///
/// That is not theoretical: `gh attestation verify` sets it to 0 a few steps
/// before the self-check, so an unlaunchable binary passed its own gate and the
/// install went on to rewrite `settings.json` and delete the user's superseded
/// scripts. `Invoke-Binary` clears it first and reports whether the process ran
/// at all; this asserts nothing goes back to reading the variable raw.
#[test]
fn powershell_installers_never_read_a_stale_exit_code() {
    let mut failures = Failures::default();

    for rel in INSTALL_PS1 {
        let body = read_repo_file(rel);
        let mut defines_helper = false;

        for (n, line) in code_lines(&body, '#') {
            if line.contains("function Invoke-Binary") {
                defines_helper = true;
            }
            // The helper owns the only legitimate reads: the clear, and the
            // two checks immediately after the invocation it guards.
            let is_helper_internal = line.contains("$global:LASTEXITCODE = $null")
                || line.contains("if ($null -eq $LASTEXITCODE)")
                || line.contains("Code = $LASTEXITCODE");
            if line.contains("LASTEXITCODE") && !is_helper_internal {
                failures.check(&format!("{rel}:{n}"), false, || {
                    format!("reads $LASTEXITCODE directly instead of Invoke-Binary: {line}")
                });
            }
            // `& $exe` outside the helper bypasses the clear, so the next
            // reader inherits whatever this one left behind.
            let calls_native = line.trim_start().starts_with("& $") || line.contains("| & $");
            if calls_native && !line.contains("Invoke-Binary") {
                failures.check(&format!("{rel}:{n}"), false, || {
                    format!("invokes a native command outside Invoke-Binary: {line}")
                });
            }
        }

        // Guards against the assertions above passing vacuously on a file that
        // simply stopped calling anything.
        failures.check(
            &format!("{rel}: defines Invoke-Binary"),
            defines_helper,
            || "the helper is gone, so nothing clears $LASTEXITCODE before a check".to_string(),
        );
    }
    failures.assert_empty("stale $LASTEXITCODE");
}

/// `/releases/latest` redirects to the releases *index*, not to a tag, when no
/// stable release exists — so the final path segment is `releases`. Guarding
/// only against the literal `latest` let that through, and the install built a
/// download URL from it and failed on a 404 reported as "Download failed",
/// which names neither the cause nor the fix. Both installers must match a tag
/// shape instead.
#[test]
fn release_resolution_requires_a_tag_shape() {
    let mut failures = Failures::default();

    let sh = read_repo_file("install/install.sh");
    failures.check(
        "install.sh matches a tag shape",
        sh.contains("v[0-9]*)"),
        || "no `v[0-9]*)` case guarding the resolved tag".to_string(),
    );
    failures.check(
        "install.sh dropped the bare `latest` guard",
        !sh.contains(r#"$TAG == "latest""#),
        || "still guards on the literal `latest`, which `releases` walks past".to_string(),
    );

    let ps1 = read_repo_file("install/install.ps1");
    failures.check(
        "install.ps1 matches a tag shape",
        ps1.contains("$tag -notmatch '^v[0-9]'"),
        || "no `-notmatch '^v[0-9]'` guarding the resolved tag".to_string(),
    );
    failures.check(
        "install.ps1 dropped the bare `latest` guard",
        !ps1.contains("$tag -eq 'latest'"),
        || "still guards on the literal `latest`, which `releases` walks past".to_string(),
    );

    // PowerShell 6+ rebuilt Invoke-WebRequest on HttpClient, where BaseResponse
    // is an HttpResponseMessage with no ResponseUri at all — the redirected URI
    // lives on RequestMessage.RequestUri. Reading only the 5.1 spelling aborted
    // every stable install on PowerShell 7.
    failures.check(
        "install.ps1 reads the PowerShell 7 redirect spelling",
        ps1.contains("RequestMessage.RequestUri"),
        || "only reads BaseResponse.ResponseUri, which is absent on PowerShell 7".to_string(),
    );
    failures.assert_empty("release resolution");
}

/// The `irm | iex` exception in `.gitattributes`. A BOM survives `irm` as a
/// stray U+FEFF that breaks `iex` on the first token — fixed once in 762dcc0,
/// then regressed by re-applying the repo's BOM rule mechanically. ASCII-only
/// keeps them safe to run from a local clone too.
#[test]
fn fetched_powershell_installers_are_bomless_ascii() {
    let mut failures = Failures::default();
    for rel in INSTALL_PS1 {
        let bytes = std::fs::read(repo_file(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        failures.check(rel, !bytes.starts_with(&[0xEF, 0xBB, 0xBF]), || {
            "has a UTF-8 BOM, which `iex` chokes on".to_string()
        });
        if let Some(pos) = bytes.iter().position(|b| *b > 127) {
            failures.check(rel, false, || format!("non-ASCII byte at offset {pos}"));
        }
    }
    failures.assert_empty("installer encoding");
}

/// The icon is the one artifact the installers fetch outside the release's
/// SHA256SUMS, so each pins its content hash. The pin has to track the asset
/// and both dialects have to agree — drifting apart silently turns the icon
/// install into a permanent no-op, and a pin that no longer matches the asset
/// is indistinguishable from a tampered download.
#[test]
fn the_pinned_icon_hash_matches_the_asset_in_both_installers() {
    use sha2::Digest;
    let icon = std::fs::read(repo_file("assets/claude-icon.png"))
        .unwrap_or_else(|e| panic!("could not read assets/claude-icon.png: {e}"));
    let actual = format!("{:x}", sha2::Sha256::digest(&icon));

    let mut failures = Failures::default();
    for (rel, marker) in [
        ("install/install.sh", "ICON_SHA256=\""),
        ("install/install.ps1", "$iconSha256 = \""),
    ] {
        let text = read_repo_file(rel);
        let pinned = text
            .split(marker)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string();
        failures.check(rel, pinned == actual, || {
            format!("pins {pinned:?} but assets/claude-icon.png hashes to {actual}")
        });
    }
    failures.assert_empty("pinned icon hash");
}

/// The broad-write gate is only as strong as its principal list, and losing
/// an entry silently reopens the replace-after-verification hole it closes —
/// Authenticated Users is the one it originally shipped without.
#[test]
fn the_acl_gate_names_all_three_broad_principals() {
    let ps = read_repo_file("install/install.ps1");
    let mut failures = Failures::default();
    for sid in ["WorldSid", "BuiltinUsersSid", "AuthenticatedUserSid"] {
        failures.check(sid, ps.contains(sid), || {
            "missing from install.ps1's broad-write ACL check".to_string()
        });
    }
    failures.assert_empty("ACL principal list");
}

/// Staging in a shared world-writable temp reopens exactly the window the
/// staging rules exist to close: another user swapping the file between the
/// checksum passing and the binary being placed.
#[test]
fn downloads_are_staged_in_the_destination_directory() {
    let mut failures = Failures::default();

    let sh = read_repo_file("install/install.sh");
    failures.check("install.sh", sh.contains("STAGE=\"$BIN_DIR/"), || {
        "does not stage inside the install directory".to_string()
    });
    for bad in ["mktemp", "/tmp/", "$TMPDIR"] {
        failures.check("install.sh", !sh.contains(bad), || {
            format!("stages via `{bad}`, outside the destination directory")
        });
    }

    let ps = read_repo_file("install/install.ps1");
    failures.check(
        "install.ps1",
        ps.contains("Join-Path $binDir \"$stagePrefix"),
        || "does not stage inside the install directory".to_string(),
    );
    for bad in ["$env:TEMP", "GetTempPath", "GetTempFileName"] {
        failures.check("install.ps1", !ps.contains(bad), || {
            format!("stages via `{bad}`, outside the destination directory")
        });
    }
    failures.assert_empty("staging location");
}

/// The checksum and attestation fail directions, which are deliberately different from each
/// other and from the runtime guard. The checksum is the fail-closed gate; the
/// attestation is opportunistic but still fails closed when it actually runs.
#[test]
fn verification_is_pinned_and_fails_closed() {
    let mut failures = Failures::default();

    for rel in ["install/install.sh", "install/install.ps1"] {
        let body = read_repo_file(rel);

        // An unpinned attestation check accepts any valid Sigstore bundle from
        // anywhere, which makes it close to decorative.
        failures.check(rel, body.contains("--signer-workflow"), || {
            "attestation verification is not pinned to a signer workflow".to_string()
        });
        failures.check(rel, body.contains("--repo"), || {
            "attestation verification is not pinned to a repository".to_string()
        });
        // Verified against the published bundle, not the attestation API: the
        // API serves it Snappy-compressed and needs an authenticated gh.
        failures.check(rel, body.contains("--bundle"), || {
            "verifies through the attestation API instead of the published bundle".to_string()
        });
        failures.check(rel, body.contains("--require-attestation"), || {
            "offers no way to demand attestation".to_string()
        });
        failures.check(rel, body.contains("checksums.txt"), || {
            "never fetches the checksum file".to_string()
        });
    }
    failures.assert_empty("verification pinning");
}

/// Three ways to choose a release, and the default has to stay
/// the conservative one.
///
/// `/releases/latest` excludes prereleases, which is what keeps a
/// pipeline-verification tag from ever reaching a user who just ran the
/// one-liner. `--pre` opts into the prerelease channel through the atom feed —
/// plain unauthenticated HTTPS, so no token and no shared-IP rate limit — and
/// a pinned version overrides both.
#[test]
fn release_resolution_defaults_to_stable_and_opts_in_to_prereleases() {
    let mut failures = Failures::default();
    for rel in ["install/install.sh", "install/install.ps1"] {
        let body = read_repo_file(rel);

        failures.check(rel, body.contains("releases/latest"), || {
            "does not resolve through /releases/latest, which is what excludes prereleases"
                .to_string()
        });
        failures.check(rel, body.contains("--pre"), || {
            "offers no way to opt into the prerelease channel".to_string()
        });
        failures.check(rel, body.contains("releases.atom"), || {
            "resolves prereleases some way other than the atom feed".to_string()
        });
        failures.check(rel, body.contains("CLAUDE_STATUSLINE_VERSION"), || {
            "offers no pinned-version override".to_string()
        });

        // The API would need a token for anything useful and burns a rate limit
        // shared by everyone behind one IP. Both are why the atom feed is used.
        failures.check(rel, !body.contains("api.github.com"), || {
            "resolves through the GitHub API, which is ruled out".to_string()
        });
    }
    failures.assert_empty("release resolution");
}

/// Every `raw.githubusercontent.com/.../<branch>/<path>` URL in `body`.
///
/// Both branches are published: stable installs come from `master`, and the
/// prerelease commands come from `dev`, which is where the installer matching a
/// prerelease lives. A path is a path either way — if it is not in the tree, the
/// URL 404s whichever branch serves it.
fn published_raw_paths(body: &str) -> Vec<String> {
    const ROOT: &str = "raw.githubusercontent.com/axlaser/claude-statusline/";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(offset) = body[from..].find(ROOT) {
        let after_root = from + offset + ROOT.len();
        // Skip the branch segment; what follows is the repo-relative path.
        let Some(slash) = body[after_root..].find('/') else {
            break;
        };
        let start = after_root + slash + 1;
        let end = body[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ')' | '`' | '>'))
            .map_or(body.len(), |n| start + n);
        out.push(body[start..end].to_string());
        from = end.max(start + 1);
    }
    out
}

/// Every raw URL README hands a user has to resolve to a file that exists.
///
/// This is the only place the published paths are asserted at all, and the
/// failure it guards is silent: `curl -fsSL <404> | bash` prints nothing
/// (`-s`), hands bash an empty stdin, and exits 0. The user sees no error and
/// no install. Moving `install/install.sh` or `assets/claude-icon.png` is all
/// it takes, and nothing else in the suite would notice.
#[test]
fn every_url_the_readme_publishes_resolves_to_a_file() {
    let body = read_repo_file("README.md");
    let paths = published_raw_paths(&body);
    assert!(
        !paths.is_empty(),
        "README publishes no raw URLs, so this test is asserting nothing"
    );

    let mut failures = Failures::default();
    for path in paths {
        failures.check(&path, repo_file(&path).is_file(), || {
            "README publishes this URL but the file is not in the repository".to_string()
        });
    }
    failures.assert_empty("published README URLs");
}

/// Everything irreversible an installer does has to happen after the
/// self-check. A binary can pass its checksum, launch, and still render
/// wrongly, and the silent-degradation contract guarantees that failure reaches
/// the user as an absent status line and nothing else — so deleting the scripts
/// of a working installation before the replacement has proved itself is how an
/// upgrade leaves someone with no status line and no way back.
#[test]
fn the_self_check_gates_every_destructive_step() {
    struct Gate {
        rel: &'static str,
        /// The invocation, not the section comment: the ordering claim is
        /// about where the check actually runs.
        check: &'static str,
        after: &'static [&'static str],
    }

    let gates = [
        Gate {
            rel: "install/install.sh",
            check: "\"$BIN_PATH\" self-check",
            after: &["rm -f \"$CLAUDE_DIR/$_script\"", "settings apply --binary"],
        },
        Gate {
            rel: "install/install.ps1",
            // Routed through `Invoke-Binary` so the gate can tell a binary that
            // rendered wrongly from one that never launched — a raw `& $binPath`
            // leaves `$LASTEXITCODE` holding the previous command's 0 and reads
            // an unlaunchable binary as a pass. See
            // `powershell_installers_never_read_a_stale_exit_code`.
            check: "Invoke-Binary $binPath @('self-check')",
            after: &[
                "Remove-Item $path -Force",
                "'settings', 'apply', '--binary'",
            ],
        },
    ];

    let mut failures = Failures::default();
    for gate in gates {
        let body = read_repo_file(gate.rel);
        let Some(at) = body.find(gate.check) else {
            failures.check(gate.rel, false, || {
                format!("never runs `{}` after placing the binary", gate.check)
            });
            continue;
        };
        for marker in gate.after {
            match body.find(marker) {
                Some(pos) => failures.check(gate.rel, pos > at, || {
                    format!("`{marker}` runs before the self-check has passed")
                }),
                None => failures.check(gate.rel, false, || {
                    format!("`{marker}` is missing, so the gate guards nothing")
                }),
            }
        }
    }
    failures.assert_empty("self-check gating");
}

/// An upgrade from a script installation has to leave nothing
/// orphaned — but `notify-config.json` is the user's configuration, not ours:
/// its schema is unchanged and the binary reads it as-is, so removing it
/// would silently reset everyone's notification preferences.
#[test]
fn a_script_installation_is_removed_but_its_config_is_kept() {
    let cases: [(&str, [&str; 4]); 2] = [
        (
            "install/install.sh",
            [
                "statusline.sh",
                "notify.sh",
                "git-refresh.sh",
                "subagent-statusline.sh",
            ],
        ),
        (
            "install/install.ps1",
            [
                "statusline.ps1",
                "notify.ps1",
                "git-refresh.ps1",
                "subagent-statusline.ps1",
            ],
        ),
    ];

    let mut failures = Failures::default();
    for (rel, scripts) in cases {
        let body = read_repo_file(rel);
        for script in scripts {
            failures.check(rel, body.contains(script), || {
                format!("never removes the superseded `{script}`")
            });
        }
        for (n, line) in code_lines(&body, '#') {
            if !line.contains("notify-config") {
                continue;
            }
            let deletes = line.contains("rm -f") || line.contains("Remove-Item");
            failures.check(&format!("{rel}:{n}"), !deletes, || {
                format!("removes the user's notification config: {line}")
            });
        }
    }
    failures.assert_empty("script migration");
}

// ---------------------------------------------------------------------------
// settings.json merge
// ---------------------------------------------------------------------------
//
// This is the user's file. Everything below is really one property stated four
// ways: the installer owns exactly the entries it wrote, and touching anything
// else — content, ordering, or a hook someone added themselves — is a bug.

const WIN_BINARY: &str = "\"C:\\Users\\a b\\.claude\\bin\\claude-statusline.exe\"";
const UNIX_BINARY: &str = "/home/u/.claude/bin/claude-statusline";

/// A settings file with content the installer must not disturb, including a
/// hook the user registered on the same event the installer writes to.
fn user_settings() -> serde_json::Value {
    serde_json::json!({
        "theme": "dark",
        "model": "opus",
        "hooks": {
            "PostToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "~/my-own-hook.sh" }] }
            ]
        }
    })
}

fn all() -> settings::ApplySpec {
    settings::ApplySpec {
        statusline: true,
        subagent: true,
        git_refresh: true,
        notify: true,
        quote: false,
    }
}

fn all_quoted() -> settings::ApplySpec {
    settings::ApplySpec {
        quote: true,
        ..all()
    }
}

/// An idempotent re-run. A second install must not append a second copy of
/// every hook — the shape that turns a re-run into four notification sounds.
#[test]
fn applying_twice_leaves_one_entry_each() {
    let mut once = user_settings();
    settings::apply(&mut once, UNIX_BINARY, &all());
    let mut twice = once.clone();
    settings::apply(&mut twice, UNIX_BINARY, &all());

    assert_eq!(once, twice, "a second apply changed the file");

    let post = twice["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(
        post.len(),
        2,
        "expected the user's hook plus exactly one of ours, got {post:#?}"
    );
}

/// The installer writes into a file it does not own. A user's own hook on the
/// same event, and every unrelated key, has to come through untouched.
#[test]
fn unrelated_content_and_foreign_hooks_survive() {
    let mut root = user_settings();
    settings::apply(&mut root, UNIX_BINARY, &all());

    let mut failures = Failures::default();
    failures.check("theme", root["theme"] == "dark", || {
        "an unrelated key was lost".to_string()
    });
    failures.check("model", root["model"] == "opus", || {
        "an unrelated key was lost".to_string()
    });

    let post = root["hooks"]["PostToolUse"].as_array().unwrap();
    let kept = post
        .iter()
        .any(|e| e["hooks"][0]["command"] == "~/my-own-hook.sh" && e["matcher"] == "Bash");
    failures.check("foreign-hook", kept, || {
        "the user's own PostToolUse hook was dropped".to_string()
    });
    failures.assert_empty("settings merge");
}

/// The space-in-path shape that once deleted the user's own hooks. Recovering
/// the binary from the composed command split on the first space, and
/// `entry_references` matches on substring, so an unquoted Unix `$HOME` with a
/// space turned the key into `/home/a` — which matched, and removed, every
/// user hook that merely mentioned a path starting there.
#[test]
fn a_space_in_the_unix_path_leaves_foreign_hooks_alone() {
    const SPACED: &str = "/home/a b/.claude/bin/claude-statusline";
    let mut root = serde_json::json!({
        "hooks": {
            "PostToolUse": [
                { "matcher": "Bash", "hooks": [{ "type": "command", "command": "/home/a-tools/backup.sh" }] }
            ]
        }
    });

    settings::apply(&mut root, SPACED, &all());
    let mut twice = root.clone();
    settings::apply(&mut twice, SPACED, &all());
    assert_eq!(
        root, twice,
        "a second apply with a spaced path changed the file"
    );

    let post = root["hooks"]["PostToolUse"].as_array().unwrap();
    assert!(
        post.iter()
            .any(|e| e["hooks"][0]["command"] == "/home/a-tools/backup.sh"),
        "the user's hook sharing the space-split prefix `/home/a` was dropped"
    );
    assert_eq!(
        post.len(),
        2,
        "expected the user's hook plus exactly one of ours, got {post:#?}"
    );
}

/// Uninstall has to leave no trace it can avoid leaving, which means pruning
/// the containers our entries were the only occupants of — but not the ones
/// still holding someone else's hook.
#[test]
fn remove_restores_the_pre_install_file() {
    let before = user_settings();
    let mut root = before.clone();
    settings::apply(&mut root, UNIX_BINARY, &all());
    assert_ne!(
        root, before,
        "apply did nothing, so the test proves nothing"
    );

    settings::remove(&mut root, UNIX_BINARY);
    assert_eq!(
        root, before,
        "uninstall did not restore the file to its pre-install state"
    );
}

/// Windows quoting, driven the way an installer drives it: with the
/// **bare** path.
///
/// Quoting is the binary's job precisely because the caller is a shell and
/// shells eat quotes. PowerShell consumes the surrounding quotes of a pre-quoted
/// argument as delimiters, so an installer that passed `"C:\path\x.exe"` handed
/// the merge a bare path and silently wrote an unquoted command. That shipped
/// once and was only caught by installing from a real release — the earlier
/// version of this test passed a pre-quoted string straight to the function and
/// never crossed the boundary where the bug lived.
#[test]
fn quoting_is_applied_on_this_side_of_the_shell_boundary() {
    let bare = WIN_BINARY.trim_matches('"');

    let mut quoted = serde_json::json!({});
    settings::apply(&mut quoted, bare, &all_quoted());
    let command = quoted["statusLine"]["command"].as_str().unwrap_or_default();
    assert_eq!(
        command, WIN_BINARY,
        "a bare path handed in was not quoted on the way out"
    );
    assert!(
        quoted["subagentStatusLine"]["command"]
            .as_str()
            .unwrap_or_default()
            .starts_with(WIN_BINARY),
        "the subcommand form lost its quoting"
    );

    // Unix entries stay bare, which is what the shell installers always wrote.
    let mut unquoted = serde_json::json!({});
    settings::apply(&mut unquoted, UNIX_BINARY, &all());
    assert_eq!(
        unquoted["statusLine"]["command"]
            .as_str()
            .unwrap_or_default(),
        UNIX_BINARY,
        "a Unix entry was quoted, which the shell installers never did"
    );

    // Quoting must also be idempotent: an already-quoted path stays as it is
    // rather than accumulating a second pair.
    let mut twice = serde_json::json!({});
    settings::apply(&mut twice, WIN_BINARY, &all_quoted());
    assert_eq!(
        twice["statusLine"]["command"].as_str().unwrap_or_default(),
        WIN_BINARY,
        "an already-quoted path was quoted again"
    );
}

/// Whatever form the command was stored in, every query and the removal path
/// have to recognise it — the uninstaller holds a bare path where the installer
/// wrote a quoted one.
#[test]
fn quoted_windows_paths_round_trip() {
    let mut root = serde_json::json!({});
    settings::apply(&mut root, WIN_BINARY, &all());

    let mut failures = Failures::default();
    let command = root["statusLine"]["command"].as_str().unwrap_or_default();
    failures.check(
        "quoted",
        command.starts_with('"') && command.ends_with('"'),
        || format!("statusLine command is not quoted: {command}"),
    );

    for feature in ["statusline", "subagent", "git-refresh", "notify"] {
        failures.check(feature, settings::has(&root, WIN_BINARY, feature), || {
            "written but not detected by has()".to_string()
        });
        // The uninstaller may hold the bare path where the installer wrote a
        // quoted one; both have to match the same entry.
        let bare = WIN_BINARY.trim_matches('"');
        failures.check(feature, settings::has(&root, bare, feature), || {
            "not detected when queried with the unquoted path".to_string()
        });
    }

    settings::remove(&mut root, WIN_BINARY.trim_matches('"'));
    failures.check("removed", root.get("statusLine").is_none(), || {
        "removal by unquoted path left the entry behind".to_string()
    });
    failures.assert_empty("windows quoting");
}

/// The installer prompts before overwriting a `statusLine` it did not write.
/// Detecting "occupied by someone else" is what drives that prompt.
#[test]
fn a_foreign_statusline_is_distinguished_from_ours() {
    let foreign = serde_json::json!({
        "statusLine": { "type": "command", "command": "~/some-other-tool.sh" }
    });
    assert!(
        settings::has_foreign(&foreign, UNIX_BINARY, "statusline"),
        "another tool's statusLine was not recognised as foreign"
    );
    assert!(
        !settings::has(&foreign, UNIX_BINARY, "statusline"),
        "another tool's statusLine was mistaken for ours"
    );

    let mut ours = serde_json::json!({});
    settings::apply(&mut ours, UNIX_BINARY, &all());
    assert!(
        !settings::has_foreign(&ours, UNIX_BINARY, "statusline"),
        "our own entry was reported as foreign, which would prompt on every re-run"
    );
}

/// `settings.json` exactly as the script installers left it, including a hook
/// of the user's own on an event the binary also writes to, and one under
/// `~/.claude/hooks/` whose filename collides with a superseded script.
fn script_install_settings() -> serde_json::Value {
    serde_json::json!({
        "theme": "dark",
        "statusLine": {
            "type": "command",
            "command": "~/.claude/statusline.sh",
            "refreshInterval": 1
        },
        "subagentStatusLine": {
            "type": "command",
            "command": "~/.claude/subagent-statusline.sh"
        },
        "hooks": {
            "PostToolUse": [
                { "matcher": "Edit|Write|MultiEdit|Bash|NotebookEdit",
                  "hooks": [{ "type": "command", "command": "~/.claude/git-refresh.sh", "async": true }] },
                { "matcher": "Bash",
                  "hooks": [{ "type": "command", "command": "~/my-own-hook.sh" }] }
            ],
            "PermissionRequest": [
                { "hooks": [{ "type": "command", "command": "~/.claude/notify.sh permission", "async": true }] }
            ],
            "Stop": [
                { "hooks": [{ "type": "command", "command": "~/.claude/notify.sh stop", "async": true }] },
                { "hooks": [{ "type": "command", "command": "~/.claude/hooks/notify.sh" }] }
            ],
            "PreCompact": [
                { "matcher": "*",
                  "hooks": [{ "type": "command", "command": "~/.claude/notify.sh compaction_start", "async": true }] }
            ],
            "PostCompact": [
                { "matcher": "*",
                  "hooks": [{ "type": "command", "command": "~/.claude/notify.sh compaction_done", "async": true }] }
            ]
        }
    })
}

/// Every `command` string anywhere in the document.
fn commands(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if key == "command" {
                    if let Some(text) = child.as_str() {
                        out.push(text.to_string());
                    }
                }
                commands(child, out);
            }
        }
        serde_json::Value::Array(list) => {
            for child in list {
                commands(child, out);
            }
        }
        _ => {}
    }
}

/// The upgrade's whole point: after it, no entry points at a file the
/// same run deleted. Rewriting rather than appending is what distinguishes this
/// from a fresh install over the top — the latter leaves both entries, and
/// Claude Code then runs a script that is gone.
#[test]
fn a_script_installation_is_rewritten_not_left_beside_ours() {
    let mut root = script_install_settings();
    settings::apply(&mut root, UNIX_BINARY, &all());

    let mut found = Vec::new();
    commands(&root, &mut found);

    let mut failures = Failures::default();
    for command in &found {
        failures.check(
            "orphan",
            !settings::references_legacy_script(command),
            || format!("still points at a deleted script: {command}"),
        );
    }

    for (key, expected) in [
        ("statusLine", UNIX_BINARY.to_string()),
        ("subagentStatusLine", format!("{UNIX_BINARY} subagent")),
    ] {
        let actual = root[key]["command"].as_str().unwrap_or_default();
        failures.check(key, actual == expected, || {
            format!("expected `{expected}`, got `{actual}`")
        });
    }

    // One of ours per event, not one of ours beside one of theirs.
    for (event, expected) in [
        ("PostToolUse", 2),
        ("PermissionRequest", 1),
        ("Stop", 2),
        ("PreCompact", 1),
        ("PostCompact", 1),
    ] {
        let count = root["hooks"][event].as_array().map_or(0, Vec::len);
        failures.check(event, count == expected, || {
            format!(
                "expected {expected} entries, got {count}: {:#?}",
                root["hooks"][event]
            )
        });
    }

    failures.check(
        "user-hook",
        found.iter().any(|c| c == "~/my-own-hook.sh"),
        || "the user's own PostToolUse hook was pruned as legacy".to_string(),
    );
    // The one that would be a real user's real loss: a hook they keep in
    // `~/.claude/hooks/` that happens to share a name with a script we remove.
    failures.check(
        "namesake-hook",
        found.iter().any(|c| c == "~/.claude/hooks/notify.sh"),
        || "a user hook under ~/.claude/hooks/ was pruned by a basename match".to_string(),
    );
    failures.check("unrelated-key", root["theme"] == "dark", || {
        "an unrelated key was lost".to_string()
    });
    failures.assert_empty("script-install migration");
}

/// A script installation is this tool's own previous entry, not a
/// stranger's. Prompting "existing config found, overwrite?" for it asks the
/// user to approve replacing us with us — and a declined prompt leaves
/// `settings.json` pointing at a script the same run is about to delete.
#[test]
fn a_script_entry_is_recognised_as_ours_not_as_foreign() {
    let legacy = script_install_settings();
    let mut failures = Failures::default();

    for feature in ["statusline", "subagent"] {
        failures.check(
            feature,
            !settings::has_foreign(&legacy, UNIX_BINARY, feature),
            || "a script installation was treated as another tool's config".to_string(),
        );
    }
    for feature in ["statusline", "subagent", "git-refresh", "notify"] {
        failures.check(
            feature,
            settings::has_legacy(&legacy, Some(feature)),
            || {
                "a script installation went undetected, so its setting is lost on upgrade"
                    .to_string()
            },
        );
    }
    failures.check("any", settings::has_legacy(&legacy, None), || {
        "the unscoped query missed a script installation".to_string()
    });

    // A genuinely foreign entry still has to prompt, and a decoy that merely
    // looks like ours must not be mistaken for it.
    let stranger = serde_json::json!({
        "statusLine": { "type": "command", "command": "~/.claude/my-statusline.sh" }
    });
    failures.check(
        "decoy-foreign",
        settings::has_foreign(&stranger, UNIX_BINARY, "statusline"),
        || "another tool's status line stopped prompting".to_string(),
    );
    failures.check(
        "decoy-legacy",
        !settings::has_legacy(&stranger, None),
        || "`my-statusline.sh` was matched as `statusline.sh`".to_string(),
    );
    failures.assert_empty("legacy detection");
}

/// A settings file that exists but does not parse must stop the install, never
/// be silently replaced with an empty object — that would discard everything
/// the user had configured and report success.
#[test]
fn unparseable_settings_is_an_error_not_a_fresh_start() {
    let dir = scratch_dir("settings-malformed");
    let path = dir.join("settings.json");
    std::fs::write(&path, b"{ this is not json").unwrap();
    assert!(
        settings::load(&path).is_err(),
        "a corrupt settings.json parsed as an empty object"
    );

    let missing = dir.join("absent.json");
    assert!(
        settings::load(&missing).is_ok_and(|v| v.as_object().is_some_and(|m| m.is_empty())),
        "an absent settings.json should read as an empty object"
    );
}

/// The whole file is rewritten on every apply, so key order is a property of
/// the writer. Re-sorting the user's keys would make the installer's own verification —
/// "settings.json diffs show only intended entries" — impossible to perform.
#[test]
fn existing_key_order_is_preserved() {
    let dir = scratch_dir("settings-order");
    let path = dir.join("settings.json");
    std::fs::write(&path, br#"{"theme":"dark","model":"opus","zzz":1,"aaa":2}"#).unwrap();

    let mut root = settings::load(&path).unwrap();
    settings::apply(&mut root, UNIX_BINARY, &all());
    settings::save(&path, &root).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let order: Vec<&str> = ["theme", "model", "zzz", "aaa"]
        .into_iter()
        .filter(|k| text.contains(&format!("\"{k}\"")))
        .collect();
    assert_eq!(
        order,
        vec!["theme", "model", "zzz", "aaa"],
        "keys were re-sorted; the install diff would show the entire file"
    );
    let theme_at = text.find("\"theme\"").unwrap();
    let aaa_at = text.find("\"aaa\"").unwrap();
    assert!(
        theme_at < aaa_at,
        "alphabetical re-sorting detected in the written file"
    );
}

/// An upgrade rewrites `type` and `command` and nothing else in those entries.
///
/// The whole-object replacement this replaced reset a tuned `refreshInterval`
/// to the default and deleted `padding` outright, on every single upgrade,
/// with nothing said about it — and README documents both as things to tune,
/// so the settings most likely to be present were the ones most likely to be
/// lost. The default is only a default: it is written when the key is absent
/// and never over a value the user chose.
#[test]
fn an_upgrade_keeps_the_users_own_status_line_keys() {
    let mut root = serde_json::json!({
        "statusLine": {
            "type": "command",
            "command": "~/.claude/bin/claude-statusline",
            "refreshInterval": 7,
            "padding": 2
        },
        "subagentStatusLine": {
            "type": "command",
            "command": "~/.claude/bin/claude-statusline subagent",
            "padding": 3
        }
    });

    settings::apply(&mut root, UNIX_BINARY, &all());

    let line = &root["statusLine"];
    assert_eq!(
        line["refreshInterval"], 7,
        "a tuned refreshInterval was reset to the default by an upgrade"
    );
    assert_eq!(
        line["padding"], 2,
        "the user's padding was deleted by an upgrade"
    );
    assert_eq!(
        line["command"], UNIX_BINARY,
        "the command must still be repointed at the new binary"
    );
    assert_eq!(
        root["subagentStatusLine"]["padding"], 3,
        "the subagent entry drops the user's keys too"
    );

    // A first install still gets the default, which is the other half of the
    // contract: absent means write it, present means leave it.
    let mut fresh = serde_json::json!({});
    settings::apply(&mut fresh, UNIX_BINARY, &all());
    assert_eq!(
        fresh["statusLine"]["refreshInterval"],
        settings::REFRESH_INTERVAL,
        "a fresh install should carry the default cadence"
    );
}

// ---------------------------------------------------------------------------
// Fixture and harness contract
// ---------------------------------------------------------------------------
//
// Fixtures are the only thing the ported components will be checked against, so
// a fixture that cannot be regenerated is not evidence — it is an unfalsifiable
// claim. These cases assert that every captured fixture carries what it needs to be
// regenerated, and that the case table the harness drives stays consistent with
// the state matrix it references.

/// Minimal object reader. Pulling in a YAML or full JSON dependency for four
/// tests would put a build-time cost on every `cargo test` for the crate's
/// entire life; these files are generated by the harness to a fixed shape.
fn json_string_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let start = body.find(&needle)? + needle.len();
    let rest = body[start..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        Some(&quoted[..quoted.find('"')?])
    } else {
        let end = rest.find([',', '\n', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

fn fixture_case_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.file_name().is_some_and(|n| n == "case.json") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&repo_file("tests/fixtures"), &mut found);
    found.sort();
    found
}

/// A fixture missing any of these cannot be regenerated: without
/// the source commit there is no way to re-run the scripts that produced it,
/// and without the pinned clock and config input the re-run is a different
/// experiment.
#[test]
fn every_fixture_records_what_it_takes_to_regenerate_it() {
    let mut failures = Failures::default();
    let files = fixture_case_files();

    for path in &files {
        let name = path
            .parent()
            .and_then(|p| p.strip_prefix(repo_file("tests/fixtures")).ok())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let body = std::fs::read_to_string(path).expect("could not read a fixture");

        for key in ["source_commit", "clock", "notify_config", "observable"] {
            let value = json_string_field(&body, key).unwrap_or("");
            failures.check(&name, !value.is_empty() && value != "null", || {
                format!("`{key}` is missing or empty")
            });
        }

        let commit = json_string_field(&body, "source_commit").unwrap_or("");
        failures.check(&name, commit.len() == 40, || {
            format!("`source_commit` is `{commit}`, not a full commit sha")
        });

        // CLAUDE.md forbids committing a personal absolute path, and a fixture
        // is a committed file. The harness scrubs and then refuses; this is the
        // check that survives if someone hand-edits one.
        for leak in ["/Users/", "/home/", "C:\\Users\\", "C:/Users/"] {
            failures.check(&name, !body.contains(leak), || {
                format!("contains a machine-local path (`{leak}`)")
            });
        }
    }

    // Every component's fixtures have landed, so an empty set here no longer
    // means "not ported yet" -- it means discovery broke. Assert the count
    // rather than only reporting it, or a vacuous pass reads as coverage.
    println!("checked {} fixture(s)", files.len());
    assert!(!files.is_empty(), "no fixture metadata files were checked");
    failures.assert_empty("fixture metadata");
}

/// The two harness drivers read the same case table, so a case naming a git
/// state that `states.json` does not define fails on whichever platform runs
/// first — after doing all the setup work.
#[test]
fn every_case_references_a_defined_git_state() {
    let cases = read_repo_file("tests/harness/cases.json");
    let states = read_repo_file("tests/harness/states.json");

    let defined: Vec<&str> = states
        .lines()
        .filter_map(|l| l.trim().strip_prefix("\"name\": \""))
        .filter_map(|l| l.split('"').next())
        .collect();
    assert!(
        defined.len() >= 10,
        "expected the full §4 git-state matrix, found {}: {defined:?}",
        defined.len()
    );

    let mut failures = Failures::default();
    for (n, line) in cases.lines().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("\"git_state\": ") else {
            continue;
        };
        let value = rest.trim_end_matches(',');
        if value == "null" {
            continue;
        }
        let state = value.trim_matches('"');
        failures.check(&format!("line {}", n + 1), defined.contains(&state), || {
            format!("references undefined git state `{state}`")
        });
    }
    failures.assert_empty("case table git states");
}

/// §4's matrix is the reason the harness exists. A state quietly dropped from
/// `states.json` would shrink coverage without failing anything.
#[test]
fn the_git_state_matrix_covers_every_documented_state() {
    let states = read_repo_file("tests/harness/states.json");
    let mut failures = Failures::default();

    for state in [
        "unborn-head",
        "clean",
        "untracked-only",
        "dirty",
        "stash-present",
        "stash-cleared",
        "ahead-of-upstream",
        "no-upstream",
        "detached-head",
        "collapsed-untracked-dir",
    ] {
        failures.check(
            state,
            states.contains(&format!("\"name\": \"{state}\"")),
            || "documented in docs/performance.md §4 but absent from the matrix".to_string(),
        );
    }
    failures.assert_empty("§4 git-state coverage");
}

/// The shims the harness intercepts through. Both drivers install
/// them by name, so a renamed or deleted shim body turns every notification
/// fixture into a silent empty capture.
#[test]
fn the_harness_ships_every_shim_it_installs() {
    let mut failures = Failures::default();
    for shim in ["record.sh", "record.cmd", "record.ps1"] {
        let path = repo_file("tests/harness/shims").join(shim);
        failures.check(shim, path.is_file(), || "shim body is missing".to_string());
    }

    let sh = read_repo_file("tests/harness/capture.sh");
    let ps = read_repo_file("tests/harness/capture.ps1");
    for name in ["afplay", "paplay", "terminal-notifier", "notify-send"] {
        failures.check(name, sh.contains(name), || {
            "named here but not installed by capture.sh".to_string()
        });
    }
    failures.check("powershell.cmd", ps.contains("powershell.cmd"), || {
        "the Windows PATH shim is not installed by capture.ps1".to_string()
    });
    failures.assert_empty("harness shims");
}

/// The message closure must not be evaluated when logging is off — this is the
/// Rust equivalent of the PowerShell rule that arguments evaluate before the
/// callee's guard.
#[test]
fn debug_log_does_not_evaluate_its_message_when_disabled() {
    let dir = scratch_dir("debug-lazy");
    let log = dir.join("statusline-debug.log");
    let mut evaluated = false;

    debug::log_to(&log, false, || {
        evaluated = true;
        String::new()
    });

    assert!(
        !evaluated,
        "the message closure ran even though logging was disabled"
    );
}

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

/// Reads a pinned payload from `tests/harness/payloads/`, resolving the two
/// placeholders the capture harness substitutes. The same files feed the
/// fixture captures, so a payload that drifts breaks both at once.
fn payload_fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("harness")
        .join("payloads")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
        .replace("{REPO}", "/scratch/repo")
        .replace("{HOME}", "/scratch/home")
}

fn shown<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

/// One name-to-value lookup so the tables below read as the parity block does,
/// rather than as twenty separate assertions.
fn field(p: &Payload, name: &str) -> String {
    match name {
        "session_id" => p.session_id().to_string(),
        "cwd" => p.cwd().to_string(),
        "git_cwd" => p.git_cwd().to_string(),
        "model_display_name" => p.model_display_name().to_string(),
        "model_id" => p.model_id().to_string(),
        "context_window_size" => shown(p.context_window_size()),
        "used_percentage" => shown(p.used_percentage()),
        "total_input_tokens" => shown(p.total_input_tokens()),
        "effort_level" => p.effort_level().to_string(),
        "total_cost_usd" => shown(p.total_cost_usd()),
        "duration_ms" => shown(p.duration_ms()),
        "transcript_path" => p.transcript_path().to_string(),
        "rate_five_hour_percentage" => shown(p.rate_five_hour_percentage()),
        "rate_five_hour_resets_at" => p.rate_five_hour_resets_at().to_string(),
        "rate_seven_day_percentage" => shown(p.rate_seven_day_percentage()),
        "rate_seven_day_resets_at" => p.rate_seven_day_resets_at().to_string(),
        "agent_name" => p.agent_name().to_string(),
        "agent_input_tokens" => p.agent_input_tokens().to_string(),
        "agent_output_tokens" => p.agent_output_tokens().to_string(),
        other => panic!("no accessor named {other} — the table and the model disagree"),
    }
}

/// Every field the parity block extracts, with the value `full.json` carries.
/// The three duplicated spellings — the second `workspace.current_dir` read,
/// and the legacy cost and duration keys — are covered by `git_cwd` and by the
/// fallback tests below rather than by separate rows.
const FULL_PAYLOAD_FIELDS: &[(&str, &str)] = &[
    ("session_id", "fixture-session-0001"),
    ("cwd", "/scratch/repo"),
    ("git_cwd", "/scratch/repo"),
    ("model_display_name", "Opus 5"),
    ("model_id", "claude-opus-5"),
    ("context_window_size", "200000"),
    ("used_percentage", "42.5"),
    ("total_input_tokens", "85000"),
    ("effort_level", "high"),
    ("total_cost_usd", "1.2345"),
    ("duration_ms", "654321"),
    (
        "transcript_path",
        "/scratch/home/.claude/projects/fixtures/transcript.jsonl",
    ),
    ("rate_five_hour_percentage", "31"),
    ("rate_five_hour_resets_at", "2026-01-01T05:00:00Z"),
    ("rate_seven_day_percentage", "12"),
    ("rate_seven_day_resets_at", "2026-01-05T00:00:00Z"),
    ("agent_name", "main"),
    ("agent_input_tokens", "84000"),
    ("agent_output_tokens", "1000"),
];

#[test]
fn full_payload_reads_every_documented_field() {
    let raw = payload_fixture("full.json");
    let p = Payload::parse(&raw).expect("the full fixture is a JSON object");

    let mut failures = Failures::default();
    for (name, want) in FULL_PAYLOAD_FIELDS {
        let got = field(&p, name);
        failures.check(name, got == *want, || format!("want {want:?}, got {got:?}"));
    }
    failures.assert_empty("full-payload extraction");
}

/// A payload carrying only `session_id` and the workspace directory. Absent is
/// not an error anywhere: every other field reads as its fallback, and the two
/// token counts read as 0 rather than as absent, which is what both scripts pin
/// them to.
#[test]
fn minimal_payload_falls_back_without_failing() {
    let raw = payload_fixture("minimal.json");
    let p = Payload::parse(&raw).expect("the minimal fixture is a JSON object");

    let expected: &[(&str, &str)] = &[
        ("session_id", "fixture-session-0001"),
        ("cwd", "/scratch/repo"),
        ("git_cwd", "/scratch/repo"),
        ("model_display_name", ""),
        ("model_id", ""),
        ("context_window_size", ""),
        ("used_percentage", ""),
        ("total_input_tokens", ""),
        ("effort_level", ""),
        ("total_cost_usd", ""),
        ("duration_ms", ""),
        ("transcript_path", ""),
        ("rate_five_hour_percentage", ""),
        ("rate_five_hour_resets_at", ""),
        ("agent_name", ""),
        ("agent_input_tokens", "0"),
        ("agent_output_tokens", "0"),
    ];

    let mut failures = Failures::default();
    for (name, want) in expected {
        let got = field(&p, name);
        failures.check(name, got == *want, || format!("want {want:?}, got {got:?}"));
    }
    failures.assert_empty("minimal-payload fallback");
}

/// One field carrying the wrong JSON type costs exactly its own row. The
/// whole reason the payload is read as a generic value: a derived model
/// would reject the document and blank the entire status line.
#[test]
fn one_wrong_typed_field_degrades_only_its_own_row() {
    let raw = payload_fixture("full.json")
        .replace("\"used_percentage\": 42.5", "\"used_percentage\": {}");
    let p = Payload::parse(&raw).expect("a wrong-typed field must not fail the document");

    assert_eq!(
        p.used_percentage(),
        None,
        "an object where a number belongs must read as absent"
    );

    let mut failures = Failures::default();
    for (name, want) in FULL_PAYLOAD_FIELDS {
        if *name == "used_percentage" {
            continue;
        }
        let got = field(&p, name);
        failures.check(name, got == *want, || {
            format!("collateral damage: want {want:?}, got {got:?}")
        });
    }
    failures.assert_empty("single-row degradation");
}

/// The scripts read the payload as 23 newline-separated rows, so a field whose
/// *value* looks like more payload is the classic way to shift every field
/// after it. Structured parsing cannot be fooled that way, and this pins it.
#[test]
fn decoy_field_names_inside_values_do_not_shift_extraction() {
    let raw = r#"{
      "session_id": "real-session",
      "model": { "display_name": "\"session_id\": \"decoy\", \"model\": {" },
      "effort": { "level": "high" }
    }"#;
    let p = Payload::parse(raw).expect("decoy payload is a JSON object");

    assert_eq!(p.session_id(), "real-session");
    assert_eq!(p.effort_level(), "high");
    assert_eq!(
        p.model_display_name(),
        "\"session_id\": \"decoy\", \"model\": {"
    );
}

/// A newline inside a string value shifts every later field in the bash
/// scripts: `jq -r` prints it literally and `mapfile` splits on it, so
/// `_jf[4]` onward move by one. Rust reads fields by name and cannot shift.
/// A deliberate divergence in an exotic case, recorded rather than reproduced —
/// the bash behaviour is a bug, and no fixture exercises it.
#[test]
fn a_newline_inside_a_value_does_not_shift_later_fields() {
    let raw = r#"{
      "session_id": "line-one\nline-two",
      "model": { "display_name": "Opus 5" },
      "context_window": { "context_window_size": 200000 }
    }"#;
    let p = Payload::parse(raw).expect("multi-line value payload is a JSON object");

    assert_eq!(p.session_id(), "line-one\nline-two");
    assert_eq!(p.model_display_name(), "Opus 5");
    assert_eq!(p.context_window_size(), Some(200_000));
}

/// Astral-plane characters survive the round trip. The token-extraction
/// incident behind this fixture is the transcript scan's, but the payload has to carry the
/// characters intact before the transcript scan can mishandle them.
#[test]
fn astral_plane_characters_survive_the_round_trip() {
    let raw = payload_fixture("astral.json");
    let p = Payload::parse(&raw).expect("the astral fixture is a JSON object");

    assert_eq!(p.model_display_name(), "Opus 5 🚀");
    assert_eq!(p.agent_name(), "𝕬gent 🧪");
    assert_eq!(
        sanitize_display(p.agent_name()),
        "𝕬gent 🧪",
        "the render scrub must not damage characters outside the BMP"
    );
}

/// Paths are read, never validated. A payload from a machine whose paths this
/// host could not create still has to parse, because the binary that reads it
/// may be running on a different platform than the one that wrote the session.
#[test]
fn hostile_and_overlong_paths_read_without_error() {
    let illegal = r#"/tmp/a<b>c:d"e|f?g*h/transcript.jsonl"#;
    let overlong = format!("/tmp/{}/transcript.jsonl", "d".repeat(300));
    // The quote is part of the hostile input, so it has to reach the parser as
    // a JSON escape rather than as a string terminator.
    let escaped = illegal.replace('"', "\\\"");
    let raw = format!(
        r#"{{ "session_id": "s", "transcript_path": "{escaped}", "workspace": {{ "current_dir": "{overlong}" }} }}"#
    );
    let p = Payload::parse(&raw).expect("hostile paths must not fail the parse");

    assert_eq!(
        p.transcript_path(),
        illegal,
        "the transcript path is opened, not rendered, so it is never scrubbed — \
         including the pipe, which the display scrub would have replaced"
    );
    assert_eq!(p.cwd(), overlong);
}

/// The render sink strips anything that could move the cursor, colour the
/// line, or forge a column separator.
#[test]
fn display_scrub_removes_escape_and_control_bytes() {
    let cases: &[(&str, &str, &str)] = &[
        ("esc-sequence", "\u{1b}[31mred\u{1b}[0m", "[31mred [0m"),
        ("bare-esc", "a\u{1b}b", "a b"),
        ("c0-bell-and-soh", "a\u{7}b\u{1}c", "a b c"),
        ("del", "a\u{7f}b", "a b"),
        ("nul", "a\u{0}b", "a b"),
        ("carriage-return", "a\rb", "a b"),
        ("pipe-forges-a-separator", "main|fake", "main fake"),
        ("trims-to-empty", "\u{1b}\u{1}\u{7f}", ""),
        ("leading-and-trailing", "  branch  ", "branch"),
        ("interior-spaces-kept", "a  b", "a  b"),
        (
            "clean-value-untouched",
            "dev-rust-migration",
            "dev-rust-migration",
        ),
    ];

    let mut failures = Failures::default();
    for (name, input, want) in cases {
        let got = sanitize_display(input);
        failures.check(name, got == *want, || format!("want {want:?}, got {got:?}"));
    }
    failures.assert_empty("display scrub");
}

/// The fatal cases: the two inputs that make the scripts print
/// `[statusline: bad JSON]` instead of a status line.
#[test]
fn parse_rejects_exactly_what_the_scripts_reject() {
    let malformed = payload_fixture("malformed.json");
    let cases: &[(&str, &str, bool)] = &[
        ("empty", "", false),
        ("whitespace-only", "   \n  ", false),
        ("malformed-fixture", &malformed, false),
        ("json-array", "[1, 2, 3]", false),
        ("json-string", "\"just a string\"", false),
        ("json-number", "42", false),
        ("json-null", "null", false),
        ("json-true", "true", false),
        ("trailing-garbage", "{} trailing", false),
        ("empty-object", "{}", true),
        ("object", "{\"session_id\": \"s\"}", true),
    ];

    let mut failures = Failures::default();
    for (name, raw, want_ok) in cases {
        let got = Payload::parse(raw).is_some();
        failures.check(name, got == *want_ok, || {
            format!("want parse ok = {want_ok}, got {got}")
        });
    }
    failures.assert_empty("parse acceptance");
}

/// The tolerant helpers, at the type boundaries that decide whether a row
/// renders. Numeric strings are accepted because jq hands bash every field as
/// text and bash re-parses it, so quoting a number has never changed the
/// rendered line.
#[test]
fn tolerant_reads_match_the_scripts_accepted_types() {
    let raw = r#"{
      "session_id": "s",
      "quoted_int": "200000",
      "quoted_float": "42.5",
      "integral_float": 200000.0,
      "negative": -5,
      "signed_string": "-5",
      "spaced_string": " 42 ",
      "flag_false": false,
      "flag_true": true,
      "as_object": {},
      "as_array": [],
      "as_null": null,
      "empty_string": ""
    }"#;
    let p = Payload::parse(raw).expect("type-matrix payload is a JSON object");

    let mut f = Failures::default();
    let mut check = |name: &str, ok: bool, detail: String| f.check(name, ok, || detail);

    check(
        "quoted-int-as-uint",
        p.uint(&["quoted_int"]) == Some(200_000),
        format!("{:?}", p.uint(&["quoted_int"])),
    );
    check(
        "quoted-float-as-number",
        p.number(&["quoted_float"]) == Some(42.5),
        format!("{:?}", p.number(&["quoted_float"])),
    );
    check(
        "integral-float-as-uint",
        p.uint(&["integral_float"]) == Some(200_000),
        format!("{:?}", p.uint(&["integral_float"])),
    );
    check(
        "negative-rejected-by-uint",
        p.uint(&["negative"]).is_none(),
        format!("{:?}", p.uint(&["negative"])),
    );
    check(
        "negative-accepted-by-number",
        p.number(&["negative"]) == Some(-5.0),
        format!("{:?}", p.number(&["negative"])),
    );
    check(
        "signed-string-rejected-by-uint",
        p.uint(&["signed_string"]).is_none(),
        format!("{:?}", p.uint(&["signed_string"])),
    );
    check(
        "spaced-string-rejected-by-uint",
        p.uint(&["spaced_string"]).is_none(),
        format!("{:?}", p.uint(&["spaced_string"])),
    );
    // jq's `//` returns its right-hand side for `false` as well as null, so a
    // false-valued field has always read as absent. Reproduced, not fixed.
    check(
        "false-reads-as-absent",
        p.number(&["flag_false"]).is_none() && p.text(&["flag_false"]).is_empty(),
        "false leaked through".to_string(),
    );
    check(
        "true-is-not-a-string",
        p.text(&["flag_true"]).is_empty(),
        format!("{:?}", p.text(&["flag_true"])),
    );
    for name in ["as_object", "as_array", "as_null", "empty_string"] {
        check(
            name,
            p.text(&[name]).is_empty() && p.number(&[name]).is_none(),
            format!("{name} did not read as absent"),
        );
    }
    check(
        "absent-path",
        p.text(&["nope", "deeper"]).is_empty() && p.uint(&["nope"]).is_none(),
        "a missing path must not panic or invent a value".to_string(),
    );
    check(
        "descend-through-a-scalar",
        p.text(&["session_id", "deeper"]).is_empty(),
        "walking into a string must read as absent".to_string(),
    );

    f.assert_empty("tolerant type handling");
}

/// The fallback chains, which live in the model rather than at each call site
/// because both scripts spell them out identically and a second copy would
/// eventually disagree.
#[test]
fn legacy_field_spellings_fall_back_in_the_scripts_order() {
    let legacy = r#"{
      "session_id": "s",
      "cwd": "/fallback/dir",
      "total_cost_usd": 9.99,
      "duration_ms": 1234
    }"#;
    let p = Payload::parse(legacy).expect("legacy payload is a JSON object");

    assert_eq!(
        p.cwd(),
        "/fallback/dir",
        "cwd falls back to the top-level key"
    );
    assert_eq!(
        p.git_cwd(),
        "",
        "the git row reads workspace.current_dir only — it has no cwd fallback"
    );
    assert_eq!(p.total_cost_usd(), Some(9.99));
    assert_eq!(p.duration_ms(), Some(1234.0));

    let preferred = r#"{
      "session_id": "s",
      "cwd": "/fallback/dir",
      "workspace": { "current_dir": "/preferred/dir" },
      "cost": { "total_cost_usd": 1.0, "total_duration_ms": 10 },
      "total_cost_usd": 9.99,
      "total_duration_ms": 20,
      "duration_ms": 30
    }"#;
    let p = Payload::parse(preferred).expect("preferred payload is a JSON object");

    assert_eq!(p.cwd(), "/preferred/dir");
    assert_eq!(p.git_cwd(), "/preferred/dir");
    assert_eq!(p.total_cost_usd(), Some(1.0));
    assert_eq!(p.duration_ms(), Some(10.0));
}

// ---------------------------------------------------------------------------
// Transcript scan
// ---------------------------------------------------------------------------

fn transcript_input(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("harness")
        .join("inputs")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// One scan case. Lines are joined with a trailing newline so the table reads
/// as a transcript rather than as an escaped blob.
struct ScanCase {
    name: &'static str,
    lines: &'static [&'static str],
    init_idle: bool,
    want: Scan,
}

/// The counting and voting rules, case by case. Every filter here exists
/// because its absence produced a visible bug: without the synthetic-entry
/// filters the idle detector sticks on "working" after any slash command, and
/// without the `"assistant"` guard a user entry quoting token counts would
/// inflate the totals.
#[test]
fn scan_counts_and_votes_the_way_the_scripts_do() {
    let cases = &[
        ScanCase {
            name: "user-then-assistant-is-working",
            lines: &[
                r#"{"type":"user","message":{"content":"go"}}"#,
                r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"cache_creation_input_tokens":2,"cache_read_input_tokens":30,"output_tokens":5}}}"#,
            ],
            init_idle: true,
            want: Scan {
                messages: 1,
                input_tokens: 10,
                cache_write_tokens: 2,
                cache_read_tokens: 30,
                output_tokens: 5,
                idle: false,
                ..Default::default()
            },
        },
        ScanCase {
            name: "end-turn-is-idle",
            lines: &[
                r#"{"type":"assistant","stop_reason":"end_turn","message":{"usage":{"output_tokens":5}}}"#,
            ],
            init_idle: false,
            want: Scan {
                output_tokens: 5,
                idle: true,
                ..Default::default()
            },
        },
        ScanCase {
            name: "interrupted-user-is-idle",
            lines: &[r#"{"type":"user","message":{"content":"Request interrupted by user"}}"#],
            init_idle: false,
            want: Scan {
                messages: 1,
                idle: true,
                ..Default::default()
            },
        },
        ScanCase {
            name: "meta-entry-neither-counts-nor-votes",
            lines: &[
                r#"{"type":"assistant","stop_reason":"end_turn","message":{"usage":{"output_tokens":1}}}"#,
                r#"{"type":"user","isMeta":true,"message":{"content":"x"}}"#,
            ],
            init_idle: false,
            want: Scan {
                output_tokens: 1,
                idle: true,
                ..Default::default()
            },
        },
        ScanCase {
            name: "slash-command-does-not-stick-on-working",
            lines: &[
                r#"{"type":"assistant","stop_reason":"end_turn","message":{}}"#,
                r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#,
            ],
            init_idle: false,
            want: Scan {
                idle: true,
                ..Default::default()
            },
        },
        ScanCase {
            name: "local-command-stdout-neither-counts-nor-votes",
            lines: &[
                r#"{"type":"user","message":{"content":"<local-command-stdout>ok</local-command-stdout>"}}"#,
            ],
            init_idle: true,
            want: Scan {
                idle: true,
                ..Default::default()
            },
        },
        ScanCase {
            name: "tool-result-neither-counts-nor-votes",
            lines: &[r#"{"type":"user","toolUseResult":{"ok":true}}"#],
            init_idle: true,
            want: Scan {
                idle: true,
                ..Default::default()
            },
        },
        ScanCase {
            name: "tokens-come-only-from-assistant-entries",
            lines: &[
                r#"{"type":"user","message":{"content":"quoting \"input_tokens\": 999 back at you"}}"#,
            ],
            init_idle: false,
            want: Scan {
                messages: 1,
                idle: false,
                ..Default::default()
            },
        },
        ScanCase {
            name: "decoy-key-without-a-colon-does-not-clobber",
            lines: &[
                r#"{"type":"assistant","message":{"usage":{"input_tokens":7},"text":"the \"input_tokens\" field"}}"#,
            ],
            init_idle: false,
            want: Scan {
                input_tokens: 7,
                idle: false,
                ..Default::default()
            },
        },
        ScanCase {
            name: "last-colon-bearing-occurrence-wins",
            lines: &[
                r#"{"type":"assistant","message":{"usage":{"input_tokens":1},"retry":{"input_tokens":9}}}"#,
            ],
            init_idle: false,
            want: Scan {
                input_tokens: 9,
                idle: false,
                ..Default::default()
            },
        },
        ScanCase {
            name: "non-numeric-value-reads-zero",
            lines: &[
                r#"{"type":"assistant","message":{"usage":{"input_tokens":null,"output_tokens":4}}}"#,
            ],
            init_idle: false,
            want: Scan {
                output_tokens: 4,
                idle: false,
                ..Default::default()
            },
        },
        ScanCase {
            name: "whitespace-around-colons-is-tolerated",
            lines: &[r#"{"type" : "assistant","message":{"usage":{"input_tokens"  :   12}}}"#],
            init_idle: false,
            want: Scan {
                input_tokens: 12,
                idle: false,
                ..Default::default()
            },
        },
        ScanCase {
            name: "nothing-votes-so-the-initial-verdict-holds",
            lines: &[r#"{"type":"system","subtype":"init"}"#],
            init_idle: true,
            want: Scan {
                idle: true,
                ..Default::default()
            },
        },
    ];

    let mut failures = Failures::default();
    for case in cases {
        let body = format!("{}\n", case.lines.join("\n"));
        let got = transcript::scan(body.as_bytes(), Some(body.len() as u64), case.init_idle);
        let want = Scan {
            consumed: body.len() as u64,
            ..case.want.clone()
        };
        failures.check(case.name, got == want, || {
            format!("want {want:?}, got {got:?}")
        });
    }
    failures.assert_empty("transcript scan");
}

/// The pinned inputs the fixture captures use. Their totals are the sum of
/// every assistant entry's four usage fields, which is what the tokens row
/// renders.
#[test]
fn pinned_transcript_inputs_scan_to_their_recorded_totals() {
    let plain = transcript_input("transcript.jsonl");
    let got = transcript::scan(&plain, Some(plain.len() as u64), true);
    assert_eq!(
        got,
        Scan {
            messages: 2,
            input_tokens: 2700,
            cache_write_tokens: 1400,
            cache_read_tokens: 82000,
            output_tokens: 400,
            idle: false,
            consumed: plain.len() as u64,
        }
    );
}

/// The gawk incident, pinned. A UTF-8-aware extractor mis-sliced any line
/// carrying a character outside the BMP and returned zero for its token
/// counts, which reads as a plausible total rather than as an obvious break.
/// Both entries in this fixture carry astral-plane characters, so a regression
/// shows up as a total, not as a rounding difference.
#[test]
fn astral_plane_characters_do_not_zero_the_token_counts() {
    let astral = transcript_input("transcript-astral.jsonl");
    let got = transcript::scan(&astral, Some(astral.len() as u64), true);
    assert_eq!(
        got,
        Scan {
            messages: 1,
            input_tokens: 2100,
            cache_write_tokens: 1100,
            cache_read_tokens: 81000,
            output_tokens: 320,
            idle: false,
            consumed: astral.len() as u64,
        }
    );
    assert!(
        got.input_tokens > 0 && got.output_tokens > 0,
        "the incident's signature is zeroed totals, not wrong ones"
    );
}

/// A trailing line with no newline is being written right now. Counting it
/// would double-count when the rest of it arrives, so it is left for the next
/// scan and the consumed count stops before it.
#[test]
fn a_torn_trailing_line_is_left_for_the_next_scan() {
    let complete = "{\"type\":\"assistant\",\"message\":{\"usage\":{\"output_tokens\":5}}}\n";
    let torn = "{\"type\":\"assistant\",\"message\":{\"usage\":{\"output_to";
    let body = format!("{complete}{torn}");

    let got = transcript::scan(body.as_bytes(), Some(body.len() as u64), true);
    assert_eq!(got.output_tokens, 5, "the torn line must not be counted");
    assert_eq!(
        got.consumed,
        complete.len() as u64,
        "the consumed count must stop at the last complete record"
    );
}

/// The size is sampled before the read, so a transcript that grows during the
/// scan yields the same numbers it would have a moment earlier. Everything
/// after the first record that does not fit is skipped, including records that
/// would have fit — the prefix has to stay unbroken.
#[test]
fn records_appended_after_the_size_was_sampled_are_not_consumed() {
    let first = "{\"type\":\"assistant\",\"message\":{\"usage\":{\"output_tokens\":5}}}\n";
    let appended = "{\"type\":\"assistant\",\"message\":{\"usage\":{\"output_tokens\":7}}}\n";
    let body = format!("{first}{appended}");

    let sampled = transcript::scan(body.as_bytes(), Some(first.len() as u64), true);
    assert_eq!(sampled.output_tokens, 5);
    assert_eq!(sampled.consumed, first.len() as u64);

    let ungated = transcript::scan(body.as_bytes(), None, true);
    assert_eq!(
        ungated.output_tokens, 12,
        "without a sampled size every record present is consumed"
    );
    assert_eq!(ungated.consumed, body.len() as u64);
}

/// The record is what makes the `(+N)` deltas renderable, so its round trip is
/// a render-correctness test, not a serialization detail.
#[test]
fn the_token_record_round_trips_through_the_state_guard() {
    let dir = scratch_dir("token-record");
    let path = dir.join("statusline-tokens-session.txt");
    let record = TokenRecord {
        mtime: 1_700_000_000,
        size: 8_406_985,
        messages: 41,
        idle: false,
        input_tokens: 425_266,
        cache_write_tokens: 26_436_150,
        cache_read_tokens: 486_683_613,
        output_tokens: 4_243_856,
        delta_in: 12,
        delta_cache_write: 34,
        delta_cache_read: 56,
        delta_out: 78,
    };

    assert_eq!(
        state::write_guarded(&path, record.to_line().as_bytes()),
        WriteOutcome::Written
    );
    let bytes = state::read_trusted(&path).expect("the record must read back through the guard");
    let text = String::from_utf8(bytes).expect("the record is ASCII");
    assert_eq!(
        TokenRecord::parse(&text).as_ref(),
        Some(&record),
        "the record must survive the guarded write it is stored through"
    );
}

/// Every rejection lands where an absent record lands — deltas against zero —
/// so being strict here costs one inflated increment, never a wrong number.
#[test]
fn a_damaged_token_record_reads_as_no_record() {
    let good = TokenRecord {
        mtime: 10,
        size: 20,
        messages: 9,
        idle: true,
        input_tokens: 1,
        cache_write_tokens: 2,
        cache_read_tokens: 3,
        output_tokens: 4,
        delta_in: 5,
        delta_cache_write: 6,
        delta_cache_read: 7,
        delta_out: 8,
    }
    .to_line();

    let cases: &[(&str, String, bool)] = &[
        ("well-formed", good.clone(), true),
        ("trailing-newline", format!("{good}\n"), true),
        ("trailing-crlf", format!("{good}\r\n"), true),
        ("empty", String::new(), false),
        ("wrong-version", good.replacen("v4", "v3", 1), false),
        (
            "too-few-fields",
            "v4|10|20|9|true|1|2|3|4".to_string(),
            false,
        ),
        ("too-many-fields", format!("{good}|9"), false),
        ("non-numeric", good.replacen("|20|", "|twenty|", 1), false),
        (
            "negative-size",
            "v4|10|-20|9|true|1|2|3|4|5|6|7|8".to_string(),
            false,
        ),
        (
            "overlong-digit-run",
            format!("v4|10|20|9|true|{}|2|3|4|5|6|7|8", "9".repeat(25)),
            false,
        ),
        // `idle` now decides whether the transcript is read at all, so a value
        // that is not exactly one of the two literals rejects the record rather
        // than defaulting to a verdict nobody computed.
        (
            "idle-not-a-bool",
            "v4|10|20|9|maybe|1|2|3|4|5|6|7|8".to_string(),
            false,
        ),
    ];

    let mut failures = Failures::default();
    for (name, raw, want_ok) in cases {
        let got = TokenRecord::parse(raw).is_some();
        failures.check(name, got == *want_ok, || {
            format!("want parse ok = {want_ok}, got {got}")
        });
    }
    failures.assert_empty("token record parsing");
}

/// The delta rules, which are rendered output: the `(+N)` beside each bucket.
#[test]
fn deltas_follow_the_scripts_rules() {
    let scan = Scan {
        input_tokens: 100,
        cache_write_tokens: 200,
        cache_read_tokens: 300,
        output_tokens: 400,
        ..Default::default()
    };

    // First run: no record, so the whole total renders as one increment. This
    // is the scripts' behaviour, not an accident of the port.
    let (first, write) = TokenRecord::fold(None, &scan, 10, 20);
    assert!(write);
    assert_eq!(
        (
            first.delta_in,
            first.delta_cache_write,
            first.delta_cache_read,
            first.delta_out
        ),
        (100, 200, 300, 400)
    );

    // Transcript grew: deltas are the difference.
    let grown = Scan {
        input_tokens: 130,
        cache_write_tokens: 200,
        cache_read_tokens: 350,
        output_tokens: 480,
        ..Default::default()
    };
    let (second, write) = TokenRecord::fold(Some(&first), &grown, 11, 25);
    assert!(write);
    assert_eq!(
        (
            second.delta_in,
            second.delta_cache_write,
            second.delta_cache_read,
            second.delta_out
        ),
        (30, 0, 50, 80)
    );

    // Unchanged transcript: the stored deltas are re-displayed rather than
    // recomputed to zero, and nothing needs writing.
    let (idle_tick, write) = TokenRecord::fold(Some(&second), &grown, 11, 25);
    assert_eq!(idle_tick, second);
    assert!(!write, "an idle tick must not rewrite an identical record");

    // Same mtime and size, different content — the same-size rewrite the
    // scripts needed a head checksum to notice. Scanning every tick sees it.
    let rewritten = Scan {
        input_tokens: 999,
        ..Default::default()
    };
    let (after, write) = TokenRecord::fold(Some(&second), &rewritten, 11, 25);
    assert_eq!(
        after.input_tokens, 999,
        "totals always come from this tick's scan"
    );
    assert_eq!(
        after.delta_in, second.delta_in,
        "an unchanged mtime and size still re-displays the stored delta"
    );
    assert!(write, "changed totals must be stored");

    // A shrinking transcript clamps rather than rendering a negative increment.
    let shrunk = Scan {
        input_tokens: 1,
        ..Default::default()
    };
    let (smaller, _) = TokenRecord::fold(Some(&second), &shrunk, 12, 5);
    assert_eq!(
        (smaller.delta_in, smaller.delta_out),
        (0, 0),
        "deltas saturate at zero"
    );
}

/// The record's path is derived from the payload's session id, so it is a
/// path-traversal sink like every other per-session file.
#[test]
fn the_record_path_cannot_escape_the_temp_root() {
    let temp = scratch_dir("record-path-traversal");

    let ordinary = transcript::record_path(&temp, "abc-123").expect("an ordinary id yields a path");
    assert_eq!(ordinary.parent(), Some(temp.as_path()));
    assert_eq!(
        ordinary.file_name().and_then(|n| n.to_str()),
        Some("statusline-tokens-abc-123.txt")
    );

    let traversal = transcript::record_path(&temp, "../../etc/passwd")
        .expect("a traversal id still yields a path");
    assert_eq!(
        traversal.parent(),
        Some(temp.as_path()),
        "the sanitized id must not reintroduce a separator"
    );
    assert_eq!(
        traversal.file_name().and_then(|n| n.to_str()),
        Some("statusline-tokens-etcpasswd.txt")
    );

    assert_eq!(
        transcript::record_path(&temp, "///"),
        None,
        "an id that sanitizes to nothing must not share one record with every other such session"
    );
}

/// Absent and empty transcripts are ordinary, not errors: a session renders
/// before its first entry is written.
#[test]
fn an_empty_transcript_scans_to_zero_without_voting() {
    for (name, init_idle) in [("idle", true), ("working", false)] {
        let got = transcript::scan(b"", Some(0), init_idle);
        assert_eq!(
            got,
            Scan {
                idle: init_idle,
                ..Default::default()
            },
            "empty transcript with a {name} starting verdict"
        );
    }
}

// ---------------------------------------------------------------------------
// Git status
// ---------------------------------------------------------------------------

struct PorcelainCase {
    name: &'static str,
    lines: &'static [&'static str],
    want: Porcelain,
}

/// The ten git states of `docs/performance.md` §4, as the porcelain text each
/// one produces. Parsing is pure over text, so every state is reachable
/// here without building the repository that emits it — including the ones a
/// fixture capture cannot easily stage.
#[test]
fn porcelain_v2_parses_every_documented_state() {
    let cases = &[
        PorcelainCase {
            name: "clean",
            lines: &[
                "# branch.oid 1111111111111111111111111111111111111111",
                "# branch.head main",
                "# branch.upstream origin/main",
                "# branch.ab +0 -0",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "1111111111111111111111111111111111111111".into(),
                ..Default::default()
            },
        },
        PorcelainCase {
            name: "untracked-only",
            lines: &[
                "# branch.oid 2222222222222222222222222222222222222222",
                "# branch.head main",
                "? one.txt",
                "? two.txt",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "2222222222222222222222222222222222222222".into(),
                untracked: 2,
                ..Default::default()
            },
        },
        PorcelainCase {
            // git collapses a wholly untracked directory into one entry. A
            // reimplementation getting this wrong reads as an untracked count
            // that jumps with directory size — one of the reasons the port shells
            // out rather than reimplementing.
            name: "collapsed-untracked-dir",
            lines: &[
                "# branch.oid 3333333333333333333333333333333333333333",
                "# branch.head main",
                "? newdir/",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "3333333333333333333333333333333333333333".into(),
                untracked: 1,
                ..Default::default()
            },
        },
        PorcelainCase {
            name: "dirty-tracked-changes",
            lines: &[
                "# branch.oid 4444444444444444444444444444444444444444",
                "# branch.head main",
                "1 .M N... 100644 100644 100644 aaa bbb src/lib.rs",
                "2 R. N... 100644 100644 100644 ccc ddd R100 new.rs\told.rs",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "4444444444444444444444444444444444444444".into(),
                ..Default::default()
            },
        },
        PorcelainCase {
            name: "stashes-present",
            lines: &[
                "# branch.oid 5555555555555555555555555555555555555555",
                "# branch.head main",
                "# stash 3",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "5555555555555555555555555555555555555555".into(),
                stash: 3,
                ..Default::default()
            },
        },
        PorcelainCase {
            // The zero case emits no line at all rather than `# stash 0`.
            name: "stash-cleared",
            lines: &[
                "# branch.oid 6666666666666666666666666666666666666666",
                "# branch.head main",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "6666666666666666666666666666666666666666".into(),
                ..Default::default()
            },
        },
        PorcelainCase {
            name: "ahead-of-upstream",
            lines: &[
                "# branch.oid 7777777777777777777777777777777777777777",
                "# branch.head main",
                "# branch.upstream origin/main",
                "# branch.ab +4 -2",
            ],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "7777777777777777777777777777777777777777".into(),
                ahead: 4,
                behind: 2,
                ..Default::default()
            },
        },
        PorcelainCase {
            // No upstream means no `# branch.ab` line at all. Absent is zero,
            // not missing data.
            name: "no-upstream",
            lines: &[
                "# branch.oid 8888888888888888888888888888888888888888",
                "# branch.head feature",
            ],
            want: Porcelain {
                branch: "feature".into(),
                head_oid: "8888888888888888888888888888888888888888".into(),
                ..Default::default()
            },
        },
        PorcelainCase {
            name: "detached-head",
            lines: &[
                "# branch.oid 9999999999999999999999999999999999999999",
                "# branch.head (detached)",
            ],
            want: Porcelain {
                branch: "(detached)".into(),
                head_oid: "9999999999999999999999999999999999999999".into(),
                ..Default::default()
            },
        },
        PorcelainCase {
            // Unborn HEAD: the field that normally carries a hash carries a
            // word. Arithmetic on it would be the bug.
            name: "unborn-head",
            lines: &["# branch.oid (initial)", "# branch.head main"],
            want: Porcelain {
                branch: "main".into(),
                head_oid: "(initial)".into(),
                ..Default::default()
            },
        },
    ];

    let mut failures = Failures::default();
    for case in cases {
        let text = format!("{}\n", case.lines.join("\n"));
        let got = git::parse_porcelain_v2(&text);
        failures.check(case.name, got == case.want, || {
            format!("want {:?}, got {got:?}", case.want)
        });
    }
    failures.assert_empty("porcelain v2 parsing");
}

/// A malformed `# branch.ab` line must not reach arithmetic as anything but a
/// number — the scripts guard every one of these fields for the same reason.
#[test]
fn malformed_porcelain_fields_read_as_zero() {
    let cases: &[(&str, &str, u64, u64)] = &[
        ("well-formed", "# branch.ab +4 -2", 4, 2),
        ("unsigned", "# branch.ab 4 2", 4, 2),
        ("non-numeric", "# branch.ab +x -y", 0, 0),
        ("empty", "# branch.ab ", 0, 0),
        // No space: the scripts' first-token and last-token expansions both
        // yield the whole string, so both counts come from it.
        ("no-space", "# branch.ab +7", 7, 0),
    ];

    let mut failures = Failures::default();
    for (name, line, ahead, behind) in cases {
        let got = git::parse_porcelain_v2(&format!("{line}\n"));
        failures.check(name, got.ahead == *ahead && got.behind == *behind, || {
            format!(
                "want ahead {ahead} behind {behind}, got {} {}",
                got.ahead, got.behind
            )
        });
    }

    let stash = git::parse_porcelain_v2("# stash notanumber\n");
    failures.check("stash-non-numeric", stash.stash == 0, || {
        format!("got {}", stash.stash)
    });
    failures.assert_empty("malformed porcelain fields");
}

/// The `(detached)` collision: git prints the same sentinel for a detached HEAD
/// and for a branch that is literally named `(detached)`, so both resolve to the
/// abbreviated hash. The scripts accept that, and so does this — it is the
/// recorded divergence, not a bug to fix here.
#[test]
fn the_detached_sentinel_resolves_through_git_not_by_truncation() {
    let detached = Porcelain {
        branch: "(detached)".into(),
        head_oid: "abcdef1234567890abcdef1234567890abcdef12".into(),
        ..Default::default()
    };

    assert_eq!(
        git::resolve_branch(&detached, || Some("abcdef1".to_string())),
        "abcdef1",
        "the abbreviation comes from git, which lengthens it for uniqueness"
    );
    assert_eq!(
        git::resolve_branch(&detached, || None),
        "HEAD",
        "a failed abbreviation falls back to the literal HEAD"
    );
    assert_eq!(
        git::resolve_branch(&detached, || Some(String::new())),
        "HEAD",
        "empty output is a failure too, not a branch named nothing"
    );

    let unborn = Porcelain {
        branch: "main".into(),
        head_oid: "(initial)".into(),
        ..Default::default()
    };
    assert_eq!(
        git::resolve_branch(&unborn, || panic!(
            "unborn HEAD must not spawn a second git"
        )),
        "",
        "unborn HEAD renders no git segment on this platform"
    );

    let ordinary = Porcelain {
        branch: "main".into(),
        head_oid: "1234567890123456789012345678901234567890".into(),
        ..Default::default()
    };
    assert_eq!(
        git::resolve_branch(&ordinary, || panic!(
            "an ordinary branch must not spawn a second git"
        )),
        "main"
    );
}

#[test]
fn shortstat_counts_are_optional_and_independent() {
    let cases: &[(&str, &str, u64, u64)] = &[
        (
            "both",
            " 3 files changed, 12 insertions(+), 4 deletions(-)",
            12,
            4,
        ),
        ("insertions-only", " 1 file changed, 7 insertions(+)", 7, 0),
        ("deletions-only", " 1 file changed, 2 deletions(-)", 0, 2),
        ("neither", " 1 file changed", 0, 0),
        ("empty", "", 0, 0),
        (
            "single",
            " 1 file changed, 1 insertion(+), 1 deletion(-)",
            1,
            1,
        ),
    ];

    let mut failures = Failures::default();
    for (name, text, ins, del) in cases {
        let got = git::parse_shortstat(text);
        failures.check(name, got == (*ins, *del), || {
            format!("want ({ins}, {del}), got {got:?}")
        });
    }
    failures.assert_empty("shortstat parsing");
}

/// The record shares its name and shape with the scripts' on purpose:
/// `git-refresh` deletes exactly `statusline-git-<id>.txt`, so a binary caching
/// anywhere else would keep a stale git row alive through every edit.
#[test]
fn the_git_cache_record_round_trips_and_fails_safe() {
    let status = GitStatus {
        branch: "dev-rust-migration".into(),
        insertions: 12,
        deletions: 4,
        untracked: 3,
        ahead: 2,
        behind: 1,
        stash: 5,
    };
    let line = git::cache_record(1_700_000_000, &status);
    assert_eq!(
        git::parse_cache_record(&line),
        Some((1_700_000_000, status.clone()))
    );

    let mut failures = Failures::default();
    for (name, raw) in [
        ("empty", String::new()),
        ("too-few-fields", "1700000000\u{1f}main".to_string()),
        ("too-many-fields", format!("{line}\u{1f}extra")),
        ("non-numeric-mtime", line.replacen("1700000000", "soon", 1)),
    ] {
        failures.check(name, git::parse_cache_record(&raw).is_none(), || {
            "a damaged record must read as no record".to_string()
        });
    }

    // A planted numeric field degrades to zero rather than reaching arithmetic.
    let planted = git::cache_record(1, &status).replacen(
        "\u{1f}12\u{1f}",
        "\u{1f}9999999999999999999999\u{1f}",
        1,
    );
    let (_, parsed) = git::parse_cache_record(&planted).expect("field count is still valid");
    failures.check("overlong-digit-run", parsed.insertions == 0, || {
        format!("got {}", parsed.insertions)
    });
    failures.assert_empty("git cache record");
}

/// The deadline actually kills. A child that outlives its timeout must come
/// back as `None` within a bound, not whenever it deigns to exit — this loop
/// is what stands between a repo on a stalled network mount and a status line
/// blocked forever, and until this test nothing drove a child past the
/// deadline to prove the kill fires.
#[test]
fn a_child_past_its_deadline_is_killed_not_awaited() {
    #[cfg(unix)]
    let command = {
        let mut c = std::process::Command::new("sleep");
        c.arg("5");
        c
    };
    #[cfg(windows)]
    let command = {
        // ~5 seconds; `ping` because it exists on every Windows without
        // spawning an interpreter.
        let mut c = std::process::Command::new("ping");
        c.args(["-n", "6", "127.0.0.1"]);
        c
    };

    let started = std::time::Instant::now();
    let out = git::run_bounded(command, std::time::Duration::from_millis(200));
    let elapsed = started.elapsed();

    assert!(
        out.is_none(),
        "a child killed at the deadline must not report output"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "run_bounded took {elapsed:?} against a 200ms deadline — the kill did not fire \
         and the call blocked on the child instead"
    );
}

/// The TTL, exercised through the injected clock rather than by sleeping.
/// Its expiry is a staleness bound: `.git/index` mtime does not move when an
/// untracked file appears or when `git fetch` rewrites `packed-refs`.
#[test]
fn the_git_ttl_expires_through_the_injected_clock() {
    let dir = scratch_dir("git-ttl");
    let index = dir.join(".git").join("index");
    std::fs::create_dir_all(index.parent().expect("index has a parent"))
        .expect("failed to create the fake .git directory");
    std::fs::write(&index, b"not a real index").expect("failed to write the fake index");

    let session = "git-ttl-session";
    let cache = git::cache_path(&dir, session).expect("an ordinary session id yields a cache path");
    let cached = GitStatus {
        branch: "cached-branch".into(),
        insertions: 1,
        ..Default::default()
    };
    std::fs::write(&cache, git::cache_record(1000, &cached)).expect("failed to seed the cache");

    let fresh = TestClock::at(2000)
        .with_mtime(&index, 1000)
        .with_mtime(&cache, 1996);
    assert_eq!(
        git::status(&fresh, &inherited(&dir), &dir, session).as_ref(),
        Some(&cached),
        "a record 4s old, taken at the current index mtime, is a hit"
    );

    let expired = TestClock::at(2000)
        .with_mtime(&index, 1000)
        .with_mtime(&cache, 1995);
    assert_ne!(
        git::status(&expired, &inherited(&dir), &dir, session).as_ref(),
        Some(&cached),
        "at exactly the TTL the record is stale, so git is consulted"
    );

    let moved = TestClock::at(2000)
        .with_mtime(&index, 1001)
        .with_mtime(&cache, 1999);
    assert_ne!(
        git::status(&moved, &inherited(&dir), &dir, session).as_ref(),
        Some(&cached),
        "an index that moved invalidates the record however fresh it is"
    );

    let _ = std::fs::remove_file(&cache);
}

/// A directory with no repository in it renders no git segment, and does so
/// without an error path — the row simply is not there.
#[test]
fn a_directory_without_a_repository_renders_no_git_row() {
    let dir = scratch_dir("git-no-repo");
    let clock = TestClock::at(2000);
    assert_eq!(
        git::status(&clock, &inherited(&dir), &dir, "no-repo-session"),
        None
    );
}

#[test]
fn the_git_cache_path_cannot_escape_the_temp_root() {
    let temp = scratch_dir("git-cache-traversal");
    let traversal =
        git::cache_path(&temp, "../../etc/passwd").expect("a traversal id still yields a path");
    assert_eq!(traversal.parent(), Some(temp.as_path()));
    assert_eq!(
        traversal.file_name().and_then(|n| n.to_str()),
        Some("statusline-git-etcpasswd.txt")
    );
    assert_eq!(git::cache_path(&temp, "///"), None);
}

#[test]
fn an_absent_workspace_directory_falls_back_to_the_process_directory() {
    let expected = std::env::current_dir().expect("the test process has a working directory");
    assert_eq!(git::resolve_cwd(""), expected);
    assert_eq!(
        git::resolve_cwd("/somewhere/else"),
        Path::new("/somewhere/else")
    );
}

// ---------------------------------------------------------------------------
// Subagent rows and window resolution
// ---------------------------------------------------------------------------

/// Removes any per-task state left by an earlier run, so a linger assertion
/// cannot pass or fail on a stale stamp.
fn clear_task_state(session_id: &str) {
    let prefix = format!("statusline-sa-{session_id}-task-");
    if let Ok(entries) = std::fs::read_dir(claude_statusline::session::temp_dir()) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&prefix))
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn learned(pairs: &[(&str, u64)]) -> std::collections::BTreeMap<String, u64> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
}

/// Every tier of the resolver, in order. The order is the point: the session's
/// own window is live truth for this session, while a learned entry is a
/// historical observation that may have come from another machine.
#[test]
fn window_resolution_walks_every_tier_in_order() {
    let session = Windows::new(
        "claude-opus-5",
        Some(1_000_000),
        learned(&[("claude-sonnet-5", 123_456), ("claude-opus-5", 999)]),
    );

    let mut failures = Failures::default();
    let mut check = |name: &str, got: u64, want: u64| {
        failures.check(name, got == want, || format!("want {want}, got {got}"))
    };

    check(
        "session-tier-wins-over-learned",
        session.resolve("claude-opus-5"),
        1_000_000,
    );
    check(
        "session-tier-matches-across-the-1m-variant",
        session.resolve("claude-opus-5[1m]"),
        1_000_000,
    );
    check(
        "session-tier-ignores-a-date-suffix",
        session.resolve("claude-opus-5-20260101"),
        1_000_000,
    );
    check("learned-tier", session.resolve("claude-sonnet-5"), 123_456);
    check(
        "learned-tier-after-normalizing-the-date",
        session.resolve("claude-sonnet-5-20260715"),
        123_456,
    );

    let seeds = Windows::new("", None, learned(&[]));
    check(
        "seed-tier-million",
        seeds.resolve("claude-sonnet-5"),
        1_000_000,
    );
    check("seed-tier-200k", seeds.resolve("claude-haiku-4-5"), 200_000);
    check(
        "seed-tier-tolerates-a-bare-id",
        seeds.resolve("sonnet-4-6"),
        1_000_000,
    );
    check(
        "marker-tier-bracketed",
        seeds.resolve("claude-experimental-9[1m]"),
        1_000_000,
    );
    check(
        "marker-tier-suffixed",
        seeds.resolve("claude-experimental-9-1m"),
        1_000_000,
    );
    check("default-tier", seeds.resolve("claude-unknown-7"), 200_000);
    check("default-for-an-absent-model", seeds.resolve(""), 200_000);

    // A session window of zero is no window at all: the tier is skipped rather
    // than resolving to something that would divide by zero downstream.
    let zeroed = Windows::new("claude-opus-5", Some(0), learned(&[]));
    check(
        "a-zero-session-window-is-skipped",
        zeroed.resolve("claude-opus-5"),
        200_000,
    );

    failures.assert_empty("window resolution");
}

/// The learned map is an optimization over the seed table, never a
/// prerequisite: every way it can fail leaves the seeds reachable.
#[test]
fn an_unusable_learned_map_degrades_to_the_seed_table() {
    let dir = scratch_dir("learned-map");

    let absent = dir.join("missing.json");
    assert!(Windows::load_learned(&absent).is_empty());

    let mut failures = Failures::default();
    for (name, body) in [
        ("malformed", "{\"claude-sonnet-5\": "),
        ("not-an-object", "[1, 2, 3]"),
        ("empty-file", ""),
    ] {
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, body).expect("failed to write the learned map fixture");
        failures.check(name, Windows::load_learned(&path).is_empty(), || {
            "an unusable map must read as empty".to_string()
        });
    }

    // A usable map with junk entries keeps the usable ones and drops the rest.
    let mixed = dir.join("mixed.json");
    std::fs::write(
        &mixed,
        r#"{"claude-sonnet-5": 400000, "quoted": "500000", "bad": "not a number", "null": null, "": 1}"#,
    )
    .expect("failed to write the mixed learned map");
    let map = Windows::load_learned(&mixed);
    failures.check(
        "keeps-numbers",
        map.get("claude-sonnet-5") == Some(&400_000),
        || format!("{map:?}"),
    );
    failures.check(
        "keeps-numeric-strings",
        map.get("quoted") == Some(&500_000),
        || format!("{map:?}"),
    );
    failures.check("drops-junk", map.len() == 2, || format!("{map:?}"));

    // And the seed table is still reachable for anything the map lacks.
    let windows = Windows::new("", None, Windows::load_learned(&absent));
    failures.check(
        "seed-still-reachable",
        windows.resolve("claude-haiku-4-5") == 200_000,
        || "the seed tier became unreachable".to_string(),
    );
    failures.assert_empty("learned map degradation");
}

#[test]
fn feed_statuses_fail_open_to_working() {
    let mut failures = Failures::default();
    for done in [
        "completed",
        "complete",
        "done",
        "finished",
        "failed",
        "cancelled",
        "canceled",
        "killed",
        "stopped",
        "error",
        "COMPLETED",
        "Done",
    ] {
        failures.check(done, !subagent::status_is_active(done), || {
            "a terminal status must not read as active".to_string()
        });
    }
    // Anything unrecognized reads as working, so a status Claude Code adds
    // later shows up as a visible row rather than silently vanishing.
    for active in ["running", "in_progress", "", "queued", "something-new"] {
        failures.check(active, subagent::status_is_active(active), || {
            "an unknown status must read as active".to_string()
        });
    }
    failures.assert_empty("feed status polarity");
}

#[test]
fn the_feed_display_chain_falls_through_blank_candidates() {
    let feed = r#"{"tasks": [
        {"id": "a", "description": "  build the thing  ", "type": "explore", "name": "n"},
        {"id": "b", "description": "|||", "type": "explore", "name": "n"},
        {"id": "c", "description": "", "type": "", "name": "last resort"},
        {"id": "d"}
    ]}"#;
    let tasks = subagent::parse_feed(feed).expect("a well-formed feed parses");
    let displays: Vec<&str> = tasks.iter().map(|t| t.display.as_str()).collect();
    assert_eq!(
        displays,
        vec!["build the thing", "explore", "last resort", ""],
        "description, then type, then name — first non-blank *after* scrubbing"
    );
}

#[test]
fn a_payload_that_is_not_a_feed_drops_the_tier() {
    let mut failures = Failures::default();
    for (name, raw, want) in [
        ("object-without-tasks", r#"{"session_id": "s"}"#, Some(0)),
        ("null-tasks", r#"{"tasks": null}"#, Some(0)),
        ("empty-tasks", r#"{"tasks": []}"#, Some(0)),
        ("tasks-not-an-array", r#"{"tasks": {"a": 1}}"#, None),
        ("not-an-object", "[1,2,3]", None),
        ("malformed", "{", None),
        (
            "non-object-entries-are-dropped",
            r#"{"tasks": [1, "two", null, {"id": "real"}]}"#,
            Some(1),
        ),
    ] {
        let got = subagent::parse_feed(raw).map(|t| t.len());
        failures.check(name, got == want, || format!("want {want:?}, got {got:?}"));
    }
    failures.assert_empty("feed parsing");
}

/// A fresh feed is the whole tier: per-task model and window come from it, not
/// from the resolver, and rows sort by start time rather than feed order.
#[test]
fn a_fresh_feed_renders_its_own_models_and_windows() {
    let session = "feedsession1";
    let temp = scratch_dir("feed-models");
    let clock = TestClock::at(1_000);
    let windows = Windows::new("", None, learned(&[]));

    let feed = r#"{"tasks": [
        {"id": "second", "startTime": "2026-07-27T10:05:00Z", "description": "later task",
         "status": "running", "model": "claude-haiku-4-5", "contextWindowSize": 200000,
         "tokenCount": 4321, "effort": "low"},
        {"id": "first", "startTime": "2026-07-27T10:00:00Z", "description": "earlier task",
         "status": "running", "model": "claude-unknown-9", "contextWindowSize": 777000,
         "tokenCount": 1234}
    ]}"#;

    let rows = subagent::rows_from_feed(&clock, &inherited(&temp), session, feed, &windows)
        .expect("a well-formed feed yields rows");

    assert_eq!(
        rows,
        vec![
            Row {
                used: 1234,
                // From the feed, not the resolver: the resolver would have
                // returned the 200K default for this unknown model.
                window: 777_000,
                model: "claude-unknown-9".into(),
                display: "earlier task".into(),
                effort: String::new(),
                done: false,
            },
            Row {
                used: 4321,
                window: 200_000,
                model: "claude-haiku-4-5".into(),
                display: "later task".into(),
                effort: "low".into(),
                done: false,
            },
        ],
        "rows sort by start time, and each carries the feed's own window"
    );
    clear_task_state(session);
}

/// Without a per-task window the row falls back to the tiered resolver — the
/// Claude Code < v2.1.205 case.
#[test]
fn a_feed_without_windows_falls_back_to_the_resolver() {
    let session = "feedsession2";
    let temp = scratch_dir("feed-windows");
    let clock = TestClock::at(1_000);
    let windows = Windows::new("", None, learned(&[("claude-sonnet-5", 424_242)]));

    let feed = r#"{"tasks": [
        {"id": "a", "status": "running", "model": "claude-sonnet-5", "tokenCount": 10},
        {"id": "b", "status": "running", "model": "claude-mystery-1", "tokenCount": 20}
    ]}"#;
    let rows = subagent::rows_from_feed(&clock, &inherited(&temp), session, feed, &windows)
        .expect("feed parses");
    let sizes: Vec<u64> = rows.iter().map(|r| r.window).collect();
    assert_eq!(sizes, vec![424_242, 200_000]);
}

/// The done linger, both signals: a task reporting a terminal status, and a
/// task that simply stops appearing in the feed. Both stamp once, stay visible
/// for the linger, and then disappear.
#[test]
fn finished_tasks_linger_then_disappear() {
    let session = "feedsession3";
    let temp = scratch_dir("feed-linger");
    let windows = Windows::new("", None, learned(&[]));
    let running = r#"{"tasks": [{"id": "t1", "status": "running", "model": "claude-haiku-4-5",
                                 "tokenCount": 5, "description": "the task"}]}"#;
    let finished = r#"{"tasks": [{"id": "t1", "status": "completed", "model": "claude-haiku-4-5",
                                  "tokenCount": 5, "description": "the task"}]}"#;
    let empty = r#"{"tasks": []}"#;

    // Running.
    let rows = subagent::rows_from_feed(
        &TestClock::at(1_000),
        &inherited(&temp),
        session,
        running,
        &windows,
    )
    .expect("feed parses");
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].done);

    // Completed at t=1000: still visible, now marked done.
    let rows = subagent::rows_from_feed(
        &TestClock::at(1_000),
        &inherited(&temp),
        session,
        finished,
        &windows,
    )
    .expect("feed parses");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].done, "a terminal status marks the row done");

    // Still inside the linger at t=1030, measured from completion rather than
    // from this observation.
    let rows = subagent::rows_from_feed(
        &TestClock::at(1_030),
        &inherited(&temp),
        session,
        finished,
        &windows,
    )
    .expect("feed parses");
    assert_eq!(rows.len(), 1, "30s is still within the linger");

    // Past it at t=1031.
    let rows = subagent::rows_from_feed(
        &TestClock::at(1_031),
        &inherited(&temp),
        session,
        finished,
        &windows,
    )
    .expect("feed parses");
    assert!(rows.is_empty(), "past the linger the row is gone");

    // The other done signal: a task that vanishes from a fresh feed. Its state
    // file is still there, so it renders as done and then expires.
    let temp = scratch_dir("feed-linger-vanish");
    let _ = subagent::rows_from_feed(
        &TestClock::at(2_000),
        &inherited(&temp),
        session,
        running,
        &windows,
    );
    let rows = subagent::rows_from_feed(
        &TestClock::at(2_001),
        &inherited(&temp),
        session,
        empty,
        &windows,
    )
    .expect("feed parses");
    assert_eq!(rows.len(), 1, "a task that left the feed has finished");
    assert!(rows[0].done);
    assert_eq!(rows[0].display, "the task", "its last known title survives");

    let rows = subagent::rows_from_feed(
        &TestClock::at(2_040),
        &inherited(&temp),
        session,
        empty,
        &windows,
    )
    .expect("feed parses");
    assert!(rows.is_empty(), "and then it expires");

    let leftover = std::fs::read_dir(&temp)
        .expect("the temp directory is readable")
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with(&format!("statusline-sa-{session}-task-")))
        })
        .count();
    assert_eq!(leftover, 0, "an expired task's state file is removed");
}

// --- Fallback tier -------------------------------------------------------

#[test]
fn the_subagents_directory_is_derived_from_the_transcript_path() {
    assert_eq!(
        subagent::subagents_dir("/home/u/.claude/projects/p/abc123.jsonl"),
        Some(PathBuf::from("/home/u/.claude/projects/p/abc123/subagents"))
    );
    assert_eq!(subagent::subagents_dir(""), None);
}

#[test]
fn terminal_stop_reasons_mean_done_and_nothing_else_does() {
    let mut failures = Failures::default();
    for done in [
        "end_turn",
        "max_tokens",
        "refusal",
        "model_context_window_exceeded",
        "stop_sequence",
    ] {
        failures.check(done, subagent::stop_reason_is_done(done), || {
            "a terminal reason must read as done".to_string()
        });
    }
    for working in ["tool_use", "pause_turn", "", "something_new"] {
        failures.check(working, !subagent::stop_reason_is_done(working), || {
            "a non-terminal reason must read as working".to_string()
        });
    }
    failures.assert_empty("stop reasons");
}

/// The last assistant entry wins, and a torn or malformed line is skipped
/// rather than ending the scan — the file is being appended to while it reads.
#[test]
fn an_agent_transcript_reports_its_last_assistant_entry() {
    let body = concat!(
        r#"{"type":"user","message":{"content":"go"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-haiku-4-5","usage":{"input_tokens":10,"cache_creation_input_tokens":2,"cache_read_input_tokens":30,"output_tokens":1}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"stop_reason":"end_turn","model":"claude-sonnet-5","usage":{"input_tokens":100,"cache_creation_input_tokens":20,"cache_read_input_tokens":300,"output_tokens":9}}}"#,
        "\n",
        r#"{"type":"assistant","message":{"stop_re"#,
    );
    let reading = subagent::read_agent(body.as_bytes());
    assert_eq!(reading.stop_reason, "end_turn");
    assert_eq!(reading.model, "claude-sonnet-5");
    assert_eq!(
        reading.used(),
        420,
        "used is input plus both cache buckets, and excludes output"
    );

    let empty = subagent::read_agent(b"{\"type\":\"user\"}\n");
    assert_eq!(
        empty,
        Default::default(),
        "no assistant entry yet is zeros and no model, not an error"
    );
}

#[test]
fn the_agent_title_chain_falls_through_to_the_filename() {
    let mut failures = Failures::default();
    for (name, meta, want) in [
        (
            "description",
            Some(r#"{"description": "  read the docs  ", "agentType": "Explore"}"#),
            "read the docs",
        ),
        ("agent-type", Some(r#"{"agentType": "Explore"}"#), "Explore"),
        (
            "blank-description",
            Some(r#"{"description": "|||", "agentType": "Explore"}"#),
            "Explore",
        ),
        ("no-meta", None, "abc123"),
        ("malformed-meta", Some("{"), "abc123"),
        ("empty-meta-object", Some("{}"), "abc123"),
    ] {
        let got = subagent::agent_display(meta, "agent-abc123");
        failures.check(name, got == want, || format!("want {want:?}, got {got:?}"));
    }
    failures.assert_empty("agent title chain");
}

/// The fallback tier reads the transcripts directly, skips agents whose files
/// have gone quiet, and resolves each window through the tiers.
#[test]
fn the_fallback_tier_reads_transcripts_and_skips_stale_agents() {
    let dir = scratch_dir("fallback-tier");
    let transcript = dir.join("session-abc.jsonl");
    let agents = dir.join("session-abc").join("subagents");
    std::fs::create_dir_all(&agents).expect("failed to create the subagents directory");

    let live = agents.join("agent-live.jsonl");
    std::fs::write(
        &live,
        concat!(
            r#"{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-sonnet-5","usage":{"input_tokens":50,"cache_read_input_tokens":50}}}"#,
            "\n"
        ),
    )
    .expect("failed to write the live agent transcript");
    std::fs::write(
        agents.join("agent-live.meta.json"),
        r#"{"description": "the live one"}"#,
    )
    .expect("failed to write the live agent meta");

    let stale = agents.join("agent-stale.jsonl");
    std::fs::write(&stale, "{\"type\":\"assistant\",\"message\":{}}\n")
        .expect("failed to write the stale agent transcript");

    let clock = TestClock::at(10_000)
        .with_mtime(&live, 9_990)
        .with_mtime(&stale, 9_000);
    let windows = Windows::new("", None, learned(&[]));

    let temp = dir.join("temp");
    std::fs::create_dir_all(&temp).expect("failed to create the temp root");

    let rows = subagent::rows_from_transcripts(
        &clock,
        &inherited(&temp),
        "session-abc",
        transcript.to_str().expect("the scratch path is UTF-8"),
        &windows,
    );

    assert_eq!(
        rows,
        vec![Row {
            used: 100,
            window: 1_000_000,
            model: "claude-sonnet-5".into(),
            display: "the live one".into(),
            // A transcript cannot report an effort override, and nothing is
            // inferred from the session's own effort.
            effort: String::new(),
            done: false,
        }],
        "an agent quiet for more than three minutes is not this session's"
    );
}

/// The linger the module doc promises for *both* tiers, on the tier that had
/// none. A finished fallback row used to stay visible for the whole 180s
/// staleness window instead of `DONE_LINGER_SECS`, because the port dropped the
/// per-agent stamp the scripts kept at
/// `eb56345:linux/statusline.sh:1287` and applied at `:1357`. No fixture covered
/// it, so a green table said nothing either way.
#[test]
fn a_finished_fallback_agent_lingers_then_disappears() {
    let dir = scratch_dir("fallback-linger");
    let transcript = dir.join("session-fin.jsonl");
    let agents = dir.join("session-fin").join("subagents");
    let temp = dir.join("temp");
    std::fs::create_dir_all(&agents).expect("failed to create the subagents directory");
    std::fs::create_dir_all(&temp).expect("failed to create the temp root");

    let done = agents.join("agent-done.jsonl");
    std::fs::write(
        &done,
        concat!(
            r#"{"type":"assistant","message":{"stop_reason":"end_turn","model":"claude-sonnet-5","usage":{"input_tokens":10}}}"#,
            "\n"
        ),
    )
    .expect("failed to write the finished agent transcript");

    let windows = Windows::new("", None, learned(&[]));
    let rows_at = |t: i64| {
        // One mtime throughout: the file stops changing once the agent is done,
        // which is also what exercises the read skip.
        let clock = TestClock::at(t).with_mtime(&done, 10_000);
        subagent::rows_from_transcripts(
            &clock,
            &inherited(&temp),
            "session-fin",
            transcript.to_str().expect("the scratch path is UTF-8"),
            &windows,
        )
    };

    // First sighting stamps the completion and shows the row.
    let first = rows_at(10_000);
    assert_eq!(
        first.len(),
        1,
        "the finished row is visible when first seen"
    );
    assert!(first[0].done, "a terminal stop reason renders as done");

    assert_eq!(
        rows_at(10_000 + subagent::DONE_LINGER_SECS).len(),
        1,
        "the row is still visible at the linger boundary"
    );
    assert!(
        rows_at(10_000 + subagent::DONE_LINGER_SECS + 1).is_empty(),
        "one second past the linger the row is gone, rather than lasting the \
         full 180s staleness window"
    );

    // The stamp has to survive the process that observed the completion, which
    // is the whole reason it is a file rather than a field.
    let stamped = std::fs::read_dir(&temp)
        .expect("the temp root is readable")
        .flatten()
        .any(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("statusline-sa-session-fin-agent-done"))
        });
    assert!(
        stamped,
        "the done stamp is persisted per agent, keyed off the `-task-` namespace \
         that `disappeared_rows` scans"
    );
}

/// The read skip that pairs with it. An agent file whose mtime has not moved is
/// served from the stored record, not re-read -- the fallback tier used to
/// `std::fs::read` and re-parse every agent transcript on every refresh, which
/// is the same unconditional-scan cost that made the main transcript 4x slower
/// than the script it replaced.
#[test]
fn an_unchanged_agent_transcript_is_not_rescanned() {
    let dir = scratch_dir("fallback-skip");
    let transcript = dir.join("session-skip.jsonl");
    let agents = dir.join("session-skip").join("subagents");
    let temp = dir.join("temp");
    std::fs::create_dir_all(&agents).expect("failed to create the subagents directory");
    std::fs::create_dir_all(&temp).expect("failed to create the temp root");

    let agent = agents.join("agent-one.jsonl");
    std::fs::write(
        &agent,
        concat!(
            r#"{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-sonnet-5","usage":{"input_tokens":42}}}"#,
            "\n"
        ),
    )
    .expect("failed to write the agent transcript");

    let windows = Windows::new("", None, learned(&[]));
    let run = |t: i64| {
        let clock = TestClock::at(t).with_mtime(&agent, 9_990);
        subagent::rows_from_transcripts(
            &clock,
            &inherited(&temp),
            "session-skip",
            transcript.to_str().expect("the scratch path is UTF-8"),
            &windows,
        )
    };

    let first = run(10_000);
    assert_eq!(first.len(), 1, "the agent is read on the first tick");
    assert_eq!(first[0].used, 42);

    // Rewritten with contradictory content but the SAME mtime. A tick that
    // still reads the file would report the new number; one honouring the
    // record reports the old one. Asserting the stale value is the only way to
    // prove the read was skipped -- a correct token count would be produced by
    // both the skipping and the non-skipping implementation.
    std::fs::write(
        &agent,
        concat!(
            r#"{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-sonnet-5","usage":{"input_tokens":999}}}"#,
            "\n"
        ),
    )
    .expect("failed to rewrite the agent transcript");

    let second = run(10_001);
    assert_eq!(
        second[0].used, 42,
        "an unchanged mtime serves the stored record instead of re-reading"
    );
}

#[test]
fn feed_freshness_is_measured_through_the_injected_clock() {
    let dir = scratch_dir("feed-freshness");
    let feed = dir.join("statusline-tasks-session.json");
    std::fs::write(&feed, "{}").expect("failed to write the feed");

    assert!(subagent::feed_is_fresh(
        &TestClock::at(1_000).with_mtime(&feed, 990),
        &feed
    ));
    assert!(
        !subagent::feed_is_fresh(&TestClock::at(1_000).with_mtime(&feed, 989), &feed),
        "past the window the feed tier is dropped and the fallback tier runs"
    );
    assert!(
        !subagent::feed_is_fresh(&TestClock::at(1_000), &feed),
        "an unreadable mtime is not freshness"
    );
}

// ---------------------------------------------------------------------------
// Render, thresholds, notification spawn
// ---------------------------------------------------------------------------

/// Drops SGR escapes so an assertion can talk about what the user sees.
/// Deliberately not `render::visible_width`'s stripper: a test that shared the
/// implementation under test would agree with it even when both were wrong.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('\u{1b}') {
        out.push_str(&rest[..start]);
        match rest[start..].find('m') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Width is what the box is padded to, so it is the one thing that cannot be
/// checked by reading the output: an escape counted as columns, or a bar cell
/// counted as bytes, both render a box whose rows disagree. The second is what
/// the bash captures recorded before the harness pinned a UTF-8 locale.
#[test]
fn visible_width_counts_columns_not_bytes_or_escapes() {
    let cases: [(&str, usize, &str); 7] = [
        ("plain", 5, "ASCII is its own length"),
        ("\u{1b}[31mred\u{1b}[0m", 3, "SGR escapes occupy no columns"),
        (
            "\u{1b}[38;5;242m░\u{1b}[0m",
            1,
            "a 3-byte bar cell is one column",
        ),
        (
            "██████",
            6,
            "six bar cells are six columns, not eighteen bytes",
        ),
        ("·", 1, "a 2-byte middot is one column"),
        ("日本", 4, "CJK counts as two cells each"),
        ("🙂", 2, "astral emoji counts as two cells"),
    ];
    for (input, expected, why) in cases {
        assert_eq!(render::visible_width(input), expected, "[{input:?}] {why}");
    }
}

#[test]
fn an_unterminated_escape_does_not_eat_the_rest_of_the_row() {
    // A branch name is an untrusted field. A stripper that swallowed everything
    // after a stray ESC[ would under-count the row and over-pad the box.
    assert_eq!(render::visible_width("a\u{1b}[31b"), 6);
}

#[test]
fn token_and_window_labels_truncate_rather_than_round() {
    let cases: [(u64, &str); 13] = [
        (0, "0"),
        (400, "400"),
        (999, "999"),
        (1_000, "1.0K"),
        (2_749, "2.7K"),
        (999_999, "999.9K"),
        (1_000_000, "1.0M"),
        (1_250_000, "1.2M"),
        // The B tier. The invariant each ladder step exists to hold is that no
        // unit ever renders a four-digit mantissa, so the pair either side of
        // the boundary is the case that matters: 999.9M must not become
        // 1000.0M.
        (999_999_999, "999.9M"),
        (1_000_000_000, "1.00B"),
        (1_234_567_890, "1.23B"),
        // B is the only tier with two decimals, so it is the only one that can
        // lose a leading zero in the fractional part. Without the `{:02}` pad
        // this renders `1.5B` — off by a factor of ten.
        (1_050_000_000, "1.05B"),
        (999_999_999_999, "999.99B"),
    ];
    for (input, expected) in cases {
        assert_eq!(render::format_tokens(input), expected, "[{input}]");
    }
    assert_eq!(render::format_window_label(200_000), "200K");
    assert_eq!(render::format_window_label(1_000_000), "1M");
}

#[test]
fn cost_color_turns_over_fifty_cents_exactly_at_the_boundary() {
    let cases: [(&str, &str, bool); 14] = [
        ("1.2345", "$1.2345", true),
        ("0.5", "$0.5000", false),
        ("0.50", "$0.5000", false),
        ("0.5000001", "$0.5000", true),
        ("0.6", "$0.6000", true),
        ("0.4999", "$0.4999", false),
        ("-3", "$-3.0000", false),
        ("garbage", "$0.0000", false),
        // The grouped tier, and the precision switch that rides along with it.
        // 999.9999 is the last four-decimal value; 1000 is the first grouped
        // one, so this pair pins where the display changes shape.
        ("999.9999", "$999.9999", true),
        ("1000", "$1,000.00", true),
        ("1123.45", "$1,123.45", true),
        // Grouping is on threes from the right, so a value with a leading digit
        // group of one is the case an off-by-one comma placement fails.
        ("1234567.891", "$1,234,567.89", true),
        ("87654.321", "$87,654.32", true),
        // Magnitude decides grouping, but the sign still suppresses the warning
        // colour — `decimal_exceeds_half` rejects anything negative outright.
        ("-1234.5", "$-1,234.50", false),
    ];
    for (raw, formatted, over) in cases {
        assert_eq!(
            render::format_cost(raw),
            (formatted.to_string(), over),
            "[{raw}]"
        );
    }
}

#[test]
fn model_ids_prettify_without_losing_the_variant_marker_tier() {
    let cases: [(&str, &str); 6] = [
        ("claude-sonnet-5", "Sonnet 5"),
        ("claude-opus-4-8", "Opus 4.8"),
        ("claude-haiku-4-5-20251001", "Haiku 4.5"),
        // The family match is a prefix, so a marker suffix still resolves.
        ("claude-opus-5[1m]", "Opus 5"),
        ("fable-5", "Fable 5"),
        ("some-internal-build", "some-internal-build"),
    ];
    for (id, expected) in cases {
        assert_eq!(render::prettify_model_id(id), expected, "[{id}]");
    }
}

#[test]
fn the_bar_fills_from_a_clamped_rounded_percentage() {
    let filled = |pct| {
        render::render_bar(pct, "")
            .chars()
            .filter(|c| *c == '█')
            .count()
    };
    assert_eq!(filled(0), 0);
    assert_eq!(filled(42), 13, "30 * 42 + 50, integer-divided by 100");
    assert_eq!(filled(100), 30);
    assert_eq!(filled(-5), 0, "a negative percentage clamps to empty");
    assert_eq!(filled(300), 30, "an over-100 percentage clamps to full");
}

#[test]
fn rate_windows_render_burn_against_the_injected_clock() {
    // 40% used with 25% of a 5h window elapsed: 15 points ahead of linear.
    let now = 1_767_225_600;
    let resets = now + 13_500; // 3h45m left of 5h
    let rendered = render::format_rate_window("5h", Some(40.0), &resets.to_string(), 18_000, now);
    assert_eq!(
        strip_ansi(&rendered),
        "5h 40% ⇡15% (3h45m)",
        "rendered: {rendered:?}"
    );

    let under = render::format_rate_window("5h", Some(10.0), &resets.to_string(), 18_000, now);
    assert_eq!(strip_ansi(&under), "5h 10% ⇣15% (3h45m)");

    assert_eq!(
        render::format_rate_window("7d", None, "", 604_800, now),
        "",
        "an absent percentage renders no window at all"
    );
    assert_eq!(
        strip_ansi(&render::format_rate_window(
            "5h",
            Some(40.0),
            "",
            18_000,
            now
        )),
        "5h 40%",
        "no resets_at means no burn arrow and no countdown"
    );
}

/// `resets_at` reaches both consumers through the payload, and Claude Code is
/// free to send it as a JSON number. Reading it with `as_str` alone dropped
/// that form, and the test above never noticed because it hands
/// `format_rate_window` a string directly, skipping the accessor where the
/// value is actually lost.
///
/// It costs two things at once, one visible and one not: the cost row loses its
/// burn arrow and countdown, and the rate-limit alert stops re-arming — the
/// latch decides that by comparing the stored `resets_at` against the current
/// one, and two empty strings never differ, so the alert fires once per install
/// and then never again.
#[test]
fn a_numeric_resets_at_is_read_the_same_as_a_quoted_one() {
    let now = 1_767_225_600;
    let resets = now + 13_500; // 3h45m left of a 5h window

    let payload_with = |value: String| {
        Payload::parse(&format!(
            r#"{{"rate_limits":{{"five_hour":{{"used_percentage":40,"resets_at":{value}}}}}}}"#
        ))
        .expect("the fixture is a JSON object")
    };
    let numeric = payload_with(resets.to_string());
    let quoted = payload_with(format!("\"{resets}\""));
    let iso = payload_with("\"2026-01-01T05:00:00Z\"".to_string());

    let window = |p: &Payload| {
        strip_ansi(&render::format_rate_window(
            "5h",
            p.rate_five_hour_percentage(),
            &p.rate_five_hour_resets_at(),
            18_000,
            now,
        ))
    };

    assert_eq!(
        window(&numeric),
        "5h 40% ⇡15% (3h45m)",
        "a numeric resets_at lost the burn arrow and the countdown"
    );
    assert_eq!(
        window(&numeric),
        window(&quoted),
        "the quoted and numeric spellings must render identically"
    );
    // Unchanged on purpose: neither script could parse an ISO-8601 instant
    // either, so rendering nothing for it is the behaviour being preserved.
    assert_eq!(
        window(&iso),
        "5h 40%",
        "an ISO-8601 resets_at renders no arrow, matching the scripts"
    );

    assert!(
        !numeric.rate_five_hour_resets_at().is_empty(),
        "the latch compares this across ticks; empty can never differ from empty, \
         so the rate alert would never re-arm"
    );
}

/// The same strictness cost the effort segment. Agent frontmatter may write the
/// level as an integer, and both scripts rendered it: bash took `jq -r`'s `3`
/// and fell to its `*)` colour arm, PowerShell found `if ($effortLevel)` truthy
/// for a number and fell to `default`. Reading it with `as_str` dropped the
/// whole segment — and made `effort_color`'s catch-all arm, which exists for
/// precisely these values, unreachable from any payload.
#[test]
fn a_numeric_effort_level_renders_like_the_scripts() {
    let payload_with = |value: &str| {
        Payload::parse(&format!(r#"{{"effort":{{"level":{value}}}}}"#))
            .expect("the fixture is a JSON object")
    };

    assert_eq!(
        payload_with("3").effort_level(),
        "3",
        "a numeric effort level was dropped, taking the segment with it"
    );
    assert_eq!(payload_with("\"high\"").effort_level(), "high");
    assert_eq!(
        render::effort_color("3"),
        render::WHITE,
        "an unrecognised level takes the catch-all colour, as both scripts did"
    );

    // Unchanged: a wrong-typed value is still absent, because that is where the
    // two scripts genuinely disagreed and there was no behaviour to preserve.
    assert_eq!(payload_with("{\"a\":1}").effort_level(), "");
}

#[test]
fn elapsed_and_countdown_formats_match_their_scales() {
    assert_eq!(render::format_elapsed(45_000.0), "45s");
    assert_eq!(render::format_elapsed(654_000.0), "10m54s");
    assert_eq!(render::format_elapsed(7_500_000.0), "2h05m");

    assert_eq!(
        render::format_duration(0),
        "",
        "an expired window shows nothing"
    );
    assert_eq!(render::format_duration(-1), "");
    assert_eq!(render::format_duration(300), "5m");
    assert_eq!(render::format_duration(3_600), "1h");
    assert_eq!(render::format_duration(8_100), "2h15m");
    assert_eq!(render::format_duration(97_200), "1d3h");
}

#[test]
fn a_cwd_under_home_collapses_to_a_tilde_and_others_to_two_segments() {
    let cases: [(&str, Option<&str>, &str); 6] = [
        ("/home/dev", Some("/home/dev"), "~"),
        ("/home/dev/src/thing", Some("/home/dev"), "~/src/thing"),
        ("/var/repo/work", Some("/home/dev"), ".../repo/work"),
        ("/tmp", Some("/home/dev"), "/tmp"),
        // Both separators resolve through one implementation.
        ("C:\\Users\\dev\\src", Some("C:\\Users\\dev"), "~/src"),
        ("D:\\a\\b\\c", None, ".../b/c"),
    ];
    for (cwd, home, expected) in cases {
        assert_eq!(render::format_cwd(cwd, home), expected, "[{cwd}]");
    }
}

/// The two `format_cwd` shapes where the port and `windows/statusline.ps1`
/// disagree, both resolved to the port's behaviour and recorded in the plan's
/// Scope Boundaries.
///
/// Neither is reachable from a pinned fixture — Claude Code supplies native
/// backslash paths in canonical casing on Windows — so each is asserted against
/// a literal here, which is what a recorded divergence needs. `breaks` is carried in the table
/// rather than in prose because a resolution that stops naming the behaviour it
/// broke is a resolution nobody can re-evaluate later.
#[test]
fn resolved_cwd_divergences_keep_the_ports_behaviour() {
    struct Divergence {
        name: &'static str,
        cwd: &'static str,
        home: &'static str,
        expected: &'static str,
        /// What `windows/statusline.ps1:287` renders for the same input.
        breaks: &'static str,
    }

    let cases = [
        Divergence {
            name: "forward-slash cwd under home",
            cwd: "C:/Users/me/src/thing",
            home: "C:\\Users\\me",
            expected: "~/src/thing",
            // The script normalises the cwd's separators but never
            // $USERPROFILE's own, so neither StartsWith arm matches.
            breaks: ".../src/thing",
        },
        Divergence {
            name: "home prefix differing only in case",
            cwd: "c:\\users\\me\\src\\thing",
            home: "C:\\Users\\me",
            expected: ".../src/thing",
            // The script compares with OrdinalIgnoreCase.
            breaks: "~/src/thing",
        },
    ];

    let mut failures = Failures::default();
    for case in cases {
        let actual = render::format_cwd(case.cwd, Some(case.home));
        failures.check(case.name, actual == case.expected, || {
            format!("expected `{}`, got `{actual}`", case.expected)
        });
        // The other half of the claim: this is still a divergence. If the port
        // starts agreeing with the script, the resolution is stale and the
        // record above needs revisiting rather than silently passing.
        failures.check(case.name, case.expected != case.breaks, || {
            "the recorded resolution no longer differs from what it breaks".to_string()
        });
    }
    failures.assert_empty("resolved cwd divergences");
}

#[test]
fn a_subagent_row_scrubs_its_untrusted_fields_at_the_sink() {
    // the escape arrives through the row, which is what every source
    // path -- live feed, read-back cache, transcript fallback -- funnels into.
    let row = Row {
        used: 18_000,
        window: 200_000,
        model: "claude-sonnet-5".into(),
        display: "hostile\u{1b}[31m|title".into(),
        effort: String::new(),
        done: false,
    };
    let rendered = render::format_subagent_row(&row);
    let body = strip_ansi(&rendered);
    assert!(
        !rendered.contains("\u{1b}[31m"),
        "the planted escape must not reach the terminal: {rendered:?}"
    );
    assert!(
        body.contains("hostile") && body.contains("title") && !body.contains('|'),
        "control bytes and the column separator become spaces: {body:?}"
    );
    assert!(
        body.contains("9%") && body.contains("18.0K/200K"),
        "{body:?}"
    );
    assert!(body.ends_with("○ working"), "{body:?}");
}

#[test]
fn a_long_subagent_title_is_ellipsised_by_characters() {
    let row = Row {
        used: 0,
        window: 200_000,
        model: String::new(),
        // 45 astral characters: truncating by bytes would split one.
        display: "🙂".repeat(45),
        effort: String::new(),
        done: true,
    };
    let body = strip_ansi(&render::format_subagent_row(&row));
    assert!(body.contains(&format!("{}…", "🙂".repeat(39))), "{body:?}");
    assert!(body.ends_with("✓ done"), "{body:?}");
}

/// The whole point of the latch is one notification per crossing, so the
/// re-arm and the window rollover matter as much as the fire.
#[test]
fn context_and_rate_alerts_fire_once_per_crossing() {
    let fresh = || LatchState::Usable(Latch::default());

    let crossed = decide(fresh(), 75, 70, 0, 80, "");
    assert_eq!(crossed.alerts.len(), 1, "crossing fires once");
    assert_eq!(crossed.alerts[0].event, "context_high");
    assert_eq!(crossed.alerts[0].value, 75);
    assert!(crossed.latch.context_high && crossed.changed);

    let still_above = decide(LatchState::Usable(crossed.latch.clone()), 78, 70, 0, 80, "");
    assert!(
        still_above.alerts.is_empty() && !still_above.changed,
        "staying above the threshold must not re-fire or rewrite the latch"
    );

    let dropped = decide(LatchState::Usable(crossed.latch.clone()), 40, 70, 0, 80, "");
    assert!(
        dropped.alerts.is_empty() && !dropped.latch.context_high && dropped.changed,
        "dropping below re-arms without notifying"
    );

    let recrossed = decide(LatchState::Usable(dropped.latch), 90, 70, 0, 80, "");
    assert_eq!(
        recrossed.alerts.len(),
        1,
        "the next crossing fires again -- otherwise the alert is once per session"
    );
}

#[test]
fn a_rate_window_rollover_rearms_the_rate_latch() {
    let latched = Latch {
        context_high: false,
        rate_limit: true,
        rate_resets_at: "1767225600".into(),
    };
    let same = decide(
        LatchState::Usable(latched.clone()),
        0,
        70,
        95,
        80,
        "1767225600",
    );
    assert!(same.alerts.is_empty(), "the same window stays latched");

    let rolled = decide(LatchState::Usable(latched), 0, 70, 95, 80, "1767243600");
    assert_eq!(
        rolled.alerts.len(),
        1,
        "a new window is a new crossing, or one busy window silences the rest"
    );
    assert_eq!(rolled.latch.rate_resets_at, "1767243600");
}

#[test]
fn an_unreadable_latch_suppresses_rather_than_spams() {
    // a state file that exists but cannot be read reads as
    // already-notified. Treated as "never notified" it would re-fire on every
    // refresh for as long as the collision lasted.
    let decision = decide(LatchState::Unusable, 99, 70, 99, 80, "1767225600");
    assert!(decision.alerts.is_empty());
    assert!(
        !decision.changed,
        "nothing is written either -- the next refresh reads a whole file"
    );
}

#[test]
fn the_latch_serialises_with_the_field_names_the_scripts_wrote() {
    let latch = Latch {
        context_high: true,
        rate_limit: false,
        rate_resets_at: "1767225600".into(),
    };
    let json = notify_state::latch_json(&latch);
    assert_eq!(
        json,
        "{\"notified_context_high\":true,\"notified_rate_limit\":false,\"last_rate_resets_at\":\"1767225600\"}"
    );

    let dir = scratch_dir("latch-roundtrip");
    let path = dir.join("statusline-notify-s.json");
    std::fs::write(&path, &json).expect("failed to write the latch");
    assert_eq!(
        notify_state::read_latch(&path),
        LatchState::Usable(latch),
        "what the reader recovers is what the writer stored"
    );
}

/// The write side owns making the round trip survivable. The scripts escaped
/// backslashes and quotes but left control bytes raw, so a `resets_at`
/// carrying one wrote a latch `read_latch` could never parse again — and the
/// rewrite that would repair the file only happens when the latch is usable,
/// so that session's notifications stayed dead.
#[test]
fn a_control_byte_in_resets_at_still_round_trips() {
    let latch = Latch {
        context_high: false,
        rate_limit: true,
        rate_resets_at: "17672\u{1}25600".into(),
    };
    let json = notify_state::latch_json(&latch);

    let dir = scratch_dir("latch-control-byte");
    let path = dir.join("statusline-notify-s.json");
    std::fs::write(&path, &json).expect("failed to write the latch");
    assert_eq!(
        notify_state::read_latch(&path),
        LatchState::Usable(latch),
        "a control byte in resets_at produced a latch the reader rejects — \
         and Unusable is permanent, because the repairing rewrite is gated \
         on usable"
    );
}

#[test]
fn degraded_input_renders_the_notice_and_touches_no_state() {
    // The bad-JSON path must not write the token record or the latch: a
    // tick that could not be understood overwriting the last good one is how a
    // single malformed refresh would erase a session's deltas.
    let dir = scratch_dir("degraded-render");
    let dir_str = dir.to_str().expect("scratch path is not UTF-8");
    for payload in ["", "not json", "[1,2,3]"] {
        let run = run_bin(
            &["statusline"],
            payload,
            &[("TMPDIR", dir_str), ("TEMP", dir_str)],
        );
        assert_eq!(run.code, Some(0), "[{payload:?}] must exit 0");
        assert_eq!(run.stderr, "", "[{payload:?}] must write nothing to stderr");
        assert_eq!(
            run.stdout, "\u{1b}[31m[statusline: bad JSON]\u{1b}[0m",
            "[{payload:?}]"
        );
    }
    let stray: Vec<String> = std::fs::read_dir(&dir)
        .expect("failed to read the scratch dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(stray.is_empty(), "degraded input wrote state: {stray:?}");
}

#[test]
fn the_box_pads_every_row_to_one_width() {
    // The failure this catches is the one the bash captures shipped: rows
    // padded to a width computed differently from the width they occupy, so
    // the frame's right edge is ragged. Every line must be identical width.
    let payload = Payload::parse(
        r#"{"session_id":"render-test","workspace":{"current_dir":"/var/repo/work"},
            "model":{"display_name":"Opus 5","id":"claude-opus-5"},
            "context_window":{"context_window_size":200000,"used_percentage":42.4,
                              "total_input_tokens":85000},
            "effort":{"level":"high"},"cost":{"total_cost_usd":1.2345}}"#,
    )
    .expect("the payload should parse");

    let rows = [Row {
        used: 18_000,
        window: 200_000,
        model: "claude-sonnet-5".into(),
        // CJK and emoji in one row: the case the width helper exists for.
        display: "日本語のタスク 🙂".into(),
        effort: "xhigh".into(),
        done: false,
    }];
    let out = render::render(&render::Inputs {
        payload: &payload,
        home: Some("/home/dev"),
        git: None,
        scan: None,
        record: None,
        subagents: &rows,
        now: 1_767_225_600,
    });

    let widths: Vec<usize> = out.lines().map(render::visible_width).collect();
    let first = widths[0];
    assert!(
        widths.iter().all(|w| *w == first),
        "ragged box: widths {widths:?}\n{out}"
    );
    assert!(
        !out.ends_with('\n'),
        "the render carries no trailing newline"
    );
    let plain = strip_ansi(&out);
    assert!(plain.contains(".../repo/work"), "{plain}");
    assert!(plain.contains("Opus 5 · high effort"), "{plain}");
    assert!(plain.contains("42%"), "42.4 rounds to 42: {plain}");
}

// ---------------------------------------------------------------------------
// Statusline fixture replay
// ---------------------------------------------------------------------------

/// The cases whose three captures do not agree, and the platform whose
/// behaviour the port keeps.
///
/// Each is a divergence resolved and written down in the plan's Scope
/// Boundaries. Listing the *winner* rather than skipping the case keeps the
/// fixture doing work: the replay asserts the port matches the chosen platform
/// and, just as importantly, that it still differs from the one it breaks. A
/// skip would let a resolution silently stop being true.
const STATUSLINE_DIVERGENCES: [(&str, &str, &str); 3] = [
    (
        "git-unborn-head-staged",
        "linux",
        "unborn HEAD renders no git segment; Windows substitutes the literal HEAD",
    ),
    (
        "payload-empty",
        "linux",
        "empty stdin renders the bad-JSON notice; Windows renders a defaults-only box",
    ),
    (
        "feed-fresh",
        "windows",
        "one row per running task; bash's greedy id strip renders a phantom done row",
    ),
];

fn harness_git_env(states: &serde_json::Value) -> Vec<(String, String)> {
    states["git_env"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
        .collect()
}

fn harness_git_config(states: &serde_json::Value) -> Vec<String> {
    states["git_config"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect()
}

/// Runs one pinned git command. Every invocation carries the identity, the
/// dates and the config, because a single one that does not makes the commit
/// hashes drift and the detached-HEAD fixture stops reproducing.
fn harness_git(states: &serde_json::Value, cwd: Option<&Path>, args: &[&str]) -> bool {
    let mut cmd = Command::new("git");
    if let Some(cwd) = cwd {
        cmd.arg("-C").arg(cwd);
    }
    for c in harness_git_config(states) {
        cmd.arg("-c").arg(c);
    }
    cmd.args(args)
        .envs(harness_git_env(states))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Builds one `states.json` git state, the same steps the capture drivers run.
///
/// Ported rather than shelled out to: the replay has to construct the identical
/// repository on every published target, and `states.json` is the single
/// description both drivers already read.
fn build_git_state(states: &serde_json::Value, name: &str, work: &Path, remote: &Path) {
    let state = states["git_states"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("unknown git state `{name}`"));

    for step in state["steps"].as_array().into_iter().flatten() {
        let parts: Vec<&str> = step
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        let Some((verb, rest)) = parts.split_first() else {
            continue;
        };
        match *verb {
            "git" => {
                assert!(
                    harness_git(states, Some(work), rest),
                    "git step failed in state `{name}`: {rest:?}"
                );
            }
            "write" => {
                let target = work.join(rest[0]);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).expect("failed to create the parent directory");
                }
                // Exact bytes, LF preserved: this content reaches a blob hash,
                // and through it the commit hash the detached-HEAD state
                // renders. Both drivers have got this wrong once already.
                std::fs::write(&target, rest[1].as_bytes()).expect("failed to write the file");
            }
            "mkdir" => {
                std::fs::create_dir_all(work.join(rest[0]))
                    .expect("failed to create the directory");
            }
            "remote-track" | "remote-only" => {
                assert!(
                    harness_git(
                        states,
                        None,
                        &["init", "--bare", "-b", "main", &remote.to_string_lossy()]
                    ),
                    "bare remote init failed in state `{name}`"
                );
                assert!(
                    harness_git(
                        states,
                        Some(work),
                        &["remote", "add", "origin", &remote.to_string_lossy()]
                    ),
                    "remote add failed in state `{name}`"
                );
                if *verb == "remote-track" {
                    assert!(
                        harness_git(states, Some(work), &["push", "-u", "origin", "HEAD"]),
                        "push failed in state `{name}`"
                    );
                }
            }
            other => panic!("unknown step verb `{other}` in state `{name}`"),
        }
    }
}

/// Placeholders carry forward slashes on every platform, which is why the
/// capture drivers never had to escape a Windows backslash into JSON.
fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn substitute(text: &str, home: &Path, tmp: &Path, work: &Path, session: &str) -> String {
    text.replace("{HOME}", &slashed(home))
        .replace("{TMP}", &slashed(tmp))
        .replace("{REPO}", &slashed(work))
        .replace("{SESSION}", session)
}

/// Every captured statusline case, replayed against the port.
///
/// This is the parity gate. The unit tests above check the renderer's pieces;
/// only this compares whole rendered bytes against what the scripts actually
/// produced, for the whole `docs/performance.md` §4 state matrix.
#[test]
fn rendered_output_matches_the_captured_fixtures() {
    let root = repo_file("tests/fixtures/statusline");
    let Ok(entries) = std::fs::read_dir(&root) else {
        println!("no statusline fixtures captured yet");
        return;
    };
    let states: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_file("tests/harness/states.json"))
            .expect("states.json is readable"),
    )
    .expect("states.json parses");
    let cases_table: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo_file("tests/harness/cases.json"))
            .expect("cases.json is readable"),
    )
    .expect("cases.json parses");

    let mut failures = Failures::default();
    let mut checked = 0usize;

    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let case = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("case.json")).unwrap_or_default(),
        )
        .unwrap_or(serde_json::Value::Null);

        let session = meta["session_id"].as_str().unwrap_or("");
        let pinned_now = meta["clock"].as_i64().unwrap_or(0);

        // --- isolated roots, laid out exactly as the drivers lay them out ---
        let case_root = scratch_dir(&format!("statusline-fixture-{case}"));
        let home = case_root.join("home");
        let tmp = case_root.join("tmp");
        let work = case_root.join("repo").join("work");
        let remote = case_root.join("repo").join("remote.git");
        for d in [&home.join(".claude"), &tmp, &work] {
            std::fs::create_dir_all(d).expect("failed to create an isolated root");
        }

        let notify_config = meta["notify_config"]
            .as_str()
            .unwrap_or("configs/default.json");
        std::fs::copy(
            repo_file(&format!("tests/harness/{notify_config}")),
            home.join(".claude").join("notify-config.json"),
        )
        .expect("failed to stage notify-config.json");

        if let Some(state) = meta["git_state"].as_str() {
            build_git_state(&states, state, &work, &remote);
        }

        // --- staged state files, with their intended modification times -----
        // The scripts read the real wall clock, so the harness materialised an
        // mtime and recorded it as an offset from the pinned clock. Here the
        // offset is applied to the clock instead, which is the whole reason
        // The Clock covers filesystem timestamps as well as `now`.
        let mut clock = TestClock::at(pinned_now);
        let defaults = &cases_table["defaults"]["inputs_by_component"]["statusline"];
        let staged = defaults
            .as_array()
            .into_iter()
            .flatten()
            .chain(meta["inputs"].as_array().into_iter().flatten());

        for input in staged {
            let target = input["target"].as_str().unwrap_or_default();
            let content = input["content"].as_str().unwrap_or_default();
            let offset = input["mtime_offset"].as_i64().unwrap_or(0);
            let abs = PathBuf::from(substitute(target, &home, &tmp, &work, session));
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent).expect("failed to create an input's directory");
            }
            std::fs::copy(repo_file(&format!("tests/harness/{content}")), &abs)
                .unwrap_or_else(|e| panic!("failed to stage {target}: {e}"));
            clock = clock.with_mtime(&abs, pinned_now + offset);
        }

        let payload_rel = meta["payload"].as_str().unwrap_or_default();
        let payload_raw =
            std::fs::read_to_string(repo_file(&format!("tests/harness/{payload_rel}")))
                .unwrap_or_default();
        let payload = substitute(&payload_raw, &home, &tmp, &work, session);

        let roots = cmd_statusline::Roots {
            home: Some(home.clone()),
            temp: claude_statusline::session::StateRoot::inherited(tmp.clone()),
        };
        let rendered = cmd_statusline::run(&clock, &roots, &payload);

        // The drivers refuse to write a fixture still containing a real path;
        // the replay refuses to compare one. A leak here would mean the render
        // is carrying machine-local state into what is supposed to be a
        // portable golden file.
        for (needle, label) in [
            (slashed(&work), "{REPO}"),
            (slashed(&tmp), "{TMP}"),
            (slashed(&home), "{HOME}"),
        ] {
            failures.check(&case, !rendered.contains(&needle), || {
                format!("rendered output leaked a machine-local path where {label} belongs")
            });
        }

        let resolution = STATUSLINE_DIVERGENCES.iter().find(|(c, _, _)| *c == case);
        // The winner's *bytes*, not its name. A divergence is between
        // behaviours, and two platforms can share one — resolving to `linux`
        // resolves to macOS too, because their captures are identical. Deriving
        // the losing set from the fixtures rather than naming it keeps that
        // from having to be restated (and mis-stated) per case.
        let winning_bytes = resolution.and_then(|(_, winner, _)| {
            std::fs::read_to_string(dir.join("expected").join(format!("{winner}.txt"))).ok()
        });

        for platform in ["linux", "macos", "windows"] {
            let Ok(expected) =
                std::fs::read_to_string(dir.join("expected").join(format!("{platform}.txt")))
            else {
                continue;
            };
            let label = format!("{case}/{platform}");

            match resolution {
                // An agreeing case: the port must equal every platform.
                None => {
                    checked += 1;
                    failures.check(&label, rendered == expected, || {
                        first_difference(&expected, &rendered)
                    });
                }
                // A resolved divergence: equal to every platform that shares
                // the winning behaviour, and still different from every one
                // that does not. The second half is what stops a resolution
                // from quietly becoming a no-op.
                Some((_, _, why)) => {
                    checked += 1;
                    if winning_bytes.as_deref() == Some(expected.as_str()) {
                        failures.check(&label, rendered == expected, || {
                            format!("{why}\n{}", first_difference(&expected, &rendered))
                        });
                    } else {
                        failures.check(&label, rendered != expected, || {
                            format!("{why} -- but the port matched {platform}, so the divergence is gone and the resolution is stale")
                        });
                    }
                }
            }
        }
    }

    println!("replayed {checked} captured platform fixture(s)");
    assert!(checked > 0, "no statusline fixtures were replayed");
    failures.assert_empty("statusline fixture equivalence");
}

/// Points at the first line that differs, with both sides escaped, rather than
/// dumping two boxes of escape codes and leaving the reader to diff them.
fn first_difference(expected: &str, got: &str) -> String {
    let e: Vec<&str> = expected.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    for i in 0..e.len().max(g.len()) {
        let (le, lg) = (e.get(i), g.get(i));
        if le != lg {
            return format!(
                "line {} differs\n    script: {:?}\n    port:   {:?}\n  (script has {} line(s), port has {})",
                i + 1,
                le.unwrap_or(&"<missing>"),
                lg.unwrap_or(&"<missing>"),
                e.len(),
                g.len()
            );
        }
    }
    "line contents agree; the difference is the trailing newline".to_string()
}

/// The self-check's expectation is compiled in from a fixture the case
/// table also asserts, so the two cannot drift.
///
/// This test is the joint. `rendered_output_matches_the_captured_fixtures`
/// proves the renderer reproduces the `self-check` case; this proves the bytes
/// the *binary* carries are that same case's. Without it the `include_str!`
/// could be repointed at a stale or hand-edited file and everything would still
/// pass — which is exactly the failure this guards against: a self-check that drifts
/// from the renderer starts refusing every install.
#[test]
fn the_self_check_expectation_is_the_captured_fixture() {
    let dir = repo_file("tests/fixtures/statusline/self-check/expected");
    let mut compared = 0usize;
    for platform in ["linux", "macos", "windows"] {
        let Ok(captured) = std::fs::read_to_string(dir.join(format!("{platform}.txt"))) else {
            continue;
        };
        compared += 1;
        assert_eq!(
            claude_statusline::SELF_CHECK_FIXTURE,
            captured,
            "the compiled-in expectation is not what {platform} captured"
        );
    }
    assert!(
        compared == 3,
        "the self-check case must be captured on all three platforms; found {compared}"
    );
}

/// The self-check has to fail a broken renderer, which means it must actually
/// render. A stub that returns its own expectation passes forever.
#[test]
fn the_self_check_renders_the_box_rather_than_echoing_a_literal() {
    let run = run_bin(&["self-check"], "", &[]);
    assert_eq!(run.code, Some(0));

    let plain = strip_ansi(&run.stdout);
    assert!(
        plain.lines().count() >= 8 && plain.contains('┏') && plain.contains('┛'),
        "self-check output is not a rendered box: {plain:?}"
    );
    assert!(
        plain.contains(".../repo/work"),
        "the payload's placeholder should render as a truncated path: {plain:?}"
    );
    assert!(
        !plain.contains(" on "),
        "the hermetic case must render no git segment, whatever repository the \
         check happens to run inside: {plain:?}"
    );

    // The real working directory must not reach the output. Running the test
    // from inside this repository is precisely the situation that would leak.
    let cwd = std::env::current_dir().expect("the test process has a working directory");
    let leaf = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    assert!(
        !plain.contains(&leaf),
        "self-check leaked the real working directory `{leaf}`: {plain:?}"
    );
}

/// The rescan skip, and precisely what it costs.
///
/// An unchanged transcript is not read at all: everything the tokens and model
/// rows render is already in the record. The statusline measurements found the
/// unconditional rescan at ~50 ms on 8 MB — invisible next to PowerShell's
/// ~124 ms interpreter floor, four times bash's entire tick.
///
/// The second half of this test is the part worth reading. It rewrites the
/// transcript to *different content at the identical byte length* and pins the
/// same mtime, and asserts the render does not change. That is the trade, made
/// executable rather than described: `(mtime, size)` is the freshness signal,
/// and a same-length rewrite defeats it. Transcripts are append-only JSONL, so
/// this is close to unreachable in practice — but it is not nothing, and a
/// future reader deserves to see it fail deliberately rather than discover it.
#[test]
fn an_unchanged_transcript_is_not_rescanned() {
    let root = scratch_dir("rescan-skip");
    let home = root.join("home");
    let temp = root.join("tmp");
    let transcript = home.join(".claude/projects/fixtures/transcript.jsonl");
    std::fs::create_dir_all(transcript.parent().expect("the transcript has a parent"))
        .expect("failed to create the transcript directory");
    std::fs::create_dir_all(&temp).expect("failed to create the temp root");

    let original = std::fs::read_to_string(repo_file("tests/harness/inputs/transcript.jsonl"))
        .expect("the pinned transcript is readable");
    std::fs::write(&transcript, &original).expect("failed to stage the transcript");

    let payload = format!(
        r#"{{"session_id":"rescan","workspace":{{"current_dir":"/a/b/c"}},
             "model":{{"display_name":"Opus 5","id":"claude-opus-5"}},
             "transcript_path":"{}"}}"#,
        transcript.to_string_lossy().replace('\\', "/")
    );
    let roots = cmd_statusline::Roots {
        home: Some(home.clone()),
        temp: claude_statusline::session::StateRoot::inherited(temp.clone()),
    };
    let clock = TestClock::at(1_767_225_600).with_mtime(&transcript, 1_767_225_000);

    let first = cmd_statusline::run(&clock, &roots, &payload);
    assert!(
        strip_ansi(&first).contains("in 2.7K"),
        "the first render must actually scan the transcript: {}",
        strip_ansi(&first)
    );

    // Same bytes, same everything: the record is now warm.
    let second = cmd_statusline::run(&clock, &roots, &payload);
    assert_eq!(second, first, "a warm record must render identically");

    // Different content, identical length, identical mtime. The scan is
    // skipped, so the new numbers are not seen — the accepted cost.
    let rewritten = original.replacen("\"input_tokens\":1200", "\"input_tokens\":9900", 1);
    assert_eq!(
        rewritten.len(),
        original.len(),
        "the rewrite must be the same length or it is not testing the hazard"
    );
    assert_ne!(rewritten, original);
    std::fs::write(&transcript, &rewritten).expect("failed to rewrite the transcript");

    let third = cmd_statusline::run(&clock, &roots, &payload);
    assert_eq!(
        third, first,
        "a same-length, same-mtime rewrite is invisible: this is the documented \
         cost of skipping the rescan, not a bug to be fixed silently"
    );

    // A length change is seen immediately, which is what makes the trade
    // acceptable: real transcripts only ever grow.
    std::fs::write(&transcript, format!("{original}{}", &original[..200]))
        .expect("failed to grow the transcript");
    let grown_mtime = 1_767_225_100;
    let grown_clock = TestClock::at(1_767_225_600).with_mtime(&transcript, grown_mtime);
    let fourth = cmd_statusline::run(&grown_clock, &roots, &payload);
    assert_ne!(fourth, first, "a transcript that grew must be rescanned");
}

// ---------------------------------------------------------------------------
// Click-to-focus: record, key and capture
// ---------------------------------------------------------------------------
//
// The focus record is the contract between the process that raises a toast
// and the process that handles the click, possibly hours later and with none
// of the session's environment. The cases below pin its grammar in both
// directions: what capture writes, and what the loader refuses.

/// A guarded state root under a fresh scratch directory, as the tick resolves
/// it on a clean machine.
fn guarded_root(case: &str) -> (PathBuf, claude_statusline::session::StateRoot) {
    let dir = scratch_dir(case);
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&tmp).expect("tmp");
    let root = claude_statusline::session::state_dir_in(&tmp);
    assert!(root.is_guarded(), "a clean temp root must resolve guarded");
    (dir, root)
}

fn env_vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: std::collections::BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k: &str| map.get(k).cloned()
}

fn observation(random: Option<[u8; 24]>) -> Observation {
    Observation {
        anchor: Some(Anchor {
            pid: 42,
            start: 7,
            boot_id: Some("9d1c-boot".to_string()),
        }),
        terminal: Some((41, 6)),
        tty: Some("/dev/ttys003".to_string()),
        cwd: "/repo/work".to_string(),
        window: None,
        random,
    }
}

fn sample_record(session: &str) -> Record {
    Record {
        session: session.to_string(),
        token: "A".repeat(32),
        captured_at: 1,
        anchor: Anchor {
            pid: 42,
            start: 7,
            boot_id: Some("9d1c-boot".to_string()),
        },
        cwd: "/repo/work".to_string(),
        debug: false,
        identity: Identity::default(),
    }
}

/// The names of every `statusline-focus-*` file under `dir`.
fn focus_files(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("statusline-focus-"))
                .collect()
        })
        .unwrap_or_default()
}

/// The state directory a fresh-process run created under `tmp`, if any.
fn state_dir_under(tmp: &Path) -> Option<PathBuf> {
    std::fs::read_dir(tmp)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.is_dir()
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("claude-statusline-"))
        })
}

/// Only a toast gets a click key: a visual alert writes the record inside the
/// guarded state directory, a sound-only alert writes nothing.
#[test]
fn a_visual_alert_writes_the_focus_record_and_a_sound_only_alert_does_not() {
    let mut failures = Failures::default();
    for (name, config, want_record) in [
        ("visual", "{}", true),
        (
            "sound-only",
            r#"{"context_high":{"visual":false},"rate_limit":{"visual":false}}"#,
            false,
        ),
    ] {
        let dir = scratch_dir(&format!("focus-alert-{name}"));
        let home = dir.join("home");
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(home.join(".claude")).expect("home");
        std::fs::create_dir_all(&tmp).expect("tmp");
        std::fs::write(home.join(".claude").join("notify-config.json"), config).expect("config");
        let resolved = claude_statusline::session::state_dir_in(&tmp);
        let session = format!("focus-alert-{name}");
        let payload = format!(
            r#"{{"session_id":"{session}","cwd":"{cwd}","workspace":{{"current_dir":"{cwd}"}},"model":{{"display_name":"Opus 5","id":"claude-opus-5"}},"context_window":{{"context_window_size":200000,"used_percentage":91.5}},"rate_limits":{{"five_hour":{{"used_percentage":95}}}}}}"#,
            cwd = slashed(&dir),
        );
        let roots = cmd_statusline::Roots {
            home: Some(home.clone()),
            temp: resolved.clone(),
        };
        let rendered = cmd_statusline::run(&TestClock::at(1_000), &roots, &payload);
        failures.check(name, rendered.contains('\u{250f}'), || {
            "the render produced no box".to_string()
        });
        let files = focus_files(resolved.path());
        failures.check(name, files.is_empty() != want_record, || {
            format!("focus records after the alert: {files:?}")
        });
        if want_record {
            let bytes = std::fs::read(
                resolved
                    .path()
                    .join(format!("statusline-focus-{session}.json")),
            )
            .unwrap_or_default();
            let record = Record::load(&bytes, &session, None);
            failures.check(name, record.is_some(), || {
                format!(
                    "the written record does not load: {}",
                    String::from_utf8_lossy(&bytes)
                )
            });
            if let Some(r) = record {
                failures.check(name, r.anchor.pid != 0 || r.anchor.start == 0, || {
                    "an anchor with a start time but no pid".to_string()
                });
            }
        }
        // A sanitised session id is the whole filename: nothing may land flat.
        failures.check(name, focus_files(&tmp).is_empty(), || {
            "a focus record landed flat in the temp root".to_string()
        });
    }
    failures.assert_empty("focus capture on alerts");
}

/// Every toast a session raises keeps working: the token is minted once and
/// reused while the stored record is trusted and well-formed, and replaced
/// the moment it is not.
#[test]
fn a_second_capture_keeps_the_token_and_a_bad_stored_token_is_replaced() {
    let (_dir, root) = guarded_root("focus-token-reuse");
    let var = env_vars(&[("TERM_PROGRAM", "Apple_Terminal")]);
    let first = focus::capture_with(
        &root,
        "reuse-1",
        false,
        Platform::Macos,
        &var,
        &observation(Some([1u8; 24])),
        10,
    );
    assert_eq!(first.outcome, CaptureOutcome::Written);
    let first_key = first.key.expect("first capture yields a key");

    let second = focus::capture_with(
        &root,
        "reuse-1",
        true,
        Platform::Macos,
        &env_vars(&[("TERM_PROGRAM", "iTerm.app")]),
        &observation(Some([2u8; 24])),
        20,
    );
    assert_eq!(second.outcome, CaptureOutcome::Reused);
    assert_eq!(
        second.key.as_ref().map(Key::as_string),
        Some(first_key.as_string()),
        "the second capture must keep the first token"
    );
    let path = focus::record_path(&root, "reuse-1");
    let stored = Record::load(&std::fs::read(&path).unwrap(), "reuse-1", None).unwrap();
    assert_eq!(
        stored.identity.term_program.as_deref(),
        Some("iTerm.app"),
        "the identity fields are refreshed on every capture"
    );
    assert!(stored.debug, "the debug flag is refreshed too");
    assert_eq!(stored.captured_at, 20);

    // A trusted record whose token is the wrong length is regenerated.
    let mut broken = sample_record("reuse-1");
    broken.token = "A".repeat(31);
    std::fs::write(&path, broken.to_json()).unwrap();
    let third = focus::capture_with(
        &root,
        "reuse-1",
        false,
        Platform::Macos,
        &var,
        &observation(Some([3u8; 24])),
        30,
    );
    assert_eq!(third.outcome, CaptureOutcome::Written);
    let third_key = third.key.expect("a fresh token");
    assert_ne!(third_key.token(), first_key.token());
    assert_eq!(third_key.token(), focus::encode_token(&[3u8; 24]));
}

/// The loader is a closed gate: size before parsing, then version, session
/// and token, and it tolerates only what the grammar leaves open.
#[test]
fn the_loader_refuses_what_it_must_and_tolerates_unknown_identity_keys() {
    let record = sample_record("load-1");
    let base: serde_json::Value = serde_json::from_str(&record.to_json()).unwrap();
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = base.clone();
        edit(&mut v);
        v.to_string().into_bytes()
    };
    let mut failures = Failures::default();

    let good = Record::load(
        &base.to_string().into_bytes(),
        "load-1",
        Some(&"A".repeat(32)),
    );
    failures.check("round-trip", good.as_ref() == Some(&record), || {
        format!("the record did not round-trip: {good:?}")
    });

    let cases: Vec<(&str, Vec<u8>, &str, Option<String>)> = vec![
        (
            "version-2",
            with(&|v| v["version"] = serde_json::json!(2)),
            "load-1",
            None,
        ),
        (
            "oversize",
            with(&|v| v["pad"] = serde_json::json!("x".repeat(65 * 1024))),
            "load-1",
            None,
        ),
        (
            "token-off-by-one",
            base.to_string().into_bytes(),
            "load-1",
            Some(format!("{}B", "A".repeat(31))),
        ),
        (
            "session-differs",
            base.to_string().into_bytes(),
            "load-2",
            None,
        ),
        (
            "token-wrong-length",
            with(&|v| v["token"] = serde_json::json!("A".repeat(31))),
            "load-1",
            None,
        ),
        (
            "debug-not-bool",
            with(&|v| v["debug"] = serde_json::json!("yes")),
            "load-1",
            None,
        ),
        ("not-an-object", b"[1,2,3]".to_vec(), "load-1", None),
    ];
    for (name, bytes, session, token) in cases {
        let got = Record::load(&bytes, session, token.as_deref());
        failures.check(name, got.is_none(), || {
            format!("loaded a record it must refuse: {got:?}")
        });
    }

    let tolerated = with(&|v| v["identity"]["future_terminal"] = serde_json::json!({"a": 1}));
    failures.check(
        "unknown-identity-key",
        Record::load(&tolerated, "load-1", None).is_some(),
        || "an unknown optional identity key must be ignored, not refused".to_string(),
    );
    failures.assert_empty("focus loader");
}

/// One identity field failing its grammar costs that field, named in the
/// outcome, and never the record.
#[test]
fn a_kitty_tcp_socket_is_omitted_by_name_and_the_record_still_lands() {
    let (_dir, root) = guarded_root("focus-omit");
    let var = env_vars(&[
        ("KITTY_LISTEN_ON", "tcp:localhost:1"),
        ("KITTY_WINDOW_ID", "3"),
        ("WEZTERM_PANE", "not-a-number"),
    ]);
    let capture = focus::capture_with(
        &root,
        "omit-1",
        false,
        Platform::Linux,
        &var,
        &observation(Some([5u8; 24])),
        1,
    );
    assert_eq!(capture.outcome, CaptureOutcome::Written);
    assert!(capture.key.is_some());
    assert!(
        capture.omitted.contains(&"kitty_socket") && capture.omitted.contains(&"wezterm_pane"),
        "omitted fields must be named: {:?}",
        capture.omitted
    );
    let bytes = std::fs::read(focus::record_path(&root, "omit-1")).unwrap();
    let record = Record::load(&bytes, "omit-1", None).expect("the record loads");
    assert_eq!(record.identity.kitty_socket, None);
    assert_eq!(record.identity.kitty_window_id, Some(3));
    assert_eq!(record.identity.wezterm_pane, None);
    assert_eq!(record.identity.terminal_pid, Some(41));
}

/// Inside a multiplexer the tty and window id the process sees belong to the
/// multiplexer; storing them would select the wrong tab.
#[test]
fn a_capture_inside_tmux_stores_only_the_multiplexer_identity() {
    let var = env_vars(&[
        ("TMUX", "/tmp/tmux-1000/default,123,0"),
        ("TMUX_PANE", "%3"),
        ("WINDOWID", "99"),
        ("KITTY_LISTEN_ON", "unix:/tmp/kitty"),
        ("KITTY_WINDOW_ID", "1"),
        ("__CFBundleIdentifier", "com.apple.Terminal"),
    ]);
    let mut failures = Failures::default();
    for platform in [Platform::Macos, Platform::Linux] {
        let (id, omitted) = focus::identity_from_env(platform, &var, &observation(None));
        let name = format!("{platform:?}");
        failures.check(&name, omitted.is_empty(), || format!("omitted {omitted:?}"));
        failures.check(
            &name,
            id.tmux
                .as_ref()
                .map(|t| (t.socket.as_str(), t.pane.as_str()))
                == Some(("/tmp/tmux-1000/default", "%3")),
            || format!("tmux identity: {:?}", id.tmux),
        );
        failures.check(&name, id.tty.is_none() && id.window_id.is_none(), || {
            format!(
                "tty {:?} window_id {:?} stored inside tmux",
                id.tty, id.window_id
            )
        });
        failures.check(
            &name,
            id.kitty_socket.is_none() && id.terminal_pid.is_none(),
            || "terminal-level identity stored inside tmux".to_string(),
        );
        failures.check(
            &name,
            id.bundle_id.as_deref() == Some("com.apple.Terminal"),
            || "the bundle id still travels, for app activation".to_string(),
        );
    }
    failures.assert_empty("tmux capture");
}

/// One bad value per R18 field makes the whole record unusable.
#[test]
fn one_bad_value_per_field_makes_the_record_unusable() {
    let mut record = sample_record("bad-1");
    record.identity = Identity {
        bundle_id: Some("com.apple.Terminal".into()),
        tty: Some("/dev/ttys003".into()),
        kitty_socket: Some("unix:/tmp/kitty".into()),
        kitty_window_id: Some(1),
        tmux: Some(focus::Tmux {
            socket: "/tmp/tmux-1000/default".into(),
            pane: "%3".into(),
        }),
        screen: Some(focus::Screen {
            session: "1234.pts-0.host".into(),
            window: 2,
        }),
        konsole: Some(focus::Konsole {
            service: "org.kde.konsole-4242".into(),
            window: "/Windows/1".into(),
            session: 7,
        }),
        window: Some(focus::WindowIdentity {
            handle: 0x30914,
            owner_pid: 9416,
            owner_start: 133_000_000,
            class: "CASCADIA_HOSTING_WINDOW_CLASS".into(),
            host: "windows-terminal".into(),
        }),
        ..Identity::default()
    };
    let base: serde_json::Value = serde_json::from_str(&record.to_json()).unwrap();
    assert!(
        Record::load(&base.to_string().into_bytes(), "bad-1", None).is_some(),
        "the full record must load before its fields are broken one at a time"
    );

    type Edit = Box<dyn Fn(&mut serde_json::Value)>;
    let edits: Vec<(&str, Edit)> = vec![
        (
            "tmux-name-target",
            Box::new(|v| v["identity"]["tmux"]["pane"] = serde_json::json!("main")),
        ),
        (
            "tmux-relative-socket",
            Box::new(|v| v["identity"]["tmux"]["socket"] = serde_json::json!("tmp/sock")),
        ),
        (
            "kitty-tcp-socket",
            Box::new(|v| v["identity"]["kitty_socket"] = serde_json::json!("tcp:localhost:1")),
        ),
        (
            "tty-outside-pattern",
            Box::new(|v| v["identity"]["tty"] = serde_json::json!("/dev/ttyp1")),
        ),
        (
            "cwd-newline",
            Box::new(|v| v["cwd"] = serde_json::json!("/repo\nwork")),
        ),
        (
            "cwd-relative",
            Box::new(|v| v["cwd"] = serde_json::json!("repo/work")),
        ),
        (
            "handle-not-numeric",
            Box::new(|v| v["identity"]["window"]["handle"] = serde_json::json!("12")),
        ),
        (
            "handle-float",
            Box::new(|v| v["identity"]["window"]["handle"] = serde_json::json!(1.5)),
        ),
        (
            "window-class-unknown",
            Box::new(|v| v["identity"]["window"]["class"] = serde_json::json!("Notepad")),
        ),
        (
            "window-class-pseudo-console",
            Box::new(|v| {
                v["identity"]["window"]["class"] = serde_json::json!("PseudoConsoleWindow")
            }),
        ),
        (
            "host-unknown",
            Box::new(|v| v["identity"]["window"]["host"] = serde_json::json!("emacs")),
        ),
        (
            "konsole-service",
            Box::new(|v| {
                v["identity"]["konsole"]["service"] = serde_json::json!("org.kde.konsole")
            }),
        ),
        (
            "konsole-window",
            Box::new(|v| v["identity"]["konsole"]["window"] = serde_json::json!("/MainWindow_1")),
        ),
        (
            "screen-session",
            Box::new(|v| v["identity"]["screen"]["session"] = serde_json::json!("pts-0.host")),
        ),
        (
            "bundle-id",
            Box::new(|v| v["identity"]["bundle_id"] = serde_json::json!("com.apple Terminal")),
        ),
        (
            "anchor-pid-negative",
            Box::new(|v| v["anchor"]["pid"] = serde_json::json!(-1)),
        ),
        (
            "boot-id-space",
            Box::new(|v| v["anchor"]["boot_id"] = serde_json::json!("a b")),
        ),
        (
            "session-type",
            Box::new(|v| v["identity"]["session_type"] = serde_json::json!("mir")),
        ),
    ];
    let mut failures = Failures::default();
    for (name, edit) in edits {
        let mut v = base.clone();
        edit(&mut v);
        let got = Record::load(&v.to_string().into_bytes(), "bad-1", None);
        failures.check(name, got.is_none(), || {
            "loaded despite the bad field".to_string()
        });
    }
    failures.assert_empty("R18 grammar");
}

/// R12: length first, then byte-exact. Nothing is trimmed, decoded or folded.
#[test]
fn key_grammar_rejects_every_variation() {
    let token = "abcdefghijklmnopqrstuvwxyzABCDEF";
    let good = format!("sess-1.{token}");
    let parsed = Key::parse(&good).expect("a valid key parses");
    assert_eq!(parsed.session(), "sess-1");
    assert_eq!(parsed.token(), token);
    assert_eq!(parsed.as_string(), good);
    assert_eq!(parsed.uri(), format!("claude-statusline:{good}"));
    assert_eq!(
        Key::parse_argument(&format!("claude-statusline:{good}")).map(|k| k.as_string()),
        Some(good.clone()),
        "the exact lowercase scheme prefix is accepted"
    );
    assert_eq!(
        parsed.record_path(Path::new("/state")),
        PathBuf::from("/state").join("statusline-focus-sess-1.json")
    );

    let mut failures = Failures::default();
    let bad = [
        ("dots-in-session", format!("a..b.{token}")),
        ("slash", format!("a/b.{token}")),
        ("backslash", format!("a\\b.{token}")),
        ("uppercase-scheme", format!("CLAUDE-STATUSLINE:{good}")),
        ("scheme-slashes", format!("claude-statusline://{good}")),
        ("token-short", format!("sess-1.{}", &token[..31])),
        ("token-long", format!("sess-1.{token}A")),
        ("percent-encoded", format!("sess%2D1.{token}")),
        ("embedded-space", format!("sess 1.{token}")),
        ("leading-space", format!(" {good}")),
        ("trailing-newline", format!("{good}\n")),
        ("empty-session", format!(".{token}")),
        ("no-separator", format!("sess-1{token}")),
        ("session-too-long", format!("{}.{token}", "s".repeat(129))),
        ("empty", String::new()),
    ];
    for (name, arg) in bad {
        failures.check(name, Key::parse_argument(&arg).is_none(), || {
            format!("accepted {arg:?}")
        });
    }
    failures.assert_empty("key grammar");
}

/// A record reached through a symlink reads as no record, exactly like the
/// latch and the token record. A foreign owner needs a second uid to stage
/// and is covered by `owner_check_fail_direction_matches_the_shipped_fix`.
#[cfg(unix)]
#[test]
fn a_symlinked_focus_record_reads_as_none() {
    let (dir, root) = guarded_root("focus-symlink");
    let capture = focus::capture_with(
        &root,
        "link-1",
        false,
        Platform::Linux,
        &env_vars(&[]),
        &observation(Some([9u8; 24])),
        1,
    );
    assert_eq!(capture.outcome, CaptureOutcome::Written);
    let path = focus::record_path(&root, "link-1");
    let elsewhere = dir.join("elsewhere.json");
    std::fs::copy(&path, &elsewhere).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(make_symlink(&elsewhere, &path));
    assert!(
        state::read_trusted(&path).is_none(),
        "a symlinked record must not be read"
    );
    // And capture regenerates through it: the link is removed, not followed.
    let again = focus::capture_with(
        &root,
        "link-1",
        false,
        Platform::Linux,
        &env_vars(&[]),
        &observation(Some([8u8; 24])),
        2,
    );
    assert_eq!(again.outcome, CaptureOutcome::Written);
    assert!(!std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
}

/// R19: on the flat temp-root fallback nothing is written, the outcome says
/// so, and the toast is planned exactly as it is today.
#[test]
fn capture_on_an_unguarded_root_writes_nothing_and_leaves_the_toast_unchanged() {
    let dir = scratch_dir("focus-unguarded");
    let root = inherited(&dir);
    let capture = focus::capture_with(
        &root,
        "flat-1",
        false,
        Platform::Macos,
        &env_vars(&[]),
        &observation(Some([4u8; 24])),
        1,
    );
    assert_eq!(capture.outcome, CaptureOutcome::Unguarded);
    assert!(capture.key.is_none());
    assert!(
        focus_files(&dir).is_empty(),
        "a record landed on the flat root"
    );

    let today = as_records(&notify::plan(
        Platform::Macos,
        "permission",
        "",
        PERMISSION_PAYLOAD,
        &NotifyConfig::default(),
        &full_env(),
        capture.key.as_ref(),
    ));
    assert_eq!(
        today,
        vec![
            "afplay\t/System/Library/Sounds/Tink.aiff".to_string(),
            "terminal-notifier\t-title\tClaude Code\t-message\tBash: git status --porcelain"
                .to_string(),
        ]
    );
}

/// KTD13: no randomness, no token, no record; and the tokens the OS does
/// produce are 32 characters of the URL-safe alphabet and differ.
#[test]
fn a_randomness_failure_writes_no_record_and_tokens_are_well_formed() {
    let (_dir, root) = guarded_root("focus-random");
    let capture = focus::capture_with(
        &root,
        "rand-1",
        false,
        Platform::Linux,
        &env_vars(&[]),
        &observation(None),
        1,
    );
    assert_eq!(capture.outcome, CaptureOutcome::NoRandomness);
    assert!(capture.key.is_none());
    assert!(focus_files(root.path()).is_empty());

    let a = platform::focus::random_bytes().expect("the OS supplies randomness");
    let b = platform::focus::random_bytes().expect("the OS supplies randomness");
    assert_ne!(a, b, "two draws must differ");
    for bytes in [a, b] {
        let token = focus::encode_token(&bytes);
        assert_eq!(token.len(), 32);
        assert!(token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }
    assert_eq!(focus::encode_token(&[0u8; 24]), "A".repeat(32));
    assert_eq!(focus::encode_token(&[0xff; 24]), "_".repeat(32));
}

/// A tick that crosses nothing touches nothing new.
#[test]
fn a_tick_without_an_alert_writes_no_focus_record() {
    let dir = scratch_dir("focus-quiet-tick");
    let home = dir.join("home");
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    std::fs::create_dir_all(&tmp).expect("tmp");
    let resolved = claude_statusline::session::state_dir_in(&tmp);
    let payload = format!(
        r#"{{"session_id":"quiet-1","cwd":"{cwd}","workspace":{{"current_dir":"{cwd}"}},"model":{{"display_name":"Opus 5","id":"claude-opus-5"}},"context_window":{{"context_window_size":200000,"used_percentage":12}},"rate_limits":{{"five_hour":{{"used_percentage":5}}}}}}"#,
        cwd = slashed(&dir),
    );
    let roots = cmd_statusline::Roots {
        home: Some(home),
        temp: resolved.clone(),
    };
    let rendered = cmd_statusline::run(&TestClock::at(1_000), &roots, &payload);
    assert!(rendered.contains('\u{250f}'));
    assert!(
        focus_files(resolved.path()).is_empty() && focus_files(&tmp).is_empty(),
        "a quiet tick wrote a focus record"
    );
}

/// KTD11: every hook event reads its payload when stdin is a pipe, so a stop
/// toast can name its session; a null stdin proceeds with no record; a payload
/// past the cap is truncated with a log line and the notification still goes
/// out.
#[test]
fn notify_stop_records_the_session_from_its_piped_payload() {
    let dir = scratch_dir("focus-hook-stdin");
    let home = dir.join("home");
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    std::fs::create_dir_all(&tmp).expect("tmp");
    let home_s = home.to_str().unwrap();
    let tmp_s = tmp.to_str().unwrap();
    // No toast may reach this machine's desktop: the Windows interpreter is
    // resolved under an empty SystemRoot and the Unix helpers under an empty
    // PATH, so the planned spawn fails silently, exactly as a missing helper
    // does in the field.
    let env = [
        ("HOME", home_s),
        ("USERPROFILE", home_s),
        ("TMPDIR", tmp_s),
        ("TEMP", tmp_s),
        ("SystemRoot", tmp_s),
        ("PATH", ""),
        ("STATUSLINE_DEBUG", "1"),
    ];

    let piped = run_bin(&["notify", "stop"], r#"{"session_id":"hook-stop-1"}"#, &env);
    assert_eq!(piped.code, Some(0));
    assert_eq!(piped.stderr, "");
    let state = state_dir_under(&tmp).expect("the hook created the state directory");
    assert!(
        state.join("statusline-focus-hook-stop-1.json").is_file(),
        "the stop hook did not record its session: {:?}",
        focus_files(&state)
    );

    let silent = run_bin(&["notify", "compaction_done"], "", &env);
    assert_eq!(silent.code, Some(0));
    assert_eq!(
        focus_files(&state).len(),
        1,
        "a hook with no payload must not write a record"
    );

    // 32 MiB and one byte more: the take truncates, the JSON no longer parses,
    // and the log says why. The notification itself still renders (exit 0).
    let mut huge = String::from(r#"{"session_id":"hook-cap-1","pad":""#);
    huge.push_str(&"x".repeat(32 * 1024 * 1024));
    huge.push_str("\"}");
    let capped = run_bin(&["notify", "stop"], &huge, &env);
    assert_eq!(capped.code, Some(0));
    assert_eq!(capped.stderr, "");
    assert!(
        !state.join("statusline-focus-hook-cap-1.json").exists(),
        "a truncated payload must not yield a session"
    );
    let log = std::fs::read_to_string(home.join(".claude").join("statusline-debug.log"))
        .unwrap_or_default();
    assert!(
        log.contains("stdin capped at"),
        "the cap must be logged: {log}"
    );
    assert!(
        !log.contains("hook-stop-1.") || !log.contains("token"),
        "no log line may carry a token"
    );
}

/// The detached child carries the key as a fourth argv value, and the
/// three-value form the hooks have always used still works.
#[test]
fn the_detached_child_carries_the_key_as_a_fourth_argument() {
    let alert = notify_state::Alert {
        event: "context_high",
        value: 82,
    };
    assert_eq!(
        notify_state::spawn_args(&alert, None),
        vec!["notify", "context_high", "82"]
    );
    let key = Key::parse(&format!("s1.{}", "k".repeat(32))).unwrap();
    assert_eq!(
        notify_state::spawn_args(&alert, Some(&key)),
        vec![
            "notify",
            "context_high",
            "82",
            &format!("s1.{}", "k".repeat(32))
        ]
    );

    // The fourth value is parsed, not trusted: a malformed one yields no key
    // and the notification still goes out, as a fresh process shows.
    let dir = scratch_dir("focus-fourth-arg");
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let tmp_s = tmp.to_str().unwrap();
    let run = run_bin(
        &["notify", "context_high", "82", "not a key"],
        "",
        &[
            ("TMPDIR", tmp_s),
            ("TEMP", tmp_s),
            ("SystemRoot", tmp_s),
            ("PATH", ""),
        ],
    );
    assert_eq!(run.code, Some(0));
    assert_eq!(run.stderr, "");
}

/// The anchor is Claude Code: named by `CLAUDE_PID` when it is in the chain,
/// otherwise the first non-shell ancestor; the terminal is the first non-shell
/// above it.
#[test]
fn the_anchor_is_the_first_non_shell_ancestor_unless_claude_pid_names_it() {
    let p = |pid: u64, ppid: u64, name: &str| focus::ProcessInfo {
        pid,
        ppid,
        name: name.to_string(),
        start: pid * 10,
    };
    let chain = vec![
        p(50, 40, "claude-statusline"),
        p(40, 30, "bash.exe"),
        p(30, 20, "claude.exe"),
        p(20, 10, "pwsh.exe"),
        p(10, 1, "WindowsTerminal.exe"),
    ];
    let (anchor, terminal) = focus::select_anchor(&chain, None);
    assert_eq!(anchor.as_ref().map(|a| a.pid), Some(30));
    assert_eq!(
        terminal.as_ref().map(|t| t.name.as_str()),
        Some("WindowsTerminal.exe")
    );

    // CLAUDE_PID overrides the shell heuristic when it is genuinely an ancestor.
    let (anchor, _) = focus::select_anchor(&chain, Some(20));
    assert_eq!(anchor.as_ref().map(|a| a.pid), Some(20));
    // ...and is ignored when it is not in the chain.
    let (anchor, _) = focus::select_anchor(&chain, Some(999));
    assert_eq!(anchor.as_ref().map(|a| a.pid), Some(30));

    let unix = vec![
        p(5, 4, "claude-statusline"),
        p(4, 3, "sh"),
        p(3, 2, "node"),
        p(2, 1, "-zsh"),
        p(1, 0, "gnome-terminal-server"),
    ];
    let (anchor, terminal) = focus::select_anchor(&unix, None);
    assert_eq!(anchor.as_ref().map(|a| a.pid), Some(3));
    assert_eq!(terminal.as_ref().map(|t| t.pid), Some(1));

    assert_eq!(
        focus::select_anchor(&[p(1, 0, "claude-statusline")], None),
        (None, None)
    );
    assert!(focus::is_shell("-bash") && focus::is_shell("PWSH.EXE") && !focus::is_shell("node"));
}
