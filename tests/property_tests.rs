//! Property-based tests using proptest.
//!
//! Tests the key invariants of the system:
//! - LSN comparison forms a total order
//! - Frontier JSON serialization roundtrips
//! - DAG cycle detection correctness
//! - Topological sort ordering respects edges
//! - DtStatus/RefreshMode enum roundtrips
//! - Canonical period selection bounds
//! - Hash determinism and collision resistance

// These tests exercise pure functions from the library.
// We use `pg_stream` as a lib crate (cdylib + lib).

use pg_stream::dag::{DagNode, DtDag, DtStatus, NodeId, RefreshMode};
use pg_stream::dvm::diff::{col_list, prefixed_col_list, quote_ident};
use pg_stream::dvm::parser::{AggFunc, Expr};
use pg_stream::version::{Frontier, lsn_gt, lsn_gte, select_canonical_period_secs};
use proptest::prelude::*;
use std::time::Duration;

// ── LSN comparison properties ──────────────────────────────────────────────

/// Strategy: generate a valid LSN string `"HI/LO"` where HI ∈ [0, 0xFF] and LO ∈ [0, 0xFFFFFFFF].
fn arb_lsn() -> impl Strategy<Value = String> {
    (0u32..=0xFF, 0u32..=0xFFFF_FFFF).prop_map(|(hi, lo)| format!("{:X}/{:X}", hi, lo))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    // ── LSN total order ────────────────────────────────────────────

    #[test]
    fn prop_lsn_reflexive(lsn in arb_lsn()) {
        // a >= a  (reflexive)
        prop_assert!(lsn_gte(&lsn, &lsn));
        // a > a is false (irreflexive for strict)
        prop_assert!(!lsn_gt(&lsn, &lsn));
    }

    #[test]
    fn prop_lsn_antisymmetric(a in arb_lsn(), b in arb_lsn()) {
        // If a > b then NOT b > a
        if lsn_gt(&a, &b) {
            prop_assert!(!lsn_gt(&b, &a));
        }
    }

    #[test]
    fn prop_lsn_gte_consistent(a in arb_lsn(), b in arb_lsn()) {
        // lsn_gte(a, b) iff (a == b || lsn_gt(a, b))
        let expected = a == b || lsn_gt(&a, &b);
        prop_assert_eq!(lsn_gte(&a, &b), expected);
    }

    #[test]
    fn prop_lsn_trichotomy(a in arb_lsn(), b in arb_lsn()) {
        // Exactly one of: a > b, a == b (as LSN), b > a
        let gt = lsn_gt(&a, &b);
        let lt = lsn_gt(&b, &a);
        let eq = !gt && !lt;
        // At most one is true for gt/lt (already tested).
        // eq is the "neither" case.
        if gt {
            prop_assert!(!lt);
            prop_assert!(!eq); // eq would require both false
        }
        if lt {
            prop_assert!(!gt);
        }
        // At least one must be true
        prop_assert!(gt || lt || eq);
    }

    // ── Frontier JSON roundtrip ────────────────────────────────────

    #[test]
    fn prop_frontier_json_roundtrip(
        oids in prop::collection::vec(1u32..10000, 0..5),
        data_ts in prop::option::of("[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z"),
    ) {
        let mut f = Frontier::new();
        for oid in &oids {
            f.set_source(*oid, format!("0/{:X}", oid), "2024-01-01T00:00:00Z".to_string());
        }
        if let Some(ts) = &data_ts {
            f.set_data_timestamp(ts.clone());
        }

        let json = f.to_json().unwrap();
        let f2 = Frontier::from_json(&json).unwrap();

        // All source OIDs should be present
        for oid in &oids {
            prop_assert_eq!(f.get_lsn(*oid), f2.get_lsn(*oid));
        }
    }

    #[test]
    fn prop_frontier_set_get_roundtrip(
        oid in 1u32..100000,
        hi in 0u32..0xFF,
        lo in 0u32..0xFFFFFFFF,
    ) {
        let lsn = format!("{hi:X}/{lo:X}");
        let ts = "2024-01-01".to_string();
        let mut f = Frontier::new();
        f.set_source(oid, lsn.clone(), ts.clone());
        prop_assert_eq!(f.get_lsn(oid), lsn);
        prop_assert_eq!(f.get_snapshot_ts(oid), Some(ts));
    }

    #[test]
    fn prop_frontier_is_empty(num_sources in 0usize..5) {
        let mut f = Frontier::new();
        prop_assert!(f.is_empty());
        for i in 0..num_sources {
            f.set_source(i as u32, "0/0".into(), "ts".into());
        }
        if num_sources > 0 {
            prop_assert!(!f.is_empty());
        }
    }

    // ── Canonical period selection ─────────────────────────────────

    #[test]
    fn prop_canonical_period_is_48_power_of_2(schedule in 96u64..1_000_000) {
        let period = select_canonical_period_secs(schedule);
        // period must be 48 * 2^n for some n >= 0
        let mut p = period;
        prop_assert!(p >= 48, "period must be >= 48, got {}", p);
        // Divide out the 48
        prop_assert_eq!(p % 48, 0);
        p /= 48;
        // p must be a power of 2
        prop_assert!(p.is_power_of_two(), "period/48 = {} is not power of 2", p);
    }

    #[test]
    fn prop_canonical_period_within_bounds(schedule in 96u64..1_000_000) {
        let period = select_canonical_period_secs(schedule);
        let half_schedule = schedule / 2;
        prop_assert!(period <= half_schedule, "period {period} > half_schedule {half_schedule}");
    }

    // ── quote_ident correctness ────────────────────────────────────

    #[test]
    fn prop_quote_ident_wraps_in_double_quotes(name in "[a-zA-Z_][a-zA-Z0-9_]{0,20}") {
        let quoted = quote_ident(&name);
        prop_assert!(quoted.starts_with('"'));
        prop_assert!(quoted.ends_with('"'));
        // Strip outer quotes and unescape
        let inner = &quoted[1..quoted.len()-1];
        let unescaped = inner.replace("\"\"", "\"");
        prop_assert_eq!(unescaped, name);
    }

    #[test]
    fn prop_quote_ident_roundtrip(name in "[ -~]{0,30}") {
        let quoted = quote_ident(&name);
        // The inner content with "" → " unescaping must yield the original
        let inner = &quoted[1..quoted.len()-1];
        let unescaped = inner.replace("\"\"", "\"");
        prop_assert_eq!(unescaped, name);
    }

    // ── col_list properties ────────────────────────────────────────

    #[test]
    fn prop_col_list_contains_all_columns(
        cols in prop::collection::vec("[a-z]{1,8}", 0..5),
    ) {
        let result = col_list(&cols);
        for c in &cols {
            prop_assert!(result.contains(c), "col_list missing column {c}");
        }
    }

    #[test]
    fn prop_prefixed_col_list_has_prefix(
        prefix in "[a-z]{1,5}",
        cols in prop::collection::vec("[a-z]{1,8}", 1..5),
    ) {
        let result = prefixed_col_list(&prefix, &cols);
        // Every segment should start with the prefix
        for segment in result.split(", ") {
            prop_assert!(segment.starts_with(&format!("{prefix}.")),
                "segment '{segment}' doesn't start with '{prefix}.'");
        }
    }

    // ── Expr::to_sql determinism ───────────────────────────────────

    #[test]
    fn prop_expr_to_sql_deterministic(
        table in prop::option::of("[a-z]{1,5}"),
        col_name in "[a-z]{1,10}",
    ) {
        let e = Expr::ColumnRef {
            table_alias: table.clone(),
            column_name: col_name.clone(),
        };
        // Calling to_sql twice yields the same result
        prop_assert_eq!(e.to_sql(), e.to_sql());
    }

    // ── AggFunc exhaustive ─────────────────────────────────────────

    #[test]
    fn prop_agg_func_sql_name_nonempty(idx in 0u8..6) {
        let f = match idx {
            0 => AggFunc::Count,
            1 => AggFunc::CountStar,
            2 => AggFunc::Sum,
            3 => AggFunc::Avg,
            4 => AggFunc::Min,
            _ => AggFunc::Max,
        };
        let name = f.sql_name();
        prop_assert!(!name.is_empty());
        // SQL aggregate names are all uppercase
        prop_assert_eq!(name, name.to_uppercase());
    }

    // ── DtStatus/RefreshMode roundtrip ─────────────────────────────

    #[test]
    fn prop_pgs_status_roundtrip(idx in 0u8..4) {
        let status = match idx {
            0 => DtStatus::Initializing,
            1 => DtStatus::Active,
            2 => DtStatus::Suspended,
            _ => DtStatus::Error,
        };
        let s = status.as_str();
        let parsed = DtStatus::from_str(s).unwrap();
        prop_assert_eq!(status, parsed);
    }

    #[test]
    fn prop_refresh_mode_roundtrip(idx in 0u8..2) {
        let mode = match idx {
            0 => RefreshMode::Full,
            _ => RefreshMode::Differential,
        };
        let s = mode.as_str();
        let parsed = RefreshMode::from_str(s).unwrap();
        prop_assert_eq!(mode, parsed);
    }
}

