#!/usr/bin/env python3
"""Validate the v0.97 monitoring and OpenTelemetry compatibility surface."""

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    dashboard_path = ROOT / "monitoring/grafana/dashboards/pg_trickle_assurance.json"
    dashboard = json.loads(dashboard_path.read_text(encoding="utf-8"))
    dashboard_text = json.dumps(dashboard)
    required_dashboard_metrics = (
        "pg_trickle_target_freshness_seconds",
        "pg_trickle_freshness_p95_seconds",
        "pg_trickle_status_counts_stream_tables_total",
        "pg_trickle_cdc_buffers_pending_rows",
        "pg_trickle_disk_projected_bytes",
        "pg_trickle_full_fallbacks_1h",
        "pg_trickle_external_graph_failures_1h",
        "pg_trickle_output_delta_consumer_lag_batches",
        "pg_trickle_cleanup_backlog_rows",
    )
    for metric in required_dashboard_metrics:
        assert metric in dashboard_text, f"dashboard missing {metric}"

    queries = (ROOT / "monitoring/prometheus/pg_trickle_queries.yml").read_text(
        encoding="utf-8"
    )
    for key in (
        "pg_trickle_cdc_buffers:",
        "pg_trickle_disk:",
        "pg_trickle_full:",
        "pg_trickle_external_graph:",
        "pg_trickle_output_delta:",
        "pg_trickle_cleanup:",
    ):
        assert key in queries, f"exporter query missing {key}"
    assert "active_count AS active_tables" in queries
    assert "error_count AS error_tables" in queries
    assert "scheduler_status = 'ACTIVE'" in queries

    alerts = (ROOT / "monitoring/prometheus/alerts.yml").read_text(encoding="utf-8")
    for alert in (
        "PgTrickleTableSuspended",
        "PgTrickleCdcBufferDepthHigh",
        "PgTrickleDiskPressure",
        "PgTrickleFullFallbackRate",
        "PgTrickleExternalGraphFailures",
        "PgTrickleOutputDeltaConsumerLag",
        "PgTrickleCleanupBacklog",
    ):
        assert f"alert: {alert}" in alerts, f"alert missing {alert}"

    source = (ROOT / "src/otel.rs").read_text(encoding="utf-8")
    docs = (ROOT / "docs/OPENTELEMETRY.md").read_text(encoding="utf-8")
    span_names = re.findall(r'pub const SPAN_[A-Z0-9_]+: &str = "([^"]+)"', source)
    assert span_names, "no stable OTel span names found"
    for span_name in span_names:
        assert span_name in docs, f"OTel span {span_name} is undocumented"
    assert "test_otel_unreachable_endpoint_does_not_block_refresh" in (
        ROOT / "tests/e2e_otel_tests.rs"
    ).read_text(encoding="utf-8")

    collector = (ROOT / "monitoring/otel/collector-config.yml").read_text(
        encoding="utf-8"
    )
    for fragment in ("otlp:", "http:", "traces:", "metrics:", "exporters:"):
        assert fragment in collector, f"collector contract missing {fragment}"

    print("monitoring contract passed")


if __name__ == "__main__":
    main()
