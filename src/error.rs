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

    /// An SPI permission error (SQLSTATE 42xxx) — not retryable.
    ///
    /// F34 (G3.4): Surfaces clear error message when the background worker's
    /// role lacks SELECT on a source table or INSERT on the stream table.
    #[error("SPI permission error: {0}")]
    SpiPermissionError(String),

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
        match self {
            PgTrickleError::LockTimeout(_)
            | PgTrickleError::ReplicationSlotError(_)
            | PgTrickleError::WalTransitionError(_)
            | PgTrickleError::RefreshSkipped(_) => true,
            // F29 (G8.6): Classify SPI errors by SQLSTATE for retry decisions.
            // Only truly transient errors (serialization, lock, connection) are
            // retryable. Permission errors (42xxx), constraint violations (23xxx),
            // and division-by-zero are NOT retryable.
            PgTrickleError::SpiError(msg) => classify_spi_error_retryable(msg),
            // Permission errors are never retryable.
            PgTrickleError::SpiPermissionError(_) => false,
            _ => false,
        }
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
        !matches!(
            self,
            PgTrickleError::RefreshSkipped(_) | PgTrickleError::SpiPermissionError(_)
        )
    }
}

/// F29 (G8.6): Classify an SPI error message for retry eligibility.
///
/// Heuristic: looks for PostgreSQL SQLSTATE patterns in the error string.
/// Only truly transient errors are retryable:
/// - Serialization failure (40001)
/// - Deadlock detected (40P01)
/// - Lock not available (55P03)
/// - Connection/statement errors
///
/// Non-retryable patterns:
/// - Permission denied (42501, 42xxx)
/// - Constraint violation (23xxx)
/// - Division by zero (22012)
/// - Undefined table/column (42P01, 42703)
/// - Syntax error (42601)
///
/// If no pattern matches, defaults to retryable (safe for unknown errors).
fn classify_spi_error_retryable(msg: &str) -> bool {
    let msg_lower = msg.to_lowercase();

    // Non-retryable patterns (permission, constraint, data errors)
    let non_retryable_patterns = [
        "permission denied",
        "insufficient_privilege",
        "42501", // insufficient_privilege
        "42000", // syntax_error_or_access_rule_violation
        "42601", // syntax_error
        "42p01", // undefined_table
        "42703", // undefined_column
        "23",    // integrity_constraint_violation class
        "22012", // division_by_zero
        "22",    // data_exception class
        "2200",  // data_exception subclass
        "42p07", // duplicate_table
        "42710", // duplicate_object
    ];

    for pat in &non_retryable_patterns {
        if msg_lower.contains(pat) {
            return false;
        }
    }

    // Explicitly retryable patterns
    let retryable_patterns = [
        "serialization",
        "deadlock",
        "40001", // serialization_failure
        "40p01", // deadlock_detected
        "55p03", // lock_not_available
        "could not obtain lock",
        "canceling statement due to lock timeout",
        "connection",
        "server closed the connection",
    ];

    for pat in &retryable_patterns {
        if msg_lower.contains(pat) {
            return true;
        }
    }

    // Default: retry unknown SPI errors (conservative — better to retry
    // a non-retryable error once than to permanently fail a retryable one)
    true
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

            // F34: Permission errors are user-facing, not system-level.
            PgTrickleError::SpiPermissionError(_) => PgTrickleErrorKind::User,

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
        // F34: SpiPermissionError is classified as User, not System
        assert_eq!(
            PgTrickleError::SpiPermissionError("x".into()).kind(),
            PgTrickleErrorKind::User
        );
    }

    #[test]
    fn test_retryable_errors() {
        assert!(PgTrickleError::LockTimeout("x".into()).is_retryable());
        assert!(PgTrickleError::ReplicationSlotError("x".into()).is_retryable());
        // F29: SpiError is now conditionally retryable based on SQLSTATE
        assert!(PgTrickleError::SpiError("connection lost".into()).is_retryable());
        assert!(PgTrickleError::SpiError("serialization failure 40001".into()).is_retryable());
        assert!(!PgTrickleError::SpiError("permission denied for table foo".into()).is_retryable());
        assert!(!PgTrickleError::SpiError("23505 unique constraint".into()).is_retryable());
        assert!(PgTrickleError::RefreshSkipped("x".into()).is_retryable());

        // F34: SpiPermissionError is never retryable
        assert!(!PgTrickleError::SpiPermissionError("x".into()).is_retryable());

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
        // F34: SpiPermissionError does not count toward suspension
        assert!(!PgTrickleError::SpiPermissionError("x".into()).counts_toward_suspension());
    }

    #[test]
    fn test_classify_spi_error_retryable() {
        // F29: SQLSTATE-based retry classification
        // Non-retryable patterns
        assert!(!classify_spi_error_retryable(
            "permission denied for table orders"
        ));
        assert!(!classify_spi_error_retryable(
            "ERROR: 42501 insufficient_privilege"
        ));
        assert!(!classify_spi_error_retryable(
            "23505: duplicate key value violates unique constraint"
        ));
        assert!(!classify_spi_error_retryable("22012 division_by_zero"));
        assert!(!classify_spi_error_retryable("42P01: undefined_table"));

        // Retryable patterns
        assert!(classify_spi_error_retryable(
            "40001: could not serialize access"
        ));
        assert!(classify_spi_error_retryable("deadlock detected"));
        assert!(classify_spi_error_retryable("55P03: lock_not_available"));
        assert!(classify_spi_error_retryable(
            "server closed the connection unexpectedly"
        ));

        // Unknown error: default retryable
        assert!(classify_spi_error_retryable("something weird happened"));
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
