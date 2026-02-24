//! Dependency DAG construction, topological sort, and cycle detection.
//!
//! The DAG tracks relationships between stream tables and their upstream
//! sources (base tables, views, or other stream tables).
//!
//! # Prior Art — Scheduling Theory
//!
//! The earliest-deadline-first (EDF) scheduler in `scheduler.rs` follows the
//! classic EDF algorithm:
//! - Liu, C.L. & Layland, J.W. (1973). "Scheduling Algorithms for
//!   Multiprogramming in a Hard-Real-Time Environment." Journal of the ACM,
//!   20(1), 46–61.
//!   EDF is optimal for uniprocessor preemptive scheduling and is widely used
//!   in operating systems and real-time databases.
//!
//! # Prior Art — Graph Algorithms
//!
//! The dependency graph algorithms (topological sort, cycle detection) use
//! Kahn's algorithm:
//! - Kahn, A.B. (1962). "Topological sorting of large networks."
//!   Communications of the ACM, 5(11), 558–562.
//!   This is standard computer science curriculum and appears in every major
//!   algorithms textbook (Cormen et al., Sedgewick, Kleinberg & Tardos).

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crate::error::PgStreamError;

#[cfg(feature = "pg18")]
use pgrx::prelude::*;

/// Identifies a node in the dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// A regular base table or view, identified by PostgreSQL OID.
    BaseTable(u32),
    /// A stream table, identified by its `pgs_id` from the catalog.
    StreamTable(i64),
}

/// Status of a stream table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StStatus {
    Initializing,
    Active,
    Suspended,
    Error,
}

impl StStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StStatus::Initializing => "INITIALIZING",
            StStatus::Active => "ACTIVE",
            StStatus::Suspended => "SUSPENDED",
            StStatus::Error => "ERROR",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, PgStreamError> {
        match s {
            "INITIALIZING" => Ok(StStatus::Initializing),
            "ACTIVE" => Ok(StStatus::Active),
            "SUSPENDED" => Ok(StStatus::Suspended),
            "ERROR" => Ok(StStatus::Error),
            other => Err(PgStreamError::InvalidArgument(format!(
                "unknown status: {other}"
            ))),
        }
    }
}

/// Refresh mode for a stream table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshMode {
    Full,
    Differential,
}

impl RefreshMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RefreshMode::Full => "FULL",
            RefreshMode::Differential => "DIFFERENTIAL",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, PgStreamError> {
        match s.to_uppercase().as_str() {
            "FULL" => Ok(RefreshMode::Full),
            "DIFFERENTIAL" => Ok(RefreshMode::Differential),
            // Accept INCREMENTAL as a deprecated alias for backward compatibility.
            "INCREMENTAL" => Ok(RefreshMode::Differential),
            other => Err(PgStreamError::InvalidArgument(format!(
                "unknown refresh mode: {other}. Must be 'FULL' or 'DIFFERENTIAL'"
            ))),
        }
    }
}

/// Metadata for a node in the DAG.
#[derive(Debug, Clone)]
pub struct DagNode {
    pub id: NodeId,
    /// User-specified schedule. `None` means CALCULATED or cron-scheduled.
    pub schedule: Option<Duration>,
    /// Resolved effective schedule (including CALCULATED resolution).
    pub effective_schedule: Duration,
    /// Name for display and error messages.
    pub name: String,
    /// Status of this ST (only meaningful for ST nodes).
    pub status: StStatus,
    /// Raw schedule string from the catalog (e.g. "5m" or "*/5 * * * *").
    /// `None` for CALCULATED.
    pub schedule_raw: Option<String>,
}

/// In-memory dependency graph of stream tables and their sources.
pub struct StDag {
    /// Forward edges: source → list of downstream ST node IDs.
    edges: HashMap<NodeId, Vec<NodeId>>,
    /// Reverse edges: ST node → list of upstream source node IDs.
    reverse_edges: HashMap<NodeId, Vec<NodeId>>,
    /// Node metadata (only for ST nodes).
    nodes: HashMap<NodeId, DagNode>,
    /// All node IDs in the graph.
    all_nodes: HashSet<NodeId>,
}

