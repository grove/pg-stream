//! E2E test harness that boots a PostgreSQL 18.1 container with
//! the pg_trickle extension pre-installed.
//!
//! # Harness Selection
//!
//! - **Full (default)**: Custom Docker image with `shared_preload_libraries`,
//!   background worker, and shared memory.  Requires
//!   `./tests/build_e2e_image.sh`.
//! - **Light** (`--features light-e2e`): Stock `postgres:18.1` container with
//!   bind-mounted extension artifacts.  No background worker or scheduler.
//!   Much faster to build — only needs `cargo pgrx package`.
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

// ── Light-E2E feature gate ─────────────────────────────────────────────
// When the `light-e2e` feature is active, use the lightweight harness that
// bind-mounts `cargo pgrx package` output into a stock PostgreSQL container.
#[cfg(feature = "light-e2e")]
mod light;
#[cfg(feature = "light-e2e")]
pub use light::E2eDb;

// ── Full E2E harness (default) ─────────────────────────────────────────
#[cfg(not(feature = "light-e2e"))]
use sqlx::{PgPool, postgres::PgPoolOptions};
#[cfg(not(feature = "light-e2e"))]
use std::sync::{
    Arc, LazyLock, Mutex,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(not(feature = "light-e2e"))]
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, Mount, WaitFor},
    runners::AsyncRunner,
};

#[cfg(not(feature = "light-e2e"))]
const IMAGE_NAME: &str = "pg_trickle_e2e";
#[cfg(not(feature = "light-e2e"))]
const IMAGE_TAG: &str = "latest";
#[cfg(not(feature = "light-e2e"))]
static SHARED_DB_COUNTER: AtomicUsize = AtomicUsize::new(1);
#[cfg(not(feature = "light-e2e"))]
static SHARED_CONTAINER: tokio::sync::OnceCell<SharedContainer> =
    tokio::sync::OnceCell::const_new();
#[cfg(not(feature = "light-e2e"))]
static SHARED_POSTGRES_DB_LOCK: LazyLock<Arc<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));

#[cfg(not(feature = "light-e2e"))]
struct SharedContainer {
    admin_connection_string: String,
    port: u16,
    container_id: String,
    _container: Mutex<ContainerAsync<GenericImage>>,
}

#[cfg(not(feature = "light-e2e"))]
enum ContainerLease {
    Shared {
        _shared: &'static SharedContainer,
    },
    Dedicated {
        _container: Box<ContainerAsync<GenericImage>>,
    },
}

#[cfg(not(feature = "light-e2e"))]
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

#[cfg(not(feature = "light-e2e"))]
fn coverage_mount() -> Option<Mount> {
    match std::env::var("PGS_E2E_COVERAGE_DIR") {
        Ok(dir) if !dir.is_empty() => Some(Mount::bind_mount(dir, "/coverage")),
        _ => None,
    }
}

