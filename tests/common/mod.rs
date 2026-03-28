//! Shared test helpers for integration tests using Testcontainers.

use sqlx::PgPool;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

/// SQL to create the pgtrickle catalog schema and tables.
/// Mirrors the extension_sql!() in lib.rs, but for standalone testing.
#[allow(dead_code)]
pub const CATALOG_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS pgtrickle;
CREATE SCHEMA IF NOT EXISTS pgtrickle_changes;

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_stream_tables (
    pgt_id           BIGSERIAL PRIMARY KEY,
    pgt_relid        OID NOT NULL UNIQUE,
    pgt_name         TEXT NOT NULL,
    pgt_schema       TEXT NOT NULL,
    defining_query  TEXT NOT NULL,
    original_query  TEXT,
    schedule      TEXT,
    refresh_mode    TEXT NOT NULL DEFAULT 'DIFFERENTIAL'
                     CHECK (refresh_mode IN ('FULL', 'DIFFERENTIAL', 'INCREMENTAL')),
    status          TEXT NOT NULL DEFAULT 'INITIALIZING'
                     CHECK (status IN ('INITIALIZING', 'ACTIVE', 'SUSPENDED', 'ERROR')),
    is_populated    BOOLEAN NOT NULL DEFAULT FALSE,
    data_timestamp  TIMESTAMPTZ,
    frontier        JSONB,
    last_refresh_at TIMESTAMPTZ,
    consecutive_errors INT NOT NULL DEFAULT 0,
    needs_reinit    BOOLEAN NOT NULL DEFAULT FALSE,
    topk_limit      INT,
    topk_order_by   TEXT,
    requested_cdc_mode TEXT
                     CHECK (requested_cdc_mode IN ('auto', 'trigger', 'wal')),
    pooler_compatibility_mode BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_pgt_name ON pgtrickle.pgt_stream_tables (pgt_schema, pgt_name);
CREATE INDEX IF NOT EXISTS idx_pgt_status ON pgtrickle.pgt_stream_tables (status);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_dependencies (
    pgt_id        BIGINT NOT NULL REFERENCES pgtrickle.pgt_stream_tables(pgt_id) ON DELETE CASCADE,
    source_relid OID NOT NULL,
    source_type  TEXT NOT NULL CHECK (source_type IN ('TABLE', 'STREAM_TABLE', 'VIEW', 'MATVIEW', 'FOREIGN_TABLE')),
    columns_used TEXT[],
    PRIMARY KEY (pgt_id, source_relid)
);

CREATE INDEX IF NOT EXISTS idx_deps_source ON pgtrickle.pgt_dependencies (source_relid);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_refresh_history (
    refresh_id      BIGSERIAL PRIMARY KEY,
    pgt_id           BIGINT NOT NULL,
    data_timestamp  TIMESTAMPTZ NOT NULL,
    start_time      TIMESTAMPTZ NOT NULL,
    end_time        TIMESTAMPTZ,
    action          TEXT NOT NULL
                     CHECK (action IN ('NO_DATA', 'FULL', 'DIFFERENTIAL', 'INCREMENTAL', 'REINITIALIZE', 'SKIP')),
    rows_inserted   BIGINT DEFAULT 0,
    rows_deleted    BIGINT DEFAULT 0,
    error_message   TEXT,
    status          TEXT NOT NULL
                     CHECK (status IN ('RUNNING', 'COMPLETED', 'FAILED', 'SKIPPED')),
    initiated_by    TEXT
                     CHECK (initiated_by IN ('SCHEDULER', 'MANUAL', 'INITIAL')),
    freshness_deadline TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_hist_pgt_ts ON pgtrickle.pgt_refresh_history (pgt_id, data_timestamp);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_change_tracking (
    source_relid        OID PRIMARY KEY,
    slot_name           TEXT NOT NULL,
    last_consumed_lsn   PG_LSN,
    tracked_by_pgt_ids   BIGINT[]
);

CREATE TABLE IF NOT EXISTS pgtrickle.pgt_scheduler_jobs (
    job_id          BIGSERIAL PRIMARY KEY,
    dag_version     BIGINT NOT NULL,
    unit_key        TEXT NOT NULL,
    unit_kind       TEXT NOT NULL
                     CHECK (unit_kind IN ('singleton', 'atomic_group', 'immediate_closure')),
    member_pgt_ids  BIGINT[] NOT NULL,
    root_pgt_id     BIGINT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'QUEUED'
                     CHECK (status IN ('QUEUED', 'RUNNING', 'SUCCEEDED',
                                       'RETRYABLE_FAILED', 'PERMANENT_FAILED', 'CANCELLED')),
    scheduler_pid   INT NOT NULL,
    worker_pid      INT,
    attempt_no      INT NOT NULL DEFAULT 1,
    enqueued_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ,
    outcome_detail  TEXT,
    retryable       BOOLEAN
);

CREATE INDEX IF NOT EXISTS idx_sched_jobs_status_enqueued
    ON pgtrickle.pgt_scheduler_jobs (status, enqueued_at);
CREATE INDEX IF NOT EXISTS idx_sched_jobs_unit_status
    ON pgtrickle.pgt_scheduler_jobs (unit_key, status);

CREATE OR REPLACE FUNCTION pgtrickle.parse_duration_seconds(input TEXT)
RETURNS BIGINT LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    s TEXT := trim(input);
    total BIGINT := 0;
    num TEXT := '';
    c CHAR;
BEGIN
    IF s IS NULL OR s = '' THEN RETURN NULL; END IF;
    FOR i IN 1..length(s) LOOP
        c := substr(s, i, 1);
        IF c BETWEEN '0' AND '9' THEN
            num := num || c;
        ELSIF c = 'h' THEN
            total := total + num::bigint * 3600; num := '';
        ELSIF c = 'm' THEN
            total := total + num::bigint * 60; num := '';
        ELSIF c = 's' THEN
            total := total + num::bigint; num := '';
        ELSIF c = 'd' THEN
            total := total + num::bigint * 86400; num := '';
        END IF;
    END LOOP;
    IF num <> '' THEN total := total + num::bigint; END IF;
    RETURN total;
END; $$;

CREATE OR REPLACE VIEW pgtrickle.stream_tables_info AS
SELECT st.*,
       now() - st.data_timestamp AS staleness,
       CASE WHEN st.schedule IS NOT NULL
                 AND st.schedule !~ '[\s@]'
            THEN EXTRACT(EPOCH FROM (now() - st.data_timestamp)) >
                 pgtrickle.parse_duration_seconds(st.schedule)
            ELSE NULL::boolean
       END AS stale
FROM pgtrickle.pgt_stream_tables st;
"#;

/// A test database backed by a Testcontainers PostgreSQL 18.3 instance.
///
/// The container is automatically cleaned up when `TestDb` is dropped.
pub struct TestDb {
    pub pool: PgPool,
    _container: ContainerAsync<Postgres>,
}

#[allow(dead_code)]
impl TestDb {
    /// Start a fresh PostgreSQL 18.3 container and connect to it.
    pub async fn new() -> Self {
        let container = Postgres::default()
            .with_tag("18.3-alpine")
            .start()
            .await
            .expect("Failed to start PostgreSQL 18.3 container");

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get mapped port");

        let connection_string = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let pool = PgPool::connect(&connection_string)
            .await
            .expect("Failed to connect to test database");

        TestDb {
            pool,
            _container: container,
        }
    }

    /// Start a fresh container with the pg_trickle catalog schema pre-created.
    pub async fn with_catalog() -> Self {
        let db = Self::new().await;
        // Use raw_sql to execute multiple DDL statements in one call
        sqlx::raw_sql(CATALOG_DDL)
            .execute(&db.pool)
            .await
            .expect("Failed to create pg_trickle catalog schema");
        db
    }

    /// Execute a SQL statement.
    pub async fn execute(&self, sql: &str) {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("SQL execution failed: {}\nSQL: {}", e, sql));
    }

    /// Execute a SQL statement, returning Ok/Err instead of panicking.
    pub async fn try_execute(&self, sql: &str) -> Result<(), sqlx::Error> {
        sqlx::query(sql).execute(&self.pool).await.map(|_| ())
    }

    /// Get a single scalar value from a query.
    pub async fn query_scalar<T>(&self, sql: &str) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
        (T,): for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        sqlx::query_scalar(sql)
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("Scalar query failed: {}\nSQL: {}", e, sql))
    }

    /// Get an optional scalar value from a query.
    ///
    /// Returns `None` both when no rows are returned *and* when the single
    /// returned value is `NULL` (e.g. `max()` / `min()` over an empty set).
    pub async fn query_scalar_opt<T>(&self, sql: &str) -> Option<T>
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
        (T,): for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        sqlx::query_scalar::<_, Option<T>>(sql)
            .fetch_optional(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("Scalar query failed: {}\nSQL: {}", e, sql))
            .flatten()
    }

    /// Count rows in a table.
    pub async fn count(&self, table: &str) -> i64 {
        self.query_scalar::<i64>(&format!("SELECT count(*) FROM {}", table))
            .await
    }

    /// Assert that two tables/subqueries contain exactly the same multiset of rows.
    ///
    /// Uses the symmetric set-difference pattern:
    /// ```sql
    /// SELECT NOT EXISTS (
    ///   (SELECT cols FROM a EXCEPT ALL SELECT cols FROM b)
    ///   UNION ALL
    ///   (SELECT cols FROM b EXCEPT ALL SELECT cols FROM a)
    /// )
    /// ```
    /// This catches: missing rows, extra rows, duplicate discrepancies, and column
    /// value mutations. Both `table_a` and `table_b` can be table names or
    /// parenthesized subqueries.
    pub async fn assert_sets_equal(&self, table_a: &str, table_b: &str, cols: &[&str]) {
        let col_list = cols.join(", ");
        let sql = format!(
            "SELECT NOT EXISTS (
                (SELECT {col_list} FROM {a} EXCEPT ALL SELECT {col_list} FROM {b})
                UNION ALL
                (SELECT {col_list} FROM {b} EXCEPT ALL SELECT {col_list} FROM {a})
            )",
            col_list = col_list,
            a = table_a,
            b = table_b
        );
        let matches: bool = self.query_scalar(&sql).await;
        assert!(
            matches,
            "Set mismatch between {} and {} (columns: {})",
            table_a, table_b, col_list
        );
    }

    /// Assert that two table names expose exactly the same column names **and**
    /// data types (in declaration order).
    ///
    /// When `assert_types_match` is `false` this is a no-op, so callers can
    /// keep the parameter in their call-sites while deferring the type check.
    pub async fn assert_column_types_match(
        &self,
        table_a: &str,
        table_b: &str,
        assert_types_match: bool,
    ) {
        if !assert_types_match {
            return;
        }
        let type_sql = |table: &str| {
            // Strip optional schema prefix so the WHERE clause works for
            // both `public.my_st` and plain `my_st`.
            let (schema_filter, name_filter) = if let Some(dot) = table.rfind('.') {
                (
                    format!("table_schema = '{}'", &table[..dot]),
                    format!("table_name = '{}'", &table[dot + 1..]),
                )
            } else {
                (
                    "table_schema NOT IN ('pg_catalog','information_schema')".to_string(),
                    format!("table_name = '{table}'"),
                )
            };
            format!(
                "SELECT column_name, data_type \
                 FROM information_schema.columns \
                 WHERE {schema_filter} AND {name_filter} \
                   AND column_name NOT LIKE '__pgt_%' \
                 ORDER BY ordinal_position"
            )
        };

        let cols_a: Vec<(String, String)> = sqlx::query_as(&type_sql(table_a))
            .fetch_all(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("type query for {table_a} failed: {e}"));
        let cols_b: Vec<(String, String)> = sqlx::query_as(&type_sql(table_b))
            .fetch_all(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("type query for {table_b} failed: {e}"));

        assert_eq!(
            cols_a, cols_b,
            "Column type mismatch between {table_a} and {table_b}"
        );
    }
}
