#!/usr/bin/env bash
# =============================================================================
# scripts/unsafe_inventory.sh — Audit unsafe block counts against the baseline.
#
# Counts `unsafe {` occurrences per .rs file under src/ and compares them to
# the committed baseline in .unsafe-baseline. Exits 1 if any file exceeds its
# baseline or if a file that is not in the baseline gains unsafe blocks.
#
# Usage:
#   scripts/unsafe_inventory.sh              # check + exit 1 on regression
#   scripts/unsafe_inventory.sh --report-only # print report, always exit 0
#   scripts/unsafe_inventory.sh --update      # regenerate .unsafe-baseline
#
# In CI the script writes a Markdown table to $GITHUB_STEP_SUMMARY when that
# variable is set.
# =============================================================================
set -euo pipefail

BASELINE_FILE=".unsafe-baseline"
MODE="${1:-check}"

# ---------------------------------------------------------------------------
# --update: regenerate the baseline from the current counts and exit
# ---------------------------------------------------------------------------
if [[ "$MODE" == "--update" ]]; then
  echo "# pg_trickle unsafe { block baseline — generated $(date +%Y-%m-%d)"
  echo "# Tracks \`grep -c \"unsafe {\"\` counts per source file."
  echo "# Files with 0 unsafe blocks are omitted."
  echo "#"
  echo "# To update after a justified increase:"
  echo "#   scripts/unsafe_inventory.sh --update"
  echo "#"
  echo "# Files and their unsafe block counts:"
  find src/ -name "*.rs" | sort | while IFS= read -r f; do
    count=$(grep -c "unsafe {" "$f" 2>/dev/null || true)
    count="${count//[[:space:]]/}"
    if [[ "${count:-0}" -gt 0 ]]; then
      echo "$f $count"
    fi
  done
  echo ""
  echo "Baseline written to stdout. Redirect to .unsafe-baseline to save." >&2
  exit 0
fi

# ---------------------------------------------------------------------------
# Parse the committed baseline
# ---------------------------------------------------------------------------
declare -A baseline
while IFS= read -r line; do
  [[ "$line" =~ ^# ]] && continue
  [[ -z "${line// }" ]] && continue
  file="${line%% *}"
  count="${line##* }"
  baseline["$file"]="$count"
done < "$BASELINE_FILE"

# ---------------------------------------------------------------------------
# Count current unsafe blocks
# ---------------------------------------------------------------------------
declare -A current
while IFS= read -r f; do
  c=$(grep -c "unsafe {" "$f" 2>/dev/null || true)
  c="${c//[[:space:]]/}"
  current["$f"]="${c:-0}"
done < <(find src/ -name "*.rs" | sort)

# ---------------------------------------------------------------------------
# Compare
# ---------------------------------------------------------------------------
FAIL=0
declare -a exceeded=()
declare -a new_unsafe=()
declare -a reduced=()
declare -a unchanged=()

# Collect all unique file keys
declare -A all_files
for f in "${!baseline[@]}"; do all_files["$f"]=1; done
for f in "${!current[@]}"; do all_files["$f"]=1; done

for f in $(printf '%s\n' "${!all_files[@]}" | sort); do
  cur="${current[$f]:-0}"
  base="${baseline[$f]:-0}"

  if [[ "$cur" -gt "$base" ]]; then
    delta=$((cur - base))
    exceeded+=("$f" "$base" "$cur" "+${delta}")
    FAIL=1
  elif [[ "$base" -eq 0 && "$cur" -gt 0 ]]; then
    new_unsafe+=("$f" "0" "$cur" "+${cur}")
    FAIL=1
  elif [[ "$cur" -lt "$base" ]]; then
    delta=$((base - cur))
    reduced+=("$f" "$base" "$cur" "-${delta}")
  else
    unchanged+=("$f" "$base" "$cur" "—")
  fi
done

# ---------------------------------------------------------------------------
# GitHub Step Summary
# ---------------------------------------------------------------------------
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "## Unsafe Block Inventory"
    echo ""
    echo "| File | Baseline | Current | Δ |"
    echo "|------|----------|---------|---|"

    for f in $(printf '%s\n' "${!all_files[@]}" | sort); do
      cur="${current[$f]:-0}"
      base="${baseline[$f]:-0}"
      if [[ "$cur" -gt "$base" ]]; then
        echo "| \`$f\` | $base | $cur | ⚠️ +$((cur - base)) |"
      elif [[ "$cur" -lt "$base" ]]; then
        echo "| \`$f\` | $base | $cur | ✅ -$((base - cur)) |"
      elif [[ "$cur" -gt 0 ]]; then
        echo "| \`$f\` | $base | $cur | — |"
      fi
    done

    echo ""
    if [[ "$FAIL" -ne 0 ]]; then
      echo "### ❌ Unsafe block count exceeded baseline"
      echo ""
      echo "Update the baseline only after a deliberate review:"
      echo "\`\`\`bash"
      echo "scripts/unsafe_inventory.sh --update > .unsafe-baseline"
      echo "git add .unsafe-baseline && git commit -m 'chore: update unsafe baseline'"
      echo "\`\`\`"
    else
      echo "### ✅ Unsafe block count within baseline"
      if [[ ${#reduced[@]} -gt 0 ]]; then
        echo ""
        echo "Some files reduced their unsafe block count — consider running"
        echo "\`scripts/unsafe_inventory.sh --update\` to tighten the baseline."
      fi
    fi
  } >> "$GITHUB_STEP_SUMMARY"
fi

# ---------------------------------------------------------------------------
# Terminal output
# ---------------------------------------------------------------------------
BOLD="\033[1m"; RED="\033[31m"; GREEN="\033[32m"; YELLOW="\033[33m"; RESET="\033[0m"

echo ""
echo -e "${BOLD}Unsafe block inventory${RESET}"
printf "%-52s %8s %8s %8s\n" "File" "Baseline" "Current" "Δ"
printf "%-52s %8s %8s %8s\n" "----" "--------" "-------" "-"

for f in $(printf '%s\n' "${!all_files[@]}" | sort); do
  cur="${current[$f]:-0}"
  base="${baseline[$f]:-0}"
  [[ "$cur" -eq 0 && "$base" -eq 0 ]] && continue

  if [[ "$cur" -gt "$base" ]]; then
    delta="+$((cur - base))"
    printf "${RED}%-52s %8s %8s %8s${RESET}\n" "$f" "$base" "$cur" "$delta"
  elif [[ "$cur" -lt "$base" ]]; then
    delta="-$((base - cur))"
    printf "${GREEN}%-52s %8s %8s %8s${RESET}\n" "$f" "$base" "$cur" "$delta"
  else
    printf "%-52s %8s %8s %8s\n" "$f" "$base" "$cur" "—"
  fi
done

echo ""
if [[ "$FAIL" -ne 0 ]]; then
  echo -e "${RED}${BOLD}FAIL: unsafe block count exceeds baseline in one or more files.${RESET}"
  echo ""
  echo "Update the baseline only after a deliberate review:"
  echo "  scripts/unsafe_inventory.sh --update > .unsafe-baseline"
  echo "  git add .unsafe-baseline && git commit -m 'chore: update unsafe baseline'"
fi

if [[ "$MODE" == "--report-only" ]]; then
  exit 0
fi

exit "$FAIL"
