//! Light-E2E test harness — stock PostgreSQL container with bind-mounted
//! extension artifacts.  No `shared_preload_libraries`, no background
//! worker, no shared memory.
//!
//! # How It Works
//!
//! 1. `cargo pgrx package` produces compiled extension artifacts in
//!    `target/release/pg_trickle-pg18/`.
//! 2. The artifacts are bind-mounted to `/tmp/pg_ext` inside a stock
//!    `postgres:18.3` container.
//! 3. An `exec` copies the files to the standard PostgreSQL extension
//!    directories.
//! 4. `CREATE EXTENSION pg_trickle` loads the extension on-demand.
//!
//! # Prerequisites
//!
//! ```bash
//! cargo pgrx package --pg-config $(pg_config --bindir)/pg_config
//! ```
//!
//! Or use the justfile target:
//! ```bash
//! just test-light-e2e
//! ```
//!
//! # Limitations
//!
//! - No background worker / scheduler (no `shared_preload_libraries`).
//! - No auto-refresh (`wait_for_auto_refresh` will always time out).
//! - Custom GUCs (`SET pg_trickle.*`) may not be available in all
//!   connections (registered only when `.so` is first loaded).
//! - On macOS, the Light E2E runner must package Linux artifacts via the
//!   Docker builder image and pass them through `PGT_EXTENSION_DIR`.

use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{ExecCommand, IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
};

static SHARED_DB_COUNTER: AtomicUsize = AtomicUsize::new(1);
static SHARED_CONTAINER: tokio::sync::OnceCell<SharedContainer> =
    tokio::sync::OnceCell::const_new();

struct SharedContainer {
    admin_connection_string: String,
    port: u16,
    container_id: String,
    _container: Mutex<ContainerAsync<GenericImage>>,
}

enum ContainerLease {
    Shared { _shared: &'static SharedContainer },
}

/// Find the `cargo pgrx package` output directory.
///
/// Checks `PGT_EXTENSION_DIR` env var first, then falls back to the
/// default pgrx package output path.
fn is_valid_light_e2e_package_dir(dir: &str) -> bool {
    let base = std::path::Path::new(dir);
    base.join("usr/share/postgresql/18/extension/pg_trickle.control")
        .exists()
        && base
            .join("usr/lib/postgresql/18/lib/pg_trickle.so")
            .exists()
}

fn find_extension_dir() -> String {
    if let Ok(dir) = std::env::var("PGT_EXTENSION_DIR")
        && !dir.is_empty()
        && is_valid_light_e2e_package_dir(&dir)
    {
        return dir;
    }

    // Default: cargo pgrx package output
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let default_path = format!("{}/target/release/pg_trickle-pg18", manifest_dir);
    if is_valid_light_e2e_package_dir(&default_path) {
        return default_path;
    }

    panic!(
        "Valid Linux light-E2E extension package directory not found.\n\
         Expected packaged artifacts under usr/share/postgresql/18/extension and\n\
         usr/lib/postgresql/18/lib.\n\
         Run `bash ./scripts/run_light_e2e_tests.sh --package-only` first,\n\
         or set PGT_EXTENSION_DIR to a valid Linux package output directory."
    );
}

fn shared_db_name(prefix: &str) -> String {
    let sequence = SHARED_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{}", std::process::id(), sequence)
}

fn connection_string(port: u16, db_name: &str) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{db_name}")
}

async fn create_database(admin_connection_string: &str, db_name: &str) {
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_connection_string)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect for CREATE DATABASE {db_name}: {e}"));

    sqlx::query(&format!("CREATE DATABASE \"{db_name}\""))
        .execute(&admin_pool)
        .await
        .unwrap_or_else(|e| panic!("Failed to CREATE DATABASE {db_name}: {e}"));

    admin_pool.close().await;
}