impl StDag {
    /// Create an empty DAG.
    pub fn new() -> Self {
        StDag {
            edges: HashMap::new(),
            reverse_edges: HashMap::new(),
            nodes: HashMap::new(),
            all_nodes: HashSet::new(),
        }
    }

    /// Build the DAG from the catalog tables via SPI.
    ///
    /// Loads all stream tables and their dependencies, constructs the graph,
    /// and resolves CALCULATED schedules.
    #[cfg(feature = "pg18")]
    pub fn build_from_catalog(fallback_schedule_secs: i32) -> Result<Self, PgStreamError> {
        let mut dag = StDag::new();

        Spi::connect(|client| {
            // Load all stream tables
            let st_table = client
                .select(
                    "SELECT pgs_id, pgs_relid, pgs_name, pgs_schema, \
                     schedule AS schedule_secs, \
                     status, refresh_mode, is_populated, needs_reinit \
                     FROM pgstream.pgs_stream_tables",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            for row in st_table {
                let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());

                let pgs_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let _pgs_relid = row.get::<pg_sys::Oid>(2).map_err(map_spi)?;
                let pgs_name = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
                let pgs_schema = row.get::<String>(4).map_err(map_spi)?.unwrap_or_default();
                let schedule_text = row.get::<String>(5).map_err(map_spi)?;
                let status_str = row.get::<String>(6).map_err(map_spi)?.unwrap_or_default();
                let _mode_str = row.get::<String>(7).map_err(map_spi)?.unwrap_or_default();

                // For duration-based schedule, parse to Duration.
                // For cron expressions, treat as None (CALCULATED) for DAG
                // resolution — cron STs are scheduled independently.
                let schedule = schedule_text.as_ref().and_then(|s| {
                    crate::api::parse_duration(s)
                        .ok()
                        .map(|secs| Duration::from_secs(secs.max(0) as u64))
                });
                let status = StStatus::from_str(&status_str).unwrap_or(StStatus::Error);
                let effective_schedule = schedule.unwrap_or(Duration::ZERO);

                dag.add_dt_node(DagNode {
                    id: NodeId::StreamTable(pgs_id),
                    schedule,
                    effective_schedule,
                    name: format!("{}.{}", pgs_schema, pgs_name),
                    status,
                    schedule_raw: schedule_text,
                });
            }

            // Load all dependency edges
            let dep_table = client
                .select(
                    "SELECT d.pgs_id, d.source_relid, d.source_type, \
                     st.pgs_id AS source_pgs_id \
                     FROM pgstream.pgs_dependencies d \
                     LEFT JOIN pgstream.pgs_stream_tables st ON st.pgs_relid = d.source_relid",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string()))?;

            for row in dep_table {
                let map_spi = |e: pgrx::spi::SpiError| PgStreamError::SpiError(e.to_string());

                let pgs_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let source_relid = row
                    .get::<pg_sys::Oid>(2)
                    .map_err(map_spi)?
                    .unwrap_or(pg_sys::InvalidOid);
                let source_pgs_id = row.get::<i64>(4).map_err(map_spi)?;

                // Determine the source node type
                let source_node = match source_pgs_id {
                    Some(src_pgs_id) => NodeId::StreamTable(src_pgs_id),
                    None => NodeId::BaseTable(source_relid.to_u32()),
                };

                let downstream_node = NodeId::StreamTable(pgs_id);
                dag.add_edge(source_node, downstream_node);
            }

            Ok::<(), PgStreamError>(())
        })?;

        // Resolve CALCULATED schedules
        dag.resolve_calculated_schedule(fallback_schedule_secs);