// ── DAG property tests (non-proptest, structured randomization) ────────────
// These use explicit construction because DAG topology generation is
// complex with proptest strategies.

#[test]
fn prop_dag_acyclic_topological_order_respects_edges() {
    // Build a DAG: 1→2, 1→3, 2→4, 3→4
    let mut dag = DtDag::new();
    for &id in &[1i64, 2, 3, 4] {
        dag.add_dt_node(DagNode {
            id: NodeId::StreamTable(id),
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: format!("dt_{id}"),
            status: DtStatus::Active,
            schedule_raw: None,
        });
    }
    dag.add_edge(NodeId::StreamTable(1), NodeId::StreamTable(2));
    dag.add_edge(NodeId::StreamTable(1), NodeId::StreamTable(3));
    dag.add_edge(NodeId::StreamTable(2), NodeId::StreamTable(4));
    dag.add_edge(NodeId::StreamTable(3), NodeId::StreamTable(4));

    assert!(dag.detect_cycles().is_ok());

    let order = dag.topological_order().unwrap();
    // Verify: for every edge (u, v), u appears before v
    let pos: std::collections::HashMap<_, _> =
        order.iter().enumerate().map(|(i, n)| (*n, i)).collect();

    assert!(pos[&NodeId::StreamTable(1)] < pos[&NodeId::StreamTable(2)]);
    assert!(pos[&NodeId::StreamTable(1)] < pos[&NodeId::StreamTable(3)]);
    assert!(pos[&NodeId::StreamTable(2)] < pos[&NodeId::StreamTable(4)]);
    assert!(pos[&NodeId::StreamTable(3)] < pos[&NodeId::StreamTable(4)]);
}

