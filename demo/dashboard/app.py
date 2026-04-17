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

    /* nav tabs dark theme */
    .nav-tabs { border-bottom-color: var(--border); }
    .nav-tabs .nav-link { color: var(--muted); border: 1px solid transparent;
                          border-radius: 6px 6px 0 0; padding: 6px 16px; }
    .nav-tabs .nav-link:hover { color: var(--text); border-color: var(--border);
                                background: var(--hdr); }
    .nav-tabs .nav-link.active { color: var(--text); background: var(--card);
                                 border-color: var(--border) var(--border) var(--card); }
    /* inline code in card headers */
    .g-card-hdr code { font-size: 11px; background: rgba(99,110,123,0.2);
                       border-radius: 3px; padding: 1px 5px; font-weight: 400; }
    /* g-table td code */
    .g-table td code { font-size: 11px; }
    /* score colours */
    .score-keep   { color: var(--muted); }
    .score-switch { color: var(--yellow); }
    .conf-high    { color: var(--green); }
    .conf-medium  { color: var(--yellow); }
    .conf-low     { color: var(--muted); }
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

  <!-- Tab navigation -->
  <ul class="nav nav-tabs mb-3" id="mainTabs" role="tablist">
    <li class="nav-item" role="presentation">
      <button class="nav-link active" id="tab-fraud-btn"
              data-bs-toggle="tab" data-bs-target="#tab-fraud"
              type="button" role="tab">🔍 Fraud Detection</button>
    </li>
    <li class="nav-item" role="presentation">
      <button class="nav-link" id="tab-internals-btn"
              data-bs-toggle="tab" data-bs-target="#tab-internals"
              type="button" role="tab">⚙️ pg_trickle Internals</button>
    </li>
  </ul>

  <div class="tab-content">
  <div class="tab-pane fade show active" id="tab-fraud" role="tabpanel">

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

  <!-- Row 4: alert_summary + risk_scores by category -->
  <div class="row g-3 mb-3">
    <div class="col-5">
      <div class="g-card h-100">
        <div class="g-card-hdr">🎯 Alert Summary</div>
        <table class="g-table">
          <thead>
            <tr><th>Risk Level</th><th>Transactions</th><th>Avg Amount</th><th>Total Volume</th></tr>
          </thead>
          <tbody id="tb-alert-summary"></tbody>
        </table>
      </div>
    </div>
    <div class="col-7">
      <div class="g-card h-100">
        <div class="g-card-hdr">🔍 Risk Distribution by Category</div>
        <div class="overflow-auto" style="max-height:200px;">
          <table class="g-table">
            <thead>
              <tr><th>Category</th><th>Low</th><th>Medium</th><th>High</th><th>High%</th></tr>
            </thead>
            <tbody id="tb-risk-by-cat"></tbody>
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

  </div><!-- /tab-pane tab-fraud -->

  <!-- ═══════════════════ TAB 2: pg_trickle Internals ═══════════════════ -->
  <div class="tab-pane fade" id="tab-internals" role="tabpanel">

    <!-- I-1: Stream table health (full width) -->
    <div class="g-card mb-3">
      <div class="g-card-hdr">🏥 Stream Table Health — <code>pgtrickle.pgt_status()</code></div>
      <div class="overflow-auto">
        <table class="g-table">
          <thead>
            <tr><th>Name</th><th>Mode</th><th>Schedule</th><th>Populated</th>
                <th>Last Refresh</th><th>Duration</th><th>Rows Δ</th><th>Status</th></tr>
          </thead>
          <tbody id="tb-int-health"></tbody>
        </table>
      </div>
    </div>

    <!-- I-2: Dep tree + Refresh history -->
    <div class="row g-3 mb-3">
      <div class="col-4">
        <div class="g-card h-100">
          <div class="g-card-hdr">🌳 Dependency Tree — <code>pgtrickle.dependency_tree()</code></div>
          <pre class="dag-pre" id="pre-dep-tree">loading…</pre>
        </div>
      </div>
      <div class="col-8">
        <div class="g-card h-100">
          <div class="g-card-hdr">📜 Refresh History — <code>pgtrickle.pgt_refresh_history</code></div>
          <div class="overflow-auto" style="max-height:320px;">
            <table class="g-table">
              <thead>
                <tr><th>Table</th><th>Action</th><th>Started</th><th>ms</th><th>Rows Δ</th><th>Status</th></tr>
              </thead>
              <tbody id="tb-int-history"></tbody>
            </table>
          </div>
        </div>
      </div>
    </div>

    <!-- I-3: Efficiency (full width) -->
    <div class="g-card mb-3">
      <div class="g-card-hdr">⚡ Refresh Efficiency — <code>pgtrickle.refresh_efficiency()</code></div>
      <div class="overflow-auto">
        <table class="g-table">
          <thead>
            <tr><th>Table</th><th>Total</th><th>DIFF</th><th>FULL</th>
                <th>Avg DIFF ms</th><th>Avg FULL ms</th><th>Speedup</th><th>Avg Change Ratio</th></tr>
          </thead>
          <tbody id="tb-int-eff"></tbody>
        </table>
      </div>
    </div>

    <!-- I-4: Mode advisor (full width) -->
    <div class="g-card">
      <div class="g-card-hdr">🤖 Refresh Mode Advisor — <code>pgtrickle.recommend_refresh_mode()</code></div>
      <div class="overflow-auto">
        <table class="g-table">
          <thead>
            <tr><th>Table</th><th>Current</th><th>Effective</th><th>Recommendation</th>
                <th>Confidence</th><th>Reason</th></tr>
          </thead>
          <tbody id="tb-int-advisor"></tbody>
        </table>
      </div>
    </div>

  </div><!-- /tab-pane tab-internals -->

  </div><!-- /tab-content -->

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

  // ── Alert summary detail ──────────────────────────────────────────────
  const riskColors = { HIGH: 'var(--red)', MEDIUM: 'var(--yellow)', LOW: 'var(--green)' };
  setRows('tb-alert-summary', (d.alert_summary||[]).map(r => {
    const col = riskColors[r.risk_level] || 'inherit';
    return `<tr>
       <td><span class="rbadge rbadge-${esc(r.risk_level)}">${esc(r.risk_level)}</span></td>
       <td style="color:${col}">${fmt(r.txn_count)}</td>
       <td>${fmtM(r.avg_amount)}</td>
       <td>${fmtM(r.total_amount)}</td>
     </tr>`;
  }).join(''));

  // ── Risk distribution by category ─────────────────────────────────────
  setRows('tb-risk-by-cat', (d.risk_by_category||[]).map(r =>
    `<tr>
       <td><span class="rbadge" style="background:#21262d;color:#ccc">${esc(r.merchant_category)}</span></td>
       <td style="color:var(--green)">${fmt(r.low_count)}</td>
       <td style="color:var(--yellow)">${fmt(r.medium_count)}</td>
       <td style="color:var(--red)">${fmt(r.high_count)}</td>
       <td>${r.high_pct}%</td>
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

// ── Internals tab ────────────────────────────────────────────────────────────
async function refreshInternals() {
  let d;
  try {
    const r = await fetch('/api/internals');
    d = await r.json();
  } catch(e) { return; }

  // Health
  setRows('tb-int-health', (d.st_health||[]).map(r => {
    const dot = r.is_populated
      ? '<span style="color:var(--green)">●</span>'
      : '<span style="color:var(--yellow)">○</span>';
    const ts  = r.last_refresh  ? new Date(r.last_refresh).toLocaleTimeString()  : '—';
    const dur = r.duration_ms   != null ? r.duration_ms + ' ms' : '—';
    const rows = r.rows_affected != null ? fmt(r.rows_affected)  : '—';
    return `<tr>
       <td style="font-family:monospace">${esc(r.name)}</td>
       <td><code>${esc(r.refresh_mode)}</code></td>
       <td style="color:var(--muted);font-size:11px">${esc(r.schedule||'calc')}</td>
       <td>${dot}</td>
       <td style="color:var(--muted);font-size:11px">${ts}</td>
       <td style="text-align:right;color:var(--muted)">${dur}</td>
       <td style="text-align:right">${rows}</td>
       <td style="font-size:11px">${esc(r.status)}</td>
     </tr>`;
  }).join(''));

  // Dependency tree
  document.getElementById('pre-dep-tree').textContent =
    (d.dep_tree||[]).join('\n') || '(no dependency data)';

  // Refresh history
  setRows('tb-int-history', (d.refresh_history||[]).map(r => {
    const fallback = r.was_full_fallback
      ? ' <span style="color:var(--yellow);font-size:10px" title="fell back to FULL">▲</span>' : '';
    const statusCol = r.status === 'FAILED' ? 'color:var(--red)' : 'color:var(--muted)';
    const ts = r.start_time ? new Date(r.start_time).toLocaleTimeString() : '—';
    return `<tr>
       <td style="font-family:monospace;font-size:12px">${esc(r.name)}</td>
       <td><code>${esc(r.refresh_mode)}</code>${fallback}</td>
       <td style="color:var(--muted);font-size:11px">${ts}</td>
       <td style="text-align:right">${r.duration_ms != null ? r.duration_ms : '—'}</td>
       <td style="text-align:right">${r.rows_affected != null ? fmt(r.rows_affected) : '—'}</td>
       <td style="${statusCol};font-size:11px">${esc(r.status)}</td>
     </tr>`;
  }).join(''));

  // Efficiency
  setRows('tb-int-eff', (d.efficiency||[]).map(r =>
    `<tr>
       <td style="font-family:monospace">${esc(r.name)}</td>
       <td style="text-align:right">${fmt(r.total_refreshes)}</td>
       <td style="text-align:right;color:var(--green)">${fmt(r.diff_count)}</td>
       <td style="text-align:right;color:var(--yellow)">${fmt(r.full_count)}</td>
       <td style="text-align:right">${r.avg_diff_ms != null ? Number(r.avg_diff_ms).toFixed(1) : '—'}</td>
       <td style="text-align:right">${r.avg_full_ms != null ? Number(r.avg_full_ms).toFixed(1) : '—'}</td>
       <td style="text-align:right;color:var(--green)">${esc(r.diff_speedup||'—')}</td>
       <td style="text-align:right">${r.avg_change_ratio != null ? Number(r.avg_change_ratio).toFixed(3) : '—'}</td>
     </tr>`).join(''));

  // Mode advisor
  setRows('tb-int-advisor', (d.opt_hints||[]).map(r => {
    const keep = r.recommended_mode === 'KEEP';
    const recHtml = keep
      ? `<span class="score-keep">✓ KEEP <code>${esc(r.current_mode)}</code></span>`
      : `<span class="score-switch">→ SWITCH TO <code>${esc(r.recommended_mode)}</code></span>`;
    const confClass = r.confidence === 'high' ? 'conf-high'
                    : r.confidence === 'medium' ? 'conf-medium' : 'conf-low';
    return `<tr>
       <td style="font-family:monospace">${esc(r.name)}</td>
       <td><code>${esc(r.current_mode)}</code></td>
       <td style="color:var(--muted)"><code>${esc(r.effective_mode||'—')}</code></td>
       <td>${recHtml}</td>
       <td class="${confClass}" style="font-size:11px">${esc(r.confidence||'—')}</td>
       <td style="color:var(--muted);font-size:11px">${esc(r.reason||'')}</td>
     </tr>`;
  }).join(''));
}

// fetch internals immediately when the tab is opened
document.getElementById('tab-internals-btn')
  .addEventListener('shown.bs.tab', refreshInternals);

refresh();
setInterval(refresh, 2000);
setInterval(refreshInternals, 5000);
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

        risk_by_category = safe_query(conn, """
            SELECT merchant_category,
                   COUNT(*) FILTER (WHERE risk_level = 'LOW')    AS low_count,
                   COUNT(*) FILTER (WHERE risk_level = 'MEDIUM') AS medium_count,
                   COUNT(*) FILTER (WHERE risk_level = 'HIGH')   AS high_count,
                   ROUND(100.0 * COUNT(*) FILTER (WHERE risk_level = 'HIGH')
                         / NULLIF(COUNT(*), 0), 1)               AS high_pct
            FROM   risk_scores
            GROUP  BY merchant_category
            ORDER  BY high_pct DESC NULLS LAST
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
                "risk_by_category":    serialize(risk_by_category),
            }
        )
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

        # Enrich health rows with latest refresh timing
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
