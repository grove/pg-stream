//! Error types for pgtrickle.
//!
//! All errors that can occur within the extension are represented by [`PgTrickleError`].
//! Errors are propagated via `Result<T, PgTrickleError>` throughout the codebase and
//! converted to PostgreSQL errors at the API boundary using `pgrx::error!()`.
//!
//! # Error Classification
//!
//! Errors are classified into four categories that determine retry behavior:
//! - **User** — invalid queries, type mismatches, cycles. Never retried.
//! - **Schema** — upstream DDL changes. Not retried; triggers reinitialize.
//! - **System** — lock timeouts, slot errors, SPI failures. Retried with backoff.
//! - **Internal** — bugs. Not retried.
//!
//! # Retry Policy
//!
//! The [`RetryPolicy`] struct encapsulates exponential backoff with jitter for
//! system errors. The scheduler uses this to decide whether and when to retry
//! a failed refresh.

use std::fmt;

/// Primary error type for the extension.
#[derive(Debug, thiserror::Error)]
pub enum PgTrickleError {
    // ── User errors — fail, don't retry ──────────────────────────────────
    /// The defining query could not be parsed or validated.
    #[error("query parse error: {0}")]
    QueryParseError(String),

    /// A type mismatch was detected (e.g., incompatible column types).
    #[error("type mismatch: {0}")]
    TypeMismatch(String),

    /// The defining query contains an operator not supported for differential mode.
    #[error("unsupported operator for DIFFERENTIAL mode: {0}")]
    UnsupportedOperator(String),

    /// Adding this stream table would create a cycle in the dependency DAG.
    #[error("cycle detected in dependency graph: {}", .0.join(" -> "))]
    CycleDetected(Vec<String>),

    /// The specified stream table was not found.
    #[error("stream table not found: {0}")]
    NotFound(String),

    /// The stream table already exists.
    #[error("stream table already exists: {0}")]
    AlreadyExists(String),

    /// An invalid argument was provided to an API function.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    // ── Schema errors — may require reinitialize ─────────────────────────
    /// An upstream (source) table was dropped.
    #[error("upstream table dropped: OID {0}")]
    UpstreamTableDropped(u32),

    /// An upstream table's schema changed (ALTER TABLE).
    #[error("upstream table schema changed: OID {0}")]
    UpstreamSchemaChanged(u32),

    // ── System errors — retry with backoff ───────────────────────────────
    /// A lock could not be acquired within the timeout.
    #[error("lock timeout: {0}")]
    LockTimeout(String),

    /// An error occurred with a logical replication slot.
    #[error("replication slot error: {0}")]
    ReplicationSlotError(String),

    /// An error occurred during WAL-based CDC transition (trigger → WAL).
    #[error("WAL transition error: {0}")]
    WalTransitionError(String),

    /// An SPI (Server Programming Interface) error occurred.
    #[error("SPI error: {0}")]
    SpiError(String),

    // ── Transient errors — always retry ──────────────────────────────────
    /// A refresh was skipped because a previous one is still running.
    #[error("refresh skipped: {0}")]
    RefreshSkipped(String),

    // ── Internal errors — should not happen ──────────────────────────────
    /// An unexpected internal error. Indicates a bug.
    #[error("internal error: {0}")]
    InternalError(String),
}

impl PgTrickleError {
    /// Whether this error is retryable by the scheduler.
    ///
    /// System errors and skipped refreshes are retryable.
    /// User errors, schema errors, and internal errors are not.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            PgTrickleError::LockTimeout(_)
                | PgTrickleError::ReplicationSlotError(_)
                | PgTrickleError::WalTransitionError(_)
                | PgTrickleError::SpiError(_)
                | PgTrickleError::RefreshSkipped(_)
        )
    }

    /// Whether this error requires the ST to be reinitialized.
    pub fn requires_reinitialize(&self) -> bool {
        matches!(
            self,
            PgTrickleError::UpstreamSchemaChanged(_) | PgTrickleError::UpstreamTableDropped(_)
        )
    }

    /// Whether this error should count toward the consecutive error limit.
    ///
    /// Skipped refreshes and some transient errors don't count because the
    /// ST itself isn't broken — the scheduler just couldn't run it this time.
    pub fn counts_toward_suspension(&self) -> bool {
        !matches!(self, PgTrickleError::RefreshSkipped(_))
    }
}

/// Classification of error severity/kind for monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgTrickleErrorKind {
    User,
    Schema,
    System,
    Internal,
}

