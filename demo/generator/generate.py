"""
pg_trickle demo — scenario dispatcher.

Reads DEMO_SCENARIO env var (default: fraud) and delegates to the matching
scenario module in scenarios/.  All scenarios expose a run(conn) function
that loops indefinitely inserting data.
"""

import os
import time

import psycopg2

DATABASE_URL = os.environ.get(
    "DATABASE_URL", "postgresql://demo:demo@postgres/fraud_demo"
)
DEMO_SCENARIO = os.environ.get("DEMO_SCENARIO", "fraud")

KNOWN_SCENARIOS = ("fraud", "ecommerce", "finance")


def connect(url: str):
    """Retry until the database is ready."""
    for attempt in range(1, 61):
        try:
            conn = psycopg2.connect(url)
            conn.autocommit = True
            print(f"[GENERATOR] Connected (attempt {attempt})", flush=True)
            return conn
        except psycopg2.OperationalError as exc:
            print(
                f"[GENERATOR] Waiting for DB (attempt {attempt}/60): {exc}",
                flush=True,
            )
            time.sleep(3)
    raise RuntimeError("Could not connect to the database after 60 attempts")


def main() -> None:
    if DEMO_SCENARIO not in KNOWN_SCENARIOS:
        raise ValueError(
            f"Unknown DEMO_SCENARIO={DEMO_SCENARIO!r}. "
            f"Valid options: {', '.join(KNOWN_SCENARIOS)}"
        )

    print(f"[GENERATOR] Starting scenario: {DEMO_SCENARIO}", flush=True)

    if DEMO_SCENARIO == "fraud":
        from scenarios.fraud import run
    elif DEMO_SCENARIO == "finance":
        from scenarios.finance import run
    else:
        from scenarios.ecommerce import run

    while True:
        conn = connect(DATABASE_URL)
        try:
            run(conn)
        except psycopg2.Error as exc:
            print(f"[GENERATOR] Connection lost: {exc} — reconnecting…", flush=True)
            try:
                conn.close()
            except Exception:
                pass
            time.sleep(1)


if __name__ == "__main__":
    main()
