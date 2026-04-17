"""
pg_trickle demo — financial risk pipeline scenario generator.

Demonstrates deep DAG DIFFERENTIAL efficiency with realistic sparse updates:
  - Steady trade stream: 1–3 trades per second across 50 accounts × 30 instruments
  - Price ticks: one instrument repriced every ~2 seconds (1/30 change ratio)
  - Each price tick cascades through 10 DAG levels but touches only ~30/1500
    position rows (change ratio ≈ 0.02 at L2, shrinks further upstream)

Result: L10 breach_dashboard rarely changes more than 1–2 rows per tick,
even though 1,500 positions exist — textbook DIFFERENTIAL efficiency.
"""

import math
import random
import time

import psycopg2

# How often (in cycles) to reprice one instrument
PRICE_TICK_INTERVAL = 1   # reprice one instrument every cycle
PRICE_VOLATILITY    = 0.0015   # ±0.15% per tick (realistic intraday)
PRICE_DRIFT_BOUND   = 0.20     # max ±20% from base_price before mean-reverting

# Trade generation intervals (seconds)
TRADE_INTERVAL_NORMAL = (0.4, 1.2)    # steady flow
TRADE_INTERVAL_BURST  = (0.05, 0.20)  # algo burst

# Burst parameters: simulate an algorithmic trading burst on one account
BURST_INTERVAL   = 60    # one burst every ~60 cycles
BURST_SIZE       = (8, 20)

# Quantity range per trade (can be fractional for realism)
QUANTITY_RANGE = (10, 500)   # shares
SELL_PROBABILITY = 0.35      # 35% of trades are sells


def fetch_lookups(conn):
    with conn.cursor() as cur:
        cur.execute("SELECT id, portfolio_id, capital_limit FROM accounts ORDER BY id")
        accounts = [(row[0], row[1], float(row[2])) for row in cur.fetchall()]

        cur.execute("""
            SELECT i.id, i.ticker, i.base_price, i.asset_class, mp.bid, mp.ask
            FROM instruments i
            JOIN market_prices mp ON mp.instrument_id = i.id
            ORDER BY i.id
        """)
        instruments = [
            {
                "id": row[0],
                "ticker": row[1],
                "base_price": float(row[2]),
                "asset_class": row[3],
                "bid": float(row[4]),
                "ask": float(row[5]),
                "current_mid": (float(row[4]) + float(row[5])) / 2,
            }
            for row in cur.fetchall()
        ]

    return accounts, instruments


def tick_price(conn, instruments: list[dict], idx: int) -> None:
    """Apply a small random walk to one instrument's price."""
    instr = instruments[idx]
    mid   = instr["current_mid"]
    base  = instr["base_price"]

    # Mean-revert if drifted too far from base
    if abs(mid - base) / base > PRICE_DRIFT_BOUND:
        drift = (base - mid) * 0.1   # pull back 10% toward base
    else:
        drift = mid * random.gauss(0, PRICE_VOLATILITY)

    new_mid = round(max(0.01, mid + drift), 4)
    spread  = round(new_mid * 0.0005, 4)   # 0.05% spread
    new_bid = round(new_mid - spread, 4)
    new_ask = round(new_mid + spread, 4)

    with conn.cursor() as cur:
        cur.execute(
            "UPDATE market_prices SET bid=%s, ask=%s, updated_at=now() "
            "WHERE instrument_id=%s",
            (new_bid, new_ask, instr["id"]),
        )

    instr["bid"] = new_bid
    instr["ask"] = new_ask
    instr["current_mid"] = new_mid
    print(
        f"[TICK]  {instr['ticker']:<5} mid={new_mid:>10.4f}  "
        f"drift={drift:>+8.4f}",
        flush=True,
    )


def insert_trade(
    conn,
    account_id: int,
    instrument_id: int,
    quantity: float,
    price: float,
) -> int:
    with conn.cursor() as cur:
        cur.execute(
            "INSERT INTO trades (account_id, instrument_id, quantity, price) "
            "VALUES (%s, %s, %s, %s) RETURNING id",
            (account_id, instrument_id, round(quantity, 4), round(price, 4)),
        )
        return cur.fetchone()[0]


def run(conn) -> None:
    accounts, instruments = fetch_lookups(conn)
    instr_ids = list(range(len(instruments)))

    # Prefer active subsets to create concentrated position maps
    active_accounts = accounts[:min(20, len(accounts))]  # 20 most active accounts
    active_instrs   = instr_ids[:min(15, len(instruments))]  # top 15 instruments

    print(
        f"[GENERATOR] finance: {len(accounts)} accounts, "
        f"{len(instruments)} instruments. "
        f"Active: {len(active_accounts)} accounts × {len(active_instrs)} instruments. "
        f"Price tick every ~{PRICE_TICK_INTERVAL} cycle(s). Starting…",
        flush=True,
    )

    cycle          = 0
    tick_idx       = 0    # round-robin instrument price ticks
    burst_account  = None
    burst_remaining = 0

    while True:
        cycle += 1

        # ── Price tick (every cycle, round-robin) ─────────────────────────────
        tick_price(conn, instruments, tick_idx % len(instruments))
        tick_idx += 1

        # ── Algo burst: simulate HFT burst on one account ─────────────────────
        if burst_remaining == 0 and cycle % BURST_INTERVAL == 0:
            burst_account   = random.choice(active_accounts)
            burst_remaining = random.randint(*BURST_SIZE)
            print(
                f"[GENERATOR] ALGO BURST — account {burst_account[0]} "
                f"({burst_remaining} rapid trades)",
                flush=True,
            )

        try:
            if burst_remaining > 0 and burst_account is not None:
                acc_id = burst_account[0]
                # Burst trades concentrated in a single instrument (momentum)
                iidx   = random.choice(active_instrs)
                instr  = instruments[iidx]
                qty    = random.randint(*QUANTITY_RANGE)
                is_sell = random.random() < 0.5   # 50/50 during burst
                signed_qty = -qty if is_sell else qty
                price  = instr["ask"] if not is_sell else instr["bid"]
                burst_remaining -= 1
                if burst_remaining == 0:
                    burst_account = None
                sleep_s = random.uniform(*TRADE_INTERVAL_BURST)
            else:
                # Normal: prefer active accounts and instruments
                acc    = random.choice(active_accounts)
                acc_id = acc[0]
                iidx   = random.choice(active_instrs)
                instr  = instruments[iidx]
                qty    = random.randint(*QUANTITY_RANGE)
                is_sell = random.random() < SELL_PROBABILITY
                signed_qty = -qty if is_sell else qty
                price  = instr["ask"] if not is_sell else instr["bid"]
                sleep_s = random.uniform(*TRADE_INTERVAL_NORMAL)

            trade_id = insert_trade(conn, acc_id, instr["id"], signed_qty, price)
            side = "SELL" if signed_qty < 0 else "BUY "
            print(
                f"[TRADE] id={trade_id:>6}  acct={acc_id:>2}  "
                f"{instr['ticker']:<5}  {side}  qty={abs(signed_qty):>5}  "
                f"@ {price:>10.4f}",
                flush=True,
            )

        except psycopg2.Error as exc:
            print(f"[GENERATOR] Insert error: {exc}", flush=True)
            try:
                conn.close()
            except Exception:
                pass
            raise

        time.sleep(sleep_s)