#[cfg(not(feature = "light-e2e"))]
/// Verify an image exists locally before attempting to start a container.
///
/// testcontainers falls back to a Docker Hub pull when the image is not
/// found locally.  For local-only image names (like `pg_trickle_e2e`) that
/// produces a confusing "pull access denied" 404.  This check panics early
/// with a clear, actionable message instead.
async fn assert_docker_image_exists(name: &str, tag: &str) {
    let status = tokio::process::Command::new("docker")
        .args(["image", "inspect", &format!("{}:{}", name, tag)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .expect("Failed to run `docker image inspect` — is Docker running?");
    if !status.success() {
        panic!(
            "Docker image {name}:{tag} not found locally.\n\
             Build it first:\n\
             • E2E tests:     just build-e2e-image\n\
             • Upgrade tests: just build-upgrade-image"
        );
    }
}

#[cfg(not(feature = "light-e2e"))]
fn shared_db_name(prefix: &str) -> String {
    let sequence = SHARED_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{}", std::process::id(), sequence)
}

#[cfg(not(feature = "light-e2e"))]
fn connection_string(port: u16, db_name: &str) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{db_name}")
}

#[cfg(not(feature = "light-e2e"))]
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

#[cfg(not(feature = "light-e2e"))]
async fn terminate_other_backends(admin_pool: &PgPool) {
    // Shared-db tests reuse `postgres`, so the previous test's sqlx pool and
    // the per-database scheduler worker can still hold locks when the next
    // test starts its reset. Terminate them first to keep cleanup deterministic.
    sqlx::query(
        "SELECT pg_terminate_backend(pid) \
         FROM pg_stat_activity \
         WHERE datname = current_database() \
                     AND pid <> pg_backend_pid() \
                     AND (backend_type = 'client backend' \
                                OR application_name = 'pg_trickle scheduler')",
    )
    .execute(admin_pool)
    .await
    .unwrap_or_else(|e| panic!("Failed to terminate shared postgres backends: {e}"));

    for _ in 0..20 {
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*) \
             FROM pg_stat_activity \
             WHERE datname = current_database() \
                             AND pid <> pg_backend_pid() \
                             AND (backend_type = 'client backend' \
                                        OR application_name = 'pg_trickle scheduler')",
        )
        .fetch_one(admin_pool)
        .await
        .unwrap_or_else(|e| panic!("Failed to count shared postgres backends: {e}"));

        if remaining == 0 {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) \
         FROM pg_stat_activity \
         WHERE datname = current_database() \
                     AND pid <> pg_backend_pid() \
                     AND (backend_type = 'client backend' \
                                OR application_name = 'pg_trickle scheduler')",
    )
    .fetch_one(admin_pool)
    .await
    .unwrap_or_else(|e| panic!("Failed to count shared postgres backends: {e}"));

    panic!(
        "Timed out waiting for shared postgres backends to exit before reset; {remaining} backend(s) still active"
    );
}

#[cfg(not(feature = "light-e2e"))]
async fn reset_postgres_database(admin_connection_string: &str) {
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_connection_string)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect for postgres reset: {e}"));

    terminate_other_backends(&admin_pool).await;

    // Background-worker tests share the `postgres` database and use
    // `ALTER SYSTEM` to speed up the scheduler. Those changes persist in
    // `postgresql.auto.conf`, so clear them during reset to keep each test
    // independent of execution order.
    //
    // Drop the extension before reloading config. A pre-drop pg_reload_conf()
    // wakes the shared launcher worker, which can respawn a scheduler for
    // `postgres` while reset is still in progress.
    for sql in [
        "DROP EXTENSION IF EXISTS pg_trickle CASCADE",
        "ALTER SYSTEM RESET ALL",
        "SELECT pg_reload_conf()",
        "DROP SCHEMA IF EXISTS public CASCADE",
        "CREATE SCHEMA public",
        "GRANT ALL ON SCHEMA public TO postgres",
        "GRANT ALL ON SCHEMA public TO public",
    ] {
        sqlx::query(sql)
            .execute(&admin_pool)
            .await
            .unwrap_or_else(|e| {
                panic!("Failed to reset shared postgres database with `{sql}`: {e}")
            });
    }

    admin_pool.close().await;
}

