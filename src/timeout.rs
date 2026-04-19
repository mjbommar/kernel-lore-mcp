//! Rust-side deadline enforcement for long-running query paths.
//!
//! Python currently enforces a 5 s wall-clock cap via
//! `asyncio.wait_for`, but that cancellation only affects the
//! Python future — the Rust thread it's waiting on keeps running
//! (PyO3 `py.detach` can't cancel native code). A runaway query
//! therefore leaks a `std::thread` worker until the Rust call
//! completes naturally. Under load that's a denial-of-service
//! vector: enough runaway queries exhaust the thread pool.
//!
//! `Deadline` is the Rust-side primitive: each long path takes a
//! `&Deadline` reference and calls `deadline.check()?` at its loop
//! boundary. A timed-out scan returns `Error::QueryTimeout`; the
//! Rust thread returns promptly, the Python-side timeout maps it
//! to a structured `query_timeout` LoreError at the client.
//!
//! Scope today: infrastructure + `thread_via_parquet_scan` (the
//! one remaining known-slow fallback path). Additional surfaces
//! — `substr_subject`, `regex`, `patch_search` on huge candidate
//! sets — can adopt the same pattern without API churn.
//!
//! Design notes:
//!   * `Instant`-based, not epoch-millis: monotonic, unaffected by
//!     clock skew, cheap to read.
//!   * `Option<&Deadline>` at every scan call site so legacy callers
//!     without a budget pass `None` and get no checks.
//!   * Check granularity: at BATCH boundaries (1024-row Parquet
//!     batches). Checking per row would dominate scan cost for
//!     hot paths; per batch is ~1024 rows / ~1 ms between checks.

use std::time::Instant;

use crate::error::{Error, Result};

/// A wall-clock deadline for a single query. Immutable once constructed.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    started: Instant,
    limit_ms: u64,
}

impl Deadline {
    /// Construct a deadline that fires `limit_ms` milliseconds from now.
    pub fn new(limit_ms: u64) -> Self {
        Self {
            started: Instant::now(),
            limit_ms,
        }
    }

    /// Fast path: `Err(QueryTimeout)` iff we've exceeded the budget.
    pub fn check(&self) -> Result<()> {
        if self.elapsed_ms() > self.limit_ms {
            return Err(Error::QueryTimeout {
                limit_ms: self.limit_ms,
            });
        }
        Ok(())
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    #[allow(dead_code)]
    pub fn limit_ms(&self) -> u64 {
        self.limit_ms
    }
}

/// Convenience for call sites that may or may not have a budget.
#[allow(dead_code)]
pub fn check(deadline: Option<&Deadline>) -> Result<()> {
    match deadline {
        Some(d) => d.check(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn deadline_passes_when_budget_unused() {
        let d = Deadline::new(10_000);
        assert!(d.check().is_ok());
    }

    #[test]
    fn deadline_fires_after_budget_elapsed() {
        let d = Deadline::new(5);
        sleep(Duration::from_millis(25));
        match d.check() {
            Err(Error::QueryTimeout { limit_ms }) => assert_eq!(limit_ms, 5),
            other => panic!("expected QueryTimeout, got {other:?}"),
        }
    }

    #[test]
    fn check_helper_is_noop_when_none() {
        assert!(check(None).is_ok());
    }
}