        Ok(dag)
    }

    /// Add a stream table node to the DAG.
    pub fn add_dt_node(&mut self, node: DagNode) {
        let id = node.id;
        self.all_nodes.insert(id);
        self.nodes.insert(id, node);
    }

    /// Add an edge from `source` to `downstream_dt`.
    pub fn add_edge(&mut self, source: NodeId, downstream_dt: NodeId) {
        self.all_nodes.insert(source);
        self.all_nodes.insert(downstream_dt);
        self.edges.entry(source).or_default().push(downstream_dt);
        self.reverse_edges
            .entry(downstream_dt)
            .or_default()
            .push(source);
    }

    /// Get all upstream sources of a stream table node.
    pub fn get_upstream(&self, node: NodeId) -> Vec<NodeId> {
        self.reverse_edges.get(&node).cloned().unwrap_or_default()
    }

    /// Get all immediate downstream dependents of a node.
    pub fn get_downstream(&self, node: NodeId) -> Vec<NodeId> {
        self.edges.get(&node).cloned().unwrap_or_default()
    }

    /// Get all ST nodes in the graph.
    pub fn get_all_dt_nodes(&self) -> Vec<&DagNode> {
        self.nodes.values().collect()
    }

    /// Detect cycles using Kahn's algorithm (BFS topological sort).
    ///
    /// Returns `Ok(())` if the graph is acyclic, or `Err(CycleDetected)` with
    /// the names of nodes involved in the cycle.
    pub fn detect_cycles(&self) -> Result<(), PgStreamError> {
        let topo = self.topological_sort_inner()?;
        if topo.len() < self.all_nodes.len() {
            // Some nodes were not processed → cycle exists.
            let processed: HashSet<_> = topo.into_iter().collect();
            let cycle_nodes: Vec<String> = self
                .all_nodes
                .iter()
                .filter(|n| !processed.contains(n))
                .map(|n| self.node_name(n))
                .collect();
            Err(PgStreamError::CycleDetected(cycle_nodes))
        } else {
            Ok(())
        }
    }

    /// Return ST nodes in topological order (upstream first).
    ///
    /// Only returns `NodeId::StreamTable` entries; base tables are excluded
    /// from the output since they don't need refreshing.
    pub fn topological_order(&self) -> Result<Vec<NodeId>, PgStreamError> {
        let all = self.topological_sort_inner()?;
        Ok(all
            .into_iter()
            .filter(|n| matches!(n, NodeId::StreamTable(_)))
            .collect())
    }

    /// Resolve CALCULATED schedules.
    ///
    /// For STs with `schedule = None` (CALCULATED), compute the effective schedule
    /// as `MIN(schedule)` across all immediate downstream dependents.
    /// If no downstream dependents exist, use a fallback (the min schedule GUC).
    pub fn resolve_calculated_schedule(&mut self, fallback_seconds: i32) {
        let fallback = Duration::from_secs(fallback_seconds as u64);

        // Iterate until convergence (at most |V| iterations).
        let mut changed = true;
        let mut iterations = 0;
        let max_iterations = self.nodes.len() + 1;

        while changed && iterations < max_iterations {
            changed = false;
            iterations += 1;

            let node_ids: Vec<NodeId> = self.nodes.keys().copied().collect();
            for id in node_ids {
                let node = &self.nodes[&id];
                if let Some(tl) = node.schedule {
                    // Explicit schedule — effective_schedule = schedule.
                    if self.nodes[&id].effective_schedule != tl {
                        self.nodes.get_mut(&id).unwrap().effective_schedule = tl;
                        changed = true;
                    }
                    continue;
                }

                // CALCULATED: MIN(effective_schedule) of immediate downstream STs.
                let downstream = self.get_downstream(id);
                let min_schedule = downstream
                    .iter()
                    .filter_map(|d| self.nodes.get(d))
                    .map(|d| d.effective_schedule)
                    .min()
                    .unwrap_or(fallback);

                if self.nodes[&id].effective_schedule != min_schedule {
                    self.nodes.get_mut(&id).unwrap().effective_schedule = min_schedule;
                    changed = true;
                }
            }
        }
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Kahn's algorithm: BFS topological sort.
    fn topological_sort_inner(&self) -> Result<Vec<NodeId>, PgStreamError> {
        // Compute in-degrees.
        let mut in_degree: HashMap<NodeId, usize> = HashMap::new();
        for &node in &self.all_nodes {
            in_degree.entry(node).or_insert(0);
        }
        for targets in self.edges.values() {
            for &target in targets {
                *in_degree.entry(target).or_insert(0) += 1;
            }
        }

        // Enqueue zero-indegree nodes.
        let mut queue: VecDeque<NodeId> = in_degree
            .iter()
            .filter(|&(_, deg)| *deg == 0)
            .map(|(&node, _)| node)
            .collect();

        let mut result = Vec::with_capacity(self.all_nodes.len());

        while let Some(node) = queue.pop_front() {
            result.push(node);
            if let Some(downstream) = self.edges.get(&node) {
                for &d in downstream {
                    let deg = in_degree.get_mut(&d).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(d);
                    }
                }
            }
        }

        Ok(result)
    }

    /// Human-readable name for a node.
    fn node_name(&self, node: &NodeId) -> String {
        match self.nodes.get(node) {
            Some(n) => n.name.clone(),
            None => match node {
                NodeId::BaseTable(oid) => format!("base_table(oid={})", oid),
                NodeId::StreamTable(id) => format!("stream_table(id={})", id),
            },
        }
    }
}

