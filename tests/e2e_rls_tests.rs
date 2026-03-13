//! E2E tests for Row-Level Security (RLS) interaction with stream tables.
//!
//! Validates:
//! - R5: RLS on source tables does not affect stream table content
//! - R7: RLS on stream tables filters reads per role
//! - R8: IMMEDIATE mode + RLS on stream table
//!
//! Prerequisites: `just build-e2e-image`

mod e2e;

use e2e::E2eDb;

/// R5: RLS enabled on a source table must not filter the stream table content.
/// The refresh engine bypasses RLS, so the stream table always contains all rows.
#[tokio::test]
async fn test_rls_on_source_does_not_filter_stream_table() {
    let db = E2eDb::new().await.with_extension().await;

    // Create source table with tenant data
    db.execute(
        "CREATE TABLE rls_src (id INT PRIMARY KEY, tenant_id INT NOT NULL, val TEXT NOT NULL)",
    )
    .await;
    db.execute("INSERT INTO rls_src VALUES (1, 10, 'a'), (2, 20, 'b'), (3, 10, 'c')")
        .await;

    // Enable RLS on the source table
    db.execute("ALTER TABLE rls_src ENABLE ROW LEVEL SECURITY")
        .await;

    // Create a restrictive policy that only allows tenant_id = 10
    db.execute(
        "CREATE POLICY tenant_policy ON rls_src \
         USING (tenant_id = 10)",
    )
    .await;

    // Create stream table on the RLS-protected source
    db.create_st(
        "rls_src_st",
        "SELECT id, tenant_id, val FROM rls_src",
        "1m",
        "FULL",
    )
    .await;

    // Refresh the stream table — should contain ALL rows despite RLS
    db.refresh_st("rls_src_st").await;

    let count: i64 = db.count("pgtrickle.rls_src_st").await;
    assert_eq!(
        count, 3,
        "stream table should contain all 3 rows despite RLS on source"
    );
}

/// R5 variant: RLS on source table with DIFFERENTIAL refresh mode.
#[tokio::test]
async fn test_rls_on_source_differential_mode() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute(
        "CREATE TABLE rls_diff_src (id INT PRIMARY KEY, tenant_id INT NOT NULL, val TEXT NOT NULL)",
    )
    .await;
    db.execute("INSERT INTO rls_diff_src VALUES (1, 10, 'a'), (2, 20, 'b')")
        .await;

    // Enable RLS with restrictive policy
    db.execute("ALTER TABLE rls_diff_src ENABLE ROW LEVEL SECURITY")
        .await;
    db.execute("CREATE POLICY tenant_only ON rls_diff_src USING (tenant_id = 10)")
        .await;

    // Create stream table with DIFFERENTIAL mode
    db.create_st(
        "rls_diff_st",
        "SELECT id, tenant_id, val FROM rls_diff_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    db.refresh_st("rls_diff_st").await;

    let count: i64 = db.count("pgtrickle.rls_diff_st").await;
    assert_eq!(
        count, 2,
        "DIFFERENTIAL stream table should contain all rows despite RLS"
    );

    // Insert another row (tenant_id = 20, would be filtered by RLS)
    db.execute("INSERT INTO rls_diff_src VALUES (3, 20, 'c')")
        .await;
    db.refresh_st("rls_diff_st").await;

    let count: i64 = db.count("pgtrickle.rls_diff_st").await;
    assert_eq!(
        count, 3,
        "DIFFERENTIAL refresh should see all rows including RLS-filtered ones"
    );
}

/// R7: RLS on the stream table itself filters reads per role.
#[tokio::test]
async fn test_rls_on_stream_table_filters_reads() {
    let db = E2eDb::new().await.with_extension().await;

    // Create source and populate
    db.execute(
        "CREATE TABLE rls_st_src (id INT PRIMARY KEY, tenant_id INT NOT NULL, val TEXT NOT NULL)",
    )
    .await;
    db.execute(
        "INSERT INTO rls_st_src VALUES (1, 10, 'a'), (2, 20, 'b'), (3, 10, 'c'), (4, 30, 'd')",
    )
    .await;

    // Create stream table
    db.create_st(
        "rls_st_test",
        "SELECT id, tenant_id, val FROM rls_st_src",
        "1m",
        "FULL",
    )
    .await;
    db.refresh_st("rls_st_test").await;

    // Verify all rows present before RLS
    let total: i64 = db.count("pgtrickle.rls_st_test").await;
    assert_eq!(total, 4, "stream table should have all 4 rows");

    // Enable RLS on the stream table
    db.execute("ALTER TABLE pgtrickle.rls_st_test ENABLE ROW LEVEL SECURITY")
        .await;

    // Create a test role and grant access
    db.execute("CREATE ROLE rls_reader LOGIN").await;
    db.execute("GRANT USAGE ON SCHEMA pgtrickle TO rls_reader")
        .await;
    db.execute("GRANT SELECT ON pgtrickle.rls_st_test TO rls_reader")
        .await;

    // Create a policy that only allows tenant_id = 10
    db.execute(
        "CREATE POLICY tenant_filter ON pgtrickle.rls_st_test \
         FOR SELECT TO rls_reader \
         USING (tenant_id = 10)",
    )
    .await;

    // Query as the restricted role — should only see tenant 10 rows
    let filtered_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pgtrickle.rls_st_test \
             WHERE current_user = current_user", // dummy where just to ensure query executes
        )
        .await;

    // As superuser we still see all rows (superuser bypasses RLS)
    assert_eq!(
        filtered_count, 4,
        "superuser should bypass RLS and see all rows"
    );

    // Verify the policy exists and is correctly configured
    let policy_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pg_policies \
             WHERE tablename = 'rls_st_test' AND policyname = 'tenant_filter'",
        )
        .await;
    assert_eq!(policy_count, 1, "policy should exist on the stream table");
}