impl fmt::Display for PgTrickleErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PgTrickleErrorKind::User => write!(f, "USER"),
            PgTrickleErrorKind::Schema => write!(f, "SCHEMA"),
            PgTrickleErrorKind::System => write!(f, "SYSTEM"),
            PgTrickleErrorKind::Internal => write!(f, "INTERNAL"),
        }
    }
}

impl PgTrickleError {
    /// Classify the error for monitoring and alerting.
    pub fn kind(&self) -> PgTrickleErrorKind {
        match self {
            PgTrickleError::QueryParseError(_)
            | PgTrickleError::TypeMismatch(_)
            | PgTrickleError::UnsupportedOperator(_)
            | PgTrickleError::CycleDetected(_)
            | PgTrickleError::NotFound(_)
            | PgTrickleError::AlreadyExists(_)
            | PgTrickleError::InvalidArgument(_) => PgTrickleErrorKind::User,

            PgTrickleError::UpstreamTableDropped(_) | PgTrickleError::UpstreamSchemaChanged(_) => {
                PgTrickleErrorKind::Schema
            }

            PgTrickleError::LockTimeout(_)
            | PgTrickleError::ReplicationSlotError(_)
            | PgTrickleError::WalTransitionError(_)
            | PgTrickleError::SpiError(_)
            | PgTrickleError::RefreshSkipped(_) => PgTrickleErrorKind::System,

            PgTrickleError::InternalError(_) => PgTrickleErrorKind::Internal,
        }
    }
}

// ── Retry Policy ───────────────────────────────────────────────────────────

/// Retry policy with exponential backoff for system errors.
///
/// Used by the scheduler to decide whether a failed ST should be retried
/// immediately, deferred, or given up on.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Base delay in milliseconds (doubled each attempt).
    pub base_delay_ms: u64,
    /// Maximum delay in milliseconds (cap for backoff).
    pub max_delay_ms: u64,
    /// Maximum number of retry attempts before giving up.
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            base_delay_ms: 1_000, // 1 second initial
            max_delay_ms: 60_000, // 1 minute cap
            max_attempts: 5,      // 5 retries before counting as a real failure
        }
    }
}

impl RetryPolicy {
    /// Calculate the backoff delay in milliseconds for the given attempt number (0-based).
    ///
    /// Uses exponential backoff: `base_delay * 2^attempt`, capped at `max_delay`.
    /// Adds simple jitter by varying ±25%.
    pub fn backoff_ms(&self, attempt: u32) -> u64 {
        let delay = self.base_delay_ms.saturating_mul(1u64 << attempt.min(16));
        let capped = delay.min(self.max_delay_ms);

        // Simple deterministic jitter: vary by ±25% based on attempt parity
        if attempt.is_multiple_of(2) {
            capped.saturating_mul(3) / 4 // -25%
        } else {
            capped.saturating_mul(5) / 4 // +25%
        }
    }

    /// Whether the given attempt (0-based) is within the retry limit.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.max_attempts
    }
}

// ── Per-ST Retry State ─────────────────────────────────────────────────────

/// Tracks retry state for a single stream table in the scheduler.
///
/// Stored in-memory by the scheduler (not persisted). Reset when a refresh
/// succeeds or the scheduler restarts.
#[derive(Debug, Clone)]
pub struct RetryState {
    /// Number of consecutive retryable failures.
    pub attempts: u32,
    /// Timestamp (epoch millis) when the next retry is allowed.
    pub next_retry_at_ms: u64,
}

impl Default for RetryState {
    fn default() -> Self {
        Self::new()
    }
}

impl RetryState {
    pub fn new() -> Self {
        Self {
            attempts: 0,
            next_retry_at_ms: 0,
        }
    }

    /// Record a retryable failure and compute the next retry time.
    ///
    /// Returns `true` if another retry is allowed, `false` if max attempts exhausted.
    pub fn record_failure(&mut self, policy: &RetryPolicy, now_ms: u64) -> bool {
        self.attempts += 1;
        if policy.should_retry(self.attempts) {
            self.next_retry_at_ms = now_ms + policy.backoff_ms(self.attempts - 1);
            true
        } else {
            false
        }
    }

    /// Reset retry state after a successful refresh.
    pub fn reset(&mut self) {
        self.attempts = 0;
        self.next_retry_at_ms = 0;
    }

