//! v0.100 Graph V1 opt-in admission.

mod common;
mod e2e;

use e2e::E2eDb;

#[tokio::test]
async fn test_v100_graph_v1_opt_in_is_explicit() {
    let db = E2eDb::new().await.with_extension().await;

    let default_enabled: bool = db
        .query_scalar(
            "SELECT enabled FROM pgtrickle.integration_capabilities() \
             WHERE capability = 'external_graph_refresh'",
        )
        .await;
    assert!(!default_enabled, "Graph V1 must remain disabled by default");

    let opt_in_enabled: bool = db
        .query_scalar(
            "WITH configured AS MATERIALIZED ( \
                 SELECT set_config('pg_trickle.experimental_graph_v1', 'on', false) \
             ) \
             SELECT enabled FROM configured, pgtrickle.integration_capabilities() \
             WHERE capability = 'external_graph_refresh'",
        )
        .await;
    let phase: String = db
        .query_scalar(
            "WITH configured AS MATERIALIZED ( \
                 SELECT set_config('pg_trickle.experimental_graph_v1', 'on', false) \
             ) \
             SELECT details->>'phase' FROM configured, pgtrickle.integration_capabilities() \
             WHERE capability = 'external_graph_refresh'",
        )
        .await;
    assert!(
        opt_in_enabled,
        "Graph V1 should report enabled after opt-in"
    );
    assert_eq!(phase, "v0.100_scoped_opt_in");
}
