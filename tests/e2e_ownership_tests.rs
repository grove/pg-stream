//! TEST-6: Ownership-check privilege E2E tests (SEC-1).
//!
//! Validates that non-owner, non-superuser roles cannot drop or alter
//! stream tables they don't own, while superusers can operate on any ST.
//!
//! Uses two roles:
//! - `sec1_owner`: creates the stream table (becomes owner)
//! - `sec1_other`: a regular role that should be denied access
//! - postgres (superuser): should bypass ownership checks
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

/// Helper: create the two test roles and a source table owned by sec1_owner.
async fn setup_ownership_test(db: &E2eDb) {
    // Create roles — use EXCEPTION to handle the race where parallel tests try
    // to create the same cluster-level role simultaneously.  IF NOT EXISTS inside
    // a DO block is not atomic; we catch both duplicate_object (42710, role
    // already exists) and unique_violation (23505, concurrent INSERT race).
    db.execute(
        "DO $$ BEGIN CREATE ROLE sec1_owner LOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$",
    )
    .await;
    db.execute(
        "DO $$ BEGIN CREATE ROLE sec1_other LOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$",
    )
    .await;

    // Grant usage on extension schemas to both roles
    db.execute("GRANT USAGE ON SCHEMA pgtrickle TO sec1_owner, sec1_other")
        .await;
    db.execute("GRANT USAGE ON SCHEMA pgtrickle_changes TO sec1_owner, sec1_other")
        .await;
    db.execute("GRANT SELECT ON ALL TABLES IN SCHEMA pgtrickle TO sec1_owner, sec1_other")
        .await;

    // Create source table and grant access
    db.execute("CREATE TABLE sec1_src (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO sec1_src VALUES (1, 'a'), (2, 'b')")
        .await;
    db.execute("GRANT ALL ON TABLE sec1_src TO sec1_owner, sec1_other")
        .await;

    // Create stream table as superuser, then transfer ownership to sec1_owner
    db.execute("SELECT pgtrickle.create_stream_table('sec1_st', 'SELECT id, val FROM sec1_src')")
        .await;
    db.execute("ALTER TABLE sec1_st OWNER TO sec1_owner").await;
}

/// TEST-6a: Non-owner cannot drop a stream table.
#[tokio::test]
async fn test_ownership_nonowner_drop_denied() {
    let db = E2eDb::new().await.with_extension().await;
    setup_ownership_test(&db).await;

    // Attempt to drop as sec1_other (non-owner, non-superuser).
    // Use try_execute_with_role to run SET ROLE / target / RESET ROLE on the
    // same connection (sqlx rejects multi-statement prepared statements).
    let result = db
        .try_execute_with_role(
            "SET ROLE sec1_other",
            "SELECT pgtrickle.drop_stream_table('sec1_st')",
            "RESET ROLE",
        )
        .await;

    assert!(
        result.is_err(),
        "Non-owner should not be able to drop a stream table"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("must be owner"),
        "Error should mention ownership: {err}"
    );
}

/// TEST-6b: Non-owner cannot alter a stream table.
#[tokio::test]
async fn test_ownership_nonowner_alter_denied() {
    let db = E2eDb::new().await.with_extension().await;
    setup_ownership_test(&db).await;

    // Attempt to alter as sec1_other (non-owner, non-superuser).
    // Use try_execute_with_role to avoid multi-statement prepared-statement error.
    let result = db
        .try_execute_with_role(
            "SET ROLE sec1_other",
            "SELECT pgtrickle.alter_stream_table('sec1_st', schedule => '30s')",
            "RESET ROLE",
        )
        .await;

    assert!(
        result.is_err(),
        "Non-owner should not be able to alter a stream table"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("must be owner"),
        "Error should mention ownership: {err}"
    );
}

/// TEST-6c: Superuser can drop any stream table regardless of ownership.
#[tokio::test]
async fn test_ownership_superuser_override() {
    let db = E2eDb::new().await.with_extension().await;
    setup_ownership_test(&db).await;

    // Verify the ST is owned by sec1_owner, not the superuser
    let owner: String = db
        .query_scalar(
            "SELECT pg_catalog.pg_get_userbyid(relowner) \
             FROM pg_catalog.pg_class \
             WHERE relname = 'sec1_st'",
        )
        .await;
    assert_eq!(owner, "sec1_owner", "ST should be owned by sec1_owner");

    // Superuser (default role, postgres) should be able to drop it
    let result = db
        .try_execute("SELECT pgtrickle.drop_stream_table('sec1_st')")
        .await;
    assert!(
        result.is_ok(),
        "Superuser should be able to drop any stream table: {:?}",
        result.err()
    );

    // Verify it's gone
    let exists: bool = db
        .query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pgtrickle.pgt_stream_tables \
             WHERE pgt_name = 'sec1_st')",
        )
        .await;
    assert!(!exists, "Stream table should be dropped");
}
