#!/usr/bin/env bash
# I-4: Cross-run benchmark comparison tool.
#
# Compares two JSON benchmark result files produced by the E2E benchmarks
# (PGS_BENCH_JSON_DIR / write_results_json).
#
# Usage:
#   scripts/bench_compare.sh <baseline.json> <candidate.json>
#
# Output: per-scenario deltas with color-coded improvement/regression.

set -euo pipefail

if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <baseline.json> <candidate.json>"
    exit 1
fi

BASELINE="$1"
CANDIDATE="$2"

if [[ ! -f "$BASELINE" ]]; then
    echo "Error: baseline file not found: $BASELINE"
    exit 1
fi
if [[ ! -f "$CANDIDATE" ]]; then
    echo "Error: candidate file not found: $CANDIDATE"
    exit 1
fi

# Use Python for JSON parsing (available on macOS and most Linux).
python3 - "$BASELINE" "$CANDIDATE" <<'PYEOF'
import json
import sys
from collections import defaultdict

GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
RESET = "\033[0m"
BOLD = "\033[1m"

def load_results(path):
    with open(path) as f:
        return json.load(f)

def group_results(results):
    """Group by (scenario, table_size, change_pct, mode) → list of refresh_ms."""
    groups = defaultdict(list)
    for r in results:
        key = (r["scenario"], r["table_size"], r["change_pct"], r["mode"])
        groups[key].append(r["refresh_ms"])
    return groups

def avg(vals):
    return sum(vals) / len(vals) if vals else 0.0

def median(vals):
    s = sorted(vals)
    n = len(s)
    if n == 0:
        return 0.0
    if n % 2 == 0:
        return (s[n // 2 - 1] + s[n // 2]) / 2
    return s[n // 2]

def p95(vals):
    s = sorted(vals)
    if not s:
        return 0.0
    idx = int(0.95 * (len(s) - 1))
    return s[idx]

baseline = load_results(sys.argv[1])
candidate = load_results(sys.argv[2])

g_base = group_results(baseline)
g_cand = group_results(candidate)

all_keys = sorted(set(g_base.keys()) | set(g_cand.keys()))

print()
print(f"{BOLD}Benchmark Comparison{RESET}")
print(f"  Baseline:  {sys.argv[1]}")
print(f"  Candidate: {sys.argv[2]}")
print()

header = f"{'Scenario':<12} {'Rows':>8} {'Chg%':>6} {'Mode':<13} {'Base avg':>10} {'Cand avg':>10} {'Delta':>8} {'%':>7}  {'Verdict'}"
print(header)
print("─" * len(header))

improvements = 0
regressions = 0
neutral = 0

for key in all_keys:
    scenario, table_size, change_pct, mode = key
    b_vals = g_base.get(key, [])
    c_vals = g_cand.get(key, [])

    b_avg = avg(b_vals)
    c_avg = avg(c_vals)

    if b_avg > 0:
        delta_ms = c_avg - b_avg
        delta_pct = (delta_ms / b_avg) * 100
    else:
        delta_ms = 0
        delta_pct = 0

    # Color code: green = faster (negative delta), red = slower
    if delta_pct <= -5:
        color = GREEN
        verdict = "✓ faster"
        improvements += 1
    elif delta_pct >= 5:
        color = RED
        verdict = "✗ slower"
        regressions += 1
    else:
        color = YELLOW
        verdict = "~ neutral"
        neutral += 1

    pct_str = f"{change_pct * 100:.0f}%"
    b_str = f"{b_avg:.1f}" if b_vals else "N/A"
    c_str = f"{c_avg:.1f}" if c_vals else "N/A"

    print(f"{scenario:<12} {table_size:>8} {pct_str:>6} {mode:<13} {b_str:>10} {c_str:>10} {color}{delta_ms:>+8.1f} {delta_pct:>+6.1f}%{RESET}  {verdict}")

print()
print(f"Summary: {GREEN}{improvements} improved{RESET}, {RED}{regressions} regressed{RESET}, {YELLOW}{neutral} neutral{RESET}")
print()
PYEOF
