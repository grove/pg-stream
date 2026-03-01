//! E2E test harness that boots a PostgreSQL 18.1 container with
//! the pg_trickle extension pre-installed.
//!
//! # Prerequisites
//!
//! The Docker image must be built before running E2E tests:
//!
//! ```bash
//! ./tests/build_e2e_image.sh
//! ```
//!
//! # Usage
//!
//! ```rust
//! mod e2e;
//! use e2e::E2eDb;
//!
//! #[tokio::test]
//! async fn test_something() {
//!     let db = E2eDb::new().await.with_extension().await;
//!     db.create_st("my_st", "SELECT * FROM src", "1m", "FULL").await;
//! }
//! ```

use sqlx::PgPool;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
};

const IMAGE_NAME: &str = "pg_trickle_e2e";
const IMAGE_TAG: &str = "latest";

/// Return the Docker image name to use for E2E containers.
///
/// Reads `PGS_E2E_IMAGE` env var. If set, it is expected to be in
/// `name:tag` form (e.g. `pg_trickle_e2e_cov:latest`).
/// Falls back to `IMAGE_NAME:IMAGE_TAG`.
fn e2e_image() -> (String, String) {
    match std::env::var("PGS_E2E_IMAGE") {
        Ok(val) if !val.is_empty() => {
            // Split "name:tag" — default to "latest" if no colon
            if let Some((name, tag)) = val.split_once(':') {
                (name.to_string(), tag.to_string())
            } else {
                (val, "latest".to_string())
            }
        }
        _ => (IMAGE_NAME.to_string(), IMAGE_TAG.to_string()),
    }
}

/// If `PGS_E2E_COVERAGE_DIR` is set, return a bind mount that maps
/// that host directory to `/coverage` inside the container.
fn coverage_mount() -> Option<Mount> {
    match std::env::var("PGS_E2E_COVERAGE_DIR") {
        Ok(dir) if !dir.is_empty() => Some(Mount::bind_mount(dir, "/coverage")),
        _ => None,
    }
}

/// A test database backed by a PostgreSQL 18.1 container with
/// the compiled pg_trickle extension installed and
/// `shared_preload_libraries` configured.
///
/// The container is automatically cleaned up when `E2eDb` is dropped.
pub struct E2eDb {
    pub pool: PgPool,
    _container: ContainerAsync<GenericImage>,
}

#[allow(dead_code)]
impl E2eDb {
    /// Start a fresh PostgreSQL 18.1 container with the extension installed.
    ///
    /// The container is ready to accept connections but the extension is NOT
    /// yet created. Call [`with_extension`] to run `CREATE EXTENSION`.
    pub async fn new() -> Self {
        Self::new_with_db("pg_trickle_test").await
    }

    /// Start a container and connect to the `postgres` database.
    ///
    /// Use this for background-worker / scheduler tests: the scheduler
    /// bgworker hard-codes `connect_worker_to_spi(Some("postgres"), ...)`,
    /// so the extension + STs must live in the `postgres` database for the
    /// scheduler to see them.
    pub async fn new_on_postgres_db() -> Self {
        Self::new_with_db("postgres").await
    }

    /// Start a container configured for benchmarking with resource
    /// constraints and tuning for reduced variance.
    ///
    /// Applies:
    /// - 256 MB shared memory (`--shm-size`)
    /// - PostgreSQL tuning: `work_mem`, `effective_cache_size`,
    ///   `synchronous_commit = off`, `max_wal_size`
    /// - `log_min_messages = info` so `[PGS_PROFILE]` lines appear
    ///   in the container log
    ///
    /// For CPU pinning (further reduces variance), run the benchmark
    /// with Docker CPU constraints externally:
    /// ```bash
    /// docker run --cpus=2 --cpuset-cpus=0,1 --memory=2g ...
    /// ```
    pub async fn new_bench() -> Self {
        Self::new_with_db_bench("pg_trickle_test").await
    }

    /// Get the Docker container ID (for `docker logs` and profile capture).
    pub fn container_id(&self) -> &str {
        self._container.id()
    }