#[test]
fn prop_dag_cycle_detected() {
    // Build a cyclic graph: 1→2, 2→3, 3→1
    let mut dag = DtDag::new();
    for &id in &[1i64, 2, 3] {
        dag.add_dt_node(DagNode {
            id: NodeId::StreamTable(id),
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: format!("dt_{id}"),
            status: DtStatus::Active,
            schedule_raw: None,
        });
    }
    dag.add_edge(NodeId::StreamTable(1), NodeId::StreamTable(2));
    dag.add_edge(NodeId::StreamTable(2), NodeId::StreamTable(3));
    dag.add_edge(NodeId::StreamTable(3), NodeId::StreamTable(1));

    assert!(dag.detect_cycles().is_err());
}

#[test]
fn prop_dag_linear_chain_order() {
    // 1→2→3→4→5
    let mut dag = DtDag::new();
    for id in 1i64..=5 {
        dag.add_dt_node(DagNode {
            id: NodeId::StreamTable(id),
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: format!("dt_{id}"),
            status: DtStatus::Active,
            schedule_raw: None,
        });
    }
    for id in 1i64..5 {
        dag.add_edge(NodeId::StreamTable(id), NodeId::StreamTable(id + 1));
    }

    assert!(dag.detect_cycles().is_ok());
    let order = dag.topological_order().unwrap();
    // Should be [1, 2, 3, 4, 5]
    for i in 0..order.len() - 1 {
        match (order[i], order[i + 1]) {
            (NodeId::StreamTable(a), NodeId::StreamTable(b)) => {
                assert!(a < b, "expected {a} < {b} in linear chain topo order");
            }
            _ => panic!("unexpected node type"),
        }
    }
}

