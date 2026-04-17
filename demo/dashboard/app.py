"""
pg_trickle demo — real-time fraud detection dashboard.

Serves a single-page web app at http://localhost:8080.
JavaScript polls /api/data every 2 seconds and updates all panels in-place.
"""

import os
from datetime import datetime, timezone
from decimal import Decimal

import psycopg2
import psycopg2.extras
from flask import Flask, jsonify

DATABASE_URL = os.environ.get(
    "DATABASE_URL", "postgresql://demo:demo@postgres/fraud_demo"
)

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


# ── HTML + JavaScript ─────────────────────────────────────────────────────────

HTML = r"""<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>pg_trickle — Real-time Fraud Detection</title>
  <link rel="stylesheet"
        href="https://cdn.jsdelivr.net/npm/bootstrap@5.3.3/dist/css/bootstrap.min.css">
  <style>
    :root {
      --bg:      #0d1117;
      --card:    #161b22;
      --border:  #30363d;
      --hdr:     #21262d;
      --muted:   #8b949e;
      --green:   #3fb950;
      --yellow:  #d29922;
      --red:     #da3633;
      --text:    #e6edf3;
    }
    html, body { background: var(--bg); color: var(--text); font-size: 14px; }
    a { color: var(--green); }

    /* cards */
    .g-card { background: var(--card); border: 1px solid var(--border);
               border-radius: 8px; overflow: hidden; }
    .g-card-hdr { background: var(--hdr); padding: 8px 14px;
                  font-size: 13px; font-weight: 600; letter-spacing: .4px; }

    /* tables */
    .g-table { width: 100%; border-collapse: collapse; }
    .g-table th { color: var(--muted); font-weight: 500; font-size: 12px;
                  border-bottom: 1px solid var(--border); padding: 6px 10px; }
    .g-table td { padding: 5px 10px; border-bottom: 1px solid var(--hdr); }
    .g-table tr:last-child td { border-bottom: none; }
    .g-table tr.risk-HIGH  td { color: #ff7b72; }
    .g-table tr.risk-MED   td { color: #e3b341; }

    /* KPI counters */
    .kpi-val { font-size: 2.2rem; font-weight: 700; line-height: 1; }
    .kpi-lbl { font-size: 11px; color: var(--muted); margin-top: 4px; }

    /* badge */
    .rbadge { display: inline-block; padding: 2px 7px; border-radius: 12px;
              font-size: 11px; font-weight: 700; }
    .rbadge-LOW    { background: #1a4620; color: var(--green); }
    .rbadge-MEDIUM { background: #422d09; color: var(--yellow); }
    .rbadge-HIGH   { background: #3d1114; color: var(--red); }

    /* DAG box */
    .dag-pre { font-family: 'SFMono-Regular', Consolas, monospace;
               font-size: 12px; color: var(--muted); background: var(--bg);
               padding: 14px; border-radius: 6px; white-space: pre;
               overflow-x: auto; margin: 0; }

    /* live dot */
    .live-dot { width: 9px; height: 9px; border-radius: 50%;
                background: var(--green); display: inline-block;
                margin-right: 8px; animation: blink 1.8s ease-in-out infinite; }
    @keyframes blink { 0%,100%{opacity:1} 50%{opacity:.25} }

    /* risk bar */
    .risk-bar { height: 8px; border-radius: 4px; }
  </style>
</head>
<body>
<div class="container-fluid px-4 py-3">

  <!-- Header -->
  <div class="d-flex align-items-center mb-3 gap-3">
    <div>
      <span class="live-dot"></span>
      <span style="font-size:18px; font-weight:700;">pg_trickle</span>
      <span class="text-muted ms-1">Real-time Fraud Detection Pipeline</span>
    </div>
    <span class="ms-auto text-muted" id="ts" style="font-size:12px;">connecting…</span>
  </div>

  <!-- KPI row -->
  <div class="row g-3 mb-3">
    <div class="col-3">
      <div class="g-card p-3 text-center">
        <div class="kpi-val" id="kpi-total">—</div>
        <div class="kpi-lbl">Total Transactions</div>
      </div>
    </div>
    <div class="col-3">
      <div class="g-card p-3 text-center">
        <div class="kpi-val" id="kpi-low" style="color:var(--green)">—</div>
        <div class="kpi-lbl">LOW Risk</div>
      </div>
    </div>
    <div class="col-3">
      <div class="g-card p-3 text-center">
        <div class="kpi-val" id="kpi-med" style="color:var(--yellow)">—</div>
        <div class="kpi-lbl">MEDIUM Risk</div>
      </div>
    </div>
    <div class="col-3">
      <div class="g-card p-3 text-center">
        <div class="kpi-val" id="kpi-high" style="color:var(--red)">—</div>
        <div class="kpi-lbl">HIGH Risk</div>
      </div>
    </div>
  </div>

  <!-- Risk bar -->
  <div class="mb-3 g-card p-3">
    <div class="d-flex mb-1" style="font-size:12px;">
      <span class="text-success me-auto">LOW</span>
      <span class="text-warning">MEDIUM</span>
      <span class="text-danger ms-auto">HIGH</span>
    </div>
    <div class="d-flex gap-1" id="risk-bar" style="height:10px; border-radius:6px; overflow:hidden;"></div>
  </div>

  <!-- Row 2: alerts + risky merchants -->
  <div class="row g-3 mb-3">
    <div class="col-7">
      <div class="g-card h-100">
        <div class="g-card-hdr">⚠️ Recent HIGH / MEDIUM Risk Alerts</div>
        <div class="overflow-auto" style="max-height:280px;">
          <table class="g-table">
            <thead>
              <tr>
                <th>#</th><th>User</th><th>Merchant</th>
                <th>Category</th><th>Amount</th><th>Risk</th>
              </tr>
            </thead>
            <tbody id="tb-alerts"></tbody>
          </table>
        </div>
      </div>
    </div>
    <div class="col-5">
      <div class="g-card h-100">
        <div class="g-card-hdr">🏪 Merchant Risk Leaderboard</div>
        <div class="overflow-auto" style="max-height:280px;">
          <table class="g-table">
            <thead>
              <tr><th>Merchant</th><th>Cat.</th><th>High</th><th>Med</th><th>Risk%</th></tr>
            </thead>
            <tbody id="tb-merchants"></tbody>
          </table>
        </div>
      </div>
    </div>
  </div>

  <!-- Row 3: user velocity + country risk + category volume -->
  <div class="row g-3 mb-3">
    <div class="col-4">
      <div class="g-card h-100">
        <div class="g-card-hdr">👤 User Velocity (top 10)</div>
        <div class="overflow-auto" style="max-height:240px;">
          <table class="g-table">
            <thead>
              <tr><th>User</th><th>🌍</th><th>Txns</th><th>Avg $</th></tr>
            </thead>
            <tbody id="tb-velocity"></tbody>
          </table>
        </div>
      </div>
    </div>
    <div class="col-4">
      <div class="g-card h-100">
        <div class="g-card-hdr">🌍 Country Overview</div>
        <div class="overflow-auto" style="max-height:240px;">
          <table class="g-table">
            <thead>
              <tr><th>Country</th><th>Users</th><th>Txns</th><th>Volume $</th></tr>
            </thead>
            <tbody id="tb-country"></tbody>
          </table>
        </div>
      </div>
    </div>
    <div class="col-4">
      <div class="g-card h-100">
        <div class="g-card-hdr">📦 Category Volume</div>
        <div class="overflow-auto" style="max-height:240px;">
          <table class="g-table">
            <thead>
              <tr><th>Category</th><th>Txns</th><th>Avg $</th><th>Users</th></tr>
            </thead>
            <tbody id="tb-category"></tbody>
          </table>
        </div>
      </div>
    </div>
  </div>

  <!-- Stream table status + DAG -->
  <div class="row g-3">
    <div class="col-5">
      <div class="g-card">
        <div class="g-card-hdr">⚡ Stream Table Status</div>
        <div class="overflow-auto" style="max-height:220px;">
          <table class="g-table">
            <thead>
              <tr><th>Name</th><th>Mode</th><th>Schedule</th><th>Status</th></tr>
            </thead>
            <tbody id="tb-sts"></tbody>
          </table>
        </div>
      </div>
    </div>
    <div class="col-7">
      <div class="g-card">
        <div class="g-card-hdr" style="cursor:pointer"
             data-bs-toggle="collapse" data-bs-target="#dag-collapse">
          📊 DAG Topology — click to expand
        </div>
        <div class="collapse show" id="dag-collapse">
          <pre class="dag-pre" id="dag-pre">{{ dag }}</pre>
        </div>
      </div>
    </div>
  </div>

</div><!-- /container -->

<script src="https://cdn.jsdelivr.net/npm/bootstrap@5.3.3/dist/js/bootstrap.bundle.min.js"></script>
<script>
"use strict";

const $ = id => document.getElementById(id);
const esc = s => String(s ?? '').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
const fmt  = (n, d=0) => n == null ? '—' : Number(n).toLocaleString('en-US', {maximumFractionDigits:d});
const fmtM = n => n == null ? '—' : '$' + Number(n).toLocaleString('en-US', {minimumFractionDigits:2,maximumFractionDigits:2});
const badge = lvl => `<span class="rbadge rbadge-${esc(lvl)}">${esc(lvl)}</span>`;

function setRows(tbodyId, html) {
  $(tbodyId).innerHTML = html || '<tr><td colspan="99" class="text-muted">waiting for data…</td></tr>';
}

async function refresh() {
  let d;
  try {
    const r = await fetch('/api/data');
    d = await r.json();
  } catch(e) {
    $('ts').textContent = 'Error: ' + e.message;
    return;
  }

  $('ts').textContent = 'Last refresh: ' + new Date().toLocaleTimeString();

  // ── KPIs from alert_summary ────────────────────────────────────────────
  let low=0, med=0, high=0, total=0;
  (d.alert_summary||[]).forEach(r => {
    const n = r.txn_count|0;
    if (r.risk_level==='LOW')    low  = n;
    if (r.risk_level==='MEDIUM') med  = n;
    if (r.risk_level==='HIGH')   high = n;
    total += n;
  });
  $('kpi-total').textContent = fmt(total);
  $('kpi-low').textContent   = fmt(low);
  $('kpi-med').textContent   = fmt(med);
  $('kpi-high').textContent  = fmt(high);

  // Risk bar
  if (total > 0) {
    const lp = (low/total*100).toFixed(1), mp = (med/total*100).toFixed(1), hp = (100-lp-mp).toFixed(1);
    $('risk-bar').innerHTML =
      `<div class="risk-bar flex-fill" style="background:#1a4620;width:${lp}%" title="LOW ${lp}%"></div>` +
      `<div class="risk-bar flex-fill" style="background:#422d09;width:${mp}%" title="MED ${mp}%"></div>` +
      `<div class="risk-bar flex-fill" style="background:#3d1114;width:${hp}%" title="HIGH ${hp}%"></div>`;
  }

  // ── Recent alerts ──────────────────────────────────────────────────────
  setRows('tb-alerts', (d.recent_alerts||[]).map(r =>
    `<tr class="risk-${r.risk_level==='HIGH'?'HIGH':r.risk_level==='MEDIUM'?'MED':''}">
       <td>#${esc(r.txn_id)}</td>
       <td>${esc(r.user_name)}</td>
       <td>${esc(r.merchant_name)}</td>
       <td>${esc(r.merchant_category)}</td>
       <td>${fmtM(r.amount)}</td>
       <td>${badge(r.risk_level)}</td>
     </tr>`).join(''));

  // ── Merchant leaderboard ───────────────────────────────────────────────
  const sortedM = [...(d.top_risky_merchants||[])].sort(
    (a,b) => (b.high_risk_count-a.high_risk_count) || (b.medium_risk_count-a.medium_risk_count)
  );
  setRows('tb-merchants', sortedM.slice(0,10).map(r =>
    `<tr>
       <td>${esc(r.merchant_name)}</td>
       <td><span class="rbadge" style="background:#21262d;color:#ccc">${esc(r.merchant_category)}</span></td>
       <td style="color:var(--red)">${r.high_risk_count}</td>
       <td style="color:var(--yellow)">${r.medium_risk_count}</td>
       <td>${r.risk_rate_pct}%</td>
     </tr>`).join(''));

  // ── User velocity ──────────────────────────────────────────────────────
  const sortedU = [...(d.user_velocity||[])].sort((a,b)=>b.txn_count-a.txn_count);
  setRows('tb-velocity', sortedU.slice(0,10).map(r =>
    `<tr>
       <td>${esc(r.user_name)}</td>
       <td>${esc(r.country)}</td>
       <td>${r.txn_count}</td>
       <td>${fmtM(r.avg_txn_amount)}</td>
     </tr>`).join(''));

  // ── Country overview ───────────────────────────────────────────────────
  const sortedC = [...(d.country_risk||[])].sort((a,b)=>b.total_txns-a.total_txns);
  setRows('tb-country', sortedC.slice(0,10).map(r =>
    `<tr>
       <td>${esc(r.country)}</td>
       <td>${r.user_count}</td>
       <td>${r.total_txns}</td>
       <td>${fmtM(r.total_volume)}</td>
     </tr>`).join(''));

  // ── Category volume ────────────────────────────────────────────────────
  const sortedCat = [...(d.category_volume||[])].sort((a,b)=>b.total_volume-a.total_volume);
  setRows('tb-category', sortedCat.map(r =>
    `<tr>
       <td>${esc(r.category)}</td>
       <td>${r.txn_count}</td>
       <td>${fmtM(r.avg_txn_amount)}</td>
       <td>${r.unique_users}</td>
     </tr>`).join(''));

  // ── Stream table status ────────────────────────────────────────────────
  setRows('tb-sts', (d.st_status||[]).map(r => {
    const dot = r.is_populated
      ? '<span style="color:var(--green)">●</span>'
      : '<span style="color:var(--yellow)">○</span>';
    return `<tr>
       <td style="font-family:monospace;font-size:12px">${esc(r.name)}</td>
       <td style="font-size:11px;color:var(--muted)">${esc(r.refresh_mode)}</td>
       <td style="font-size:11px;color:var(--muted)">${esc(r.schedule||'calc')}</td>
       <td>${dot} ${esc(r.status)}</td>
     </tr>`;
  }).join(''));
}

refresh();
setInterval(refresh, 2000);
</script>
</body>
</html>
"""

