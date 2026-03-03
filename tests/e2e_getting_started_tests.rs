//! E2E tests that verify the examples from docs/GETTING_STARTED.md work correctly.
//!
//! Covers every substantive code example in the guide:
//!
//! - Step 2: `department_tree` (recursive CTE, '1m', DIFFERENTIAL) — creation
//!   and initial data
//! - Step 3: `department_stats` (LEFT JOIN + agg, NULL schedule, DIFFERENTIAL)
//!   and `department_report` (GROUP BY 1, '1m', DIFFERENTIAL) — chained creation
//! - Step 4a: Single INSERT into `employees` cascades through `department_stats`
//!   and `department_report` (depth = 2, one group updated)
//! - Step 4b: INSERT into `departments` (root table) cascades through all three
//!   layers (semi-naive evaluation in `department_tree`)
//! - Step 4c: UPDATE department name cascades via DRed through all three layers
//!   (all paths containing old name are updated)
//! - Step 4d: DELETE an employee adjusts aggregate correctly
//! - Step 7: Drop in reverse dependency order — storage tables, triggers, and
//!   catalog entries are all removed
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Base table setup helpers ─────────────────────────────────────────────────

/// Create `departments` and `employees` tables exactly as shown in Step 1.
async fn setup_base_tables(db: &E2eDb) {
    db.execute(
        "CREATE TABLE departments (
            id        SERIAL PRIMARY KEY,
            name      TEXT NOT NULL,
            parent_id INT REFERENCES departments(id)
        )",
    )
    .await;

    db.execute(
        "CREATE TABLE employees (
            id            SERIAL PRIMARY KEY,
            name          TEXT NOT NULL,
            department_id INT NOT NULL REFERENCES departments(id),
            salary        NUMERIC(10,2) NOT NULL
        )",
    )
    .await;

    // Top-level
    db.execute("INSERT INTO departments (id, name, parent_id) VALUES (1, 'Company', NULL)")
        .await;

    // Second level
    db.execute(
        "INSERT INTO departments (id, name, parent_id) VALUES
            (2, 'Engineering', 1),
            (3, 'Sales',       1),
            (4, 'Operations',  1)",
    )
    .await;

    // Third level (under Engineering)
    db.execute(
        "INSERT INTO departments (id, name, parent_id) VALUES
            (5, 'Backend',  2),
            (6, 'Frontend', 2),
            (7, 'Platform', 2)",
    )
    .await;

    // Employees
    db.execute(
        "INSERT INTO employees (name, department_id, salary) VALUES
            ('Alice',   5, 120000),
            ('Bob',     5, 115000),
            ('Charlie', 6, 110000),
            ('Diana',   7, 130000),
            ('Eve',     3, 95000),
            ('Frank',   3, 90000),
            ('Grace',   4, 100000)",
    )
    .await;
}

/// Create `department_tree` — recursive CTE, '1m', DIFFERENTIAL (Step 2).
async fn create_department_tree(db: &E2eDb) {
    db.create_st(
        "department_tree",
        "WITH RECURSIVE tree AS (
            SELECT id, name, parent_id, name AS path, 0 AS depth
            FROM departments
            WHERE parent_id IS NULL
            UNION ALL
            SELECT d.id, d.name, d.parent_id,
                   tree.path || ' > ' || d.name AS path,
                   tree.depth + 1
            FROM departments d
            JOIN tree ON d.parent_id = tree.id
        )
        SELECT id, name, parent_id, path, depth FROM tree",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
}

/// Create `department_stats` — LEFT JOIN + agg, NULL schedule, DIFFERENTIAL (Step 3).
async fn create_department_stats(db: &E2eDb) {
    // NULL schedule = CALCULATED mode (inherit from downstream).
    // The helper only accepts &str, so use execute() directly.
    db.execute(
        "SELECT pgtrickle.create_stream_table(
            'department_stats',
            $$
            SELECT
                t.id          AS department_id,
                t.name        AS department_name,
                t.path        AS full_path,
                t.depth,
                COUNT(e.id)                AS headcount,
                COALESCE(SUM(e.salary), 0) AS total_salary,
                COALESCE(AVG(e.salary), 0) AS avg_salary
            FROM department_tree t
            LEFT JOIN employees e ON e.department_id = t.id
            GROUP BY t.id, t.name, t.path, t.depth
            $$,
            NULL,
            'DIFFERENTIAL'
        )",
    )
    .await;
}

/// Create `department_report` — ordinal GROUP BY 1, '1m', DIFFERENTIAL (Step 3).
async fn create_department_report(db: &E2eDb) {
    db.create_st(
        "department_report",
        "SELECT
            split_part(full_path, ' > ', 2) AS division,
            SUM(headcount)                  AS total_headcount,
            SUM(total_salary)               AS total_payroll
        FROM department_stats
        WHERE depth >= 1
        GROUP BY 1",
        "1m",
        "DIFFERENTIAL",
    )
    .await;
}

