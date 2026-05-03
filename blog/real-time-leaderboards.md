[← Back to Blog Index](https://grove.github.io/pg-trickle/blog/) | [Documentation](https://grove.github.io/pg-trickle/)

# Real-Time Leaderboards That Don't Lie

## Maintaining ranked lists incrementally — no full recomputation, no stale scores

---

Every gaming platform, sales dashboard, and coding challenge needs a leaderboard. The requirements sound simple: show the top N items, ordered by score, updated in real time.

The implementation is where teams get stuck.

The naive approach — run `SELECT ... ORDER BY score DESC LIMIT 100` on every page load — works until the table has a few million rows and the leaderboard page takes 3 seconds to render.

The standard fix — cache the result in a materialized view and refresh it every few minutes — works until users notice their score updated 5 minutes ago and they're still not on the board.

The correct approach is to maintain the leaderboard incrementally: when a score changes, update only the affected positions. pg_trickle does this automatically.

---

## A Basic Leaderboard

```sql
CREATE TABLE player_scores (
    player_id   bigint PRIMARY KEY,
    username    text NOT NULL,
    score       bigint NOT NULL DEFAULT 0,
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- Top 100 leaderboard, updated every second
SELECT pgtrickle.create_stream_table(
    'leaderboard_top_100',
    $$SELECT player_id, username, score
      FROM player_scores
      ORDER BY score DESC
      LIMIT 100$$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);
```

`leaderboard_top_100` is a real table with exactly 100 rows. Reading it is a sequential scan of 100 rows — sub-millisecond.

When a player's score changes, pg_trickle's differential engine determines whether the change affects the top 100. If the player was already in the top 100 and their score increased, one row is updated. If a new player breaks into the top 100, one row is inserted and the player at position 100 is evicted.

The cost is proportional to the number of changes that affect the leaderboard, not the total number of players. A game with 10 million players and 50 score updates per second refreshes the leaderboard in under a millisecond per cycle.

---

## Tied Scores

Ties are the first thing that breaks naive leaderboard implementations. If players 47 through 53 all have a score of 8,500, what's the ranking?

The answer depends on your tiebreaker. pg_trickle respects whatever ORDER BY you specify:

```sql
-- Tiebreaker: earliest to reach the score wins
SELECT pgtrickle.create_stream_table(
    'leaderboard_top_100',
    $$SELECT player_id, username, score, updated_at
      FROM player_scores
      ORDER BY score DESC, updated_at ASC
      LIMIT 100$$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);
```

With `updated_at ASC` as the tiebreaker, the player who reached the score first ranks higher. This is deterministic and fair.

---

## Multi-Category Leaderboards

Games often have multiple leaderboard categories: overall score, weekly score, per-game-mode score.

```sql
CREATE TABLE match_results (
    id          bigserial PRIMARY KEY,
    player_id   bigint NOT NULL,
    game_mode   text NOT NULL,
    score       int NOT NULL,
    played_at   timestamptz NOT NULL DEFAULT now()
);

-- Overall top 50
SELECT pgtrickle.create_stream_table(
    'lb_overall_top50',
    $$SELECT player_id, SUM(score) AS total_score
      FROM match_results
      GROUP BY player_id
      ORDER BY total_score DESC
      LIMIT 50$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);

-- Per-mode top 20
SELECT pgtrickle.create_stream_table(
    'lb_per_mode_top20',
    $$SELECT game_mode, player_id, SUM(score) AS mode_score
      FROM match_results
      GROUP BY game_mode, player_id
      ORDER BY mode_score DESC
      LIMIT 20$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);

-- Weekly top 100 (temporal: only matches from the current week)
SELECT pgtrickle.create_stream_table(
    'lb_weekly_top100',
    $$SELECT player_id, SUM(score) AS weekly_score
      FROM match_results
      WHERE played_at >= date_trunc('week', now())
      GROUP BY player_id
      ORDER BY weekly_score DESC
      LIMIT 100$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);
```

Each leaderboard is independently maintained. The weekly leaderboard uses a time-window filter — pg_trickle handles the window eviction automatically as time passes.

---

## Sales Dashboards

Leaderboards aren't just for games. Sales teams live and die by rankings:

```sql
CREATE TABLE deals (
    id              bigserial PRIMARY KEY,
    sales_rep_id    bigint NOT NULL,
    amount          numeric(12,2) NOT NULL,
    stage           text NOT NULL,
    closed_at       timestamptz
);

-- Q2 leaderboard: closed deals this quarter
SELECT pgtrickle.create_stream_table(
    'sales_leaderboard_q2',
    $$SELECT
        s.id AS rep_id,
        s.name AS rep_name,
        t.name AS team_name,
        SUM(d.amount) AS closed_revenue,
        COUNT(*) AS deal_count
      FROM deals d
      JOIN sales_reps s ON s.id = d.sales_rep_id
      JOIN teams t ON t.id = s.team_id
      WHERE d.stage = 'closed_won'
        AND d.closed_at >= '2026-04-01'
        AND d.closed_at < '2026-07-01'
      GROUP BY s.id, s.name, t.name
      ORDER BY closed_revenue DESC
      LIMIT 25$$,
    schedule => '3s', refresh_mode => 'DIFFERENTIAL'
);
```

When a deal closes, the rep's ranking updates within 3 seconds. The sales manager sees it on the wall TV. No batch job, no data pipeline, no "wait until the ETL runs at midnight."

---

## The Pagination Problem

Top-100 is clean. But what about the user who's ranked #4,372 and wants to see their neighborhood — ranks 4,370 to 4,380?

You have two options.

**Option 1: Multiple stream tables with offsets.** Create a stream table for each "page" of the leaderboard. This works for fixed segments (top 100, ranks 101–200, etc.) but doesn't scale to arbitrary pagination.

**Option 2: A full ranking stream table.** Skip the LIMIT and maintain the full ranked list:

```sql
SELECT pgtrickle.create_stream_table(
    'player_rankings',
    $$SELECT player_id, username, score,
            SUM(score) AS total_score
      FROM player_scores
      GROUP BY player_id, username, score$$,
    schedule => '2s', refresh_mode => 'DIFFERENTIAL'
);

-- Then query with pagination at read time:
SELECT *, ROW_NUMBER() OVER (ORDER BY total_score DESC) AS rank
FROM player_rankings
WHERE total_score BETWEEN
    (SELECT total_score FROM player_rankings WHERE player_id = $1) - 100
    AND
    (SELECT total_score FROM player_rankings WHERE player_id = $1) + 100
ORDER BY total_score DESC;
```

The stream table maintains the pre-aggregated scores. The ranking and pagination happen at query time — which is fast because the stream table has far fewer rows than the raw events table, and you're filtering to a narrow score range around the target player.

---

## Performance

Numbers from a synthetic benchmark:

| Scenario | Players | Score updates/s | Refresh cycle | Leaderboard read |
|---|---|---|---|---|
| Top-100, simple | 1M | 100 | ~0.3ms | ~0.1ms |
| Top-100, simple | 10M | 1,000 | ~0.8ms | ~0.1ms |
| Top-50 per category, 20 categories | 1M | 500 | ~1.2ms | ~0.1ms |

The read cost is constant — you're reading a fixed-size table. The refresh cost scales with the number of score changes that affect the leaderboard boundary, not the total number of players or updates.

---

## Why Not Redis?

Redis sorted sets are the standard answer for leaderboards. They work. They're fast. But:

1. **Dual-write problem.** Your score data lives in PostgreSQL (because that's where your application logic, transactions, and constraints are). Keeping Redis in sync requires a pipeline — and that pipeline can fail, lag, or lose data.

2. **No complex queries.** Redis sorted sets support `ZRANGEBYSCORE` and `ZRANK`. They don't support JOINs, GROUP BY, time-window filters, or multi-table aggregation. Your "closed deals this quarter by team" leaderboard can't be a Redis sorted set.

3. **One more system.** Redis adds operational overhead: monitoring, failover, persistence configuration, memory management.

With pg_trickle, the leaderboard is a PostgreSQL table. It's backed by the same WAL, the same backup, the same monitoring. The read performance is comparable to Redis for small result sets (sub-millisecond for 100 rows), and the consistency guarantee is stronger.

If your only leaderboard is a simple score ranking with no JOINs, Redis is fine. For anything more complex, pg_trickle keeps it in one place.