#[test]
fn prop_dag_calculated_schedule_resolution() {
    // dt1 (schedule=60) ← dt2 (CALCULATED)
    let mut dag = DtDag::new();
    dag.add_dt_node(DagNode {
        id: NodeId::StreamTable(1),
        schedule: Some(Duration::from_secs(60)),
        effective_schedule: Duration::from_secs(60),
        name: "dt_1".into(),
        status: DtStatus::Active,
        schedule_raw: None,
    });
    dag.add_dt_node(DagNode {
        id: NodeId::StreamTable(2),
        schedule: None, // CALCULATED
        effective_schedule: Duration::ZERO,
        name: "dt_2".into(),
        status: DtStatus::Active,
        schedule_raw: None,
    });
    // dt_2 depends on dt_1: so dt_1 is upstream of dt_2
    // edge: dt_1 → dt_2 (dt_1 is source, dt_2 is downstream)
    dag.add_edge(NodeId::StreamTable(1), NodeId::StreamTable(2));

    // CALCULATED means: look at dt_2's downstream dependents.
    // dt_2 has no dependents, so fallback applies.
    dag.resolve_calculated_schedule(30); // fallback = 30s

    let nodes = dag.get_all_dt_nodes();
    for node in &nodes {
        if let NodeId::StreamTable(2) = node.id {
            // With no downstream dependents, CALCULATED gets fallback
            assert_eq!(node.effective_schedule, Duration::from_secs(30));
        }
    }
}

#[test]
fn prop_dag_empty_is_acyclic() {
    let dag = DtDag::new();
    assert!(dag.detect_cycles().is_ok());
    assert!(dag.topological_order().unwrap().is_empty());
}

#[test]
fn prop_dag_single_node_no_cycle() {
    let mut dag = DtDag::new();
    dag.add_dt_node(DagNode {
        id: NodeId::StreamTable(1),
        schedule: Some(Duration::from_secs(60)),
        effective_schedule: Duration::from_secs(60),
        name: "dt_1".into(),
        status: DtStatus::Active,
        schedule_raw: None,
    });
    assert!(dag.detect_cycles().is_ok());
    assert_eq!(dag.topological_order().unwrap().len(), 1);
}

#[test]
fn prop_dag_base_table_edges() {
    // base table → dt is valid, no cycle possible with a single ST
    let mut dag = DtDag::new();
    dag.add_dt_node(DagNode {
        id: NodeId::StreamTable(1),
        schedule: Some(Duration::from_secs(60)),
        effective_schedule: Duration::from_secs(60),
        name: "dt_1".into(),
        status: DtStatus::Active,
        schedule_raw: None,
    });
    dag.add_edge(NodeId::BaseTable(100), NodeId::StreamTable(1));

    assert!(dag.detect_cycles().is_ok());

    let upstream = dag.get_upstream(NodeId::StreamTable(1));
    assert_eq!(upstream, vec![NodeId::BaseTable(100)]);

    let downstream = dag.get_downstream(NodeId::BaseTable(100));
    assert_eq!(downstream, vec![NodeId::StreamTable(1)]);
}

// ── Hash determinism test ──────────────────────────────────────────────────

#[test]
fn prop_hash_determinism() {
    use xxhash_rust::xxh64;
    let seed = 0x517cc1b727220a95u64;

    // Same input always produces same hash
    for input in &["hello", "world", "", "a longer string with spaces", "123"] {
        let h1 = xxh64::xxh64(input.as_bytes(), seed);
        let h2 = xxh64::xxh64(input.as_bytes(), seed);
        assert_eq!(h1, h2, "hash must be deterministic for '{input}'");
    }
}

#[test]
fn prop_hash_separator_distinguishes() {
    use xxhash_rust::xxh64;
    let seed = 0x517cc1b727220a95u64;
    let sep = "\x1E";

    // hash_multi(["ab", "c"]) != hash_multi(["a", "bc"])
    let combined1 = format!("ab{sep}c");
    let combined2 = format!("a{sep}bc");
    let h1 = xxh64::xxh64(combined1.as_bytes(), seed);
    let h2 = xxh64::xxh64(combined2.as_bytes(), seed);
    assert_ne!(
        h1, h2,
        "separator must distinguish different column boundaries"
    );
}

#[test]
fn prop_hash_null_encoding_distinct() {
    use xxhash_rust::xxh64;
    let seed = 0x517cc1b727220a95u64;
    let sep = "\x1E";
    let null_marker = "\x00NULL\x00";

    // hash(["a", NULL, "b"]) != hash(["a", "b"])
    let with_null = format!("a{sep}{null_marker}{sep}b");
    let without_null = format!("a{sep}b");
    let h1 = xxh64::xxh64(with_null.as_bytes(), seed);
    let h2 = xxh64::xxh64(without_null.as_bytes(), seed);
    assert_ne!(h1, h2, "NULL marker must distinguish from missing column");
}