async fn shared_container() -> &'static SharedContainer {
    SHARED_CONTAINER
        .get_or_init(|| async {
            let ext_dir = find_extension_dir();
            let run_id = std::env::var("PGT_LIGHT_E2E_RUN_ID").ok();

            let mut image = GenericImage::new("postgres", "18.3")
                .with_exposed_port(5432_u16.tcp())
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_env_var("POSTGRES_PASSWORD", "postgres")
                .with_env_var("POSTGRES_DB", "postgres")
                .with_mount(Mount::bind_mount(ext_dir, "/tmp/pg_ext"))
                .with_label("com.pgtrickle.test", "true")
                .with_label("com.pgtrickle.suite", "light-e2e")
                .with_label("com.pgtrickle.repo", "pg-stream");

            if let Some(run_id) = run_id {
                image = image.with_label("com.pgtrickle.run-id", run_id);
            }

            let container = image.start().await.expect(
                "Failed to start shared light-e2e container.\n\
                     Ensure Docker is running and postgres:18.3 is available.",
            );

            container
                .exec(ExecCommand::new(vec![
                    "sh",
                    "-c",
                    "cp /tmp/pg_ext/usr/share/postgresql/18/extension/pg_trickle* \
                        /usr/share/postgresql/18/extension/ && \
                     cp /tmp/pg_ext/usr/lib/postgresql/18/lib/pg_trickle* \
                        /usr/lib/postgresql/18/lib/",
                ]))
                .await
                .expect("Failed to copy extension files into shared light-e2e container");

            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("Failed to get mapped port");
            let admin_connection_string = connection_string(port, "postgres");

            SharedContainer {
                admin_connection_string,
                port,
                container_id: container.id().to_string(),
                _container: Mutex::new(container),
            }
        })
        .await
}

/// A test database backed by a stock PostgreSQL 18.3 container with
/// the compiled pg_trickle extension bind-mounted and installed
/// **without** `shared_preload_libraries`.
///
/// The extension is loaded on-demand when `CREATE EXTENSION` is called.
/// Background worker, scheduler, and shared-memory features are NOT
/// available.
pub struct E2eDb {
    pub pool: PgPool,
    connection_string: String,
    container_id: String,
    _container: ContainerLease,
}

#[allow(dead_code)]
impl E2eDb {
    /// Start a fresh PostgreSQL 18.3 container, install the extension
    /// artifacts via bind-mount, and create the extension.
    pub async fn new() -> Self {
        let shared = shared_container().await;
        let db_name = shared_db_name("pgt_light_e2e");
        create_database(&shared.admin_connection_string, &db_name).await;
        let connection_string = connection_string(shared.port, &db_name);
        let pool = Self::connect_with_retry(&connection_string, 15).await;

        E2eDb {
            pool,
            connection_string,
            container_id: shared.container_id.clone(),
            _container: ContainerLease::Shared { _shared: shared },
        }
    }

    /// Light harness does not support the background worker.
    /// Falls back to `new()` (connects to `pg_trickle_test` database).
    pub async fn new_on_postgres_db() -> Self {
        panic!(
            "new_on_postgres_db() requires shared_preload_libraries.\n\
             This test needs the full E2E harness (just test-e2e)."
        );
    }

    /// Light harness does not support bench-tuned containers.
    pub async fn new_bench() -> Self {
        panic!(
            "new_bench() requires shared_preload_libraries and SHM tuning.\n\
             This test needs the full E2E harness (just test-e2e)."
        );
    }

    /// Get the Docker container ID.
    pub fn container_id(&self) -> &str {
        &self.container_id
    }

    /// Get the connection string for this database.
    pub fn connection_string(&self) -> &str {
        &self.connection_string
    }

    /// Execute SQL on a dedicated connection and collect PostgreSQL notices.
    pub async fn try_execute_with_notices(
        &self,
        sql: &str,
    ) -> Result<Vec<String>, tokio_postgres::Error> {
        let (client, mut connection) =
            tokio_postgres::connect(&self.connection_string, tokio_postgres::NoTls).await?;

        let notices = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let notices_task = notices.clone();

        let connection_task = tokio::spawn(async move {
            while let Some(message) = std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                match message {
                    Ok(tokio_postgres::AsyncMessage::Notice(notice)) => {
                        notices_task.lock().await.push(notice.to_string());
                    }
                    Ok(_) => {}
                    Err(err) => return Err(err),
                }
            }
            Ok::<(), tokio_postgres::Error>(())
        });

        let execute_result = client.batch_execute(sql).await;
        drop(client);

