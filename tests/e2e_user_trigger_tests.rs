//! E2E tests for user-defined triggers on stream tables.
//!
//! Validates that DIFFERENTIAL refresh fires triggers with correct TG_OP,
//! OLD, and NEW values via the explicit DML path, and that FULL refresh
//! correctly suppresses user row-level triggers.
//!
//! Prerequisites: `./tests/build_e2e_image.sh`

mod e2e;

use e2e::E2eDb;

// ── Helper: create an audit trigger setup ──────────────────────────────
//
// Creates an audit log table and an AFTER trigger on the given stream table
// that records TG_OP, OLD, and NEW values for each fired row-level event.

/// SQL to create the audit infrastructure on a stream table.
/// The stream table must have columns (id INT, val TEXT).
fn audit_trigger_sql(st_name: &str) -> Vec<String> {
    vec![
        // Audit log table
        "CREATE TABLE audit_log (
            audit_id SERIAL PRIMARY KEY,
            op TEXT NOT NULL,
            old_id INT,
            old_val TEXT,
            new_id INT,
            new_val TEXT,
            fired_at TIMESTAMPTZ DEFAULT now()
        )"
        .to_string(),
        // Trigger function
        "CREATE OR REPLACE FUNCTION audit_trigger_fn()
        RETURNS TRIGGER AS $$
        BEGIN
            IF TG_OP = 'INSERT' THEN
                INSERT INTO audit_log (op, new_id, new_val)
                VALUES ('INSERT', NEW.id, NEW.val);
                RETURN NEW;
            ELSIF TG_OP = 'UPDATE' THEN
                INSERT INTO audit_log (op, old_id, old_val, new_id, new_val)
                VALUES ('UPDATE', OLD.id, OLD.val, NEW.id, NEW.val);
                RETURN NEW;
            ELSIF TG_OP = 'DELETE' THEN
                INSERT INTO audit_log (op, old_id, old_val)
                VALUES ('DELETE', OLD.id, OLD.val);
                RETURN OLD;
            END IF;
            RETURN NULL;
        END;
        $$ LANGUAGE plpgsql"
            .to_string(),
        // Attach trigger to stream table
        format!(
            "CREATE TRIGGER audit_trig
            AFTER INSERT OR UPDATE OR DELETE ON {st_name}
            FOR EACH ROW EXECUTE FUNCTION audit_trigger_fn()"
        ),
    ]
}

// ── Explicit DML: INSERT trigger ───────────────────────────────────────

