//! v0.96.0 SQL contract and predefined-role checks.

mod e2e;

use e2e::E2eDb;

#[tokio::test]
async fn test_v096_diagnostics_and_roles_have_stable_contracts() {
    let db = E2eDb::new().await.with_extension().await;

    let error_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.error_catalog()")
        .await;
    assert_eq!(error_count, 6);

    let profile_count: i64 = db
        .query_scalar("SELECT count(*) FROM pgtrickle.active_profile()")
        .await;
    assert!(profile_count >= 5);

    let view_exists: bool = db
        .query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_catalog.pg_views
                  WHERE schemaname = 'pgtrickle'
                    AND viewname = 'pg_stat_progress_pgtrickle'
             )",
        )
        .await;
    assert!(view_exists);

    for role in ["pgtrickle_reader", "pgtrickle_operator", "pgtrickle_admin"] {
        let exists: bool = db
            .query_scalar(&format!(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = '{role}')"
            ))
            .await;
        assert!(exists, "missing predefined role {role}");
    }

    let operator_can_refresh: bool = db
        .query_scalar(
            "SELECT has_function_privilege(
                 'pgtrickle_operator',
                 'pgtrickle.refresh_stream_table(text)',
                 'EXECUTE'
             )",
        )
        .await;
    assert!(operator_can_refresh);

    let reader_can_refresh: bool = db
        .query_scalar(
            "SELECT has_function_privilege(
                 'pgtrickle_reader',
                 'pgtrickle.refresh_stream_table(text)',
                 'EXECUTE'
             )",
        )
        .await;
    assert!(!reader_can_refresh);
}