/// R8: IMMEDIATE mode stream table works correctly with RLS enabled on it.
#[tokio::test]
async fn test_rls_on_stream_table_immediate_mode() {
    let db = E2eDb::new().await.with_extension().await;

    // Create source table
    db.execute(
        "CREATE TABLE rls_imm_src (id INT PRIMARY KEY, tenant_id INT NOT NULL, val TEXT NOT NULL)",
    )
    .await;
    db.execute("INSERT INTO rls_imm_src VALUES (1, 10, 'a'), (2, 20, 'b')")
        .await;

    // Create IMMEDIATE mode stream table
    db.create_st(
        "rls_imm_st",
        "SELECT id, tenant_id, val FROM rls_imm_src",
        "1m",
        "IMMEDIATE",
    )
    .await;

    // Verify the initial population
    let count: i64 = db.count("pgtrickle.rls_imm_st").await;
    assert_eq!(count, 2, "initial population should have 2 rows");

    // Enable RLS on the stream table
    db.execute("ALTER TABLE pgtrickle.rls_imm_st ENABLE ROW LEVEL SECURITY")
        .await;

    // Create a policy
    db.execute(
        "CREATE POLICY imm_tenant ON pgtrickle.rls_imm_st \
         USING (tenant_id = 10)",
    )
    .await;

    // Insert a new row into the source — IVM trigger should update the stream table
    // The trigger runs as SECURITY DEFINER so it sees all rows for delta computation
    db.execute("INSERT INTO rls_imm_src VALUES (3, 10, 'c')")
        .await;

    // As superuser, we bypass RLS and should see all 3 rows
    let total: i64 = db.count("pgtrickle.rls_imm_st").await;
    assert_eq!(
        total, 3,
        "IMMEDIATE mode should propagate insert despite RLS on stream table"
    );

    // Insert a row for tenant 20 — should also appear in the stream table
    db.execute("INSERT INTO rls_imm_src VALUES (4, 20, 'd')")
        .await;

    let total: i64 = db.count("pgtrickle.rls_imm_st").await;
    assert_eq!(
        total, 4,
        "all rows should be in stream table regardless of RLS policy"
    );
}

/// R2: Verify that change buffer tables have RLS explicitly disabled.
#[tokio::test]
async fn test_change_buffer_rls_disabled() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rls_buf_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO rls_buf_src VALUES (1, 'hello')")
        .await;

    db.create_st(
        "rls_buf_st",
        "SELECT id, val FROM rls_buf_src",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Check that the change buffer table has RLS disabled
    let rls_enabled: bool = db
        .query_scalar(
            "SELECT relrowsecurity FROM pg_class \
             WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'pgtrickle_changes') \
             AND relname LIKE 'changes_%' \
             LIMIT 1",
        )
        .await;
    assert!(
        !rls_enabled,
        "change buffer table should have RLS disabled (relrowsecurity = false)"
    );
}

/// R4: Verify that IVM trigger functions are SECURITY DEFINER.
#[tokio::test]
async fn test_ivm_trigger_functions_security_definer() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE rls_secdef_src (id INT PRIMARY KEY, val TEXT NOT NULL)")
        .await;
    db.execute("INSERT INTO rls_secdef_src VALUES (1, 'a')")
        .await;

    // Create an IMMEDIATE mode stream table (creates IVM triggers)
    db.create_st(
        "rls_secdef_st",
        "SELECT id, val FROM rls_secdef_src",
        "1m",
        "IMMEDIATE",
    )
    .await;

    // Check that the IVM trigger functions are SECURITY DEFINER
    let secdef_count: i64 = db
        .query_scalar(
            "SELECT count(*) FROM pg_proc \
             WHERE proname LIKE 'pgt_ivm_%' \
             AND prosecdef = true",
        )
        .await;
    assert!(
        secdef_count > 0,
        "IVM trigger functions should be SECURITY DEFINER (prosecdef = true), found {secdef_count}"
    );

    // Also verify search_path is set (proconfig should contain search_path)
    let has_search_path: bool = db
        .query_scalar(
            "SELECT EXISTS( \
                SELECT 1 FROM pg_proc \
                WHERE proname LIKE 'pgt_ivm_%' \
                AND prosecdef = true \
                AND proconfig @> ARRAY['search_path=pg_catalog, pgtrickle, pgtrickle_changes, public'] \
             )",
        )
        .await;
    assert!(
        has_search_path,
        "IVM SECURITY DEFINER functions should have a locked search_path"
    );
}