// ── Step 2 ───────────────────────────────────────────────────────────────────

/// Step 2: `department_tree` is populated on creation with all 7 departments,
/// correct paths, and correct depths.
#[tokio::test]
async fn test_getting_started_step2_department_tree_initial_data() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;

    // 7 rows: one per department
    assert_eq!(
        db.count("public.department_tree").await,
        7,
        "Should have one row per department"
    );

    // Root department at depth 0
    let company_depth: i32 = db
        .query_scalar("SELECT depth FROM department_tree WHERE name = 'Company'")
        .await;
    assert_eq!(company_depth, 0);

    // Third-level path
    let backend_path: String = db
        .query_scalar("SELECT path FROM department_tree WHERE name = 'Backend'")
        .await;
    assert_eq!(backend_path, "Company > Engineering > Backend");

    // Third-level depth
    let backend_depth: i32 = db
        .query_scalar("SELECT depth FROM department_tree WHERE name = 'Backend'")
        .await;
    assert_eq!(backend_depth, 2);

    // Status checks
    let (status, mode, populated, errors) = db.pgt_status("department_tree").await;
    assert_eq!(status, "ACTIVE");
    assert_eq!(mode, "DIFFERENTIAL");
    assert!(populated);
    assert_eq!(errors, 0);
}

// ── Step 3 ───────────────────────────────────────────────────────────────────

/// Step 3: `department_stats` is populated with correct headcounts and salaries.
#[tokio::test]
async fn test_getting_started_step3_department_stats_initial_data() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;

    assert_eq!(
        db.count("public.department_stats").await,
        7,
        "One row per department"
    );

    // Backend: Alice (120000) + Bob (115000) = 2 employees, 235000 total
    let (headcount, total_salary): (i64, String) = sqlx::query_as(
        "SELECT headcount::bigint, total_salary::text
         FROM department_stats WHERE department_name = 'Backend'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("Backend stats query failed");
    assert_eq!(headcount, 2);
    assert_eq!(total_salary, "235000.00");

    // Company has no direct employees (no one reports to id=1)
    let company_headcount: i64 = db
        .query_scalar(
            "SELECT headcount::bigint FROM department_stats WHERE department_name = 'Company'",
        )
        .await;
    assert_eq!(company_headcount, 0);

    // Sales: Eve (95000) + Frank (90000) = 2 employees, 185000 total
    let (sales_hc, sales_sal): (i64, String) = sqlx::query_as(
        "SELECT headcount::bigint, total_salary::text
         FROM department_stats WHERE department_name = 'Sales'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("Sales stats query failed");
    assert_eq!(sales_hc, 2);
    assert_eq!(sales_sal, "185000.00");

    let (status, _, populated, errors) = db.pgt_status("department_stats").await;
    assert_eq!(status, "ACTIVE");
    assert!(populated);
    assert_eq!(errors, 0);
}

/// Step 3: `department_report` — ordinal GROUP BY 1 — is created and correctly populated.
#[tokio::test]
async fn test_getting_started_step3_department_report_initial_data() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;
    create_department_report(&db).await;

    // Depth >= 1 means Company (depth=0) is excluded; 3 divisions remain
    assert_eq!(
        db.count("public.department_report").await,
        3,
        "Engineering, Sales, Operations"
    );

    let divs: Vec<String> =
        sqlx::query_scalar("SELECT division FROM department_report ORDER BY division")
            .fetch_all(&db.pool)
            .await
            .expect("department_report query failed");
    assert_eq!(divs, vec!["Engineering", "Operations", "Sales"]);

    // Engineering: Alice(5) + Bob(5) + Charlie(6) + Diana(7) = 4 employees
    let eng_hc: i64 = db
        .query_scalar(
            "SELECT total_headcount::bigint FROM department_report WHERE division = 'Engineering'",
        )
        .await;
    assert_eq!(eng_hc, 4);

    // Engineering total payroll: 120000+115000+110000+130000 = 475000
    let eng_payroll: String = db
        .query_scalar(
            "SELECT total_payroll::text FROM department_report WHERE division = 'Engineering'",
        )
        .await;
    assert_eq!(eng_payroll, "475000.00");

    let (status, _, populated, errors) = db.pgt_status("department_report").await;
    assert_eq!(status, "ACTIVE");
    assert!(populated);
    assert_eq!(errors, 0);
}

// ── Step 4a ──────────────────────────────────────────────────────────────────

