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

use crate::error::PgTrickleError;

// ── Diamond dependency types ───────────────────────────────────────────────

/// A detected diamond in the ST dependency graph.
///
/// A diamond exists when two or more paths from shared source(s) converge at a
/// single fan-in stream table via different intermediate STs.
///
/// Example: `A → B → D` and `A → C → D` — convergence is D, shared source is
/// A, intermediates are {B, C}.
#[derive(Debug, Clone)]
pub struct Diamond {
    /// The fan-in ST that joins two or more upstream paths.
    pub convergence: NodeId,
    /// Base tables / STs that are the shared root(s) of the converging paths.
    pub shared_sources: Vec<NodeId>,
    /// Intermediate STs on all paths from shared_sources to convergence
    /// (excludes both the convergence node and the shared sources).
    pub intermediates: Vec<NodeId>,
}

/// A set of stream tables that must refresh atomically to maintain cross-path
/// consistency.
///
/// When `diamond_consistency = 'atomic'`, all members are wrapped in a single
/// SAVEPOINT. If any member's refresh fails, the entire group is rolled back.
#[derive(Debug, Clone)]
pub struct ConsistencyGroup {
    /// All members in topological order, including the convergence ST (last).
    pub members: Vec<NodeId>,
    /// The fan-in ST(s) that required this group.
    pub convergence_points: Vec<NodeId>,
    /// Monotonically increasing counter; advances on every successful group
    /// refresh.
    pub epoch: u64,
}

impl ConsistencyGroup {
    /// Returns `true` when the group contains only a single ST.
    ///
    /// Singleton groups represent non-diamond STs and skip the SAVEPOINT
    /// overhead in the scheduler.
    pub fn is_singleton(&self) -> bool {
        self.members.len() == 1
    }

    /// Advance the epoch counter after a successful group refresh.
    pub fn advance_epoch(&mut self) {
        self.epoch += 1;
    }
}

#[cfg(feature = "pg18")]
use pgrx::prelude::*;

