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

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::error::{Error, Result};

thread_local! {
    /// Per-request deadline for the current thread. Set at the top
    /// of an expensive tool entry point (e.g. via `DeadlineGuard`),
    /// checked by `scan()` at batch boundaries, and cleared on drop.
    ///
    /// Thread-local avoids plumbing `Option<&Deadline>` through
    /// every scan call site (~18 in reader.rs) for a feature that
    /// only a handful of entry points activate.
    static REQUEST_DEADLINE: RefCell<Option<Deadline>> = const { RefCell::new(None) };
    /// Cooperative cancellation token for the current request. Python
    /// flips this when `asyncio.wait_for(...)` times out or the HTTP
    /// request is cancelled, letting long-running Rust loops return
    /// promptly instead of burning CPU in a detached thread.
    static REQUEST_CANCEL_TOKEN: RefCell<Option<CancelToken>> = const { RefCell::new(None) };
}

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

/// Cooperative request-cancellation token shared between the async
/// Python caller and the native worker thread it spawned.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    inner: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.inner.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::Relaxed)
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(Error::QueryCancelled);
        }
        Ok(())
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

/// Snapshot the thread-local deadline if one is set. Safe to call
/// anywhere. Returns `None` on an unguarded thread.
pub fn current_deadline() -> Option<Deadline> {
    REQUEST_DEADLINE.with(|c| *c.borrow())
}

/// Snapshot the thread-local cancellation token if one is installed.
pub fn current_cancel_token() -> Option<CancelToken> {
    REQUEST_CANCEL_TOKEN.with(|c| c.borrow().clone())
}

/// Verify the thread-local deadline, if present. No-op on threads
/// without a guard. Used by `scan()` at batch boundaries so scans
/// fired under a `DeadlineGuard` honor the budget; scans fired
/// directly (tests, offline tools) are unaffected.
pub fn check_request_deadline() -> Result<()> {
    REQUEST_DEADLINE.with(|c| match c.borrow().as_ref() {
        Some(d) => d.check(),
        None => Ok(()),
    })?;
    REQUEST_CANCEL_TOKEN.with(|c| match c.borrow().as_ref() {
        Some(token) => token.check(),
        None => Ok(()),
    })
}

/// RAII guard: sets a thread-local deadline on construction and
/// clears it on drop. Install at the top of expensive tool entry
/// points (e.g. `trailer_mentions`, `substr_subject`, `regex` scans).
///
/// Nested usage is safe in the sense that a guard overrides the
/// current deadline for its lifetime and restores whatever was
/// there before on drop — callers can layer tighter budgets.
pub struct DeadlineGuard {
    prev: Option<Deadline>,
}

impl DeadlineGuard {
    pub fn new(limit_ms: u64) -> Self {
        let d = Deadline::new(limit_ms);
        let prev = REQUEST_DEADLINE.with(|c| c.replace(Some(d)));
        Self { prev }
    }

    /// Install a specific pre-built deadline. Useful when a caller
    /// wants to propagate the same deadline into spawned rayon
    /// tasks so they finish under the same budget.
    pub fn install(d: Deadline) -> Self {
        let prev = REQUEST_DEADLINE.with(|c| c.replace(Some(d)));
        Self { prev }
    }
}

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        REQUEST_DEADLINE.with(|c| *c.borrow_mut() = prev);
    }
}

/// RAII guard for the cooperative cancellation token. Nested usage is
/// safe; the previous token is restored on drop.
pub struct CancelGuard {
    prev: Option<CancelToken>,
}

impl CancelGuard {
    pub fn install(token: CancelToken) -> Self {
        let prev = REQUEST_CANCEL_TOKEN.with(|c| c.replace(Some(token)));
        Self { prev }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        REQUEST_CANCEL_TOKEN.with(|c| *c.borrow_mut() = prev);
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

    #[test]
    fn cancellation_token_fires_when_cancelled() {
        let token = CancelToken::new();
        let _guard = CancelGuard::install(token.clone());
        assert!(check_request_deadline().is_ok());
        token.cancel();
        match check_request_deadline() {
            Err(Error::QueryCancelled) => {}
            other => panic!("expected QueryCancelled, got {other:?}"),
        }
    }

    #[test]
    fn cancel_guard_restores_previous_token() {
        let outer = CancelToken::new();
        let inner = CancelToken::new();
        let _outer_guard = CancelGuard::install(outer.clone());
        {
            let _inner_guard = CancelGuard::install(inner.clone());
            inner.cancel();
            match check_request_deadline() {
                Err(Error::QueryCancelled) => {}
                other => panic!("expected QueryCancelled, got {other:?}"),
            }
        }
        assert!(check_request_deadline().is_ok());
        outer.cancel();
        match check_request_deadline() {
            Err(Error::QueryCancelled) => {}
            other => panic!("expected QueryCancelled, got {other:?}"),
        }
    }
}