/// Step 4a: INSERT Heidi into Frontend. Only Frontend group in `department_stats`
/// is updated; only Engineering row in `department_report` is updated.
#[tokio::test]
async fn test_getting_started_step4a_employee_insert_cascades() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;
    create_department_report(&db).await;

    // Pre-conditions
    let pre_hc: i64 = db
        .query_scalar(
            "SELECT headcount::bigint FROM department_stats WHERE department_name = 'Frontend'",
        )
        .await;
    assert_eq!(pre_hc, 1, "Frontend starts with Charlie only");

    // Insert Heidi
    db.execute("INSERT INTO employees (name, department_id, salary) VALUES ('Heidi', 6, 105000)")
        .await;

    // Refresh in source-to-sink order (department_tree not involved)
    db.refresh_st("department_stats").await;
    db.refresh_st("department_report").await;

    // Frontend: 1 → 2 employees; 110000 → 215000
    let (post_hc, post_sal): (i64, String) = sqlx::query_as(
        "SELECT headcount::bigint, total_salary::text
         FROM department_stats WHERE department_name = 'Frontend'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("Frontend stats failed");
    assert_eq!(post_hc, 2);
    assert_eq!(post_sal, "215000.00");

    // Engineering division: 4 → 5; 475000 → 580000
    let (eng_hc, eng_payroll): (i64, String) = sqlx::query_as(
        "SELECT total_headcount::bigint, total_payroll::text
         FROM department_report WHERE division = 'Engineering'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("Engineering report failed");
    assert_eq!(eng_hc, 5);
    assert_eq!(eng_payroll, "580000.00");

    // Other divisions untouched
    let sales_hc: i64 = db
        .query_scalar(
            "SELECT total_headcount::bigint FROM department_report WHERE division = 'Sales'",
        )
        .await;
    assert_eq!(sales_hc, 2, "Sales not affected by Heidi's insertion");
}

// ── Step 4b ──────────────────────────────────────────────────────────────────

/// Step 4b: INSERT DevOps under Engineering cascades through all three layers.
/// `department_tree` uses semi-naive evaluation; DevOps appears with correct
/// path and depth in all three stream tables.
#[tokio::test]
async fn test_getting_started_step4b_department_insert_cascades_all_layers() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;
    create_department_report(&db).await;

    db.execute("INSERT INTO departments (id, name, parent_id) VALUES (8, 'DevOps', 2)")
        .await;

    // Refresh all three layers in dependency order
    db.refresh_st("department_tree").await;
    db.refresh_st("department_stats").await;
    db.refresh_st("department_report").await;

    // department_tree: DevOps at depth 2 with correct path
    let (devops_path, devops_depth): (String, i32) =
        sqlx::query_as("SELECT path, depth FROM department_tree WHERE name = 'DevOps'")
            .fetch_one(&db.pool)
            .await
            .expect("DevOps tree row missing");
    assert_eq!(devops_path, "Company > Engineering > DevOps");
    assert_eq!(devops_depth, 2);
    assert_eq!(
        db.count("public.department_tree").await,
        8,
        "7 + DevOps = 8 rows"
    );

    // department_stats: DevOps present with 0 employees
    let devops_hc: i64 = db
        .query_scalar(
            "SELECT headcount::bigint FROM department_stats WHERE department_name = 'DevOps'",
        )
        .await;
    assert_eq!(devops_hc, 0);
    assert_eq!(db.count("public.department_stats").await, 8);

    // department_report: Engineering headcount unchanged (DevOps has 0 staff)
    let eng_hc: i64 = db
        .query_scalar(
            "SELECT total_headcount::bigint FROM department_report WHERE division = 'Engineering'",
        )
        .await;
    assert_eq!(
        eng_hc, 4,
        "DevOps has no employees; Engineering headcount unchanged"
    );
}

// ── Step 4c ──────────────────────────────────────────────────────────────────

