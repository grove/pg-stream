//! E2E test harness that boots a PostgreSQL 18.3 container with
//! the pg_trickle extension pre-installed.
//!
//! # Harness Selection
//!
//! - **Full (default)**: Custom Docker image with `shared_preload_libraries`,
//!   background worker, and shared memory.  Requires
//!   `./tests/build_e2e_image.sh`.
//! - **Light** (`--features light-e2e`): Stock `postgres:18.3` container with
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

/// Container ID registered for `atexit` cleanup.
///
/// `SHARED_CONTAINER` lives in a `static OnceCell` whose `Drop` is never
/// called (Rust does not run destructors for statics on process exit).
/// Ryuk — testcontainers' normal reaper — may not work in all environments
/// (e.g. macOS Docker Desktop, where the Docker socket is not reachable from
/// inside the Ryuk container). Storing the ID here lets a C-level `atexit`
/// handler stop and remove the container when the test binary exits normally.
#[cfg(not(feature = "light-e2e"))]
static SHARED_CONTAINER_CLEANUP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
#[cfg(not(feature = "light-e2e"))]
// Serialises tests that use ALTER SYSTEM against the process-local shared
// container. `cargo test --test <name>` runs each test binary in its own
// process, and each process owns its own shared container via SHARED_CONTAINER,
// so cross-binary locking is unnecessary here.
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
async fn reset_server_configuration(admin_connection_string: &str) {
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_connection_string)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect for server config reset: {e}"));

    for sql in ["ALTER SYSTEM RESET ALL", "SELECT pg_reload_conf()"] {
        sqlx::query(sql)
            .execute(&admin_pool)
            .await
            .unwrap_or_else(|e| panic!("Failed to reset server config with `{sql}`: {e}"));
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

            // Register an atexit handler to stop+remove the shared container
            // when the test binary exits.  This complements Ryuk in case Ryuk
            // cannot reach the Docker socket (common on macOS Docker Desktop).
            {
                let _ = SHARED_CONTAINER_CLEANUP_ID.set(container.id().to_string());

                unsafe extern "C" fn rm_shared_container_at_exit() {
                    if let Some(id) = SHARED_CONTAINER_CLEANUP_ID.get() {
                        // -f: stop if running; -v: also remove anonymous volumes
                        let _ = std::process::Command::new("docker")
                            .args(["rm", "-fv", id])
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status();
                    }
                }

                unsafe extern "C" {
                    fn atexit(func: unsafe extern "C" fn()) -> i32;
                }

                // SAFETY: `rm_shared_container_at_exit` is a plain C function
                // pointer that only touches `SHARED_CONTAINER_CLEANUP_ID`, a
                // `static OnceLock` safe to read after the async runtime is
                // torn down.  `std::process::Command` uses fork+exec and is
                // safe to call from an atexit handler.
                unsafe {
                    atexit(rm_shared_container_at_exit);
                }
            }

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

/// A test database backed by a PostgreSQL 18.3 container with
/// the compiled pg_trickle extension installed and
/// `shared_preload_libraries` configured.
///
/// The container is automatically cleaned up when `E2eDb` is dropped.
#[cfg(not(feature = "light-e2e"))]
pub struct E2eDb {
    pub pool: PgPool,
    connection_string: String,
    container_id: String,
    _shared_scheduler_test_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    _container: ContainerLease,
}

#[cfg(not(feature = "light-e2e"))]
#[allow(dead_code)]
impl E2eDb {
    /// Start a fresh PostgreSQL 18.3 container with the extension installed.
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
            _shared_scheduler_test_guard: None,
            _container: ContainerLease::Shared { _shared: shared },
        }
    }

    /// Historical compatibility helper for scheduler-focused tests.
    ///
    /// Dynamic scheduler workers now connect to the database name supplied in
    /// `bgw_extra`, so these tests no longer need to run inside `postgres`
    /// itself. The remaining isolation concern is server-level state:
    /// scheduler tests use `ALTER SYSTEM`, which affects the whole shared
    /// container. This helper therefore resets server config, creates a fresh
    /// per-test database, and holds a process-local guard for the test's
    /// lifetime so parallel tests in the same binary cannot interfere.
    pub async fn new_on_postgres_db() -> Self {
        let shared_scheduler_test_guard = SHARED_POSTGRES_DB_LOCK.clone().lock_owned().await;
        let shared = shared_container().await;
        reset_server_configuration(&shared.admin_connection_string).await;

        let db_name = shared_db_name("pgt_sched_e2e");
        create_database(&shared.admin_connection_string, &db_name).await;
        let connection_string = connection_string(shared.port, &db_name);
        let pool = Self::connect_with_retry(&connection_string, 15).await;

        E2eDb {
            pool,
            connection_string,
            container_id: shared.container_id.clone(),
            _shared_scheduler_test_guard: Some(shared_scheduler_test_guard),
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
            _shared_scheduler_test_guard: None,
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
            .with_shm_size(536_870_912); // 512 MB — headroom for work_mem×max_connections

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
            _shared_scheduler_test_guard: None,
            _container: ContainerLease::Dedicated {
                _container: Box::new(container),
            },
        };

        // Apply runtime PostgreSQL tuning for stable benchmarks.
        // These are SIGHUP-level parameters that take effect after reload.
        // 256 MB: large enough for q05/q07/q08/q09 multi-CTE join deltas
        // at SF=0.01 without spilling to disk (SF=0.01 delta CTEs peak
        // ~180–220 MB for the 8-table join in q08).
        db.execute("ALTER SYSTEM SET work_mem = '256MB'").await;
        db.execute("ALTER SYSTEM SET effective_cache_size = '512MB'")
            .await;
        db.execute("ALTER SYSTEM SET maintenance_work_mem = '128MB'")
            .await;
        db.execute("ALTER SYSTEM SET synchronous_commit = 'off'")
            .await;
        db.execute("ALTER SYSTEM SET max_wal_size = '1GB'").await;
        // Cap temp file usage per query to prevent runaway disk
        // consumption from CTE materialisation and sort spills.
        // q05/q07/q08/q09 are known DVM-limited — the L₁+correction delta
        // SQL for 5+ table joins exceeds this regardless of work_mem.
        // Keeping the limit low makes those queries fail fast each cycle
        // rather than writing tens of GB to disk before aborting.
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
        // Raise the scheduler tick interval to its maximum so the background
        // worker does not auto-refresh during bench tests, which use explicit
        // manual refreshes.  This is a defence-in-depth companion to using
        // '24h' schedules for bench stream tables.
        db.execute("ALTER SYSTEM SET pg_trickle.scheduler_interval_ms = '60000'")
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
    ///
    /// If the `PGT_PARALLEL_MODE` environment variable is set to `on` or
    /// `dry_run`, the parallel refresh mode GUC is enabled for this
    /// database after extension creation.
    pub async fn with_extension(self) -> Self {
        sqlx::query("CREATE EXTENSION IF NOT EXISTS pg_trickle CASCADE")
            .execute(&self.pool)
            .await
            .expect("Failed to CREATE EXTENSION pg_trickle");

        if let Ok(mode) = std::env::var("PGT_PARALLEL_MODE") {
            let mode = mode.to_ascii_lowercase();
            if mode == "on" || mode == "dry_run" {
                let sql = format!(
                    "ALTER SYSTEM SET pg_trickle.parallel_refresh_mode = '{}'",
                    mode
                );
                self.execute(&sql).await;
                self.execute("SELECT pg_reload_conf()").await;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

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

    /// Nudge the launcher so it re-probes this database promptly.
    ///
    /// `pg_reload_conf()` wakes the launcher from `wait_latch()`, but it does
    /// not change `last_attempt`. `pgtrickle._signal_launcher_rescan()` bumps
    /// the shared DAG version, which lets the launcher evict stale
    /// `last_attempt` entries on its next loop iteration.
    pub async fn nudge_launcher_rescan(&self) {
        let _ = self
            .try_execute("SELECT pgtrickle._signal_launcher_rescan()")
            .await;
        self.execute("SELECT pg_reload_conf()").await;
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

    /// Execute a query and return all result rows as a single text string.
    ///
    /// Useful for capturing EXPLAIN output where each row is a text line.
    pub async fn query_text(&self, sql: &str) -> Option<String> {
        let rows: Vec<(String,)> = sqlx::query_as(sql).fetch_all(&self.pool).await.ok()?;
        if rows.is_empty() {
            return None;
        }
        Some(
            rows.into_iter()
                .map(|(line,)| line)
                .collect::<Vec<_>>()
                .join("\n"),
        )
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

    /// Refresh a stream table, retrying when a concurrent background refresh holds the lock.
    ///
    /// The background scheduler may race with a manual refresh of a downstream ST
    /// (e.g. after the test manually refreshes an upstream ST, the scheduler detects
    /// staleness within its polling interval and acquires the session-level advisory
    /// lock on the same `pgt_id`). When `refresh_stream_table` returns
    /// "another refresh is already in progress", this helper sleeps 100 ms and retries
    /// until the lock clears or 10 seconds elapse.
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

    /// Verify a ST's contents match its defining query exactly (multiset equality).
    ///
    /// Ignores the internal `__pgt_row_id` column by comparing only the
    /// user-visible columns produced by the defining query.
    ///
    /// Uses `EXCEPT ALL` (not `EXCEPT`) so that duplicate rows are
    /// correctly accounted for — a bag/multiset comparison.
    ///
    /// Columns of type `json` are cast to `text` because the `json` type
    /// does not have an equality operator (needed by `EXCEPT ALL`).
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
        // Use EXCEPT ALL (not EXCEPT) for multiset/bag comparison so
        // that duplicate rows are properly detected.
        let sql = if raw_cols != cast_cols {
            // json columns present: cast them on both sides of EXCEPT ALL
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

    /// Wait for any pg_trickle scheduler background worker to appear in
    /// `pg_stat_activity` for the current database.
    ///
    /// The launcher spawns schedulers dynamically; on a freshly-installed
    /// database the scheduler may not start for up to the launcher's 10-second
    /// polling interval. Call this after `with_extension()` + GUC setup to
    /// ensure the scheduler is running before relying on auto-refresh behaviour.
    ///
    /// Returns `true` if the scheduler was detected within `timeout`, or
    /// `false` if it never appeared. In the latter case the caller can assert
    /// or produce a meaningful failure message rather than a generic timeout.
    pub async fn wait_for_scheduler(&self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        let nudge_interval = std::time::Duration::from_secs(10);
        // Trigger the first nudge immediately rather than waiting 10 s.
        let mut last_nudge = start - nudge_interval;
        loop {
            if start.elapsed() > timeout {
                return false;
            }

            let running: bool = self
                .query_scalar(
                    "SELECT EXISTS(\
                         SELECT 1 FROM pg_stat_activity \
                         WHERE backend_type = 'pg_trickle scheduler' \
                           AND datname = current_database()\
                     )",
                )
                .await;
            if running {
                return true;
            }

            if last_nudge.elapsed() >= nudge_interval {
                self.nudge_launcher_rescan().await;
                last_nudge = std::time::Instant::now();
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
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
