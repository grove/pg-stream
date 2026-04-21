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

    /// The defining query exceeds the maximum parse depth (G13-SD).
    #[error("query too complex: {0}")]
    QueryTooComplex(String),

    /// SEC-1: The current role does not own the stream table's storage table.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

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

    // ── Watermark errors ─────────────────────────────────────────────────
    /// A watermark advancement was rejected because the new value is older
    /// than the current watermark (monotonicity violation).
    #[error("watermark moved backward: {0}")]
    WatermarkBackwardMovement(String),

    /// A watermark group was not found.
    #[error("watermark group not found: {0}")]
    WatermarkGroupNotFound(String),

    /// A watermark group with this name already exists.
    #[error("watermark group already exists: {0}")]
    WatermarkGroupAlreadyExists(String),

    // ── Transient errors — always retry ──────────────────────────────────
    /// A refresh was skipped because a previous one is still running.
    #[error("refresh skipped: {0}")]
    RefreshSkipped(String),

    // ── Publication errors ───────────────────────────────────────────────
    /// The stream table already has a downstream publication.
    #[error("publication already exists for stream table: {0}")]
    PublicationAlreadyExists(String),

    /// The stream table does not have a downstream publication.
    #[error("no publication found for stream table: {0}")]
    PublicationNotFound(String),

    // ── SLA errors ───────────────────────────────────────────────────────
    /// The SLA interval is too small for any available tier.
    #[error("SLA interval too small for available tiers: {0}")]
    SlaTooSmall(String),

    // ── CDC errors ───────────────────────────────────────────────────────
    /// CDC-1 (v0.24.0): Failed to build the changed-columns bitmask expression.
    /// This indicates a table structure that prevents column-change tracking
    /// (e.g., all columns are part of the primary key).
    #[error("failed to build changed-columns bitmask: {0}")]
    ChangedColsBitmaskFailed(String),

    /// CDC-2 (v0.24.0): Publication rebuild failed for a partitioned source.
    #[error("publication rebuild failed: {0}")]
    PublicationRebuildFailed(String),

    // ── Diagnostic errors — failures in the diagnostics/monitoring API ──────
    /// ERR-1 (v0.26.0): An error occurred inside a diagnostic or monitoring
    /// function (e.g., `explain_refresh_mode`, `source_gates`, `watermarks`).
    /// These surface as user-visible PostgreSQL errors with context info.
    #[error("diagnostic error: {0}")]
    DiagnosticError(String),

    // ── Internal errors — should not happen ──────────────────────────────
    /// An unexpected internal error. Indicates a bug.
    #[error("internal error: {0}")]
    InternalError(String),
}

impl PgTrickleError {
    /// ERR-1/ERR-2 (v0.26.0): Convert this error into a PostgreSQL-level error.
    ///
    /// Call **only** at the `#[pg_extern]` SQL API boundary. Internal code must
    /// propagate via `Result<T, PgTrickleError>` using `?`.
    pub fn into_pg_error(self) -> ! {
        pgrx::error!("{}", self)
    }

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
            PgTrickleError::RefreshSkipped(_)
                | PgTrickleError::SpiPermissionError(_)
                | PgTrickleError::PermissionDenied(_)
        )
    }
}