/// Step 4c: UPDATE department name "Engineering" → "R&D" cascades via DRed
/// through all three stream tables. All paths and the division name are updated.
#[tokio::test]
async fn test_getting_started_step4c_department_rename_cascades() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;
    create_department_report(&db).await;

    db.execute("UPDATE departments SET name = 'R&D' WHERE id = 2")
        .await;

    db.refresh_st("department_tree").await;
    db.refresh_st("department_stats").await;
    db.refresh_st("department_report").await;

    // department_tree: no path contains "Engineering"
    let old_path_count: i64 = db
        .query_scalar("SELECT count(*) FROM department_tree WHERE path LIKE '%Engineering%'")
        .await;
    assert_eq!(old_path_count, 0, "No paths should reference 'Engineering'");

    // R&D itself + 3 sub-teams = 4 rows with 'R&D' in path
    let rnd_count: i64 = db
        .query_scalar("SELECT count(*) FROM department_tree WHERE path LIKE '%R&D%'")
        .await;
    assert_eq!(rnd_count, 4);

    let backend_path: String = db
        .query_scalar("SELECT path FROM department_tree WHERE name = 'Backend'")
        .await;
    assert_eq!(backend_path, "Company > R&D > Backend");

    // department_stats: full_path updated
    let rnd_full_path: String = db
        .query_scalar("SELECT full_path FROM department_stats WHERE department_name = 'R&D'")
        .await;
    assert_eq!(rnd_full_path, "Company > R&D");

    // department_report: "Engineering" gone, "R&D" present with same headcount
    let old_div_count: i64 = db
        .query_scalar("SELECT count(*) FROM department_report WHERE division = 'Engineering'")
        .await;
    assert_eq!(old_div_count, 0, "'Engineering' division should be removed");

    let rnd_hc: i64 = db
        .query_scalar(
            "SELECT total_headcount::bigint FROM department_report WHERE division = 'R&D'",
        )
        .await;
    assert_eq!(rnd_hc, 4, "'R&D' should have the same 4 employees");
}

// ── Step 4d ──────────────────────────────────────────────────────────────────

/// Step 4d: DELETE Bob from Backend. Headcount drops 2→1, salary 235000→120000.
/// Only Backend group is affected; other groups and Sales/Operations division untouched.
#[tokio::test]
async fn test_getting_started_step4d_employee_delete_updates_aggregates() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;
    create_department_report(&db).await;

    db.execute("DELETE FROM employees WHERE name = 'Bob'").await;

    db.refresh_st("department_stats").await;
    db.refresh_st("department_report").await;

    // Backend: 2 → 1 employee; 235000 → 120000
    let (hc, total_sal, avg_sal): (i64, String, String) = sqlx::query_as(
        "SELECT headcount::bigint,
                round(total_salary, 2)::text,
                round(avg_salary, 2)::text
         FROM department_stats WHERE department_name = 'Backend'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("Backend stats query failed");
    assert_eq!(hc, 1);
    assert_eq!(total_sal, "120000.00");
    assert_eq!(avg_sal, "120000.00");

    // Engineering division: 4 → 3
    let eng_hc: i64 = db
        .query_scalar(
            "SELECT total_headcount::bigint FROM department_report WHERE division = 'Engineering'",
        )
        .await;
    assert_eq!(eng_hc, 3);

    // Sales untouched
    let sales_hc: i64 = db
        .query_scalar(
            "SELECT total_headcount::bigint FROM department_report WHERE division = 'Sales'",
        )
        .await;
    assert_eq!(sales_hc, 2, "Sales unaffected by Bob's deletion");
}

// ── Step 7 ───────────────────────────────────────────────────────────────────

/// Step 7: Drop stream tables in reverse dependency order. Storage tables,
/// CDC triggers, and catalog entries are all cleaned up.
#[tokio::test]
async fn test_getting_started_step7_drop_in_order() {
    let db = E2eDb::new().await.with_extension().await;
    setup_base_tables(&db).await;
    create_department_tree(&db).await;
    create_department_stats(&db).await;
    create_department_report(&db).await;

    // Capture OIDs for trigger name checks before dropping
    let dept_oid = db.table_oid("departments").await;
    let empl_oid = db.table_oid("employees").await;

    // Drop in reverse dependency order (dependents first)
    db.drop_st("department_report").await;
    db.drop_st("department_stats").await;
    db.drop_st("department_tree").await;

    // Catalog entries removed
    let cat_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgtrickle.pgt_stream_tables
             WHERE pgt_name IN ('department_tree', 'department_stats', 'department_report')",
        )
        .await;
    assert_eq!(cat_count, 0, "All stream tables removed from catalog");

    // Storage tables dropped
    assert!(
        !db.table_exists("public", "department_tree").await,
        "department_tree storage table should be gone"
    );
    assert!(
        !db.table_exists("public", "department_stats").await,
        "department_stats storage table should be gone"
    );
    assert!(
        !db.table_exists("public", "department_report").await,
        "department_report storage table should be gone"
    );

    // CDC triggers removed from base tables
    let dept_trigger = format!("pg_trickle_cdc_{}", dept_oid);
    let empl_trigger = format!("pg_trickle_cdc_{}", empl_oid);
    assert!(
        !db.trigger_exists(&dept_trigger, "departments").await,
        "CDC trigger on departments should be removed"
    );
    assert!(
        !db.trigger_exists(&empl_trigger, "employees").await,
        "CDC trigger on employees should be removed"
    );

    // Base tables themselves still exist
    assert!(db.table_exists("public", "departments").await);
    assert!(db.table_exists("public", "employees").await);
}
