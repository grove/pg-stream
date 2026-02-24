//! Shared test helpers for integration tests using Testcontainers.

use sqlx::PgPool;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

/// SQL to create the pgstream catalog schema and tables.
/// Mirrors the extension_sql!() in lib.rs, but for standalone testing.
#[allow(dead_code)]
pub const CATALOG_DDL: &str = r#"
CREATE SCHEMA IF NOT EXISTS pgstream;
CREATE SCHEMA IF NOT EXISTS pgstream_changes;

CREATE TABLE IF NOT EXISTS pgstream.pgs_stream_tables (
    pgs_id           BIGSERIAL PRIMARY KEY,
    pgs_relid        OID NOT NULL UNIQUE,
    pgs_name         TEXT NOT NULL,
    pgs_schema       TEXT NOT NULL,
    defining_query  TEXT NOT NULL,
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
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_pgs_name ON pgstream.pgs_stream_tables (pgs_schema, pgs_name);
CREATE INDEX IF NOT EXISTS idx_pgs_status ON pgstream.pgs_stream_tables (status);

CREATE TABLE IF NOT EXISTS pgstream.pgs_dependencies (
    pgs_id        BIGINT NOT NULL REFERENCES pgstream.pgs_stream_tables(pgs_id) ON DELETE CASCADE,
    source_relid OID NOT NULL,
    source_type  TEXT NOT NULL CHECK (source_type IN ('TABLE', 'STREAM_TABLE', 'VIEW')),
    columns_used TEXT[],
    PRIMARY KEY (pgs_id, source_relid)
);

CREATE INDEX IF NOT EXISTS idx_deps_source ON pgstream.pgs_dependencies (source_relid);

CREATE TABLE IF NOT EXISTS pgstream.pgs_refresh_history (
    refresh_id      BIGSERIAL PRIMARY KEY,
    pgs_id           BIGINT NOT NULL,
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

CREATE INDEX IF NOT EXISTS idx_hist_pgs_ts ON pgstream.pgs_refresh_history (pgs_id, data_timestamp);

CREATE TABLE IF NOT EXISTS pgstream.pgs_change_tracking (
    source_relid        OID PRIMARY KEY,
    slot_name           TEXT NOT NULL,
    last_consumed_lsn   PG_LSN,
    tracked_by_pgs_ids   BIGINT[]
);

CREATE OR REPLACE FUNCTION pgstream.parse_duration_seconds(input TEXT)
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

CREATE OR REPLACE VIEW pgstream.stream_tables_info AS
SELECT st.*,
       now() - st.data_timestamp AS staleness,
       CASE WHEN st.schedule IS NOT NULL
                 AND st.schedule !~ '[\s@]'
            THEN EXTRACT(EPOCH FROM (now() - st.data_timestamp)) >
                 pgstream.parse_duration_seconds(st.schedule)
            ELSE NULL::boolean
       END AS stale
FROM pgstream.pgs_stream_tables st;
"#;

/// A test database backed by a Testcontainers PostgreSQL 18.1 instance.
///
/// The container is automatically cleaned up when `TestDb` is dropped.
pub struct TestDb {
    pub pool: PgPool,
    _container: ContainerAsync<Postgres>,
}

#[allow(dead_code)]
impl TestDb {
    /// Start a fresh PostgreSQL 18.1 container and connect to it.
    pub async fn new() -> Self {
        let container = Postgres::default()
            .with_tag("18.1-alpine")
            .start()
            .await
            .expect("Failed to start PostgreSQL 18.1 container");

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

    /// Start a fresh container with the pg_stream catalog schema pre-created.
    pub async fn with_catalog() -> Self {
        let db = Self::new().await;
        // Use raw_sql to execute multiple DDL statements in one call
        sqlx::raw_sql(CATALOG_DDL)
            .execute(&db.pool)
            .await
            .expect("Failed to create pg_stream catalog schema");
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
    pub async fn query_scalar_opt<T>(&self, sql: &str) -> Option<T>
    where
        T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
        (T,): for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        sqlx::query_scalar(sql)
            .fetch_optional(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("Scalar query failed: {}\nSQL: {}", e, sql))
    }

    /// Count rows in a table.
    pub async fn count(&self, table: &str) -> i64 {
        self.query_scalar::<i64>(&format!("SELECT count(*) FROM {}", table))
            .await
    }
}
