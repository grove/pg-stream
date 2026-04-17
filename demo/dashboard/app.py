"""
pg_trickle demo — dashboard dispatcher.

Reads DEMO_SCENARIO env var (default: fraud) and delegates to the matching
scenario module in scenarios/.  Each scenario exposes:
  HTML         — complete page HTML
  DAG_DIAGRAM  — ASCII art DAG for the {{ dag }} placeholder
  get_data(conn, safe_query, serialize) → dict for /api/data

The /api/internals endpoint is scenario-agnostic (queries pgtrickle system
tables) and is served from this module directly.
"""

import os
from datetime import datetime
from decimal import Decimal

import psycopg2
import psycopg2.extras
from flask import Flask, jsonify

DATABASE_URL = os.environ.get(
    "DATABASE_URL", "postgresql://demo:demo@postgres/fraud_demo"
)
DEMO_SCENARIO = os.environ.get("DEMO_SCENARIO", "fraud")

KNOWN_SCENARIOS = ("fraud", "ecommerce", "finance")

if DEMO_SCENARIO not in KNOWN_SCENARIOS:
    raise ValueError(
        f"Unknown DEMO_SCENARIO={DEMO_SCENARIO!r}. "
        f"Valid options: {', '.join(KNOWN_SCENARIOS)}"
    )

if DEMO_SCENARIO == "ecommerce":
    from scenarios.ecommerce import HTML, DAG_DIAGRAM, get_data as _get_scenario_data
elif DEMO_SCENARIO == "finance":
    from scenarios.finance import HTML, DAG_DIAGRAM, get_data as _get_scenario_data
else:
    from scenarios.fraud import HTML, DAG_DIAGRAM, get_data as _get_scenario_data

app = Flask(__name__)


def get_conn():
    return psycopg2.connect(
        DATABASE_URL, cursor_factory=psycopg2.extras.RealDictCursor
    )


def safe_query(conn, sql: str, default=None):
    try:
        with conn.cursor() as cur:
            cur.execute(sql)
            return cur.fetchall()
    except Exception as exc:
        try:
            conn.rollback()
        except Exception:
            pass
        print(f"[DASHBOARD] Query error: {exc}", flush=True)
        return default or []


def serialize(rows) -> list[dict]:
    """Convert RealDictRows with Decimal/datetime values to plain dicts."""
    out = []
    for row in rows:
        d = {}
        for k, v in row.items():
            if isinstance(v, Decimal):
                d[k] = float(v)
            elif isinstance(v, datetime):
                d[k] = v.isoformat()
            else:
                d[k] = v
        out.append(d)
    return out


# ── Flask routes ──────────────────────────────────────────────────────────────

@app.route("/")
def index():
    return HTML.replace("{{ dag }}", DAG_DIAGRAM)


@app.route("/api/data")
def api_data():
    conn = None
    try:
        conn = get_conn()
        return jsonify(_get_scenario_data(conn, safe_query, serialize))
    except Exception as exc:
        return jsonify({"error": str(exc)}), 500
    finally:
        if conn:
            conn.close()


@app.route("/api/internals")
def api_internals():
    conn = None
    try:
        conn = get_conn()

        st_health = safe_query(conn, """
            SELECT name, status, refresh_mode, is_populated, schedule
            FROM   pgtrickle.pgt_status()
            ORDER  BY name
        """)

        latest_ref = safe_query(conn, """
            SELECT DISTINCT ON (st.pgt_schema, st.pgt_name)
                st.pgt_schema || '.' || st.pgt_name AS name,
                h.start_time  AS last_refresh,
                ROUND(EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000)::bigint
                              AS duration_ms,
                (h.rows_inserted + h.rows_deleted) AS rows_affected
            FROM pgtrickle.pgt_refresh_history h
            JOIN pgtrickle.pgt_stream_tables   st ON st.pgt_id = h.pgt_id
            WHERE h.status = 'COMPLETED'
            ORDER BY st.pgt_schema, st.pgt_name, h.start_time DESC
        """)

        dep_tree = safe_query(conn, """
            SELECT tree_line FROM pgtrickle.dependency_tree()
        """)

        refresh_hist = safe_query(conn, """
            SELECT st.pgt_schema || '.' || st.pgt_name AS name,
                   h.action          AS refresh_mode,
                   h.start_time,
                   ROUND(EXTRACT(EPOCH FROM (h.end_time - h.start_time)) * 1000)::bigint
                                     AS duration_ms,
                   (h.rows_inserted + h.rows_deleted) AS rows_affected,
                   h.status,
                   h.was_full_fallback
            FROM pgtrickle.pgt_refresh_history h
            JOIN pgtrickle.pgt_stream_tables   st ON st.pgt_id = h.pgt_id
            WHERE h.status IN ('COMPLETED', 'FAILED')
            ORDER BY h.start_time DESC
            LIMIT 40
        """)

        efficiency = safe_query(conn, """
            SELECT pgt_name AS name, total_refreshes, diff_count, full_count,
                   avg_diff_ms, avg_full_ms, avg_change_ratio, diff_speedup
            FROM pgtrickle.refresh_efficiency()
            ORDER BY pgt_name
        """)

        opt_hints = safe_query(conn, """
            SELECT pgt_name AS name, current_mode, effective_mode,
                   recommended_mode, confidence, reason
            FROM pgtrickle.recommend_refresh_mode()
            ORDER BY pgt_name
        """)

        latest_by_name = {r['name']: r for r in serialize(latest_ref)}
        health_out = []
        for row in serialize(st_health):
            enriched = dict(row)
            if row['name'] in latest_by_name:
                lr = latest_by_name[row['name']]
                enriched['last_refresh']  = lr.get('last_refresh')
                enriched['duration_ms']   = lr.get('duration_ms')
                enriched['rows_affected'] = lr.get('rows_affected')
            health_out.append(enriched)

        return jsonify({
            "st_health":       health_out,
            "dep_tree":        [r['tree_line'] for r in dep_tree],
            "refresh_history": serialize(refresh_hist),
            "efficiency":      serialize(efficiency),
            "opt_hints":       serialize(opt_hints),
        })
    except Exception as exc:
        return jsonify({"error": str(exc)}), 500
    finally:
        if conn:
            conn.close()


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8080, debug=False)
