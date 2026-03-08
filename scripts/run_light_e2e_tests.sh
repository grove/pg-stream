#!/usr/bin/env bash
# scripts/run_light_e2e_tests.sh — Run curated light-E2E tests

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# Curated allowlist for tests that run against the light harness.
LIGHT_E2E_TESTS=(
    e2e_aggregate_coverage_tests
    e2e_alter_tests
    e2e_cdc_tests
    e2e_concurrent_tests
    e2e_coverage_error_tests
    e2e_coverage_parser_tests
    e2e_create_tests
    e2e_cte_tests
    e2e_dag_concurrent_tests
    e2e_dag_error_tests
    e2e_dag_immediate_tests
    e2e_dag_operations_tests
    e2e_dag_topology_tests
    e2e_diamond_tests
    e2e_drop_tests
    e2e_error_tests
    e2e_expression_tests
    e2e_full_join_tests
    e2e_getting_started_tests
    e2e_guard_trigger_tests
    e2e_having_transition_tests
    e2e_ivm_tests
    e2e_keyless_duplicate_tests
    e2e_lateral_subquery_tests
    e2e_lateral_tests
    e2e_lifecycle_tests
    e2e_mixed_mode_dag_tests
    e2e_monitoring_tests
    e2e_multi_cycle_dag_tests
    e2e_multi_window_tests
    e2e_pipeline_dag_tests
    e2e_property_tests
    e2e_refresh_tests
    e2e_rows_from_tests
    e2e_scalar_subquery_tests
    e2e_set_operation_tests
    e2e_smoke_tests
    e2e_snapshot_consistency_tests
    e2e_sublink_or_tests
    e2e_topk_tests
    e2e_view_tests
    e2e_window_tests
)

# The list order is intentionally interleaved so round-robin sharding with
# `--shard-count 3` reproduces the curated three-shard split used in CI.

usage() {
    cat <<'EOF'
Usage: scripts/run_light_e2e_tests.sh [options]

Options:
  --package                 Run cargo pgrx package before tests
  --package-only            Run cargo pgrx package and exit
  --list                    Print selected test targets and exit
  --shard-index <n>         1-based shard index
  --shard-count <n>         Total shard count
EOF
}

resolve_pg_config() {
    if [[ -n "${PG_CONFIG:-}" ]]; then
        echo "$PG_CONFIG"
        return
    fi

    if command -v pg_config >/dev/null 2>&1; then
        command -v pg_config
        return
    fi

    if command -v cargo >/dev/null 2>&1; then
        local pgrx_pg_config
        pgrx_pg_config="$(cargo pgrx info pg-config pg18 2>/dev/null || true)"
        if [[ -n "$pgrx_pg_config" ]]; then
            echo "$pgrx_pg_config"
            return
        fi
    fi

    if [[ -f "${HOME}/.pgrx/config.toml" ]]; then
        local config_pg_path
        config_pg_path="$(awk -F'"' '/^pg18/ {print $2; exit}' "${HOME}/.pgrx/config.toml")"
        if [[ -n "$config_pg_path" ]]; then
            echo "$config_pg_path"
            return
        fi
    fi

    echo "ERROR: Could not determine pg_config path for cargo pgrx package" >&2
    exit 1
}

package_extension() {
    local pg_config_path
    pg_config_path="$(resolve_pg_config)"

    echo "Packaging extension with pg_config: $pg_config_path"
    cargo pgrx package --pg-config "$pg_config_path"
}

validate_positive_integer() {
    local value="$1"
    local name="$2"

    if [[ ! "$value" =~ ^[0-9]+$ ]] || (( value < 1 )); then
        echo "ERROR: $name must be a positive integer" >&2
        exit 1
    fi
}

package_before_run=false
package_only=false
list_only=false
shard_index=1
shard_count=1

while (($# > 0)); do
    case "$1" in
        --package)
            package_before_run=true
            ;;
        --package-only)
            package_before_run=true
            package_only=true
            ;;
        --list)
            list_only=true
            ;;
        --shard-index)
            shift
            shard_index="${1:-}"
            ;;
        --shard-count)
            shift
            shard_count="${1:-}"
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown option: $1" >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

validate_positive_integer "$shard_index" "shard index"
validate_positive_integer "$shard_count" "shard count"

if (( shard_index > shard_count )); then
    echo "ERROR: shard index must be less than or equal to shard count" >&2
    exit 1
fi

selected_tests=()
selected_count=0

# Distribute tests round-robin across shards so adjacent entries do not end up
# in the same shard when new tests are appended to the allowlist.
for index in "${!LIGHT_E2E_TESTS[@]}"; do
    if (( index % shard_count == shard_index - 1 )); then
        selected_tests+=("${LIGHT_E2E_TESTS[$index]}")
        selected_count=$((selected_count + 1))
    fi
done

if (( selected_count == 0 )); then
    echo "ERROR: No tests selected for shard ${shard_index}/${shard_count}" >&2
    exit 1
fi

cd "$PROJECT_DIR"

if [[ "$package_before_run" == true ]]; then
    package_extension
fi

if [[ "$package_only" == true ]]; then
    exit 0
fi

echo "Running light-E2E shard ${shard_index}/${shard_count} with ${selected_count} test targets"
printf '  %s\n' "${selected_tests[@]}"

if [[ "$list_only" == true ]]; then
    exit 0
fi

cargo_args=(--features light-e2e)
for test_name in "${selected_tests[@]}"; do
    cargo_args+=(--test "$test_name")
done

cargo test "${cargo_args[@]}" -- --test-threads=1