    /// Internal: start a container using the given database name.
    async fn new_with_db(db_name: &str) -> Self {
        let (img_name, img_tag) = e2e_image();
        let mut image = GenericImage::new(img_name, img_tag)
            .with_exposed_port(5432_u16.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", db_name);

        // When running under the coverage harness, bind-mount a host
        // directory at /coverage so profraw files are written to the host.
        if let Some(mount) = coverage_mount() {
            image = image.with_mount(mount);
        }

        let container = image.start().await.expect(
            "Failed to start pg_trickle E2E container. \
                     Did you run ./tests/build_e2e_image.sh first?",
        );

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get mapped port");

        let connection_string = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/{}",
            port, db_name,
        );

        let pool = Self::connect_with_retry(&connection_string, 15).await;

        E2eDb {
            pool,
            _container: container,
        }
    }

    /// Internal: start a bench-specific container with SHM and PG tuning.
    async fn new_with_db_bench(db_name: &str) -> Self {
        let (img_name, img_tag) = e2e_image();
        let mut image = GenericImage::new(img_name, img_tag)
            .with_exposed_port(5432_u16.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", db_name)
            .with_shm_size(268_435_456); // 256 MB shared memory

        // When running under the coverage harness, bind-mount a host
        // directory at /coverage so profraw files are written to the host.
        if let Some(mount) = coverage_mount() {
            image = image.with_mount(mount);
        }

        let container = image.start().await.expect(
            "Failed to start bench container. \
             Did you run ./tests/build_e2e_image.sh first?",
        );

        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get mapped port");

        let connection_string = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/{}",
            port, db_name,
        );

        let pool = Self::connect_with_retry(&connection_string, 15).await;

        let db = E2eDb {
            pool,
            _container: container,
        };

        // Apply runtime PostgreSQL tuning for stable benchmarks.
        // These are SIGHUP-level parameters that take effect after reload.
        db.execute("ALTER SYSTEM SET work_mem = '64MB'").await;
        db.execute("ALTER SYSTEM SET effective_cache_size = '512MB'")
            .await;
        db.execute("ALTER SYSTEM SET maintenance_work_mem = '128MB'")
            .await;
        db.execute("ALTER SYSTEM SET synchronous_commit = 'off'")
            .await;
        db.execute("ALTER SYSTEM SET max_wal_size = '1GB'").await;
        // Limit temp file usage to prevent runaway disk consumption
        // from CTE materialisation and sort spills.
        db.execute("ALTER SYSTEM SET temp_file_limit = '512MB'")
            .await;
        // Aggressive autovacuum: change-buffer tables and stream tables
        // accumulate dead tuples rapidly during differential refreshes.
        // Without aggressive settings the default autovacuum can't keep
        // up, causing the PostgreSQL data directory to bloat (121 GB+
        // observed in TPC-H Phase 2 tests).
        db.execute("ALTER SYSTEM SET autovacuum_vacuum_scale_factor = '0.01'")
            .await;
        db.execute("ALTER SYSTEM SET autovacuum_vacuum_threshold = '50'")
            .await;
        db.execute("ALTER SYSTEM SET autovacuum_naptime = '5s'")
            .await;
        db.execute("ALTER SYSTEM SET autovacuum_vacuum_cost_delay = '2ms'")
            .await;
        db.execute("ALTER SYSTEM SET autovacuum_vacuum_cost_limit = '1000'")
            .await;
        // Enable INFO logging so [PGS_PROFILE] lines appear in server stderr
        db.execute("ALTER SYSTEM SET log_min_messages = 'info'")
            .await;
        db.execute("SELECT pg_reload_conf()").await;
        // Allow settings to propagate
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        db
    }