        connection_task
            .await
            .unwrap_or_else(|e| panic!("notice collector task failed: {e}"))?;
        execute_result?;

        Ok(notices.lock().await.clone())
    }

    /// Retry connection with backoff.
    async fn connect_with_retry(url: &str, max_attempts: u32) -> PgPool {
        for attempt in 1..=max_attempts {
            match PgPool::connect(url).await {
                Ok(pool) => match sqlx::query("SELECT 1").execute(&pool).await {
                    Ok(_) => return pool,
                    Err(e) if attempt < max_attempts => {
                        eprintln!(
                            "Light-E2E connect attempt {}/{}: ping failed: {}",
                            attempt, max_attempts, e
                        );
                    }
                    Err(e) => {
                        panic!(
                            "Light-E2E: Failed to ping after {} attempts: {}",
                            max_attempts, e
                        );
                    }
                },
                Err(e) if attempt < max_attempts => {
                    eprintln!(
                        "Light-E2E connect attempt {}/{}: {}",
                        attempt, max_attempts, e
                    );
                }
                Err(e) => {
                    panic!(
                        "Light-E2E: Failed to connect after {} attempts: {}",
                        max_attempts, e
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        unreachable!()
    }

    /// Install the extension (`CREATE EXTENSION pg_trickle`).
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

    /// Reload PostgreSQL configuration and wait briefly for SIGHUP settings to apply.
    pub async fn reload_config_and_wait(&self) {
        self.execute("SELECT pg_reload_conf()").await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// Read a GUC value after forcing the extension to load on the same backend.
    pub async fn show_setting(&self, setting: &str) -> String {
        let mut conn =
            self.pool.acquire().await.unwrap_or_else(|e| {
                panic!("Failed to acquire DB connection for SHOW {setting}: {e}")
            });

        let _: String = sqlx::query_scalar("SELECT pgtrickle.version()")
            .fetch_one(&mut *conn)
            .await
            .unwrap_or_else(|e| {
                panic!("Failed to load pg_trickle on backend before SHOW {setting}: {e}")
            });

        let show_sql = format!("SHOW {setting}");
        sqlx::query_scalar(&show_sql)
            .fetch_one(&mut *conn)
            .await
            .unwrap_or_else(|e| panic!("Scalar query failed: {}\nSQL: {}", e, show_sql))
    }

    /// Wait until `SHOW <setting>` reports the expected value.
    pub async fn wait_for_setting(&self, setting: &str, expected: &str) {
        for _ in 0..10 {
            let current = self.show_setting(setting).await;
            if current == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let current = self.show_setting(setting).await;
        panic!("{setting} did not reload to {expected}; current value is {current}");
    }

    /// Apply `ALTER SYSTEM SET` and wait for the new value to become visible.
    pub async fn alter_system_set_and_wait(&self, setting: &str, value_sql: &str, expected: &str) {
        self.execute(&format!("ALTER SYSTEM SET {setting} = {value_sql}"))
            .await;
        self.reload_config_and_wait().await;
        self.wait_for_setting(setting, expected).await;
    }

    /// Apply `ALTER SYSTEM RESET` and wait for the default value to become visible.
    pub async fn alter_system_reset_and_wait(&self, setting: &str, expected: &str) {
        self.execute(&format!("ALTER SYSTEM RESET {setting}")).await;
        self.reload_config_and_wait().await;
        self.wait_for_setting(setting, expected).await;
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

    /// Like [`refresh_st`] but retries on "another refresh is already in
    /// progress" — tolerates advisory-lock races with the background scheduler.
    pub async fn refresh_st_with_retry(&self, name: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match self
                .try_execute(&format!("SELECT pgtrickle.refresh_stream_table('{name}')"))
                .await
            {
                Ok(_) => return,
                Err(e) if e.to_string().contains("already in progress") => {
                    if std::time::Instant::now() >= deadline {
                        panic!(
                            "refresh_st_with_retry: timed out waiting for \
                             concurrent refresh of '{name}' to complete"
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(e) => panic!("refresh_stream_table('{name}') failed: {e:?}"),
            }
        }
    }

    /// Drop a stream table via `pgtrickle.drop_stream_table()`.
    pub async fn drop_st(&self, name: &str) {
        self.execute(&format!("SELECT pgtrickle.drop_stream_table('{name}')"))
            .await;
    }

    /// Alter a stream table via `pgtrickle.alter_stream_table()`.
    pub async fn alter_st(&self, name: &str, args: &str) {
        self.execute(&format!(
            "SELECT pgtrickle.alter_stream_table('{name}', {args})"
        ))
        .await;
    }

    // ── Catalog Query Helpers ──────────────────────────────────────────

    /// Get the status tuple for a specific ST from the catalog.
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

    /// Verify a ST's contents match its defining query exactly.
    pub async fn assert_st_matches_query(&self, st_table: &str, defining_query: &str) {
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
             AND column_name NOT LIKE '__pgt_%'"
        );
        let (raw_cols, cast_cols): (Option<String>, Option<String>) = sqlx::query_as(&cols_sql)
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("cols query failed: {e}"));
        let raw_cols = raw_cols.unwrap_or_else(|| "*".to_string());
        let cast_cols = cast_cols.unwrap_or_else(|| "*".to_string());

        let has_dual_counts: bool = self
            .query_scalar(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM information_schema.columns \
                    WHERE (table_schema || '.' || table_name = '{st_table}' \
                       OR table_name = '{st_table}') \
                    AND column_name = '__pgt_count_l')"
            ))
            .await;

        // Build a visibility filter for set operation STs.
        // INTERSECT/EXCEPT STs keep invisible rows for multiplicity tracking.
        // - INTERSECT (set): visible iff LEAST(count_l, count_r) > 0
        // - INTERSECT ALL:   visible rows = LEAST(count_l, count_r), expanded
        // - EXCEPT (set):    visible iff count_l > 0 AND count_r = 0
        // - EXCEPT ALL:      visible iff count_l > count_r
        let dq_upper = defining_query.to_uppercase();
        let set_op_filter = if has_dual_counts {
            if dq_upper.contains("INTERSECT ALL") {
                ", generate_series(1, LEAST(__pgt_count_l, __pgt_count_r)) WHERE LEAST(__pgt_count_l, __pgt_count_r) > 0"
            } else if dq_upper.contains("INTERSECT") {
                " WHERE __pgt_count_l > 0 AND __pgt_count_r > 0"
            } else if dq_upper.contains("EXCEPT ALL") {
                ", generate_series(1, __pgt_count_l - __pgt_count_r) WHERE __pgt_count_l > __pgt_count_r"
            } else if dq_upper.contains("EXCEPT") {
                " WHERE __pgt_count_l > 0 AND __pgt_count_r = 0"
            } else {
                ""
            }
        } else {
            ""
        };

        let sql = if raw_cols != cast_cols {
            format!(
                "SELECT NOT EXISTS ( \
                    (SELECT {cast_cols} FROM {st_table}{set_op_filter} \
                     EXCEPT ALL \
                     SELECT {cast_cols} FROM ({defining_query}) __pgt_dq) \
                    UNION ALL \
                    (SELECT {cast_cols} FROM ({defining_query}) __pgt_dq2 \
                     EXCEPT ALL \
                     SELECT {cast_cols} FROM {st_table}{set_op_filter}) \
                )"
            )
        } else {
            format!(
                "SELECT NOT EXISTS ( \
                    (SELECT {raw_cols} FROM {st_table}{set_op_filter} EXCEPT ALL ({defining_query})) \
                    UNION ALL \
                    (({defining_query}) EXCEPT ALL SELECT {raw_cols} FROM {st_table}{set_op_filter}) \
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

    /// Wait for any pg_trickle scheduler background worker to appear.
    ///
    /// **Not supported in light-e2e mode** — always returns `false`
    /// because background worker is not running.
    pub async fn wait_for_scheduler(&self, _timeout: std::time::Duration) -> bool {
        false
    }

    /// Wait for the background scheduler to auto-refresh a ST.
    ///
    /// **Not supported in light-e2e mode** — always returns `false`
    /// because background worker is not running.
    pub async fn wait_for_auto_refresh(
        &self,
        _pgt_name: &str,
        _timeout: std::time::Duration,
    ) -> bool {
        false
    }
}
