-- =============================================================================
-- pg_trickle Financial Risk Pipeline Demo — Stream Table Definitions
--
-- 10 levels deep. ONLY the two leaf tables (trades, market_prices) are
-- refreshed on a CALCULATED schedule — all derived levels cascade automatically.
--
-- Differential efficiency showcase:
--   30 instruments × 50 accounts → up to 1,500 net position rows.
--   Each price tick changes only the ~30 positions for that instrument
--   (≈50 accounts × 1 instrument). Change ratio ≈ 30/1500 = 0.02.
--   Deep propagation means L10 breach_dashboard changes ≈ 0–2 rows per tick.
-- =============================================================================

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 1 — Leaf stream tables
-- These are the ONLY tables with a fixed schedule.
-- All downstream levels use schedule => 'calculated'.
-- ─────────────────────────────────────────────────────────────────────────────

-- L1a: Latest mid-price snapshot per instrument.
-- Reads the market_prices base table directly (slowly-changing; generator
-- updates one instrument's price every ~2 seconds from ~30 instruments).
-- Change ratio per cycle ≈ 1/30 ≈ 0.033 — strongly favours DIFFERENTIAL.
SELECT pgtrickle.create_stream_table(
    name     => 'price_snapshot',
    query    => $$
        SELECT
            mp.instrument_id,
            i.ticker,
            i.name                    AS instrument_name,
            s.name                    AS sector,
            i.asset_class,
            mp.bid,
            mp.ask,
            mp.mid                    AS mid_price,
            i.base_price,
            ROUND(mp.mid - i.base_price, 4)            AS price_drift,
            ROUND(100.0 * (mp.mid - i.base_price)
                  / NULLIF(i.base_price, 0), 2)        AS drift_pct,
            mp.updated_at             AS price_updated_at
        FROM market_prices mp
        JOIN instruments i ON i.id = mp.instrument_id
        JOIN sectors     s ON s.id = i.sector_id
    $$,
    schedule     => '2s',
    refresh_mode => 'DIFFERENTIAL'
);

-- L1b: Cumulative net position per (account, instrument) from the trade ledger.
-- Reads the append-only trades table. New trades are additive; only the rows
-- for the traded instrument + account pair change.
SELECT pgtrickle.create_stream_table(
    name     => 'net_positions',
    query    => $$
        SELECT
            t.account_id,
            a.name                    AS account_name,
            a.portfolio_id,
            t.instrument_id,
            i.ticker,
            i.sector_id,
            COUNT(t.id)               AS trade_count,
            SUM(t.quantity)           AS net_quantity,
            ROUND(SUM(t.quantity * t.price)
                  / NULLIF(SUM(t.quantity), 0), 4)     AS avg_cost_basis,
            MIN(t.traded_at)          AS first_trade_at,
            MAX(t.traded_at)          AS last_trade_at
        FROM trades t
        JOIN accounts    a ON a.id = t.account_id
        JOIN instruments i ON i.id = t.instrument_id
        GROUP BY t.account_id, a.name, a.portfolio_id,
                 t.instrument_id, i.ticker, i.sector_id
        HAVING SUM(t.quantity) <> 0
    $$,
    schedule     => '1s',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 2 — Mark-to-market position values
-- Joins net_positions (L1b) with price_snapshot (L1a).
-- Only positions in instruments with a price change re-compute.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'position_values',
    query    => $$
        SELECT
            np.account_id,
            np.account_name,
            np.portfolio_id,
            np.instrument_id,
            np.ticker,
            np.sector_id,
            np.net_quantity,
            np.avg_cost_basis,
            ps.mid_price,
            ps.sector,
            ps.asset_class,
            ROUND(np.net_quantity * ps.mid_price, 2)          AS market_value,
            ROUND(np.net_quantity
                  * (ps.mid_price - np.avg_cost_basis), 2)    AS unrealized_pnl,
            ROUND(100.0 * (ps.mid_price - np.avg_cost_basis)
                  / NULLIF(np.avg_cost_basis, 0), 2)          AS pnl_pct
        FROM net_positions   np
        JOIN price_snapshot  ps ON ps.instrument_id = np.instrument_id
        WHERE np.net_quantity <> 0
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 3 — Account-level P&L and risk summary
-- Aggregates position_values (L2) to the account grain.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'account_pnl',
    query    => $$
        SELECT
            pv.account_id,
            pv.account_name,
            pv.portfolio_id,
            COUNT(*)                                  AS position_count,
            SUM(pv.market_value)                      AS total_market_value,
            SUM(pv.unrealized_pnl)                    AS total_unrealized_pnl,
            ROUND(
                CASE WHEN SUM(pv.market_value) <> 0
                     THEN 100.0 * SUM(pv.unrealized_pnl)
                          / ABS(SUM(pv.market_value))
                     ELSE 0 END, 2)                   AS portfolio_return_pct,
            SUM(CASE WHEN pv.unrealized_pnl > 0
                     THEN pv.market_value ELSE 0 END) AS winning_exposure,
            SUM(CASE WHEN pv.unrealized_pnl < 0
                     THEN pv.market_value ELSE 0 END) AS losing_exposure
        FROM position_values pv
        GROUP BY pv.account_id, pv.account_name, pv.portfolio_id
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 4 — Portfolio-level P&L roll-up
-- Aggregates account_pnl (L3) to portfolio grain.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'portfolio_pnl',
    query    => $$
        SELECT
            ap.portfolio_id,
            p.name                                    AS portfolio_name,
            p.manager,
            p.strategy,
            COUNT(DISTINCT ap.account_id)             AS account_count,
            SUM(ap.position_count)                    AS total_positions,
            SUM(ap.total_market_value)                AS total_market_value,
            SUM(ap.total_unrealized_pnl)              AS total_unrealized_pnl,
            ROUND(
                CASE WHEN SUM(ap.total_market_value) <> 0
                     THEN 100.0 * SUM(ap.total_unrealized_pnl)
                          / ABS(SUM(ap.total_market_value))
                     ELSE 0 END, 2)                   AS portfolio_return_pct,
            SUM(ap.winning_exposure)                  AS total_winning_exposure,
            SUM(ap.losing_exposure)                   AS total_losing_exposure
        FROM account_pnl ap
        JOIN portfolios  p ON p.id = ap.portfolio_id
        GROUP BY ap.portfolio_id, p.name, p.manager, p.strategy
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 5 — Sector exposure across all portfolios
-- Aggregates position_values (L2) by sector. Only the sector of the changed
-- instrument re-computes — typically 1 of 8 sector rows changes per tick.
-- Change ratio ≈ 1/8 = 0.125 — DIFFERENTIAL is clearly advantageous.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'sector_exposure',
    query    => $$
        SELECT
            pv.sector_id,
            pv.sector,
            pv.asset_class,
            COUNT(DISTINCT pv.instrument_id)          AS instrument_count,
            COUNT(DISTINCT pv.account_id)             AS account_count,
            SUM(pv.market_value)                      AS total_exposure,
            SUM(pv.unrealized_pnl)                    AS total_unrealized_pnl,
            ROUND(
                CASE WHEN SUM(pv.market_value) <> 0
                     THEN 100.0 * SUM(pv.unrealized_pnl)
                          / ABS(SUM(pv.market_value))
                     ELSE 0 END, 2)                   AS sector_return_pct,
            ROUND(
                ABS(SUM(pv.market_value))
                / NULLIF(
                    (SELECT SUM(ABS(market_value)) FROM position_values), 0
                ) * 100.0, 2)                         AS pct_of_total_book
        FROM position_values pv
        GROUP BY pv.sector_id, pv.sector, pv.asset_class
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 6 — Per-position VaR contribution (simplified parametric)
-- Uses position_values (L2) with a sector volatility factor.
-- VaR = |market_value| × volatility_factor × z_score (95% = 1.645).
-- Volatility factors are hard-coded by asset class for demo simplicity.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'var_contributions',
    query    => $$
        SELECT
            pv.account_id,
            pv.account_name,
            pv.portfolio_id,
            pv.instrument_id,
            pv.ticker,
            pv.sector,
            pv.asset_class,
            pv.market_value,
            pv.unrealized_pnl,
            -- Parametric VaR: |MV| × daily_vol × 1.645 (95% z-score)
            ROUND(
                ABS(pv.market_value)
                * CASE pv.asset_class
                    WHEN 'EQUITY'    THEN 0.018   -- ~1.8% daily vol
                    WHEN 'BOND'      THEN 0.004   -- ~0.4% daily vol
                    WHEN 'ETF'       THEN 0.012   -- ~1.2% daily vol
                    WHEN 'COMMODITY' THEN 0.025   -- ~2.5% daily vol
                    ELSE                  0.015
                  END
                * 1.645, 2)                           AS var_95,
            -- Stressed VaR at 99% (z=2.326, 3× vol assumption)
            ROUND(
                ABS(pv.market_value)
                * CASE pv.asset_class
                    WHEN 'EQUITY'    THEN 0.054
                    WHEN 'BOND'      THEN 0.012
                    WHEN 'ETF'       THEN 0.036
                    WHEN 'COMMODITY' THEN 0.075
                    ELSE                  0.045
                  END
                * 2.326, 2)                           AS stressed_var_99
        FROM position_values pv
        WHERE pv.market_value IS NOT NULL
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 7 — Account-level VaR aggregation
-- Sums VaR contributions (L6) per account with simple aggregation.
-- Only accounts holding changed instruments re-compute.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'account_var',
    query    => $$
        SELECT
            vc.account_id,
            vc.account_name,
            vc.portfolio_id,
            COUNT(*)                                  AS position_count,
            SUM(vc.market_value)                      AS total_market_value,
            SUM(vc.unrealized_pnl)                    AS total_pnl,
            -- Sum of individual VaRs (conservative; ignores correlation)
            SUM(vc.var_95)                            AS total_var_95,
            SUM(vc.stressed_var_99)                   AS total_stressed_var_99,
            -- Diversified VaR ≈ sqrt(sum of var^2) (assumes zero correlation)
            ROUND(SQRT(SUM(vc.var_95 * vc.var_95)), 2)       AS diversified_var_95,
            ROUND(SUM(vc.var_95) / NULLIF(ABS(SUM(vc.market_value)),0) * 100, 4)
                                                      AS var_pct_of_exposure
        FROM var_contributions vc
        GROUP BY vc.account_id, vc.account_name, vc.portfolio_id
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 8 — Portfolio-level VaR
-- Rolls up account_var (L7) to portfolio grain.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'portfolio_var',
    query    => $$
        SELECT
            av.portfolio_id,
            p.name                                    AS portfolio_name,
            p.manager,
            p.strategy,
            COUNT(DISTINCT av.account_id)             AS account_count,
            SUM(av.total_market_value)                AS total_market_value,
            SUM(av.total_pnl)                         AS total_pnl,
            SUM(av.total_var_95)                      AS sum_var_95,
            SUM(av.total_stressed_var_99)             AS sum_stressed_var_99,
            -- Portfolio-level diversified VaR
            ROUND(SQRT(SUM(av.diversified_var_95 * av.diversified_var_95)), 2)
                                                      AS portfolio_var_95,
            ROUND(SUM(av.total_var_95)
                  / NULLIF(ABS(SUM(av.total_market_value)), 0) * 100, 4)
                                                      AS var_pct_nav
        FROM account_var av
        JOIN portfolios  p ON p.id = av.portfolio_id
        GROUP BY av.portfolio_id, p.name, p.manager, p.strategy
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 9 — Regulatory capital requirement (simplified Basel III)
-- Reads portfolio_var (L8). Capital = 3 × 10-day VaR (scaling factor).
-- 10-day VaR ≈ 1-day VaR × sqrt(10) ≈ VaR × 3.162.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'regulatory_capital',
    query    => $$
        SELECT
            pv.portfolio_id,
            pv.portfolio_name,
            pv.manager,
            pv.strategy,
            pv.total_market_value,
            pv.total_pnl,
            pv.portfolio_var_95,
            -- Basel simplified: 10-day VaR × multiplier (3.0)
            ROUND(pv.portfolio_var_95 * 3.162 * 3.0, 2)       AS required_capital,
            -- Capital ratio: required_capital / total_market_value
            ROUND(
                pv.portfolio_var_95 * 3.162 * 3.0
                / NULLIF(ABS(pv.total_market_value), 0) * 100, 4)
                                                               AS capital_ratio_pct,
            -- Capital limits joined from accounts (avoids correlated subqueries)
            al.capital_limit,
            al.capital_limit
            - ROUND(pv.portfolio_var_95 * 3.162 * 3.0, 2)     AS capital_headroom,
            CASE
                WHEN ROUND(pv.portfolio_var_95 * 3.162 * 3.0, 2) > al.capital_limit
                THEN 'BREACH'
                WHEN ROUND(pv.portfolio_var_95 * 3.162 * 3.0, 2) > 0.8 * al.capital_limit
                THEN 'WARNING'
                ELSE 'OK'
            END                                                AS capital_status
        FROM portfolio_var pv
        JOIN (
            SELECT portfolio_id, SUM(capital_limit) AS capital_limit
            FROM accounts
            GROUP BY portfolio_id
        ) al ON al.portfolio_id = pv.portfolio_id
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);

-- ─────────────────────────────────────────────────────────────────────────────
-- LEVEL 10 — Risk breach dashboard (LIMIT 10)
-- Reads regulatory_capital (L9). Shows the top-10 most capital-stressed
-- portfolios. Fixed-cardinality output: typically 0–2 rows change per tick.
-- Change ratio ≈ 0.02–0.1 — DIFFERENTIAL SHOWCASE at the top of the DAG.
-- ─────────────────────────────────────────────────────────────────────────────

SELECT pgtrickle.create_stream_table(
    name     => 'breach_dashboard',
    query    => $$
        SELECT
            ROW_NUMBER() OVER (
                ORDER BY capital_ratio_pct DESC, portfolio_name
            )                                          AS rank,
            portfolio_name,
            manager,
            strategy,
            total_market_value,
            total_pnl,
            portfolio_var_95,
            required_capital,
            capital_limit,
            capital_headroom,
            ROUND(capital_ratio_pct, 2)                AS capital_ratio_pct,
            capital_status
        FROM regulatory_capital
        ORDER BY capital_ratio_pct DESC, portfolio_name
        LIMIT 10
    $$,
    schedule     => 'calculated',
    refresh_mode => 'DIFFERENTIAL'
);
