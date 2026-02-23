//! E2E property-based correctness tests for pgstream.
//!
//! **THE KEY INVARIANT** (DBSP §4, Gupta & Mumick 1995 §3):
//!
//! > For every ST, at every data timestamp:
//! >   Contents(ST) = Result(defining_query)   (multiset equality)
//!
//! Each test:
//! 1. Creates source tables with a fixed schema
//! 2. Inserts randomised initial data (deterministic PRNG)
//! 3. Creates a stream table (DIFFERENTIAL or FULL)
//! 4. Verifies the invariant
//! 5. Repeats N cycles of: random DML → refresh → verify invariant
//!
//! Randomisation uses a deterministic SplitMix64 PRNG seeded per test.
//! On failure the seed is printed for reproduction.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Configuration ──────────────────────────────────────────────────────

const INITIAL_ROWS: usize = 15;
const CYCLES: usize = 5;

// ── Deterministic PRNG (SplitMix64) ───────────────────────────────────

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn usize_range(&mut self, min: usize, max: usize) -> usize {
        if min >= max {
            return min;
        }
        min + (self.next_u64() as usize) % (max - min + 1)
    }

    fn i32_range(&mut self, min: i32, max: i32) -> i32 {
        if min >= max {
            return min;
        }
        let span = (max as i64 - min as i64 + 1) as u64;
        min + (self.next_u64() % span) as i32
    }

    fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        assert!(!items.is_empty());
        &items[self.usize_range(0, items.len() - 1)]
    }

    fn gen_alpha(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| (b'a' + (self.next_u64() % 26) as u8) as char)
            .collect()
    }

    fn gen_bool(&mut self) -> bool {
        self.next_u64().is_multiple_of(2)
    }
}

// ── ID tracker for source tables ───────────────────────────────────────

struct TrackedIds {
    next_id: i32,
    live: Vec<i32>,
}

impl TrackedIds {
    fn new() -> Self {
        Self {
            next_id: 1,
            live: Vec::new(),
        }
    }

    /// Allocate the next sequential ID and record it as live.
    fn alloc(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.live.push(id);
        id
    }

    /// Pick a random existing ID (non-destructive).
    fn pick(&self, rng: &mut Rng) -> Option<i32> {
        if self.live.is_empty() {
            None
        } else {
            Some(*rng.choose(&self.live))
        }
    }

    /// Remove and return a random existing ID.
    fn remove_random(&mut self, rng: &mut Rng) -> Option<i32> {
        if self.live.is_empty() {
            return None;
        }
        let idx = rng.usize_range(0, self.live.len() - 1);
        Some(self.live.swap_remove(idx))
    }
}

// ── Invariant assertion ────────────────────────────────────────────────

