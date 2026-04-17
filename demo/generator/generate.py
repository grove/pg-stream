"""
pg_trickle demo — continuous transaction generator.

Inserts ~1 transaction per second with occasional "suspicious burst" patterns
(rapid transactions from the same user at Crypto/Gambling merchants with
escalating amounts) that drive HIGH-risk scores in the fraud detection DAG.
"""

import math
import os
import random
import time

import psycopg2

DATABASE_URL = os.environ.get(
    "DATABASE_URL", "postgresql://demo:demo@postgres/fraud_demo"
)

# Per-category (low, high) amount range in USD
CATEGORY_AMOUNTS: dict[str, tuple[float, float]] = {
    "Retail":      (15.0,   300.0),
    "Electronics": (80.0,  1500.0),
    "Travel":     (150.0,  2500.0),
    "Gambling":    (25.0,   600.0),
    "Crypto":      (50.0,  4000.0),
    "Food":         (8.0,    90.0),
    "Pharmacy":    (12.0,   180.0),
}

RISKY_CATEGORIES = {"Crypto", "Gambling"}


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


def fetch_lookups(conn) -> tuple[list[int], list[tuple[int, str]]]:
    """Load user IDs and (merchant_id, category) pairs."""
    with conn.cursor() as cur:
        cur.execute("SELECT id FROM users ORDER BY id")
        users = [row[0] for row in cur.fetchall()]
        cur.execute("SELECT id, category FROM merchants ORDER BY id")
        merchants = [(row[0], row[1]) for row in cur.fetchall()]
    return users, merchants


def sample_amount(category: str, multiplier: float = 1.0) -> float:
    """Log-normal amount centred between the category's low/high range."""
    lo, hi = CATEGORY_AMOUNTS.get(category, (20.0, 200.0))
    mu = math.log((lo + hi) / 2.0)
    raw = random.lognormvariate(mu, 0.55) * multiplier
    return round(max(1.0, min(raw, 9_999.99)), 2)


def insert_txn(conn, user_id: int, merchant_id: int, amount: float) -> int:
    with conn.cursor() as cur:
        cur.execute(
            "INSERT INTO transactions (user_id, merchant_id, amount) "
            "VALUES (%s, %s, %s) RETURNING id",
            (user_id, merchant_id, amount),
        )
        return cur.fetchone()[0]


def main() -> None:
    conn = connect(DATABASE_URL)
    users, merchants = fetch_lookups(conn)
    merchant_by_id = {mid: cat for mid, cat in merchants}
    all_ids = [m[0] for m in merchants]
    risky_ids = [m[0] for m in merchants if m[1] in RISKY_CATEGORIES] or all_ids

    print(
        f"[GENERATOR] Loaded {len(users)} users, {len(merchants)} merchants. "
        "Starting stream...",
        flush=True,
    )

    cycle = 0
    burst_user: int | None = None
    burst_remaining = 0

    while True:
        cycle += 1

        # Every ~45 normal cycles start a suspicious burst for one user.
        if burst_remaining == 0 and cycle % 45 == 0:
            burst_user = random.choice(users)
            burst_remaining = random.randint(6, 14)
            print(
                f"[GENERATOR] BURST — user {burst_user} starting "
                f"({burst_remaining} rapid txns)",
                flush=True,
            )

        try:
            if burst_remaining > 0 and burst_user is not None:
                # Suspicious pattern: same user, risky merchant, escalating amounts
                user_id = burst_user
                merchant_id = random.choice(risky_ids)
                category = merchant_by_id[merchant_id]
                # Escalation factor grows as burst progresses
                escalation = 1.0 + (burst_remaining / 4.0)
                amount = sample_amount(category, multiplier=escalation)
                burst_remaining -= 1
                if burst_remaining == 0:
                    burst_user = None
                sleep_s = random.uniform(0.15, 0.45)
            else:
                # Normal transaction — random user, random merchant
                user_id = random.choice(users)
                merchant_id = random.choice(all_ids)
                category = merchant_by_id[merchant_id]
                amount = sample_amount(category)
                sleep_s = random.uniform(0.6, 1.6)

            txn_id = insert_txn(conn, user_id, merchant_id, amount)
            print(
                f"[TXN] id={txn_id:>6}  user={user_id:>2}  "
                f"merchant={merchant_id:>2} ({merchant_by_id[merchant_id]:<14})  "
                f"${amount:>9.2f}",
                flush=True,
            )

        except psycopg2.Error as exc:
            print(f"[GENERATOR] Insert error: {exc}", flush=True)
            try:
                conn.close()
            except Exception:
                pass
            conn = connect(DATABASE_URL)
            users, merchants = fetch_lookups(conn)
            merchant_by_id = {mid: cat for mid, cat in merchants}
            all_ids = [m[0] for m in merchants]
            risky_ids = (
                [m[0] for m in merchants if m[1] in RISKY_CATEGORIES] or all_ids
            )
            sleep_s = 1.0

        time.sleep(sleep_s)


if __name__ == "__main__":
    main()