DAG_DIAGRAM = r"""
  Base tables            Layer 1 — Silver        Layer 2 — Gold          Layer 3 — Platinum
  ────────────           ──────────────────────   ─────────────────────   ──────────────────────

  ┌────────────┐         ┌──────────────────┐
  │   users    │────────►│  user_velocity   │─────────────────────────►┌──────────────────┐
  └────────────┘         │  (DIFFERENTIAL)  │                          │   country_risk   │
                         └──────┬───────────┘                          │  (DIFFERENTIAL)  │
                                │                                       └──────────────────┘
  ┌────────────┐                │  ┌──────────────────┐
  │transactions│────────────────┼─►│  merchant_stats  │
  │ (stream)   │                │  │  (DIFFERENTIAL)  │
  └────────────┘                │  └──────┬───────────┘
        │                       │         │
        │         ┌─────────────┼─────────┘ ← DIAMOND DEPENDENCY
        │         │             │
        │         ▼             ▼                                       ┌───────────────────────┐
        │    ┌────────────────────────┐                                 │    alert_summary      │
        │    │      risk_scores       │────────────────────────────────►│    (DIFFERENTIAL)     │
        │    │   (FULL, calculated)   │                                 └───────────────────────┘
        │    └────────────────────────┘
        │                                                               ┌───────────────────────┐
        │                                                               │  top_risky_merchants  │
        └──────────────────────────────────────────────────────────────►│    (DIFFERENTIAL)     │
                                                                        └───────────────────────┘
  ┌────────────┐         ┌──────────────────┐
  │ merchants  │────────►│ category_volume  │
  └────────────┘         │  (DIFFERENTIAL)  │
                         └──────────────────┘

  transactions feeds user_velocity AND merchant_stats — a genuine diamond.
  risk_scores is the convergence node that joins both Layer 1 outputs.
"""