/// F29 (G8.6): Classify an SPI error message for retry eligibility.
///
/// Heuristic: looks for patterns in the human-readable error message text
/// returned by pgrx from PostgreSQL SPI. SQLSTATE codes are NOT included in
/// the SPI error string, so all patterns here are text-based rather than
/// code-based.
///
/// Non-retryable (permanent) errors:
/// - Permission denied
/// - Schema errors: column/relation/function does not exist, syntax error
/// - Constraint violations
/// - Division by zero / data exceptions
/// - Duplicate objects
///
/// Retryable (transient) errors:
/// - Serialization failures
/// - Deadlocks
/// - Lock timeouts
/// - Connection errors
///
/// If no pattern matches, defaults to retryable (safe for unknown errors).
pub fn classify_spi_error_retryable(msg: &str) -> bool {
    let msg_lower = msg.to_lowercase();

    // Non-retryable patterns — text as it appears in PostgreSQL error messages.
    // SQLSTATE codes (42703, 42p01, ...) do NOT appear in pgrx SPI error strings,
    // so all matches are against the human-readable message text.
    let non_retryable_patterns = [
        // Schema / object errors — permanent until schema is fixed
        "does not exist", // column X does not exist, relation X does not exist, etc.
        "syntax error",   // SQL syntax error — permanent until query is fixed
        "column of relation", // column-level schema error
        // Permission errors
        "permission denied",
        "insufficient_privilege",
        // Constraint / data errors
        "unique constraint",
        "foreign key constraint",
        "not-null constraint",
        "check constraint",
        "violates", // "violates unique constraint", "violates check constraint"
        "division by zero",
        // Duplicate object errors
        "already exists",
        // Dead legacy SQLSTATE code patterns (not in message text, kept as comments):
        // "42501", "42000", "42601", "42p01", "42703", "23xxx", "22xxx"
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
            | PgTrickleError::InvalidArgument(_)
            | PgTrickleError::QueryTooComplex(_)
            | PgTrickleError::PermissionDenied(_)
            | PgTrickleError::WatermarkBackwardMovement(_)
            | PgTrickleError::WatermarkGroupNotFound(_)
            | PgTrickleError::WatermarkGroupAlreadyExists(_)
            | PgTrickleError::PublicationAlreadyExists(_)
            | PgTrickleError::PublicationNotFound(_)
            | PgTrickleError::SlaTooSmall(_) => PgTrickleErrorKind::User,

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

            PgTrickleError::ChangedColsBitmaskFailed(_)
            | PgTrickleError::PublicationRebuildFailed(_) => PgTrickleErrorKind::System,

            // ERR-1 (v0.26.0): Diagnostic errors are system-level (SPI failures,
            // catalog query issues). They are not retried but are surfaced to the user.
            PgTrickleError::DiagnosticError(_) => PgTrickleErrorKind::System,
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
        // Transient errors — retryable
        assert!(PgTrickleError::SpiError("connection lost".into()).is_retryable());
        assert!(PgTrickleError::SpiError("serialization failure".into()).is_retryable());
        assert!(PgTrickleError::SpiError("deadlock detected".into()).is_retryable());
        assert!(PgTrickleError::SpiError("could not obtain lock".into()).is_retryable());
        assert!(PgTrickleError::RefreshSkipped("x".into()).is_retryable());

        // Permanent schema errors — NOT retryable (human-readable PG messages)
        assert!(
            !PgTrickleError::SpiError(r#"column "extra" of relation "src" does not exist"#.into())
                .is_retryable()
        );
        assert!(
            !PgTrickleError::SpiError(r#"relation "missing_table" does not exist"#.into())
                .is_retryable()
        );
        assert!(
            !PgTrickleError::SpiError(r#"syntax error at or near "SELEC""#.into()).is_retryable()
        );
        assert!(!PgTrickleError::SpiError("permission denied for table foo".into()).is_retryable());
        assert!(!PgTrickleError::SpiError("unique constraint violated".into()).is_retryable());
        assert!(!PgTrickleError::SpiError("violates not-null constraint".into()).is_retryable());
        assert!(!PgTrickleError::SpiError("division by zero".into()).is_retryable());

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
        // Non-retryable: permanent schema errors (human-readable PG message text)
        assert!(!classify_spi_error_retryable(
            "permission denied for table orders"
        ));
        assert!(!classify_spi_error_retryable(
            r#"column "extra" of relation "src" does not exist"#
        ));
        assert!(!classify_spi_error_retryable(
            r#"relation "missing_table" does not exist"#
        ));
        assert!(!classify_spi_error_retryable(
            r#"syntax error at or near "SELEC""#
        ));
        assert!(!classify_spi_error_retryable(
            "violates unique constraint \"orders_pkey\""
        ));
        assert!(!classify_spi_error_retryable("division by zero"));
        assert!(!classify_spi_error_retryable("already exists"));

        // Non-retryable: old SQLSTATE-in-message style still works where present
        assert!(!classify_spi_error_retryable(
            "ERROR: 42501 insufficient_privilege"
        ));

        // Retryable: transient errors
        assert!(classify_spi_error_retryable("serialization failure"));
        assert!(classify_spi_error_retryable("deadlock detected"));
        assert!(classify_spi_error_retryable(
            "server closed the connection unexpectedly"
        ));
        assert!(classify_spi_error_retryable(
            "could not obtain lock on relation"
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

    // ── G: Additional pure function tests ───────────────────────────

    #[test]
    fn test_error_kind_display_all_variants() {
        assert_eq!(format!("{}", PgTrickleErrorKind::User), "USER");
        assert_eq!(format!("{}", PgTrickleErrorKind::Schema), "SCHEMA");
        assert_eq!(format!("{}", PgTrickleErrorKind::System), "SYSTEM");
        assert_eq!(format!("{}", PgTrickleErrorKind::Internal), "INTERNAL");
    }

    #[test]
    fn test_retry_policy_default_values() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.base_delay_ms, 1_000,
            "default base delay should be 1s"
        );
        assert_eq!(
            policy.max_delay_ms, 60_000,
            "default max delay should be 60s"
        );
        assert_eq!(policy.max_attempts, 5, "default max attempts should be 5");
    }

    #[test]
    fn test_query_too_complex_is_user_error() {
        let err = PgTrickleError::QueryTooComplex("depth exceeded".into());
        assert_eq!(err.kind(), PgTrickleErrorKind::User);
        assert!(!err.is_retryable());
        assert!(err.counts_toward_suspension());
        assert!(!err.requires_reinitialize());
        assert!(
            err.to_string().contains("query too complex"),
            "error message: {}",
            err
        );
    }
}