/// Identifies a node in the dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// A regular base table or view, identified by PostgreSQL OID.
    BaseTable(u32),
    /// A stream table, identified by its `pgt_id` from the catalog.
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
    pub fn from_str(s: &str) -> Result<Self, PgTrickleError> {
        match s {
            "INITIALIZING" => Ok(StStatus::Initializing),
            "ACTIVE" => Ok(StStatus::Active),
            "SUSPENDED" => Ok(StStatus::Suspended),
            "ERROR" => Ok(StStatus::Error),
            other => Err(PgTrickleError::InvalidArgument(format!(
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
    pub fn from_str(s: &str) -> Result<Self, PgTrickleError> {
        match s.to_uppercase().as_str() {
            "FULL" => Ok(RefreshMode::Full),
            "DIFFERENTIAL" => Ok(RefreshMode::Differential),
            // Accept INCREMENTAL as a deprecated alias for backward compatibility.
            "INCREMENTAL" => Ok(RefreshMode::Differential),
            other => Err(PgTrickleError::InvalidArgument(format!(
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
    pub fn build_from_catalog(fallback_schedule_secs: i32) -> Result<Self, PgTrickleError> {
        let mut dag = StDag::new();

        Spi::connect(|client| {
            // Load all stream tables
            let st_table = client
                .select(
                    "SELECT pgt_id, pgt_relid, pgt_name, pgt_schema, \
                     schedule AS schedule_secs, \
                     status, refresh_mode, is_populated, needs_reinit \
                     FROM pgtrickle.pgt_stream_tables",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            for row in st_table {
                let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());

                let pgt_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let _pgt_relid = row.get::<pg_sys::Oid>(2).map_err(map_spi)?;
                let pgt_name = row.get::<String>(3).map_err(map_spi)?.unwrap_or_default();
                let pgt_schema = row.get::<String>(4).map_err(map_spi)?.unwrap_or_default();
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

                dag.add_st_node(DagNode {
                    id: NodeId::StreamTable(pgt_id),
                    schedule,
                    effective_schedule,
                    name: format!("{}.{}", pgt_schema, pgt_name),
                    status,
                    schedule_raw: schedule_text,
                });
            }

            // Load all dependency edges
            let dep_table = client
                .select(
                    "SELECT d.pgt_id, d.source_relid, d.source_type, \
                     st.pgt_id AS source_pgt_id \
                     FROM pgtrickle.pgt_dependencies d \
                     LEFT JOIN pgtrickle.pgt_stream_tables st ON st.pgt_relid = d.source_relid",
                    None,
                    &[],
                )
                .map_err(|e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string()))?;

            for row in dep_table {
                let map_spi = |e: pgrx::spi::SpiError| PgTrickleError::SpiError(e.to_string());

                let pgt_id = row.get::<i64>(1).map_err(map_spi)?.unwrap_or(0);
                let source_relid = row
                    .get::<pg_sys::Oid>(2)
                    .map_err(map_spi)?
                    .unwrap_or(pg_sys::InvalidOid);
                let source_pgt_id = row.get::<i64>(4).map_err(map_spi)?;

                // Determine the source node type
                let source_node = match source_pgt_id {
                    Some(src_pgt_id) => NodeId::StreamTable(src_pgt_id),
                    None => NodeId::BaseTable(source_relid.to_u32()),
                };

                let downstream_node = NodeId::StreamTable(pgt_id);
                dag.add_edge(source_node, downstream_node);
            }

            Ok::<(), PgTrickleError>(())
        })?;

        // Resolve CALCULATED schedules
        dag.resolve_calculated_schedule(fallback_schedule_secs);

        Ok(dag)
    }

    /// Add a stream table node to the DAG.
    pub fn add_st_node(&mut self, node: DagNode) {
        let id = node.id;
        self.all_nodes.insert(id);
        self.nodes.insert(id, node);
    }

    /// Add an edge from `source` to `downstream_st`.
    pub fn add_edge(&mut self, source: NodeId, downstream_st: NodeId) {
        self.all_nodes.insert(source);
        self.all_nodes.insert(downstream_st);
        self.edges.entry(source).or_default().push(downstream_st);
        self.reverse_edges
            .entry(downstream_st)
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
    pub fn get_all_st_nodes(&self) -> Vec<&DagNode> {
        self.nodes.values().collect()
    }

    /// Detect cycles using Kahn's algorithm (BFS topological sort).
    ///
    /// Returns `Ok(())` if the graph is acyclic, or `Err(CycleDetected)` with
    /// the names of nodes involved in the cycle.
    pub fn detect_cycles(&self) -> Result<(), PgTrickleError> {
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
            Err(PgTrickleError::CycleDetected(cycle_nodes))
        } else {
            Ok(())
        }
    }

    /// Return ST nodes in topological order (upstream first).
    ///
    /// Only returns `NodeId::StreamTable` entries; base tables are excluded
    /// from the output since they don't need refreshing.
    pub fn topological_order(&self) -> Result<Vec<NodeId>, PgTrickleError> {
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
                    if self.nodes[&id].effective_schedule != tl
                        && let Some(node) = self.nodes.get_mut(&id)
                    {
                        node.effective_schedule = tl;
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

                if self.nodes[&id].effective_schedule != min_schedule
                    && let Some(node) = self.nodes.get_mut(&id)
                {
                    node.effective_schedule = min_schedule;
                    changed = true;
                }
            }
        }
    }

    // ── Diamond dependency detection ──────────────────────────────────

    /// Detect all diamond dependencies in the DAG.
    ///
    /// A diamond exists when a fan-in ST node D (with ≥2 upstream ST
    /// dependencies) has two upstream paths that share a common ancestor.
    ///
    /// Returns a list of [`Diamond`] values with overlapping diamonds merged.
    pub fn detect_diamonds(&self) -> Vec<Diamond> {
        let mut diamonds = Vec::new();

        // Find all fan-in nodes: STs with ≥2 upstream ST-or-base dependencies
        // that have at least 2 upstream *ST* dependencies (the interesting case
        // for atomic groups) or 2+ paths sharing a common ancestor via base
        // tables.
        for &node in &self.all_nodes {
            if !matches!(node, NodeId::StreamTable(_)) {
                continue;
            }
            let upstream = self.get_upstream(node);
            if upstream.len() < 2 {
                continue;
            }

            // Collect all paths to roots for each upstream branch.
            // Each "path" is the set of all ancestors reachable from one
            // immediate upstream node.
            let mut branch_ancestors: Vec<(NodeId, HashSet<NodeId>)> = Vec::new();
            for &up in &upstream {
                let mut ancestors = HashSet::new();
                self.collect_ancestors(up, &mut ancestors);
                ancestors.insert(up); // include the immediate upstream itself
                branch_ancestors.push((up, ancestors));
            }

            // Compare every pair of branches for shared ancestors.
            for i in 0..branch_ancestors.len() {
                for j in (i + 1)..branch_ancestors.len() {
                    let shared: Vec<NodeId> = branch_ancestors[i]
                        .1
                        .intersection(&branch_ancestors[j].1)
                        .copied()
                        .collect();

                    if shared.is_empty() {
                        continue;
                    }

                    // Intermediates = union of both branches minus
                    // the convergence node and the shared sources.
                    let shared_set: HashSet<NodeId> = shared.iter().copied().collect();
                    let mut intermediates_set: HashSet<NodeId> = HashSet::new();

                    // Add STs from branch i between shared sources and convergence
                    for &anc in &branch_ancestors[i].1 {
                        if !shared_set.contains(&anc) && anc != node {
                            intermediates_set.insert(anc);
                        }
                    }
                    // Add STs from branch j between shared sources and convergence
                    for &anc in &branch_ancestors[j].1 {
                        if !shared_set.contains(&anc) && anc != node {
                            intermediates_set.insert(anc);
                        }
                    }

                    // Also include the immediate upstream nodes if they are STs
                    // and not themselves shared sources.
                    let up_i = branch_ancestors[i].0;
                    let up_j = branch_ancestors[j].0;
                    if !shared_set.contains(&up_i) {
                        intermediates_set.insert(up_i);
                    }
                    if !shared_set.contains(&up_j) {
                        intermediates_set.insert(up_j);
                    }

                    // Only keep ST nodes as intermediates (base tables don't refresh).
                    let intermediates: Vec<NodeId> = intermediates_set
                        .into_iter()
                        .filter(|n| matches!(n, NodeId::StreamTable(_)))
                        .collect();

                    diamonds.push(Diamond {
                        convergence: node,
                        shared_sources: shared,
                        intermediates,
                    });
                }
            }
        }

        Self::merge_overlapping_diamonds(diamonds)
    }

    /// Compute consistency groups for the scheduler.
    ///
    /// Each detected diamond produces a group containing the intermediate STs
    /// and the convergence ST, in topological order. Overlapping groups are
    /// merged transitively. STs not in any diamond get singleton groups.
    pub fn compute_consistency_groups(&self) -> Vec<ConsistencyGroup> {
        let diamonds = self.detect_diamonds();

        // Build a mapping: NodeId → set index, to enable union-find merging.
        let mut node_to_group: HashMap<NodeId, usize> = HashMap::new();
        let mut groups: Vec<HashSet<NodeId>> = Vec::new();
        let mut convergence_map: Vec<HashSet<NodeId>> = Vec::new();

        for diamond in &diamonds {
            // Members = intermediates ∪ {convergence}
            let mut members: HashSet<NodeId> = diamond.intermediates.iter().copied().collect();
            members.insert(diamond.convergence);

            // Check if any existing group overlaps with this one.
            let overlapping: Vec<usize> = members
                .iter()
                .filter_map(|n| node_to_group.get(n).copied())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            if overlapping.is_empty() {
                // New group.
                let idx = groups.len();
                for &m in &members {
                    node_to_group.insert(m, idx);
                }
                groups.push(members);
                let mut cp = HashSet::new();
                cp.insert(diamond.convergence);
                convergence_map.push(cp);
            } else {
                // Merge into the first overlapping group.
                let target = overlapping[0];
                for &m in &members {
                    node_to_group.insert(m, target);
                    groups[target].insert(m);
                }
                convergence_map[target].insert(diamond.convergence);

                // Merge other overlapping groups into target.
                for &other in overlapping.iter().skip(1) {
                    let other_members: HashSet<NodeId> = groups[other].drain().collect();
                    for m in &other_members {
                        node_to_group.insert(*m, target);
                    }
                    groups[target].extend(other_members);
                    let other_cp: HashSet<NodeId> = convergence_map[other].drain().collect();
                    convergence_map[target].extend(other_cp);
                }
            }
        }

        // Sort each group's members in topological order.
        let topo_order = self.topological_order().unwrap_or_default();
        let topo_pos: HashMap<NodeId, usize> = topo_order
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, i))
            .collect();

        let mut result: Vec<ConsistencyGroup> = Vec::new();
        let mut assigned: HashSet<NodeId> = HashSet::new();

        for (i, group_set) in groups.iter().enumerate() {
            if group_set.is_empty() {
                continue; // Merged away.
            }
            let mut members: Vec<NodeId> = group_set.iter().copied().collect();
            members.sort_by_key(|n| topo_pos.get(n).copied().unwrap_or(usize::MAX));

            let convergence_points: Vec<NodeId> = convergence_map[i].iter().copied().collect();

            for &m in &members {
                assigned.insert(m);
            }

            result.push(ConsistencyGroup {
                members,
                convergence_points,
                epoch: 0,
            });
        }

        // Add singleton groups for all STs not in any diamond group.
        for &node in &self.all_nodes {
            if matches!(node, NodeId::StreamTable(_)) && !assigned.contains(&node) {
                result.push(ConsistencyGroup {
                    members: vec![node],
                    convergence_points: vec![],
                    epoch: 0,
                });
            }
        }

        // Sort groups by the topological position of their first member
        // so the scheduler processes them in dependency order.
        result.sort_by_key(|g| {
            g.members
                .first()
                .and_then(|n| topo_pos.get(n).copied())
                .unwrap_or(usize::MAX)
        });

        result
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Kahn's algorithm: BFS topological sort.
    fn topological_sort_inner(&self) -> Result<Vec<NodeId>, PgTrickleError> {
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
                    if let Some(deg) = in_degree.get_mut(&d) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(d);
                        }
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

    /// Recursively collect all transitive ancestors of `node` (upstream walk).
    fn collect_ancestors(&self, node: NodeId, ancestors: &mut HashSet<NodeId>) {
        if let Some(upstream) = self.reverse_edges.get(&node) {
            for &up in upstream {
                if ancestors.insert(up) {
                    self.collect_ancestors(up, ancestors);
                }
            }
        }
    }

    /// Merge diamonds whose `intermediates` sets overlap into a single diamond.
    ///
    /// This handles nested diamonds (e.g., D and G both being fan-in nodes
    /// sharing an intermediate ST) by transitively merging overlapping sets.
    fn merge_overlapping_diamonds(diamonds: Vec<Diamond>) -> Vec<Diamond> {
        if diamonds.is_empty() {
            return diamonds;
        }

        // Union-Find approach: group diamonds whose intermediates overlap.
        let mut merged: Vec<Diamond> = Vec::new();

        for diamond in diamonds {
            let int_set: HashSet<NodeId> = diamond.intermediates.iter().copied().collect();

            // Find which merged diamond overlaps with this one.
            let mut merge_target: Option<usize> = None;
            for (i, existing) in merged.iter().enumerate() {
                let existing_set: HashSet<NodeId> =
                    existing.intermediates.iter().copied().collect();
                if !int_set.is_disjoint(&existing_set) {
                    merge_target = Some(i);
                    break;
                }
            }

            match merge_target {
                Some(idx) => {
                    // Merge into existing diamond.
                    let existing = &mut merged[idx];
                    let mut combined_int: HashSet<NodeId> =
                        existing.intermediates.iter().copied().collect();
                    combined_int.extend(diamond.intermediates);
                    existing.intermediates = combined_int.into_iter().collect();

                    let mut combined_sources: HashSet<NodeId> =
                        existing.shared_sources.iter().copied().collect();
                    combined_sources.extend(diamond.shared_sources);
                    existing.shared_sources = combined_sources.into_iter().collect();

                    // If the convergence points differ, the original convergence
                    // becomes an intermediate (it's now an interior node of a
                    // larger merged diamond), unless they're the same.
                    if diamond.convergence != existing.convergence {
                        // Keep the downstream-most convergence; add the other
                        // to intermediates. For simplicity, keep the new one's
                        // convergence as well — the consistency group computation
                        // handles multiple convergence points.
                    }
                }
                None => {
                    merged.push(diamond);
                }
            }
        }

        merged
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
        // base_table -> st1 -> st2
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::from_secs(120),
            name: "st2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st1);
        dag.add_edge(st1, st2);

        let order = dag.topological_order().unwrap();
        assert_eq!(order, vec![st1, st2]);
    }

    #[test]
    fn test_cycle_detection_detects_cycle() {
        let mut dag = StDag::new();
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(st1, st2);
        dag.add_edge(st2, st1);

        let result = dag.detect_cycles();
        assert!(result.is_err());
        if let Err(PgTrickleError::CycleDetected(nodes)) = result {
            assert_eq!(nodes.len(), 2);
        }
    }

    #[test]
    fn test_no_cycle_in_valid_dag() {
        let mut dag = StDag::new();
        let base1 = NodeId::BaseTable(1);
        let base2 = NodeId::BaseTable(2);
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);
        let st3 = NodeId::StreamTable(3);

        for (id, name) in [(st1, "st1"), (st2, "st2"), (st3, "st3")] {
            dag.add_st_node(DagNode {
                id,
                schedule: Some(Duration::from_secs(60)),
                effective_schedule: Duration::from_secs(60),
                name: name.to_string(),
                status: StStatus::Active,
                schedule_raw: None,
            });
        }

        // Diamond: base1 -> st1, base2 -> st2, st1 -> st3, st2 -> st3
        dag.add_edge(base1, st1);
        dag.add_edge(base2, st2);
        dag.add_edge(st1, st3);
        dag.add_edge(st2, st3);

        assert!(dag.detect_cycles().is_ok());
        let order = dag.topological_order().unwrap();
        // st3 must come after st1 and st2
        let pos1 = order.iter().position(|n| *n == st1).unwrap();
        let pos2 = order.iter().position(|n| *n == st2).unwrap();
        let pos3 = order.iter().position(|n| *n == st3).unwrap();
        assert!(pos3 > pos1);
        assert!(pos3 > pos2);
    }

    #[test]
    fn test_calculated_schedule_resolution() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        // st1 is CALCULATED (schedule = None), st2 has explicit 120s schedule
        dag.add_st_node(DagNode {
            id: st1,
            schedule: None,
            effective_schedule: Duration::ZERO,
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::from_secs(120),
            name: "st2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st1);
        dag.add_edge(st1, st2);

        dag.resolve_calculated_schedule(60);

        // st1's effective schedule should be MIN of st2's effective schedule = 120s
        let st1_node = dag.nodes.get(&st1).unwrap();
        assert_eq!(st1_node.effective_schedule, Duration::from_secs(120));
    }

    #[test]
    fn test_calculated_schedule_no_dependents_uses_fallback() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let st1 = NodeId::StreamTable(1);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: None,
            effective_schedule: Duration::ZERO,
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st1);

        dag.resolve_calculated_schedule(60);

        // No dependents → fallback = 60s
        let st1_node = dag.nodes.get(&st1).unwrap();
        assert_eq!(st1_node.effective_schedule, Duration::from_secs(60));
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
        dag.add_st_node(DagNode {
            id: st,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st1".to_string(),
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
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st1);
        dag.add_edge(st1, st2);

        assert_eq!(dag.get_upstream(st1), vec![base]);
        assert_eq!(dag.get_downstream(st1), vec![st2]);
        assert_eq!(dag.get_upstream(st2), vec![st1]);
        assert!(dag.get_downstream(st2).is_empty());
        assert!(dag.get_upstream(base).is_empty());
    }

    #[test]
    fn test_get_all_st_nodes() {
        let mut dag = StDag::new();
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(30)),
            effective_schedule: Duration::from_secs(30),
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st2".to_string(),
            status: StStatus::Suspended,
            schedule_raw: None,
        });

        let nodes = dag.get_all_st_nodes();
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn test_node_name_for_known_and_unknown_nodes() {
        let mut dag = StDag::new();
        let st = NodeId::StreamTable(42);
        dag.add_st_node(DagNode {
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
        let unknown_st = NodeId::StreamTable(999);
        assert_eq!(dag.node_name(&unknown_st), "stream_table(id=999)");

        // Unknown base table returns formatted string
        let base = NodeId::BaseTable(123);
        assert_eq!(dag.node_name(&base), "base_table(oid=123)");
    }

    #[test]
    fn test_diamond_dependency_pattern() {
        // base1 → st1 → st3
        // base2 → st2 → st3
        let mut dag = StDag::new();
        let base1 = NodeId::BaseTable(1);
        let base2 = NodeId::BaseTable(2);
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);
        let st3 = NodeId::StreamTable(3);

        for (id, name) in [(st1, "st1"), (st2, "st2"), (st3, "st3")] {
            dag.add_st_node(DagNode {
                id,
                schedule: Some(Duration::from_secs(60)),
                effective_schedule: Duration::from_secs(60),
                name: name.to_string(),
                status: StStatus::Active,
                schedule_raw: None,
            });
        }

        dag.add_edge(base1, st1);
        dag.add_edge(base2, st2);
        dag.add_edge(st1, st3);
        dag.add_edge(st2, st3);

        assert!(dag.detect_cycles().is_ok());
        let order = dag.topological_order().unwrap();
        assert_eq!(order.len(), 3);

        // st3 must be after st1 and st2
        let pos = |id: NodeId| order.iter().position(|n| *n == id).unwrap();
        assert!(pos(st3) > pos(st1));
        assert!(pos(st3) > pos(st2));
    }

    #[test]
    fn test_pgt_status_as_str_and_from_str_roundtrip() {
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
    fn test_pgt_status_from_str_unknown_returns_error() {
        let result = StStatus::from_str("UNKNOWN");
        assert!(result.is_err());
        if let Err(PgTrickleError::InvalidArgument(msg)) = result {
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
        if let Err(PgTrickleError::InvalidArgument(msg)) = result {
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
        dag.add_st_node(DagNode {
            id: st_downstream,
            schedule: None,
            effective_schedule: Duration::ZERO,
            name: "st_downstream".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st_fast,
            schedule: Some(Duration::from_secs(30)),
            effective_schedule: Duration::from_secs(30),
            name: "st_fast".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
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
    fn test_default_trait_for_st_dag() {
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
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);
        let st3 = NodeId::StreamTable(3);

        for (id, name) in [(st1, "st1"), (st2, "st2"), (st3, "st3")] {
            dag.add_st_node(DagNode {
                id,
                schedule: Some(Duration::from_secs(60)),
                effective_schedule: Duration::from_secs(60),
                name: name.to_string(),
                status: StStatus::Active,
                schedule_raw: None,
            });
        }

        dag.add_edge(st1, st2);
        dag.add_edge(st2, st3);
        dag.add_edge(st3, st1);

        let result = dag.detect_cycles();
        assert!(result.is_err());
        if let Err(PgTrickleError::CycleDetected(nodes)) = result {
            assert_eq!(nodes.len(), 3);
        }
    }

    #[test]
    fn test_topological_order_excludes_base_tables() {
        let mut dag = StDag::new();
        let base1 = NodeId::BaseTable(1);
        let base2 = NodeId::BaseTable(2);
        let st1 = NodeId::StreamTable(1);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base1, st1);
        dag.add_edge(base2, st1);

        let order = dag.topological_order().unwrap();
        assert_eq!(order, vec![st1]); // No base tables in output
    }

    #[test]
    fn test_explicit_schedule_overrides_downstream_resolution() {
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        // st1 has explicit schedule of 60s, st2 has explicit schedule of 120s
        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::ZERO, // will be set to 60
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(120)),
            effective_schedule: Duration::ZERO, // will be set to 120
            name: "st2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st1);
        dag.add_edge(st1, st2);

        dag.resolve_calculated_schedule(30);

        assert_eq!(
            dag.nodes.get(&st1).unwrap().effective_schedule,
            Duration::from_secs(60)
        );
        assert_eq!(
            dag.nodes.get(&st2).unwrap().effective_schedule,
            Duration::from_secs(120)
        );
    }

    #[test]
    fn test_resolve_downstream_schedule_chain_three_levels() {
        // base -> st1 (downstream) -> st2 (downstream) -> st3 (60s)
        let mut dag = StDag::new();
        let base = NodeId::BaseTable(1);
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);
        let st3 = NodeId::StreamTable(3);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: None, // downstream
            effective_schedule: Duration::ZERO,
            name: "st1".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: None, // downstream
            effective_schedule: Duration::ZERO,
            name: "st2".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st3,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: "st3".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(base, st1);
        dag.add_edge(st1, st2);
        dag.add_edge(st2, st3);

        dag.resolve_calculated_schedule(300);

        // st2 gets min of st3 = 60s, st1 gets min of st2 = 60s
        assert_eq!(
            dag.nodes.get(&st1).unwrap().effective_schedule,
            Duration::from_secs(60)
        );
        assert_eq!(
            dag.nodes.get(&st2).unwrap().effective_schedule,
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
        let st1 = NodeId::StreamTable(1);
        let st2 = NodeId::StreamTable(2);

        dag.add_st_node(DagNode {
            id: st1,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::ZERO,
            name: "my_view_a".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });
        dag.add_st_node(DagNode {
            id: st2,
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::ZERO,
            name: "my_view_b".to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        });

        dag.add_edge(st1, st2);
        dag.add_edge(st2, st1); // cycle!

        let err = dag.detect_cycles().unwrap_err();
        let msg = format!("{}", err);
        // Error message should reference the named nodes
        assert!(
            msg.contains("my_view_a") || msg.contains("my_view_b"),
            "cycle error should contain node names: {}",
            msg
        );
    }

    // ── Diamond detection tests ─────────────────────────────────────────

    /// Helper: create a simple DagNode for tests.
    fn make_st(id: i64, name: &str) -> DagNode {
        DagNode {
            id: NodeId::StreamTable(id),
            schedule: Some(Duration::from_secs(60)),
            effective_schedule: Duration::from_secs(60),
            name: name.to_string(),
            status: StStatus::Active,
            schedule_raw: None,
        }
    }

    #[test]
    fn test_detect_diamonds_simple() {
        // A (base) → B (ST) → D (ST)
        // A (base) → C (ST) → D (ST)
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);
        let d = NodeId::StreamTable(3);

        dag.add_st_node(make_st(1, "B"));
        dag.add_st_node(make_st(2, "C"));
        dag.add_st_node(make_st(3, "D"));

        dag.add_edge(a, b);
        dag.add_edge(a, c);
        dag.add_edge(b, d);
        dag.add_edge(c, d);

        let diamonds = dag.detect_diamonds();
        assert_eq!(diamonds.len(), 1, "expected one diamond, got {diamonds:?}");
        assert_eq!(diamonds[0].convergence, d);
        assert!(
            diamonds[0].shared_sources.contains(&a),
            "shared sources should contain A"
        );
        let int_set: HashSet<NodeId> = diamonds[0].intermediates.iter().copied().collect();
        assert!(int_set.contains(&b), "intermediates should contain B");
        assert!(int_set.contains(&c), "intermediates should contain C");
    }

    #[test]
    fn test_detect_diamonds_deep() {
        // A (base) → B → E → D
        // A (base) → C → D
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);
        let d = NodeId::StreamTable(3);
        let e = NodeId::StreamTable(4);

        dag.add_st_node(make_st(1, "B"));
        dag.add_st_node(make_st(2, "C"));
        dag.add_st_node(make_st(3, "D"));
        dag.add_st_node(make_st(4, "E"));

        dag.add_edge(a, b);
        dag.add_edge(b, e);
        dag.add_edge(e, d);
        dag.add_edge(a, c);
        dag.add_edge(c, d);

        let diamonds = dag.detect_diamonds();
        assert_eq!(diamonds.len(), 1, "expected one diamond, got {diamonds:?}");
        assert_eq!(diamonds[0].convergence, d);

        let int_set: HashSet<NodeId> = diamonds[0].intermediates.iter().copied().collect();
        // B, C, and E are all intermediates
        assert!(int_set.contains(&b), "should contain B");
        assert!(int_set.contains(&c), "should contain C");
        assert!(int_set.contains(&e), "should contain E");
    }

    #[test]
    fn test_detect_diamonds_none_linear() {
        // Linear chain: A → B → C (no diamond)
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);

        dag.add_st_node(make_st(1, "B"));
        dag.add_st_node(make_st(2, "C"));

        dag.add_edge(a, b);
        dag.add_edge(b, c);

        let diamonds = dag.detect_diamonds();
        assert!(diamonds.is_empty(), "linear chain should have no diamonds");
    }

    #[test]
    fn test_detect_diamonds_multiple_roots_no_diamond() {
        // B1 (base) → B2 → D
        // C1 (base) → C2 → D
        // Different roots — NOT a diamond (cross-source, not intra-source).
        let mut dag = StDag::new();
        let b1 = NodeId::BaseTable(1);
        let c1 = NodeId::BaseTable(2);
        let b2 = NodeId::StreamTable(1);
        let c2 = NodeId::StreamTable(2);
        let d = NodeId::StreamTable(3);

        dag.add_st_node(make_st(1, "B2"));
        dag.add_st_node(make_st(2, "C2"));
        dag.add_st_node(make_st(3, "D"));

        dag.add_edge(b1, b2);
        dag.add_edge(c1, c2);
        dag.add_edge(b2, d);
        dag.add_edge(c2, d);

        let diamonds = dag.detect_diamonds();
        assert!(
            diamonds.is_empty(),
            "different-root fan-in should not be a diamond"
        );
    }

    #[test]
    fn test_detect_diamonds_overlapping() {
        // A → B → D,  A → C → D   (diamond 1)
        // D → E → G,  D → F → G   (diamond 2, overlapping at D)
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);
        let d = NodeId::StreamTable(3);
        let e = NodeId::StreamTable(4);
        let f = NodeId::StreamTable(5);
        let g = NodeId::StreamTable(6);

        for (id, name) in [(1, "B"), (2, "C"), (3, "D"), (4, "E"), (5, "F"), (6, "G")] {
            dag.add_st_node(make_st(id, name));
        }

        dag.add_edge(a, b);
        dag.add_edge(a, c);
        dag.add_edge(b, d);
        dag.add_edge(c, d);
        dag.add_edge(d, e);
        dag.add_edge(d, f);
        dag.add_edge(e, g);
        dag.add_edge(f, g);

        let diamonds = dag.detect_diamonds();
        // Should detect 2 diamonds (convergence at D and at G).
        // They may or may not be merged depending on intermediate overlap.
        assert!(!diamonds.is_empty(), "should detect at least one diamond");

        // Check that both D and G are convergence or at least one diamond
        // captures both fan-in points.
        let convergence_nodes: HashSet<NodeId> = diamonds.iter().map(|d| d.convergence).collect();
        assert!(
            convergence_nodes.contains(&d) || convergence_nodes.contains(&g),
            "should detect diamond at D or G"
        );
    }

    // ── Consistency group tests ─────────────────────────────────────────

    #[test]
    fn test_groups_simple_diamond() {
        // A → B → D, A → C → D
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);
        let d = NodeId::StreamTable(3);

        dag.add_st_node(make_st(1, "B"));
        dag.add_st_node(make_st(2, "C"));
        dag.add_st_node(make_st(3, "D"));

        dag.add_edge(a, b);
        dag.add_edge(a, c);
        dag.add_edge(b, d);
        dag.add_edge(c, d);

        let groups = dag.compute_consistency_groups();

        // Find the non-singleton group containing D.
        let diamond_group = groups.iter().find(|g| !g.is_singleton());
        assert!(
            diamond_group.is_some(),
            "expected a non-singleton group for the diamond"
        );
        let group = diamond_group.unwrap();

        assert!(group.members.contains(&b), "group should contain B");
        assert!(group.members.contains(&c), "group should contain C");
        assert!(group.members.contains(&d), "group should contain D");
        assert!(
            group.convergence_points.contains(&d),
            "D should be a convergence point"
        );

        // D must be last in topological order within the group.
        assert_eq!(
            *group.members.last().unwrap(),
            d,
            "convergence node D should be last"
        );
    }

    #[test]
    fn test_groups_singleton_non_diamond() {
        // Linear: A → B → C (no diamond)
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);

        dag.add_st_node(make_st(1, "B"));
        dag.add_st_node(make_st(2, "C"));

        dag.add_edge(a, b);
        dag.add_edge(b, c);

        let groups = dag.compute_consistency_groups();
        assert_eq!(
            groups.len(),
            2,
            "each ST should get its own singleton group"
        );
        assert!(groups.iter().all(|g| g.is_singleton()));
    }

    #[test]
    fn test_groups_independent_diamonds() {
        // Diamond 1: A1 → B1 → D1, A1 → C1 → D1
        // Diamond 2: A2 → B2 → D2, A2 → C2 → D2
        let mut dag = StDag::new();
        let a1 = NodeId::BaseTable(1);
        let a2 = NodeId::BaseTable(2);
        let b1 = NodeId::StreamTable(1);
        let c1 = NodeId::StreamTable(2);
        let d1 = NodeId::StreamTable(3);
        let b2 = NodeId::StreamTable(4);
        let c2 = NodeId::StreamTable(5);
        let d2 = NodeId::StreamTable(6);

        for (id, name) in [
            (1, "B1"),
            (2, "C1"),
            (3, "D1"),
            (4, "B2"),
            (5, "C2"),
            (6, "D2"),
        ] {
            dag.add_st_node(make_st(id, name));
        }

        dag.add_edge(a1, b1);
        dag.add_edge(a1, c1);
        dag.add_edge(b1, d1);
        dag.add_edge(c1, d1);

        dag.add_edge(a2, b2);
        dag.add_edge(a2, c2);
        dag.add_edge(b2, d2);
        dag.add_edge(c2, d2);

        let groups = dag.compute_consistency_groups();
        let non_singleton: Vec<_> = groups.iter().filter(|g| !g.is_singleton()).collect();
        assert_eq!(
            non_singleton.len(),
            2,
            "two independent diamonds should produce two non-singleton groups"
        );

        // Each non-singleton group should have 3 members.
        for g in &non_singleton {
            assert_eq!(g.members.len(), 3, "diamond group should have 3 members");
        }
    }

    #[test]
    fn test_groups_nested_merge() {
        // A → B → D, A → C → D (diamond at D)
        // D → E → G, D → F → G (diamond at G)
        // These should merge because D is in both.
        let mut dag = StDag::new();
        let a = NodeId::BaseTable(1);
        let b = NodeId::StreamTable(1);
        let c = NodeId::StreamTable(2);
        let d = NodeId::StreamTable(3);
        let e = NodeId::StreamTable(4);
        let f = NodeId::StreamTable(5);
        let g = NodeId::StreamTable(6);

        for (id, name) in [(1, "B"), (2, "C"), (3, "D"), (4, "E"), (5, "F"), (6, "G")] {
            dag.add_st_node(make_st(id, name));
        }

        dag.add_edge(a, b);
        dag.add_edge(a, c);
        dag.add_edge(b, d);
        dag.add_edge(c, d);
        dag.add_edge(d, e);
        dag.add_edge(d, f);
        dag.add_edge(e, g);
        dag.add_edge(f, g);

        let groups = dag.compute_consistency_groups();
        let non_singleton: Vec<_> = groups.iter().filter(|g| !g.is_singleton()).collect();

        // D is shared between both diamonds, so groups should be merged.
        // Total non-singleton group(s) should contain B, C, D, E, F, G.
        let all_members: HashSet<NodeId> = non_singleton
            .iter()
            .flat_map(|g| g.members.iter().copied())
            .collect();

        assert!(all_members.contains(&b));
        assert!(all_members.contains(&c));
        assert!(all_members.contains(&d));
        assert!(all_members.contains(&e));
        assert!(all_members.contains(&f));
        assert!(all_members.contains(&g));
    }

    #[test]
    fn test_consistency_group_epoch_advance() {
        let mut group = ConsistencyGroup {
            members: vec![NodeId::StreamTable(1), NodeId::StreamTable(2)],
            convergence_points: vec![NodeId::StreamTable(2)],
            epoch: 0,
        };

        assert_eq!(group.epoch, 0);
        group.advance_epoch();
        assert_eq!(group.epoch, 1);
        group.advance_epoch();
        assert_eq!(group.epoch, 2);
    }

    #[test]
    fn test_consistency_group_is_singleton() {
        let singleton = ConsistencyGroup {
            members: vec![NodeId::StreamTable(1)],
            convergence_points: vec![],
            epoch: 0,
        };
        assert!(singleton.is_singleton());

        let multi = ConsistencyGroup {
            members: vec![NodeId::StreamTable(1), NodeId::StreamTable(2)],
            convergence_points: vec![NodeId::StreamTable(2)],
            epoch: 0,
        };
        assert!(!multi.is_singleton());
    }
}