    /// Retry connection with backoff — the container may need a moment
    /// after the "ready to accept connections" log line.
    async fn connect_with_retry(url: &str, max_attempts: u32) -> PgPool {
        for attempt in 1..=max_attempts {
            match PgPool::connect(url).await {
                Ok(pool) => {
                    // Verify the connection actually works
                    match sqlx::query("SELECT 1").execute(&pool).await {
                        Ok(_) => return pool,
                        Err(e) if attempt < max_attempts => {
                            eprintln!(
                                "E2E connect attempt {}/{}: ping failed: {}",
                                attempt, max_attempts, e
                            );
                        }
                        Err(e) => {
                            panic!("E2E: Failed to ping after {} attempts: {}", max_attempts, e);
                        }
                    }
                }
                Err(e) if attempt < max_attempts => {
                    eprintln!("E2E connect attempt {}/{}: {}", attempt, max_attempts, e);
                }
                Err(e) => {
                    panic!(
                        "E2E: Failed to connect after {} attempts: {}",
                        max_attempts, e
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        unreachable!()
    }

    /// Install the extension (`CREATE EXTENSION pg_trickle`).
    ///
    /// This creates all catalog tables, views, event triggers, and
    /// SQL functions in the `pg_trickle` schema.
    pub async fn with_extension(self) -> Self {
        sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trickle CASCADE")
            .execute(&self.pool)
            .await
            .expect("Failed to CREATE EXTENSION pg_trickle");
        self
    }

    // ── SQL Execution Helpers ──────────────────────────────────────────

    /// Execute a SQL statement (panics on error).
    pub async fn execute(&self, sql: &str) {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("SQL failed: {}\nSQL: {}", e, sql));
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

    // ── Extension API Helpers ──────────────────────────────────────────

    /// Create a stream table via `pgtrickle.create_stream_table()`.
    pub async fn create_st(&self, name: &str, query: &str, schedule: &str, refresh_mode: &str) {
        let sql = format!(
            "SELECT pgtrickle.create_stream_table('{name}', $${query}$$, \
             '{schedule}', '{refresh_mode}')"
        );
        self.execute(&sql).await;
    }

    /// Create a stream table with explicit `initialize` parameter.
    pub async fn create_st_with_init(
        &self,
        name: &str,
        query: &str,
        schedule: &str,
        refresh_mode: &str,
        initialize: bool,
    ) {
        let sql = format!(
            "SELECT pgtrickle.create_stream_table('{name}', $${query}$$, \
             '{schedule}', '{refresh_mode}', {initialize})"
        );
        self.execute(&sql).await;
    }

    /// Refresh a stream table via `pgtrickle.refresh_stream_table()`.
    pub async fn refresh_st(&self, name: &str) {
        self.execute(&format!("SELECT pgtrickle.refresh_stream_table('{name}')"))
            .await;
    }

    /// Drop a stream table via `pgtrickle.drop_stream_table()`.
    pub async fn drop_st(&self, name: &str) {
        self.execute(&format!("SELECT pgtrickle.drop_stream_table('{name}')"))
            .await;
    }

    /// Alter a stream table via `pgtrickle.alter_stream_table()`.
    ///
    /// `args` should be the named arguments after the name, e.g.:
    /// `"schedule => '5m'"` or
    /// `"status => 'SUSPENDED'"`.
    pub async fn alter_st(&self, name: &str, args: &str) {
        self.execute(&format!(
            "SELECT pgtrickle.alter_stream_table('{name}', {args})"
        ))
        .await;
    }

    // ── Catalog Query Helpers ──────────────────────────────────────────

    /// Get the status tuple for a specific ST from the catalog.
    ///
    /// Returns `(status, refresh_mode, is_populated, consecutive_errors)`.
    pub async fn pgt_status(&self, name: &str) -> (String, String, bool, i32) {
        sqlx::query_as(
            "SELECT status, refresh_mode, is_populated, consecutive_errors \
             FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_schema || '.' || pgt_name = $1 OR pgt_name = $1",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .unwrap_or_else(|e| panic!("pgt_status query failed for '{}': {}", name, e))
    }

    /// Verify a ST's contents match its defining query exactly (set equality).
    ///
    /// Ignores the internal `__pgt_row_id` column by comparing only the
    /// user-visible columns produced by the defining query.
    ///
    /// Columns of type `json` are cast to `text` because the `json` type
    /// does not have an equality operator (needed by `EXCEPT`).
    pub async fn assert_st_matches_query(&self, st_table: &str, defining_query: &str) {
        // Get column names from the ST, excluding internal columns.
        // Also get the cast expressions (json → text) for EXCEPT compatibility.
        let cols_sql = format!(
            "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position), \
                    string_agg(\
                        CASE WHEN data_type = 'json' \
                             THEN column_name || '::text' \
                             ELSE column_name END, \
                        ', ' ORDER BY ordinal_position) \
             FROM information_schema.columns \
             WHERE (table_schema || '.' || table_name = '{st_table}' \
                OR table_name = '{st_table}') \
             AND column_name NOT IN ('__pgt_row_id', '__pgt_count')"
        );
        let (raw_cols, cast_cols): (Option<String>, Option<String>) = sqlx::query_as(&cols_sql)
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("cols query failed: {e}"));
        let raw_cols = raw_cols.unwrap_or_else(|| "*".to_string());
        let cast_cols = cast_cols.unwrap_or_else(|| "*".to_string());

        // If there are json columns, wrap both sides to cast consistently.
        // Otherwise use the simpler direct comparison.
        let sql = if raw_cols != cast_cols {
            // json columns present: cast them on both sides of EXCEPT
            format!(
                "SELECT NOT EXISTS ( \
                    (SELECT {cast_cols} FROM {st_table} \
                     EXCEPT \
                     SELECT {cast_cols} FROM ({defining_query}) __pgt_dq) \
                    UNION ALL \
                    (SELECT {cast_cols} FROM ({defining_query}) __pgt_dq2 \
                     EXCEPT \
                     SELECT {cast_cols} FROM {st_table}) \
                )"
            )
        } else {
            format!(
                "SELECT NOT EXISTS ( \
                    (SELECT {raw_cols} FROM {st_table} EXCEPT ({defining_query})) \
                    UNION ALL \
                    (({defining_query}) EXCEPT SELECT {raw_cols} FROM {st_table}) \
                )"
            )
        };
        let matches: bool = self.query_scalar(&sql).await;
        assert!(
            matches,
            "ST '{}' contents do not match defining query:\n  {}",
            st_table, defining_query,
        );
    }

    // ── Infrastructure Query Helpers ───────────────────────────────────

    /// Check if a trigger exists on a table.
    pub async fn trigger_exists(&self, trigger_name: &str, table: &str) -> bool {
        self.query_scalar::<bool>(&format!(
            "SELECT EXISTS(\
                SELECT 1 FROM pg_trigger t \
                JOIN pg_class c ON t.tgrelid = c.oid \
                WHERE t.tgname = '{trigger_name}' \
                AND c.relname = '{table}'\
            )"
        ))
        .await
    }

    /// Check if a table exists in a given schema.
    pub async fn table_exists(&self, schema: &str, table: &str) -> bool {
        self.query_scalar::<bool>(&format!(
            "SELECT EXISTS(\
                SELECT 1 FROM information_schema.tables \
                WHERE table_schema = '{schema}' AND table_name = '{table}'\
            )"
        ))
        .await
    }

    /// Get the OID of a table (as i32).
    pub async fn table_oid(&self, table: &str) -> i32 {
        self.query_scalar::<i32>(&format!("SELECT '{table}'::regclass::oid::int"))
            .await
    }

    /// Wait for the background scheduler to auto-refresh a ST.
    ///
    /// Polls `data_timestamp` until it advances past the initial value
    /// or the timeout expires. Returns `true` if a refresh was detected.
    pub async fn wait_for_auto_refresh(
        &self,
        pgt_name: &str,
        timeout: std::time::Duration,
    ) -> bool {
        let start = std::time::Instant::now();
        let initial_ts: Option<String> = self
            .query_scalar_opt(&format!(
                "SELECT data_timestamp::text \
                 FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{pgt_name}'"
            ))
            .await;

        loop {
            if start.elapsed() > timeout {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let current_ts: Option<String> = self
                .query_scalar_opt(&format!(
                    "SELECT data_timestamp::text \
                     FROM pgtrickle.pgt_stream_tables WHERE pgt_name = '{pgt_name}'"
                ))
                .await;

            if current_ts != initial_ts && current_ts.is_some() {
                return true;
            }
        }
    }
}