impl Default for StDag {
    fn default() -> Self {
        Self::new()
    }
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topological_sort_simple_chain() {
        // base_table -> dt1 -> dt2
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::from_secs(120),
            name: "dt2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, dt1);
        dag.add_edge(dt1, dt2);

        let order = dag.topological_order().unwrap();
        assert_eq!(order, vec![dt1, dt2]);
    }

    #[test]
    fn test_cycle_detection_detects_cycle() {
        let mut dag = StDag::new();
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(dt1, dt2);
        dag.add_edge(dt2, dt1);

        let result = dag.detect_cycles();
        assert!(result.is_err());
        if let Err(PgStreamError::CycleDetected(nodes)) = result {
            assert_eq!(nodes.len(), 2);
        }
    }

    #[test]
    fn test_no_cycle_in_valid_dag() {
        let mut dag = StDag::new();
        let base1 = NodeId::BaseTable(1);
        let base2 = NodeId::BaseTable(2);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);
        let dt3 = NodeId::StreamTable(3);

        for (id, name) in [(dt1, "dt1"), (dt2, "dt2"), (dt3, "dt3")] {
            dag.add_dt_node(DagNode {
                id,
                schedule: Some(Duration::from_secs(60)),
                effective_schedule: Duration::from_secs(60),
                name: name.to_string(),
                status: StStatus::Active,
                schedule_raw: None,
            });
        }

        // Diamond: base1 -> dt1, base2 -> dt2, dt1 -> dt3, dt2 -> dt3
        dag.add_edge(base1, dt1);
        dag.add_edge(base2, dt2);
        dag.add_edge(dt1, dt3);
        dag.add_edge(dt2, dt3);

