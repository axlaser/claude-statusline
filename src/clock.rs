//! The single injectable time source. Filesystem mtimes live on the trait
//! beside wall-clock reads: freshness is `now - mtime`, so a test that pinned
//! only the clock would leave the stale-feed and stale-transcript states
//! unreachable in the table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock {
    /// Seconds since the Unix epoch.
    fn now_unix(&self) -> i64;

    /// Modification time of `path` in seconds since the Unix epoch, or `None`
    /// when the path does not exist or its time cannot be read.
    fn mtime_unix(&self, path: &Path) -> Option<i64>;

    /// Age of `path` in seconds. `None` when the mtime is unavailable.
    fn age_secs(&self, path: &Path) -> Option<i64> {
        self.mtime_unix(path).map(|m| self.now_unix() - m)
    }
}

/// The production clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn mtime_unix(&self, path: &Path) -> Option<i64> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        modified
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }
}

/// A clock whose `now` and per-path mtimes are both pinned, so a fixture
/// captured in the past renders identically forever.
#[derive(Debug, Default, Clone)]
pub struct TestClock {
    now: i64,
    mtimes: HashMap<PathBuf, i64>,
}

impl TestClock {
    pub fn at(now: i64) -> Self {
        Self {
            now,
            mtimes: HashMap::new(),
        }
    }

    pub fn with_mtime(mut self, path: impl AsRef<Path>, mtime: i64) -> Self {
        self.mtimes.insert(path.as_ref().to_path_buf(), mtime);
        self
    }
}

impl Clock for TestClock {
    fn now_unix(&self) -> i64 {
        self.now
    }

    fn mtime_unix(&self, path: &Path) -> Option<i64> {
        self.mtimes.get(path).copied()
    }
}