#[tokio::test]
async fn test_explicit_dml_insert() {
    let db = E2eDb::new().await.with_extension().await;

    // Source table
    db.execute("CREATE TABLE src_ins (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_ins VALUES (1, 'a')").await;

    // Create stream table (DIFFERENTIAL)
    db.create_dt(
        "st_ins",
        "SELECT id, val FROM src_ins",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Attach audit trigger on the stream table
    for sql in audit_trigger_sql("st_ins") {
        db.execute(&sql).await;
    }

    // Clear audit log (the CREATE + initial refresh may have fired triggers
    // before we attached them, but we attached after create, so log is clean)
    db.execute("TRUNCATE audit_log").await;

    // Insert a new source row and refresh
    db.execute("INSERT INTO src_ins VALUES (2, 'b')").await;
    db.refresh_dt("st_ins").await;

    // Verify the ST has both rows
    let st_count: i64 = db.count("st_ins").await;
    assert_eq!(st_count, 2, "ST should have 2 rows after refresh");

    // Verify audit log captured an INSERT for the new row
    let insert_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'INSERT'")
        .await;
    assert!(
        insert_count >= 1,
        "Audit log should have at least 1 INSERT entry, got {}",
        insert_count
    );

    // Verify the inserted row details
    let new_id: i32 = db
        .query_scalar("SELECT new_id FROM audit_log WHERE op = 'INSERT' AND new_id = 2 LIMIT 1")
        .await;
    assert_eq!(new_id, 2, "Audit should record new_id = 2");

    let new_val: String = db
        .query_scalar("SELECT new_val FROM audit_log WHERE op = 'INSERT' AND new_id = 2 LIMIT 1")
        .await;
    assert_eq!(new_val, "b", "Audit should record new_val = 'b'");
}

// ── Explicit DML: UPDATE trigger ───────────────────────────────────────

#[tokio::test]
async fn test_explicit_dml_update() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_upd (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_upd VALUES (1, 'old')").await;

    db.create_dt(
        "st_upd",
        "SELECT id, val FROM src_upd",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh to populate
    db.refresh_dt("st_upd").await;

    // Attach audit trigger and clear log
    for sql in audit_trigger_sql("st_upd") {
        db.execute(&sql).await;
    }
    db.execute("TRUNCATE audit_log").await;

    // Update source and refresh
    db.execute("UPDATE src_upd SET val = 'new' WHERE id = 1")
        .await;
    db.refresh_dt("st_upd").await;

    // ST should still have 1 row with updated value
    let val: String = db.query_scalar("SELECT val FROM st_upd WHERE id = 1").await;
    assert_eq!(val, "new", "ST should have updated value");

    // Audit should show an UPDATE with correct OLD and NEW
    let update_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'UPDATE'")
        .await;
    assert!(
        update_count >= 1,
        "Audit log should have at least 1 UPDATE entry, got {}",
        update_count
    );

    let old_val: String = db
        .query_scalar("SELECT old_val FROM audit_log WHERE op = 'UPDATE' AND old_id = 1 LIMIT 1")
        .await;
    assert_eq!(old_val, "old", "OLD.val should be 'old'");

    let new_val: String = db
        .query_scalar("SELECT new_val FROM audit_log WHERE op = 'UPDATE' AND new_id = 1 LIMIT 1")
        .await;
    assert_eq!(new_val, "new", "NEW.val should be 'new'");
}

// ── Explicit DML: DELETE trigger ───────────────────────────────────────

#[tokio::test]
async fn test_explicit_dml_delete() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_del (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_del VALUES (1, 'bye')").await;

    db.create_dt(
        "st_del",
        "SELECT id, val FROM src_del",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh
    db.refresh_dt("st_del").await;

    // Attach audit trigger and clear
    for sql in audit_trigger_sql("st_del") {
        db.execute(&sql).await;
    }
    db.execute("TRUNCATE audit_log").await;

    // Delete source row and refresh
    db.execute("DELETE FROM src_del WHERE id = 1").await;
    db.refresh_dt("st_del").await;

    // ST should be empty
    let st_count: i64 = db.count("st_del").await;
    assert_eq!(st_count, 0, "ST should be empty after source row deleted");

    // Audit should show DELETE with correct OLD
    let delete_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'DELETE'")
        .await;
    assert!(
        delete_count >= 1,
        "Audit log should have at least 1 DELETE entry, got {}",
        delete_count
    );

    let old_id: i32 = db
        .query_scalar("SELECT old_id FROM audit_log WHERE op = 'DELETE' LIMIT 1")
        .await;
    assert_eq!(old_id, 1, "OLD.id should be 1");

    let old_val: String = db
        .query_scalar("SELECT old_val FROM audit_log WHERE op = 'DELETE' LIMIT 1")
        .await;
    assert_eq!(old_val, "bye", "OLD.val should be 'bye'");
}

// ── Explicit DML: no-op skip via IS DISTINCT FROM ──────────────────────

#[tokio::test]
async fn test_explicit_dml_no_op_skip() {
    let db = E2eDb::new().await.with_extension().await;

    // Use an aggregate ST so that source row changes may not change the aggregate.
    db.execute("CREATE TABLE src_agg (id INT PRIMARY KEY, grp TEXT, amount INT)")
        .await;
    db.execute("INSERT INTO src_agg VALUES (1, 'A', 10), (2, 'A', 20)")
        .await;

    db.create_dt(
        "st_agg",
        "SELECT grp, sum(amount) AS total FROM src_agg GROUP BY grp",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh
    db.refresh_dt("st_agg").await;

    // Attach audit trigger on the aggregate ST
    // The ST has columns (grp TEXT, total BIGINT) — adapt the trigger
    db.execute(
        "CREATE TABLE audit_agg (
            audit_id SERIAL PRIMARY KEY,
            op TEXT NOT NULL,
            old_grp TEXT,
            old_total BIGINT,
            new_grp TEXT,
            new_total BIGINT
        )",
    )
    .await;
    db.execute(
        "CREATE OR REPLACE FUNCTION audit_agg_fn()
        RETURNS TRIGGER AS $$
        BEGIN
            IF TG_OP = 'INSERT' THEN
                INSERT INTO audit_agg (op, new_grp, new_total)
                VALUES ('INSERT', NEW.grp, NEW.total);
                RETURN NEW;
            ELSIF TG_OP = 'UPDATE' THEN
                INSERT INTO audit_agg (op, old_grp, old_total, new_grp, new_total)
                VALUES ('UPDATE', OLD.grp, OLD.total, NEW.grp, NEW.total);
                RETURN NEW;
            ELSIF TG_OP = 'DELETE' THEN
                INSERT INTO audit_agg (op, old_grp, old_total)
                VALUES ('DELETE', OLD.grp, OLD.total);
                RETURN OLD;
            END IF;
            RETURN NULL;
        END;
        $$ LANGUAGE plpgsql",
    )
    .await;
    db.execute(
        "CREATE TRIGGER audit_agg_trig
        AFTER INSERT OR UPDATE OR DELETE ON st_agg
        FOR EACH ROW EXECUTE FUNCTION audit_agg_fn()",
    )
    .await;
    db.execute("TRUNCATE audit_agg").await;

    // Update source such that the aggregate sum does NOT change for group A:
    // Change (1, 'A', 10) → (1, 'A', 15) and (2, 'A', 20) → (2, 'A', 15)
    // Total stays 30.
    db.execute("UPDATE src_agg SET amount = 15 WHERE id = 1")
        .await;
    db.execute("UPDATE src_agg SET amount = 15 WHERE id = 2")
        .await;
    db.refresh_dt("st_agg").await;

    // The IS DISTINCT FROM guard should have prevented an UPDATE trigger
    // because the aggregate result (grp='A', total=30) is unchanged.
    let audit_count: i64 = db.count("audit_agg").await;
    assert_eq!(
        audit_count, 0,
        "No triggers should fire when aggregate result is unchanged, got {} entries",
        audit_count
    );
}

// ── No trigger → MERGE strategy ────────────────────────────────────────

#[tokio::test]
async fn test_no_trigger_uses_merge() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_merge (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_merge VALUES (1, 'x')").await;

    // Create ST without any user triggers
    db.create_dt(
        "st_merge",
        "SELECT id, val FROM src_merge",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Insert new row
    db.execute("INSERT INTO src_merge VALUES (2, 'y')").await;
    db.refresh_dt("st_merge").await;

    // Verify data is correct (MERGE path should work normally)
    let count: i64 = db.count("st_merge").await;
    assert_eq!(count, 2, "ST should have 2 rows after MERGE refresh");

    db.assert_dt_matches_query("st_merge", "SELECT id, val FROM src_merge")
        .await;
}

// ── Audit trail: full scenario ─────────────────────────────────────────

#[tokio::test]
async fn test_trigger_audit_trail() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_audit (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_audit VALUES (1, 'first'), (2, 'second')")
        .await;

    db.create_dt(
        "st_audit",
        "SELECT id, val FROM src_audit",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh
    db.refresh_dt("st_audit").await;

    // Attach audit trigger
    for sql in audit_trigger_sql("st_audit") {
        db.execute(&sql).await;
    }
    db.execute("TRUNCATE audit_log").await;

    // Perform mixed DML on source: insert + update + delete
    db.execute("INSERT INTO src_audit VALUES (3, 'third')")
        .await;
    db.execute("UPDATE src_audit SET val = 'updated' WHERE id = 1")
        .await;
    db.execute("DELETE FROM src_audit WHERE id = 2").await;
    db.refresh_dt("st_audit").await;

    // Verify ST matches expected state
    db.assert_dt_matches_query("st_audit", "SELECT id, val FROM src_audit")
        .await;

    // Verify audit trail: should have INSERT(3), UPDATE(1), DELETE(2)
    let insert_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'INSERT' AND new_id = 3")
        .await;
    assert!(insert_count >= 1, "Should have INSERT for id=3");

    let update_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'UPDATE' AND old_id = 1")
        .await;
    assert!(update_count >= 1, "Should have UPDATE for id=1");

    let delete_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'DELETE' AND old_id = 2")
        .await;
    assert!(delete_count >= 1, "Should have DELETE for id=2");
}

// ── GUC: off suppresses triggers ───────────────────────────────────────

#[tokio::test]
async fn test_guc_off_suppresses_triggers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_guc_off (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_guc_off VALUES (1, 'a')").await;

    db.create_dt(
        "st_guc_off",
        "SELECT id, val FROM src_guc_off",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh
    db.refresh_dt("st_guc_off").await;

    // Attach trigger
    for sql in audit_trigger_sql("st_guc_off") {
        db.execute(&sql).await;
    }
    db.execute("TRUNCATE audit_log").await;

    // Set GUC to 'off' — should suppress triggers (use MERGE path)
    db.execute("SET pg_stream.user_triggers = 'off'").await;

    // Modify source and refresh
    db.execute("INSERT INTO src_guc_off VALUES (2, 'b')").await;
    db.refresh_dt("st_guc_off").await;

    // ST should have the data
    let count: i64 = db.count("st_guc_off").await;
    assert_eq!(count, 2, "ST should have 2 rows");

    // Audit log should be EMPTY — triggers were suppressed
    let audit_count: i64 = db.count("audit_log").await;
    assert_eq!(
        audit_count, 0,
        "Audit log should be empty when GUC is 'off', got {} entries",
        audit_count
    );
}

// ── GUC: auto detects triggers ─────────────────────────────────────────

#[tokio::test]
async fn test_guc_auto_detects_triggers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_guc_auto (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_guc_auto VALUES (1, 'a')").await;

    db.create_dt(
        "st_guc_auto",
        "SELECT id, val FROM src_guc_auto",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh
    db.refresh_dt("st_guc_auto").await;

    // GUC should default to 'auto'
    let guc_val: String = db.query_scalar("SHOW pg_stream.user_triggers").await;
    assert_eq!(guc_val, "auto", "Default GUC should be 'auto'");

    // Attach trigger — auto mode should detect it
    for sql in audit_trigger_sql("st_guc_auto") {
        db.execute(&sql).await;
    }
    db.execute("TRUNCATE audit_log").await;

    // Modify source and refresh
    db.execute("INSERT INTO src_guc_auto VALUES (2, 'b')").await;
    db.refresh_dt("st_guc_auto").await;

    // Auto mode should have detected the trigger and used explicit DML
    let insert_count: i64 = db
        .query_scalar("SELECT count(*) FROM audit_log WHERE op = 'INSERT'")
        .await;
    assert!(
        insert_count >= 1,
        "Auto mode should detect trigger and fire it, got {} INSERT entries",
        insert_count
    );
}

// ── FULL refresh suppresses row-level triggers ─────────────────────────

#[tokio::test]
async fn test_full_refresh_suppresses_triggers() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_full (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_full VALUES (1, 'a'), (2, 'b')")
        .await;

    // Create a FULL mode stream table
    db.create_dt("st_full", "SELECT id, val FROM src_full", "1m", "FULL")
        .await;

    // Attach audit trigger
    for sql in audit_trigger_sql("st_full") {
        db.execute(&sql).await;
    }
    db.execute("TRUNCATE audit_log").await;

    // Modify source and do a FULL refresh
    db.execute("INSERT INTO src_full VALUES (3, 'c')").await;
    db.refresh_dt("st_full").await;

    // ST should have all 3 rows
    let count: i64 = db.count("st_full").await;
    assert_eq!(count, 3, "ST should have 3 rows after FULL refresh");

    // Audit log should be EMPTY — FULL refresh suppresses row-level triggers
    let audit_count: i64 = db.count("audit_log").await;
    assert_eq!(
        audit_count, 0,
        "FULL refresh should suppress row-level triggers, got {} entries",
        audit_count
    );
}

// ── BEFORE trigger modifies NEW ────────────────────────────────────────

#[tokio::test]
async fn test_before_trigger_modifies_new() {
    let db = E2eDb::new().await.with_extension().await;

    db.execute("CREATE TABLE src_before (id INT PRIMARY KEY, val TEXT)")
        .await;
    db.execute("INSERT INTO src_before VALUES (1, 'hello')")
        .await;

    db.create_dt(
        "st_before",
        "SELECT id, val FROM src_before",
        "1m",
        "DIFFERENTIAL",
    )
    .await;

    // Initial refresh
    db.refresh_dt("st_before").await;

    // Attach a BEFORE UPDATE trigger that uppercases val
    db.execute(
        "CREATE OR REPLACE FUNCTION uppercase_fn()
        RETURNS TRIGGER AS $$
        BEGIN
            NEW.val := upper(NEW.val);
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql",
    )
    .await;
    db.execute(
        "CREATE TRIGGER uppercase_trig
        BEFORE UPDATE ON st_before
        FOR EACH ROW EXECUTE FUNCTION uppercase_fn()",
    )
    .await;

    // Update source and refresh
    db.execute("UPDATE src_before SET val = 'world' WHERE id = 1")
        .await;
    db.refresh_dt("st_before").await;

    // The BEFORE trigger should have uppercased the value
    let val: String = db
        .query_scalar("SELECT val FROM st_before WHERE id = 1")
        .await;
    assert_eq!(
        val, "WORLD",
        "BEFORE UPDATE trigger should uppercase the value"
    );
}