        assert!(dag.detect_cycles().is_ok());
        let order = dag.topological_order().unwrap();
        // dt3 must come after dt1 and dt2
        let pos1 = order.iter().position(|n| *n == dt1).unwrap();
        let pos2 = order.iter().position(|n| *n == dt2).unwrap();
        let pos3 = order.iter().position(|n| *n == dt3).unwrap();
        assert!(pos3 > pos1);
        assert!(pos3 > pos2);
    }

    #[test]
    fn test_calculated_schedule_resolution() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        // dt1 is CALCULATED (schedule = None), dt2 has explicit 120s schedule
        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: None,
            effective_schedule: Duration::ZERO,
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::from_secs(120),
            name: "dt2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, dt1);
        dag.add_edge(dt1, dt2);

        dag.resolve_calculated_schedule(60);

        // dt1's effective schedule should be MIN of dt2's effective schedule = 120s
        let dt1_node = dag.nodes.get(&dt1).unwrap();
        assert_eq!(dt1_node.effective_schedule, Duration::from_secs(120));
    }

    #[test]
    fn test_calculated_schedule_no_dependents_uses_fallback() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let dt1 = NodeId::StreamTable(1);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: None,
            effective_schedule: Duration::ZERO,
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, dt1);

        dag.resolve_calculated_schedule(60);

        // No dependents → fallback = 60s
        let dt1_node = dag.nodes.get(&dt1).unwrap();
        assert_eq!(dt1_node.effective_schedule, Duration::from_secs(60));
    }

    #[test]
    fn test_empty_dag() {
        let dag = StDag::new();
        assert!(dag.detect_cycles().is_ok());
        assert!(dag.topological_order().unwrap().is_empty());
    }

    // ── Phase 4: Edge-case tests ────────────────────────────────────

    #[test]
    fn test_single_node_no_edges() {
        let mut dag = StDag::new();
        let st = NodeId::StreamTable(1);
        dag.add_dt_node(DagNode {
            id: st,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        assert!(dag.detect_cycles().is_ok());
        let order = dag.topological_order().unwrap();
        assert_eq!(order, vec![st]);
    }

    #[test]
    fn test_get_upstream_and_downstream() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, dt1);
        dag.add_edge(dt1, dt2);

        assert_eq!(dag.get_upstream(dt1), vec![base]);
        assert_eq!(dag.get_downstream(dt1), vec![dt2]);
        assert_eq!(dag.get_upstream(dt2), vec![dt1]);
        assert!(dag.get_downstream(dt2).is_empty());
        assert!(dag.get_upstream(base).is_empty());
    }

    #[test]
    fn test_get_all_dt_nodes() {
        let mut dag = StDag::new();
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(30)),
            effective_schedule: Duration::from_secs(30),
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt2".to_string(),
            status: StStatus::Suspended,
            schedule_raw: None,
        });

        let nodes = dag.get_all_dt_nodes();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn test_node_name_for_known_and_unknown_nodes() {
        let mut dag = StDag::new();
        let st = NodeId::StreamTable(42);
        dag.add_dt_node(DagNode {
            id: st,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "my_st".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        // Known node returns its name
        assert_eq!(dag.node_name(&st), "my_st");

        // Unknown ST returns formatted string
        let unknown_dt = NodeId::StreamTable(999);
        assert_eq!(dag.node_name(&unknown_dt), "stream_table(id=999)");

        // Unknown base table returns formatted string
        let base = NodeId::BaseTable(123);
        assert_eq!(dag.node_name(&base), "base_table(oid=123)");
    }

    #[test]
    fn test_diamond_dependency_pattern() {
        // base1 → dt1 → dt3
        // base2 → dt2 → dt3
        let mut dag = StDag::new();
        let base1 = NodeId::BaseTable(1);
        let base2 = NodeId::BaseTable(2);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);
        let dt3 = NodeId::StreamTable(3);

        for (id, name) in [(dt1, "dt1"), (dt2, "dt2"), (dt3, "dt3")] {
            dag.add_dt_node(DagNode {
                id,
                schedule: Some(Duration::from_secs(60)),
                effective_schedule: Duration::from_secs(60),
                name: name.to_string(),
                status: StStatus::Active,
                schedule_raw: None,
            });
        }

        dag.add_edge(base1, dt1);
        dag.add_edge(base2, dt2);
        dag.add_edge(dt1, dt3);
        dag.add_edge(dt2, dt3);

        assert!(dag.detect_cycles().is_ok());
        let order = dag.topological_order().unwrap();
        assert_eq!(order.len(), 3);

        // dt3 must be after dt1 and dt2
        let pos = |id: NodeId| order.iter().position(|n| *n == id).unwrap();
        assert!(pos(dt3) > pos(dt1));
        assert!(pos(dt3) > pos(dt2));
    }

    #[test]
    fn test_pgs_status_as_str_and_from_str_roundtrip() {
        for status in [
            StStatus::Initializing,
            StStatus::Active,
            StStatus::Suspended,
            StStatus::Error,
        ] {
            let s = status.as_str();
            let parsed = StStatus::from_str(s).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_pgs_status_from_str_unknown_returns_error() {
        let result = StStatus::from_str("UNKNOWN");
        assert!(result.is_err());
        if let Err(PgStreamError::InvalidArgument(msg)) = result {
            assert!(msg.contains("unknown status"));
        }
    }

    #[test]
    fn test_refresh_mode_as_str_and_from_str_roundtrip() {
        for mode in [RefreshMode::Full, RefreshMode::Differential] {
            let s = mode.as_str();
            let parsed = RefreshMode::from_str(s).unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn test_refresh_mode_from_str_case_insensitive() {
        assert_eq!(RefreshMode::from_str("full").unwrap(), RefreshMode::Full);
        assert_eq!(RefreshMode::from_str("FULL").unwrap(), RefreshMode::Full);
        assert_eq!(RefreshMode::from_str("Full").unwrap(), RefreshMode::Full);
        assert_eq!(
            RefreshMode::from_str("incremental").unwrap(),
            RefreshMode::Differential
        );
    }

    #[test]
    fn test_refresh_mode_from_str_unknown_returns_error() {
        let result = RefreshMode::from_str("INVALID");
        assert!(result.is_err());
        if let Err(PgStreamError::InvalidArgument(msg)) = result {
            assert!(msg.contains("unknown refresh mode"));
        }
    }

    #[test]
    fn test_downstream_schedule_multiple_downstream_uses_minimum() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let st_downstream = NodeId::StreamTable(1);
        let st_fast = NodeId::StreamTable(2);
        let st_slow = NodeId::StreamTable(3);

        // st_downstream (DOWNSTREAM) → st_fast (30s), st_slow (120s)
        dag.add_dt_node(DagNode {
            id: st_downstream,
            schedule: None,
            effective_schedule: Duration::ZERO,
            name: "st_downstream".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: st_fast,
            schedule: Some(Duration::from_secs(30)),
            effective_schedule: Duration::from_secs(30),
            name: "st_fast".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: st_slow,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::from_secs(120),
            name: "st_slow".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st_downstream);
        dag.add_edge(st_downstream, st_fast);
        dag.add_edge(st_downstream, st_slow);

        dag.resolve_calculated_schedule(60);

        // Should use MIN(30, 120) = 30
        let node = dag.nodes.get(&st_downstream).unwrap();
        assert_eq!(node.effective_schedule, Duration::from_secs(30));
    }

    #[test]
    fn test_default_trait_for_dtdag() {
        let dag = StDag::default();
        assert!(dag.detect_cycles().is_ok());
        assert!(dag.topological_order().unwrap().is_empty());
    }

    #[test]
    fn test_node_id_equality_and_hashing() {
        use std::collections::HashSet;
        let a = NodeId::BaseTable(1);
        let b = NodeId::BaseTable(1);
        let c = NodeId::StreamTable(1);
        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b); // same as a
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_cycle_detection_three_node_cycle() {
        let mut dag = StDag::new();
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);
        let dt3 = NodeId::StreamTable(3);

        for (id, name) in [(dt1, "dt1"), (dt2, "dt2"), (dt3, "dt3")] {
            dag.add_dt_node(DagNode {
                id,
                schedule: Some(Duration::from_secs(60)),
                effective_schedule: Duration::from_secs(60),
                name: name.to_string(),
                status: StStatus::Active,
                schedule_raw: None,
            });
        }

        dag.add_edge(dt1, dt2);
        dag.add_edge(dt2, dt3);
        dag.add_edge(dt3, dt1);

        let result = dag.detect_cycles();
        assert!(result.is_err());
        if let Err(PgStreamError::CycleDetected(nodes)) = result {
            assert_eq!(nodes.len(), 3);
        }
    }

    #[test]
    fn test_topological_order_excludes_base_tables() {
        let mut dag = StDag::new();
        let base1 = NodeId::BaseTable(1);
        let base2 = NodeId::BaseTable(2);
        let dt1 = NodeId::StreamTable(1);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base1, dt1);
        dag.add_edge(base2, dt1);

        let order = dag.topological_order().unwrap();
        assert_eq!(order, vec![dt1]); // No base tables in output
    }

    #[test]
    fn test_explicit_schedule_overrides_downstream_resolution() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        // dt1 has explicit schedule of 60s, dt2 has explicit schedule of 120s
        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::ZERO, // will be set to 60
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::ZERO, // will be set to 120
            name: "dt2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, dt1);
        dag.add_edge(dt1, dt2);

        dag.resolve_calculated_schedule(30);

        assert_eq!(
            dag.nodes.get(&dt1).unwrap().effective_schedule,
            Duration::from_secs(60)
        );
        assert_eq!(
            dag.nodes.get(&dt2).unwrap().effective_schedule,
            Duration::from_secs(120)
        );
    }

    #[test]
    fn test_resolve_downstream_schedule_chain_three_levels() {
        // base -> dt1 (downstream) -> dt2 (downstream) -> dt3 (60s)
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);
        let dt3 = NodeId::StreamTable(3);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: None, // downstream
            effective_schedule: Duration::ZERO,
            name: "dt1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: None, // downstream
            effective_schedule: Duration::ZERO,
            name: "dt2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt3,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "dt3".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, dt1);
        dag.add_edge(dt1, dt2);
        dag.add_edge(dt2, dt3);

        dag.resolve_calculated_schedule(300);

        // dt2 gets min of dt3 = 60s, dt1 gets min of dt2 = 60s
        assert_eq!(
            dag.nodes.get(&dt1).unwrap().effective_schedule,
            Duration::from_secs(60)
        );
        assert_eq!(
            dag.nodes.get(&dt2).unwrap().effective_schedule,
            Duration::from_secs(60)
        );
    }

    #[test]
    fn test_node_name_unknown_base_table() {
        let dag = StDag::new();
        // Node not in the graph → should produce a fallback name
        let name = dag.node_name(&NodeId::BaseTable(99999));
        assert!(name.contains("99999"), "expected OID in name: {}", name);
        assert!(
            name.contains("base_table"),
            "expected 'base_table' prefix: {}",
            name
        );
    }

    #[test]
    fn test_node_name_unknown_stream_table() {
        let dag = StDag::new();
        let name = dag.node_name(&NodeId::StreamTable(42));
        assert!(name.contains("42"), "expected ID in name: {}", name);
        assert!(
            name.contains("stream_table"),
            "expected 'stream_table' prefix: {}",
            name
        );
    }

    #[test]
    fn test_cycle_detection_error_message_contains_node_names() {
        let mut dag = StDag::new();
        let dt1 = NodeId::StreamTable(1);
        let dt2 = NodeId::StreamTable(2);

        dag.add_dt_node(DagNode {
            id: dt1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::ZERO,
            name: "my_view_a".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_dt_node(DagNode {
            id: dt2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::ZERO,
            name: "my_view_b".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(dt1, dt2);
        dag.add_edge(dt2, dt1); // cycle!

        let err = dag.detect_cycles().unwrap_err();
        let msg = format!("{}", err);
        // Error message should reference the named nodes
        assert!(
            msg.contains("my_view_a") || msg.contains("my_view_b"),
            "cycle error should contain node names: {}",
            msg
        );
    }
}