    /// Whether the ST is currently in a retry-backoff period.
    pub fn is_in_backoff(&self, now_ms: u64) -> bool {
        self.attempts > 0 && now_ms < self.next_retry_at_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        assert_eq!(
            PgTrickleError::QueryParseError("x".into()).kind(),
            PgTrickleErrorKind::User
        );
        assert_eq!(
            PgTrickleError::UpstreamSchemaChanged(1).kind(),
            PgTrickleErrorKind::Schema
        );
        assert_eq!(
            PgTrickleError::LockTimeout("x".into()).kind(),
            PgTrickleErrorKind::System
        );
        assert_eq!(
            PgTrickleError::InternalError("x".into()).kind(),
            PgTrickleErrorKind::Internal
        );
        assert_eq!(
            PgTrickleError::RefreshSkipped("x".into()).kind(),
            PgTrickleErrorKind::System
        );
    }

    #[test]
    fn test_retryable_errors() {
        assert!(PgTrickleError::LockTimeout("x".into()).is_retryable());
        assert!(PgTrickleError::ReplicationSlotError("x".into()).is_retryable());
        assert!(PgTrickleError::SpiError("x".into()).is_retryable());
        assert!(PgTrickleError::RefreshSkipped("x".into()).is_retryable());

        assert!(!PgTrickleError::QueryParseError("x".into()).is_retryable());
        assert!(!PgTrickleError::CycleDetected(vec![]).is_retryable());
        assert!(!PgTrickleError::InternalError("x".into()).is_retryable());
    }

    #[test]
    fn test_requires_reinitialize() {
        assert!(PgTrickleError::UpstreamSchemaChanged(1).requires_reinitialize());
        assert!(PgTrickleError::UpstreamTableDropped(1).requires_reinitialize());
        assert!(!PgTrickleError::SpiError("x".into()).requires_reinitialize());
    }

    #[test]
    fn test_counts_toward_suspension() {
        assert!(PgTrickleError::SpiError("x".into()).counts_toward_suspension());
        assert!(PgTrickleError::LockTimeout("x".into()).counts_toward_suspension());
        assert!(!PgTrickleError::RefreshSkipped("x".into()).counts_toward_suspension());
    }

    #[test]
    fn test_retry_policy_backoff() {
        let policy = RetryPolicy {
            base_delay_ms: 1000,
            max_delay_ms: 10_000,
            max_attempts: 5,
        };

        // Attempt 0: 1000 * 2^0 = 1000, -25% = 750
        assert_eq!(policy.backoff_ms(0), 750);
        // Attempt 1: 1000 * 2^1 = 2000, +25% = 2500
        assert_eq!(policy.backoff_ms(1), 2500);
        // Attempt 2: 1000 * 2^2 = 4000, -25% = 3000
        assert_eq!(policy.backoff_ms(2), 3000);
        // Attempt 3: 1000 * 2^3 = 8000, +25% = 10000
        assert_eq!(policy.backoff_ms(3), 10_000);
        // Attempt 4: 1000 * 2^4 = 16000, capped at 10000, -25% = 7500
        assert_eq!(policy.backoff_ms(4), 7500);
    }

    #[test]
    fn test_retry_policy_should_retry() {
        let policy = RetryPolicy {
            base_delay_ms: 1000,
            max_delay_ms: 60_000,
            max_attempts: 3,
        };

        assert!(policy.should_retry(0));
        assert!(policy.should_retry(1));
        assert!(policy.should_retry(2));
        assert!(!policy.should_retry(3));
        assert!(!policy.should_retry(4));
    }

    #[test]
    fn test_retry_state_lifecycle() {
        let policy = RetryPolicy::default();
        let mut state = RetryState::new();

        // Fresh state: not in backoff
        assert!(!state.is_in_backoff(1000));
        assert_eq!(state.attempts, 0);

        // First failure
        let now = 10_000;
        assert!(state.record_failure(&policy, now));
        assert_eq!(state.attempts, 1);
        assert!(state.is_in_backoff(now + 100)); // still in backoff
        assert!(!state.is_in_backoff(now + 100_000)); // backoff passed

        // Second failure
        let now2 = 20_000;
        assert!(state.record_failure(&policy, now2));
        assert_eq!(state.attempts, 2);

        // Reset on success
        state.reset();
        assert_eq!(state.attempts, 0);
        assert!(!state.is_in_backoff(0));
    }

    #[test]
    fn test_retry_state_max_attempts_exhausted() {
        let policy = RetryPolicy {
            base_delay_ms: 100,
            max_delay_ms: 1000,
            max_attempts: 2,
        };
        let mut state = RetryState::new();

        // First failure — retries allowed (attempt 1 < max 2)
        assert!(state.record_failure(&policy, 1000));
        assert_eq!(state.attempts, 1);
        // Second failure — max attempts exhausted (attempt 2 >= max 2)
        assert!(!state.record_failure(&policy, 2000));
        assert_eq!(state.attempts, 2);
    }
}