/// Assert the KEY INVARIANT: ST contents == defining query result.
///
/// Compares only user-visible columns (excludes all `__pgs_*` internal
/// columns) using `EXCEPT ALL` for correct multiset (bag) comparison.
async fn assert_invariant(db: &E2eDb, pgs_name: &str, query: &str, seed: u64, cycle: usize) {
    let dt_table = format!("public.{pgs_name}");

    // User-visible columns (exclude all __pgs_* internal columns)
    let cols: String = db
        .query_scalar(&format!(
            "SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = '{pgs_name}' \
               AND column_name NOT LIKE '__pgs_%'"
        ))
        .await;

    // Multiset equality: symmetric EXCEPT ALL must be empty
    let matches: bool = db
        .query_scalar(&format!(
            "SELECT NOT EXISTS ( \
                (SELECT {cols} FROM {dt_table} EXCEPT ALL ({query})) \
                UNION ALL \
                (({query}) EXCEPT ALL SELECT {cols} FROM {dt_table}) \
            )"
        ))
        .await;

    if !matches {
        let dt_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM {dt_table}"))
            .await;
        let q_count: i64 = db
            .query_scalar(&format!("SELECT count(*) FROM ({query}) _q"))
            .await;
        let extra: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM \
                 (SELECT {cols} FROM {dt_table} EXCEPT ALL ({query})) _x"
            ))
            .await;
        let missing: i64 = db
            .query_scalar(&format!(
                "SELECT count(*) FROM \
                 (({query}) EXCEPT ALL SELECT {cols} FROM {dt_table}) _x"
            ))
            .await;

        panic!(
            "INVARIANT VIOLATED at cycle {} (seed={:#x})\n\
             ST: {}, Query: {}\n\
             ST rows: {}, Query rows: {}\n\
             Extra in ST: {}, Missing from ST: {}",
            cycle, seed, pgs_name, query, dt_count, q_count, extra, missing,
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 1: Simple scan — SELECT all columns
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_scan_differential() {
    let seed: u64 = 0xCAFE_0001;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_s (id INT PRIMARY KEY, val INT, label TEXT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let val = rng.i32_range(-100, 100);
        let label = rng.gen_alpha(4);
        db.execute(&format!(
            "INSERT INTO prop_s VALUES ({id}, {val}, '{label}')"
        ))
        .await;
    }

    let query = "SELECT id, val, label FROM prop_s";
    db.create_dt("prop_s_dt", query, "1m", "DIFFERENTIAL").await;
    assert_invariant(&db, "prop_s_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        // Inserts
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let val = rng.i32_range(-100, 100);
            let label = rng.gen_alpha(4);
            db.execute(&format!(
                "INSERT INTO prop_s VALUES ({id}, {val}, '{label}')"
            ))
            .await;
        }

        // Updates
        let n_upd = rng.usize_range(0, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(-100, 100);
                let label = rng.gen_alpha(4);
                db.execute(&format!(
                    "UPDATE prop_s SET val = {val}, label = '{label}' WHERE id = {id}"
                ))
                .await;
            }
        }

        // Deletes
        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM prop_s WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_s_dt").await;
        assert_invariant(&db, "prop_s_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 2: Filter — rows crossing the predicate boundary
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_filter_differential() {
    let seed: u64 = 0xCAFE_0002;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_f (id INT PRIMARY KEY, score INT, tag TEXT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let score = rng.i32_range(20, 80); // straddles filter boundary (> 50)
        let tag = rng.gen_alpha(3);
        db.execute(&format!(
            "INSERT INTO prop_f VALUES ({id}, {score}, '{tag}')"
        ))
        .await;
    }

    let query = "SELECT id, score, tag FROM prop_f WHERE score > 50";
    db.create_dt("prop_f_dt", query, "1m", "DIFFERENTIAL").await;
    assert_invariant(&db, "prop_f_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let score = rng.i32_range(20, 80);
            let tag = rng.gen_alpha(3);
            db.execute(&format!(
                "INSERT INTO prop_f VALUES ({id}, {score}, '{tag}')"
            ))
            .await;
        }

        // Updates — some will cross the filter boundary
        let n_upd = rng.usize_range(1, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let score = rng.i32_range(20, 80);
                db.execute(&format!(
                    "UPDATE prop_f SET score = {score} WHERE id = {id}"
                ))
                .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM prop_f WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_f_dt").await;
        assert_invariant(&db, "prop_f_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 3: Inner join — DML on both sides
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_join_differential() {
    let seed: u64 = 0xCAFE_0003;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_jl (id INT PRIMARY KEY, val INT, key INT)")
        .await;
    db.execute("CREATE TABLE prop_jr (id INT PRIMARY KEY, val INT, key INT)")
        .await;

    let mut l_ids = TrackedIds::new();
    let mut r_ids = TrackedIds::new();

    for _ in 0..INITIAL_ROWS {
        let id = l_ids.alloc();
        let val = rng.i32_range(1, 100);
        let key = rng.i32_range(1, 5); // limited range → many matches
        db.execute(&format!("INSERT INTO prop_jl VALUES ({id}, {val}, {key})"))
            .await;
    }
    for _ in 0..INITIAL_ROWS {
        let id = r_ids.alloc();
        let val = rng.i32_range(1, 100);
        let key = rng.i32_range(1, 5);
        db.execute(&format!("INSERT INTO prop_jr VALUES ({id}, {val}, {key})"))
            .await;
    }

    let query = "SELECT l.id AS lid, l.val AS lval, r.id AS rid, r.val AS rval \
                 FROM prop_jl l JOIN prop_jr r ON l.key = r.key";
    db.create_dt("prop_j_dt", query, "1m", "DIFFERENTIAL").await;
    assert_invariant(&db, "prop_j_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        // DML on left table
        let n_ins = rng.usize_range(1, 3);
        for _ in 0..n_ins {
            let id = l_ids.alloc();
            let val = rng.i32_range(1, 100);
            let key = rng.i32_range(1, 5);
            db.execute(&format!("INSERT INTO prop_jl VALUES ({id}, {val}, {key})"))
                .await;
        }
        if rng.gen_bool()
            && let Some(id) = l_ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM prop_jl WHERE id = {id}"))
                .await;
        }

        // DML on right table
        let n_ins = rng.usize_range(1, 3);
        for _ in 0..n_ins {
            let id = r_ids.alloc();
            let val = rng.i32_range(1, 100);
            let key = rng.i32_range(1, 5);
            db.execute(&format!("INSERT INTO prop_jr VALUES ({id}, {val}, {key})"))
                .await;
        }
        let n_upd = rng.usize_range(0, 2);
        for _ in 0..n_upd {
            if let Some(id) = r_ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                db.execute(&format!("UPDATE prop_jr SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_j_dt").await;
        assert_invariant(&db, "prop_j_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 4: Aggregate — GROUP BY with COUNT + SUM
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_aggregate_differential() {
    let seed: u64 = 0xCAFE_0004;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_agg (id INT PRIMARY KEY, region TEXT, amount INT)")
        .await;

    let regions = ["north", "south", "east", "west"];
    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let region = rng.choose(&regions);
        let amount = rng.i32_range(10, 500);
        db.execute(&format!(
            "INSERT INTO prop_agg VALUES ({id}, '{region}', {amount})"
        ))
        .await;
    }

    let query = "SELECT region, COUNT(*) AS cnt, SUM(amount) AS total \
                 FROM prop_agg GROUP BY region";
    db.create_dt("prop_agg_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_agg_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let region = rng.choose(&regions);
            let amount = rng.i32_range(10, 500);
            db.execute(&format!(
                "INSERT INTO prop_agg VALUES ({id}, '{region}', {amount})"
            ))
            .await;
        }

        let n_upd = rng.usize_range(0, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let amount = rng.i32_range(10, 500);
                db.execute(&format!(
                    "UPDATE prop_agg SET amount = {amount} WHERE id = {id}"
                ))
                .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM prop_agg WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_agg_dt").await;
        assert_invariant(&db, "prop_agg_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 5: DISTINCT — duplicate-aware multiset tracking
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_distinct_differential() {
    let seed: u64 = 0xCAFE_0005;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_dist (id INT PRIMARY KEY, color TEXT, size TEXT)")
        .await;

    let colors = ["red", "blue", "green"];
    let sizes = ["s", "m", "l"];
    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let color = rng.choose(&colors);
        let size = rng.choose(&sizes);
        db.execute(&format!(
            "INSERT INTO prop_dist VALUES ({id}, '{color}', '{size}')"
        ))
        .await;
    }

    let query = "SELECT DISTINCT color, size FROM prop_dist";
    db.create_dt("prop_dist_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_dist_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 4);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let color = rng.choose(&colors);
            let size = rng.choose(&sizes);
            db.execute(&format!(
                "INSERT INTO prop_dist VALUES ({id}, '{color}', '{size}')"
            ))
            .await;
        }

        let n_upd = rng.usize_range(0, 2);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let color = rng.choose(&colors);
                let size = rng.choose(&sizes);
                db.execute(&format!(
                    "UPDATE prop_dist SET color = '{color}', size = '{size}' WHERE id = {id}"
                ))
                .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM prop_dist WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_dist_dt").await;
        assert_invariant(&db, "prop_dist_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 6: LEFT JOIN — NULL padding when right side has no match
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_left_join_differential() {
    let seed: u64 = 0xCAFE_0006;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_ljl (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE prop_ljr (id INT PRIMARY KEY, left_id INT, detail TEXT)")
        .await;

    let mut l_ids = TrackedIds::new();
    let mut r_ids = TrackedIds::new();

    for _ in 0..INITIAL_ROWS {
        let id = l_ids.alloc();
        let val = rng.i32_range(1, 100);
        db.execute(&format!("INSERT INTO prop_ljl VALUES ({id}, {val})"))
            .await;
    }
    // Right side: some match existing left IDs, some don't
    for _ in 0..10 {
        let id = r_ids.alloc();
        let left_id = rng.i32_range(1, INITIAL_ROWS as i32 + 5);
        let detail = rng.gen_alpha(3);
        db.execute(&format!(
            "INSERT INTO prop_ljr VALUES ({id}, {left_id}, '{detail}')"
        ))
        .await;
    }

    let query = "SELECT l.id AS lid, l.val, r.detail \
                 FROM prop_ljl l LEFT JOIN prop_ljr r ON l.id = r.left_id";
    db.create_dt("prop_lj_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_lj_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        // DML on left
        let n_ins = rng.usize_range(1, 3);
        for _ in 0..n_ins {
            let id = l_ids.alloc();
            let val = rng.i32_range(1, 100);
            db.execute(&format!("INSERT INTO prop_ljl VALUES ({id}, {val})"))
                .await;
        }
        if rng.gen_bool()
            && let Some(id) = l_ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM prop_ljl WHERE id = {id}"))
                .await;
        }

        // DML on right — some left_ids valid, some not
        let n_ins = rng.usize_range(1, 3);
        for _ in 0..n_ins {
            let id = r_ids.alloc();
            let left_id = rng.i32_range(1, l_ids.next_id + 3);
            let detail = rng.gen_alpha(3);
            db.execute(&format!(
                "INSERT INTO prop_ljr VALUES ({id}, {left_id}, '{detail}')"
            ))
            .await;
        }
        if rng.gen_bool()
            && let Some(id) = r_ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM prop_ljr WHERE id = {id}"))
                .await;
        }

        db.refresh_dt("prop_lj_dt").await;
        assert_invariant(&db, "prop_lj_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 7: UNION ALL — DML on both branches
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_union_all_differential() {
    let seed: u64 = 0xCAFE_0007;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_ua1 (id INT PRIMARY KEY, val INT)")
        .await;
    db.execute("CREATE TABLE prop_ua2 (id INT PRIMARY KEY, val INT)")
        .await;

    let mut ids1 = TrackedIds::new();
    let mut ids2 = TrackedIds::new();

    for _ in 0..10 {
        let id = ids1.alloc();
        let val = rng.i32_range(1, 100);
        db.execute(&format!("INSERT INTO prop_ua1 VALUES ({id}, {val})"))
            .await;
    }
    for _ in 0..10 {
        let id = ids2.alloc();
        let val = rng.i32_range(1, 100);
        db.execute(&format!("INSERT INTO prop_ua2 VALUES ({id}, {val})"))
            .await;
    }

    let query = "SELECT id, val FROM prop_ua1 UNION ALL SELECT id, val FROM prop_ua2";
    db.create_dt("prop_ua_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_ua_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        // DML on source 1
        let n_ins = rng.usize_range(1, 3);
        for _ in 0..n_ins {
            let id = ids1.alloc();
            let val = rng.i32_range(1, 100);
            db.execute(&format!("INSERT INTO prop_ua1 VALUES ({id}, {val})"))
                .await;
        }
        if rng.gen_bool()
            && let Some(id) = ids1.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM prop_ua1 WHERE id = {id}"))
                .await;
        }

        // DML on source 2
        let n_ins = rng.usize_range(1, 3);
        for _ in 0..n_ins {
            let id = ids2.alloc();
            let val = rng.i32_range(1, 100);
            db.execute(&format!("INSERT INTO prop_ua2 VALUES ({id}, {val})"))
                .await;
        }
        let n_upd = rng.usize_range(0, 2);
        for _ in 0..n_upd {
            if let Some(id) = ids2.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                db.execute(&format!("UPDATE prop_ua2 SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_ua_dt").await;
        assert_invariant(&db, "prop_ua_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 8: Filter + Aggregate combined — rows crossing filter boundary
//         change group membership and aggregated values
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_filter_aggregate_differential() {
    let seed: u64 = 0xCAFE_0008;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_fa (id INT PRIMARY KEY, grp INT, val INT)")
        .await;

    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let grp = rng.i32_range(1, 4);
        let val = rng.i32_range(-50, 100); // some negative → filtered out
        db.execute(&format!("INSERT INTO prop_fa VALUES ({id}, {grp}, {val})"))
            .await;
    }

    let query = "SELECT grp, COUNT(*) AS cnt, SUM(val) AS total \
                 FROM prop_fa WHERE val > 0 GROUP BY grp";
    db.create_dt("prop_fa_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_fa_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let grp = rng.i32_range(1, 4);
            let val = rng.i32_range(-50, 100);
            db.execute(&format!("INSERT INTO prop_fa VALUES ({id}, {grp}, {val})"))
                .await;
        }

        // Updates — val may cross the filter boundary
        let n_upd = rng.usize_range(1, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(-50, 100);
                db.execute(&format!("UPDATE prop_fa SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM prop_fa WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("prop_fa_dt").await;
        assert_invariant(&db, "prop_fa_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 9: Join + Aggregate — orders joined to customers, grouped by region
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_join_aggregate_differential() {
    let seed: u64 = 0xCAFE_0009;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_ja_ord (id INT PRIMARY KEY, cust_id INT, amount INT)")
        .await;
    db.execute("CREATE TABLE prop_ja_cust (id INT PRIMARY KEY, region TEXT)")
        .await;

    let regions = ["north", "south", "east", "west"];

    // Create customers first
    let mut c_ids = TrackedIds::new();
    for _ in 0..5 {
        let id = c_ids.alloc();
        let region = rng.choose(&regions);
        db.execute(&format!(
            "INSERT INTO prop_ja_cust VALUES ({id}, '{region}')"
        ))
        .await;
    }

    // Create orders referencing customers
    let mut o_ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = o_ids.alloc();
        let cust_id = rng.i32_range(1, c_ids.next_id - 1);
        let amount = rng.i32_range(10, 500);
        db.execute(&format!(
            "INSERT INTO prop_ja_ord VALUES ({id}, {cust_id}, {amount})"
        ))
        .await;
    }

    let query = "SELECT c.region, COUNT(*) AS cnt, SUM(o.amount) AS total \
                 FROM prop_ja_ord o JOIN prop_ja_cust c ON o.cust_id = c.id \
                 GROUP BY c.region";
    db.create_dt("prop_ja_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_ja_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        // New orders
        let n_ins = rng.usize_range(2, 4);
        for _ in 0..n_ins {
            let id = o_ids.alloc();
            let cust_id = rng.i32_range(1, c_ids.next_id - 1);
            let amount = rng.i32_range(10, 500);
            db.execute(&format!(
                "INSERT INTO prop_ja_ord VALUES ({id}, {cust_id}, {amount})"
            ))
            .await;
        }

        // Update order amounts
        let n_upd = rng.usize_range(0, 2);
        for _ in 0..n_upd {
            if let Some(id) = o_ids.pick(&mut rng) {
                let amount = rng.i32_range(10, 500);
                db.execute(&format!(
                    "UPDATE prop_ja_ord SET amount = {amount} WHERE id = {id}"
                ))
                .await;
            }
        }

        // Delete some orders
        if rng.gen_bool()
            && let Some(id) = o_ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM prop_ja_ord WHERE id = {id}"))
                .await;
        }

        // Add a new customer midway through
        if cycle == 3 {
            let id = c_ids.alloc();
            let region = rng.choose(&regions);
            db.execute(&format!(
                "INSERT INTO prop_ja_cust VALUES ({id}, '{region}')"
            ))
            .await;
        }

        db.refresh_dt("prop_ja_dt").await;
        assert_invariant(&db, "prop_ja_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 10: CTE + filter + aggregate — WITH clause inlined by DVM
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_cte_differential() {
    let seed: u64 = 0xCAFE_000A;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_cte (id INT PRIMARY KEY, grp TEXT, val INT)")
        .await;

    let groups = ["x", "y", "z"];
    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let grp = rng.choose(&groups);
        let val = rng.i32_range(-30, 100);
        db.execute(&format!(
            "INSERT INTO prop_cte VALUES ({id}, '{grp}', {val})"
        ))
        .await;
    }

    let query = "WITH positive AS (SELECT id, grp, val FROM prop_cte WHERE val > 0) \
                 SELECT grp, COUNT(*) AS cnt, SUM(val) AS total \
                 FROM positive GROUP BY grp";
    db.create_dt("prop_cte_dt", query, "1m", "DIFFERENTIAL")
        .await;
    assert_invariant(&db, "prop_cte_dt", query, seed, 0).await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let grp = rng.choose(&groups);
            let val = rng.i32_range(-30, 100);
            db.execute(&format!(
                "INSERT INTO prop_cte VALUES ({id}, '{grp}', {val})"
            ))
            .await;
        }

        let n_upd = rng.usize_range(1, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(-30, 100);
                db.execute(&format!("UPDATE prop_cte SET val = {val} WHERE id = {id}"))
                    .await;
            }
        }

        if rng.gen_bool()
            && let Some(id) = ids.remove_random(&mut rng)
        {
            db.execute(&format!("DELETE FROM prop_cte WHERE id = {id}"))
                .await;
        }

        db.refresh_dt("prop_cte_dt").await;
        assert_invariant(&db, "prop_cte_dt", query, seed, cycle).await;
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Test 11: FULL refresh mode — validates the invariant with full
//          recomputation across scan, filter, and aggregate patterns
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_property_full_mode() {
    let seed: u64 = 0xCAFE_000B;
    let mut rng = Rng::new(seed);
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE prop_full (id INT PRIMARY KEY, grp TEXT, val INT)")
        .await;

    let groups = ["alpha", "beta", "gamma"];
    let mut ids = TrackedIds::new();
    for _ in 0..INITIAL_ROWS {
        let id = ids.alloc();
        let grp = rng.choose(&groups);
        let val = rng.i32_range(1, 100);
        db.execute(&format!(
            "INSERT INTO prop_full VALUES ({id}, '{grp}', {val})"
        ))
        .await;
    }

    // Three STs with different operator patterns, all FULL mode
    let q_scan = "SELECT id, grp, val FROM prop_full";
    let q_agg = "SELECT grp, COUNT(*) AS cnt, SUM(val) AS total \
                 FROM prop_full GROUP BY grp";
    let q_filt = "SELECT id, val FROM prop_full WHERE val > 50";

    db.create_dt("propf_scan", q_scan, "1m", "FULL").await;
    db.create_dt("propf_agg", q_agg, "1m", "FULL").await;
    db.create_dt("propf_filt", q_filt, "1m", "FULL").await;

    assert_invariant(&db, "propf_scan", q_scan, seed, 0).await;
    assert_invariant(&db, "propf_agg", q_agg, seed, 0).await;
    assert_invariant(&db, "propf_filt", q_filt, seed, 0).await;

    for cycle in 1..=CYCLES {
        let n_ins = rng.usize_range(2, 5);
        for _ in 0..n_ins {
            let id = ids.alloc();
            let grp = rng.choose(&groups);
            let val = rng.i32_range(1, 100);
            db.execute(&format!(
                "INSERT INTO prop_full VALUES ({id}, '{grp}', {val})"
            ))
            .await;
        }

        let n_upd = rng.usize_range(1, 3);
        for _ in 0..n_upd {
            if let Some(id) = ids.pick(&mut rng) {
                let val = rng.i32_range(1, 100);
                let grp = rng.choose(&groups);
                db.execute(&format!(
                    "UPDATE prop_full SET val = {val}, grp = '{grp}' WHERE id = {id}"
                ))
                .await;
            }
        }

        let n_del = rng.usize_range(0, 2);
        for _ in 0..n_del {
            if let Some(id) = ids.remove_random(&mut rng) {
                db.execute(&format!("DELETE FROM prop_full WHERE id = {id}"))
                    .await;
            }
        }

        db.refresh_dt("propf_scan").await;
        db.refresh_dt("propf_agg").await;
        db.refresh_dt("propf_filt").await;

        assert_invariant(&db, "propf_scan", q_scan, seed, cycle).await;
        assert_invariant(&db, "propf_agg", q_agg, seed, cycle).await;
        assert_invariant(&db, "propf_filt", q_filt, seed, cycle).await;
    }
}