# ── Flask routes ──────────────────────────────────────────────────────────────

@app.route("/")
def index():
    return HTML.replace("{{ dag }}", DAG_DIAGRAM)


@app.route("/api/data")
def api_data():
    conn = None
    try:
        conn = get_conn()

        recent_alerts = safe_query(conn, """
            SELECT txn_id, user_name, merchant_name, merchant_category,
                   amount, risk_level
            FROM   risk_scores
            WHERE  risk_level IN ('HIGH', 'MEDIUM')
            ORDER  BY txn_id DESC
            LIMIT  15
        """)

        alert_summary = safe_query(conn, """
            SELECT risk_level, txn_count, total_amount, avg_amount
            FROM   alert_summary
            ORDER  BY CASE risk_level
                          WHEN 'HIGH'   THEN 0
                          WHEN 'MEDIUM' THEN 1
                          ELSE 2 END
        """)

        top_risky_merchants = safe_query(conn, """
            SELECT merchant_name, merchant_category, total_txns,
                   high_risk_count, medium_risk_count, risk_rate_pct
            FROM   top_risky_merchants
        """)

        user_velocity = safe_query(conn, """
            SELECT user_name, country, txn_count, total_spent, avg_txn_amount
            FROM   user_velocity
            WHERE  txn_count > 0
        """)

        country_risk = safe_query(conn, """
            SELECT country, user_count, total_txns, total_volume
            FROM   country_risk
        """)

        category_volume = safe_query(conn, """
            SELECT category, txn_count, total_volume, avg_txn_amount, unique_users
            FROM   category_volume
            WHERE  txn_count > 0
        """)

        st_status = safe_query(conn, """
            SELECT name, status, refresh_mode, is_populated, schedule
            FROM   pgtrickle.pgt_status()
            ORDER  BY name
        """)

        return jsonify(
            {
                "recent_alerts":       serialize(recent_alerts),
                "alert_summary":       serialize(alert_summary),
                "top_risky_merchants": serialize(top_risky_merchants),
                "user_velocity":       serialize(user_velocity),
                "country_risk":        serialize(country_risk),
                "category_volume":     serialize(category_volume),
                "st_status":           serialize(st_status),
            }
        )
    except Exception as exc:
        return jsonify({"error": str(exc)}), 500
    finally:
        if conn:
            conn.close()


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8080, debug=False)