#[cfg(not(feature = "light-e2e"))]
async fn shared_container() -> &'static SharedContainer {
    SHARED_CONTAINER
        .get_or_init(|| async {
            let (img_name, img_tag) = e2e_image();
            assert_docker_image_exists(&img_name, &img_tag).await;
            let run_id = std::env::var("PGT_E2E_RUN_ID").ok();

            let mut image = GenericImage::new(img_name, img_tag)
                .with_exposed_port(5432_u16.tcp())
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_env_var("POSTGRES_PASSWORD", "postgres")
                .with_env_var("POSTGRES_DB", "postgres")
                .with_label("com.pgtrickle.test", "true")
                .with_label("com.pgtrickle.suite", "full-e2e")
                .with_label("com.pgtrickle.repo", "pg-stream");

            if let Some(run_id) = run_id {
                image = image.with_label("com.pgtrickle.run-id", run_id);
            }

            if let Some(mount) = coverage_mount() {
                image = image.with_mount(mount);
            }

            let container = image.start().await.expect(
                "Failed to start shared pg_trickle E2E container. \
                 Did you run ./tests/build_e2e_image.sh first?",
            );

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

/// A test database backed by a PostgreSQL 18.1 container with
/// the compiled pg_trickle extension installed and
/// `shared_preload_libraries` configured.
///
/// The container is automatically cleaned up when `E2eDb` is dropped.
#[cfg(not(feature = "light-e2e"))]
pub struct E2eDb {
    pub pool: PgPool,
    connection_string: String,
    container_id: String,
    _shared_postgres_db_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    _container: ContainerLease,
}

#[cfg(not(feature = "light-e2e"))]
#[allow(dead_code)]
impl E2eDb {
    /// Start a fresh PostgreSQL 18.1 container with the extension installed.
    ///
    /// The container is ready to accept connections but the extension is NOT
    /// yet created. Call [`with_extension`] to run `CREATE EXTENSION`.
    pub async fn new() -> Self {
        let shared = shared_container().await;
        let db_name = shared_db_name("pgt_e2e");
        create_database(&shared.admin_connection_string, &db_name).await;
        let pool = Self::connect_with_retry(&connection_string(shared.port, &db_name), 15).await;
        let connection_string = connection_string(shared.port, &db_name);

        E2eDb {
            pool,
            connection_string,
            container_id: shared.container_id.clone(),
            _shared_postgres_db_guard: None,
            _container: ContainerLease::Shared { _shared: shared },
        }
    }

    /// Start a container and connect to the `postgres` database.
    ///
    /// Use this for background-worker / scheduler tests: the scheduler
    /// bgworker hard-codes `connect_worker_to_spi(Some("postgres"), ...)`,
    /// so the extension + STs must live in the `postgres` database for the
    /// scheduler to see them.
    pub async fn new_on_postgres_db() -> Self {
        let shared_postgres_db_guard = SHARED_POSTGRES_DB_LOCK.clone().lock_owned().await;
        let shared = shared_container().await;
        reset_postgres_database(&shared.admin_connection_string).await;
        let pool = Self::connect_with_retry(&shared.admin_connection_string, 15).await;

        E2eDb {
            pool,
            connection_string: shared.admin_connection_string.clone(),
            container_id: shared.container_id.clone(),
            _shared_postgres_db_guard: Some(shared_postgres_db_guard),
            _container: ContainerLease::Shared { _shared: shared },
        }
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
        &self.container_id
    }

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

    /// Internal: start a container using the given database name.
    async fn new_with_db(db_name: &str) -> Self {
        let (img_name, img_tag) = e2e_image();
        assert_docker_image_exists(&img_name, &img_tag).await;
        let run_id = std::env::var("PGT_E2E_RUN_ID").ok();
        let mut image = GenericImage::new(img_name, img_tag)
            .with_exposed_port(5432_u16.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", db_name)
            .with_label("com.pgtrickle.test", "true")
            .with_label("com.pgtrickle.suite", "full-e2e")
            .with_label("com.pgtrickle.repo", "pg-stream");

        if let Some(run_id) = run_id {
            image = image.with_label("com.pgtrickle.run-id", run_id);
        }

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
            connection_string,
            container_id: container.id().to_string(),
            _shared_postgres_db_guard: None,
            _container: ContainerLease::Dedicated {
                _container: Box::new(container),
            },
        }
    }

    /// Internal: start a bench-specific container with SHM and PG tuning.
    async fn new_with_db_bench(db_name: &str) -> Self {
        let (img_name, img_tag) = e2e_image();
        assert_docker_image_exists(&img_name, &img_tag).await;
        let run_id = std::env::var("PGT_E2E_RUN_ID").ok();
        let mut image = GenericImage::new(img_name, img_tag)
            .with_exposed_port(5432_u16.tcp())
            .with_wait_for(WaitFor::message_on_stderr(
                "database system is ready to accept connections",
            ))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_DB", db_name)
            .with_label("com.pgtrickle.test", "true")
            .with_label("com.pgtrickle.suite", "full-e2e")
            .with_label("com.pgtrickle.repo", "pg-stream")
            .with_shm_size(268_435_456); // 256 MB shared memory

        if let Some(run_id) = run_id {
            image = image.with_label("com.pgtrickle.run-id", run_id);
        }

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
            connection_string,
            container_id: container.id().to_string(),
            _shared_postgres_db_guard: None,
            _container: ContainerLease::Dedicated {
                _container: Box::new(container),
            },
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
        // Cap temp file usage per query to prevent runaway disk
        // consumption from CTE materialisation and sort spills.
        // The L₀ via EXCEPT ALL approach for join children ≤ 3 tables
        // stays within 4GB at SF=0.01. Larger join chains (5+ tables)
        // use L₁ + Part 3 correction to avoid temp spills.
        db.execute("ALTER SYSTEM SET temp_file_limit = '4GB'").await;
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
        db.reload_config_and_wait().await;

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

    /// Reload PostgreSQL configuration and wait briefly for SIGHUP settings to apply.
    pub async fn reload_config_and_wait(&self) {
        self.execute("SELECT pg_reload_conf()").await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    /// Read a GUC value via `SHOW`.
    pub async fn show_setting(&self, setting: &str) -> String {
        self.query_scalar(&format!("SHOW {setting}")).await
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
    ///
    /// For EXCEPT STs (which keep invisible rows with dual-count tracking),
    /// the comparison filters to visible rows only.
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
             AND column_name NOT LIKE '__pgt_%'"
        );
        let (raw_cols, cast_cols): (Option<String>, Option<String>) = sqlx::query_as(&cols_sql)
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("cols query failed: {e}"));
        let raw_cols = raw_cols.unwrap_or_else(|| "*".to_string());
        let cast_cols = cast_cols.unwrap_or_else(|| "*".to_string());

        // Check whether the ST has dual-count columns (__pgt_count_l,
        // __pgt_count_r), indicating an EXCEPT or INTERSECT set operation.
        // EXCEPT STs keep invisible rows for multiplicity tracking, so we
        // must filter to visible rows only.
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
                " WHERE LEAST(__pgt_count_l, __pgt_count_r) > 0"
            } else if dq_upper.contains("INTERSECT") {
                " WHERE __pgt_count_l > 0 AND __pgt_count_r > 0"
            } else if dq_upper.contains("EXCEPT ALL") {
                " WHERE __pgt_count_l > __pgt_count_r"
            } else if dq_upper.contains("EXCEPT") {
                " WHERE __pgt_count_l > 0 AND __pgt_count_r = 0"
            } else {
                ""
            }
        } else {
            ""
        };

        // If there are json columns, wrap both sides to cast consistently.
        // Otherwise use the simpler direct comparison.
        let sql = if raw_cols != cast_cols {
            // json columns present: cast them on both sides of EXCEPT
            format!(
                "SELECT NOT EXISTS ( \
                    (SELECT {cast_cols} FROM {st_table}{set_op_filter} \
                     EXCEPT \
                     SELECT {cast_cols} FROM ({defining_query}) __pgt_dq) \
                    UNION ALL \
                    (SELECT {cast_cols} FROM ({defining_query}) __pgt_dq2 \
                     EXCEPT \
                     SELECT {cast_cols} FROM {st_table}{set_op_filter}) \
                )"
            )
        } else {
            format!(
                "SELECT NOT EXISTS ( \
                    (SELECT {raw_cols} FROM {st_table}{set_op_filter} EXCEPT ({defining_query})) \
                    UNION ALL \
                    (({defining_query}) EXCEPT SELECT {raw_cols} FROM {st_table}{set_op_filter}) \
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
