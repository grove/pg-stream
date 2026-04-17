-- =============================================================================
-- pg_trickle Real-time Financial Risk Pipeline Demo — Schema & Seed Data
--
-- DAG topology (10 stream table levels, deep cascade):
--
--   market_prices ──┐  (CALCULATED, ~2s tick)
--   trades ─────────┤  (CALCULATED, ~1s)  ← only LEAF tables have CALCULATED schedules
--                   │
--   L2:  net_positions         (DIFFERENTIAL, calculated — depends on trades, instruments)
--   L3:  position_values       (DIFFERENTIAL, calculated — positions × live price)
--   L4:  account_pnl           (DIFFERENTIAL, calculated — P&L per account)
--   L5:  portfolio_pnl         (DIFFERENTIAL, calculated — P&L per portfolio)
--   L6:  sector_exposure       (DIFFERENTIAL, calculated — notional by sector)
--   L7:  var_contributions     (DIFFERENTIAL, calculated — VaR contribution per position)
--   L8:  portfolio_var         (DIFFERENTIAL, calculated — portfolio-level 95% VaR estimate)
--   L9:  regulatory_capital    (DIFFERENTIAL, calculated — Basel simplified k×VaR capital)
--   L10: breach_dashboard      (DIFFERENTIAL, calculated — top 10 capital-limit breaches)
--
-- Differential efficiency story:
--   500 instruments × 50 accounts → 25,000 potential positions.
--   Each price tick touches only the ~20 positions in that instrument.
--   Change ratio ≈ 20/25000 = 0.0008 — deep DIFFERENTIAL propagation.
-- =============================================================================

CREATE EXTENSION IF NOT EXISTS pg_trickle;

-- ── Reference: sectors ────────────────────────────────────────────────────────

CREATE TABLE sectors (
    id   BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

-- ── Reference: instruments (stocks/ETFs traded) ───────────────────────────────

CREATE TABLE instruments (
    id          BIGSERIAL     PRIMARY KEY,
    ticker      TEXT          NOT NULL UNIQUE,
    name        TEXT          NOT NULL,
    sector_id   BIGINT        NOT NULL REFERENCES sectors(id),
    asset_class TEXT          NOT NULL DEFAULT 'EQUITY'
                              CHECK (asset_class IN ('EQUITY','BOND','ETF','COMMODITY')),
    base_price  NUMERIC(12,4) NOT NULL
);

-- ── Reference: accounts ───────────────────────────────────────────────────────

CREATE TABLE accounts (
    id             BIGSERIAL PRIMARY KEY,
    name           TEXT NOT NULL,
    portfolio_id   BIGINT NOT NULL,
    capital_limit  NUMERIC(14,2) NOT NULL DEFAULT 10000000.00
);

-- ── Reference: portfolios ─────────────────────────────────────────────────────

CREATE TABLE portfolios (
    id           BIGSERIAL PRIMARY KEY,
    name         TEXT NOT NULL,
    manager      TEXT NOT NULL,
    strategy     TEXT NOT NULL
);

-- ── Append-only trade stream — generator inserts here continuously ─────────────
-- Each row is a filled order: buy (quantity > 0) or sell (quantity < 0).

CREATE TABLE trades (
    id            BIGSERIAL      PRIMARY KEY,
    account_id    BIGINT         NOT NULL REFERENCES accounts(id),
    instrument_id BIGINT         NOT NULL REFERENCES instruments(id),
    quantity      NUMERIC(14,4)  NOT NULL,   -- positive = buy, negative = sell
    price         NUMERIC(12,4)  NOT NULL,
    traded_at     TIMESTAMPTZ    NOT NULL DEFAULT now()
);

-- ── Slowly-changing market price feed ─────────────────────────────────────────
-- The generator ticks one instrument's price every ~2 seconds.
-- Only L3 position_values re-processes the ~20 affected rows — not all 25,000.

CREATE TABLE market_prices (
    instrument_id BIGINT        PRIMARY KEY REFERENCES instruments(id),
    bid           NUMERIC(12,4) NOT NULL,
    ask           NUMERIC(12,4) NOT NULL,
    mid           NUMERIC(12,4) GENERATED ALWAYS AS ((bid + ask) / 2) STORED,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT now()
);

-- ── Seed: sectors ─────────────────────────────────────────────────────────────

INSERT INTO sectors (name) VALUES
    ('Technology'),
    ('Finance'),
    ('Healthcare'),
    ('Energy'),
    ('Consumer'),
    ('Industrials'),
    ('Materials'),
    ('Utilities');

-- ── Seed: portfolios ──────────────────────────────────────────────────────────

INSERT INTO portfolios (name, manager, strategy) VALUES
    ('Alpha Growth',     'Alice Chen',    'MOMENTUM'),
    ('Beta Income',      'Bob Kumar',     'VALUE'),
    ('Gamma Balanced',   'Carlos Rivera', 'BALANCED'),
    ('Delta Quant',      'Diana Park',    'QUANT'),
    ('Epsilon Macro',    'Ethan White',   'MACRO');

-- ── Seed: accounts (10 per portfolio = 50 total) ──────────────────────────────

INSERT INTO accounts (name, portfolio_id, capital_limit) VALUES
    -- Portfolio 1: Alpha Growth
    ('AG-US-Equity-001',  1, 12000000),
    ('AG-US-Equity-002',  1,  8000000),
    ('AG-EU-Equity-001',  1,  6000000),
    ('AG-APAC-001',       1, 10000000),
    ('AG-Sector-Tech-001',1, 15000000),
    ('AG-Sector-Fin-001', 1,  7000000),
    ('AG-Sector-HC-001',  1,  5000000),
    ('AG-Sector-Eng-001', 1,  4000000),
    ('AG-Sector-Con-001', 1,  6000000),
    ('AG-Sector-Ind-001', 1,  3000000),
    -- Portfolio 2: Beta Income
    ('BI-Bonds-001',      2,  9000000),
    ('BI-Bonds-002',      2,  7000000),
    ('BI-Dividend-001',   2, 11000000),
    ('BI-Dividend-002',   2,  8000000),
    ('BI-EU-Value-001',   2,  5000000),
    ('BI-EU-Value-002',   2,  4000000),
    ('BI-APAC-001',       2,  6000000),
    ('BI-REIT-001',       2, 10000000),
    ('BI-Utilities-001',  2,  7000000),
    ('BI-Materials-001',  2,  3000000),
    -- Portfolio 3: Gamma Balanced
    ('GB-Core-001',       3, 14000000),
    ('GB-Core-002',       3, 11000000),
    ('GB-Fixed-001',      3,  9000000),
    ('GB-Fixed-002',      3,  6000000),
    ('GB-Intl-001',       3,  8000000),
    ('GB-Intl-002',       3,  5000000),
    ('GB-Small-001',      3,  7000000),
    ('GB-Small-002',      3,  4000000),
    ('GB-Alt-001',        3, 12000000),
    ('GB-Alt-002',        3,  3000000),
    -- Portfolio 4: Delta Quant
    ('DQ-StatArb-001',    4, 20000000),
    ('DQ-StatArb-002',    4, 18000000),
    ('DQ-MktNeutral-001', 4, 16000000),
    ('DQ-MktNeutral-002', 4, 14000000),
    ('DQ-Momentum-001',   4, 12000000),
    ('DQ-Momentum-002',   4, 10000000),
    ('DQ-ML-001',         4, 22000000),
    ('DQ-ML-002',         4, 19000000),
    ('DQ-Risk-001',       4,  8000000),
    ('DQ-Risk-002',       4,  6000000),
    -- Portfolio 5: Epsilon Macro
    ('EM-FX-001',         5, 25000000),
    ('EM-FX-002',         5, 20000000),
    ('EM-Rates-001',      5, 30000000),
    ('EM-Rates-002',      5, 28000000),
    ('EM-Cmdty-001',      5, 15000000),
    ('EM-Cmdty-002',      5, 12000000),
    ('EM-EM-001',         5, 18000000),
    ('EM-EM-002',         5, 14000000),
    ('EM-Credit-001',     5, 22000000),
    ('EM-Credit-002',     5, 17000000);

-- ── Seed: instruments (30 instruments across sectors) ─────────────────────────

INSERT INTO instruments (ticker, name, sector_id, asset_class, base_price) VALUES
    -- Technology (sector 1)
    ('AAPL',  'Apple Inc.',            1, 'EQUITY',    185.00),
    ('MSFT',  'Microsoft Corp.',       1, 'EQUITY',    415.00),
    ('NVDA',  'NVIDIA Corp.',          1, 'EQUITY',    875.00),
    ('GOOGL', 'Alphabet Inc.',         1, 'EQUITY',    175.00),
    ('META',  'Meta Platforms',        1, 'EQUITY',    525.00),
    -- Finance (sector 2)
    ('JPM',   'JPMorgan Chase',        2, 'EQUITY',    205.00),
    ('GS',    'Goldman Sachs',         2, 'EQUITY',    490.00),
    ('MS',    'Morgan Stanley',        2, 'EQUITY',    105.00),
    ('BAC',   'Bank of America',       2, 'EQUITY',     40.00),
    ('V',     'Visa Inc.',             2, 'EQUITY',    280.00),
    -- Healthcare (sector 3)
    ('JNJ',   'Johnson & Johnson',     3, 'EQUITY',    155.00),
    ('UNH',   'UnitedHealth Group',    3, 'EQUITY',    510.00),
    ('PFE',   'Pfizer Inc.',           3, 'EQUITY',     28.00),
    ('ABBV',  'AbbVie Inc.',           3, 'EQUITY',    175.00),
    ('MRK',   'Merck & Co.',           3, 'EQUITY',    130.00),
    -- Energy (sector 4)
    ('XOM',   'Exxon Mobil',           4, 'EQUITY',    115.00),
    ('CVX',   'Chevron Corp.',         4, 'EQUITY',    155.00),
    ('COP',   'ConocoPhillips',        4, 'EQUITY',    120.00),
    ('SLB',   'SLB',                   4, 'EQUITY',     45.00),
    ('EOG',   'EOG Resources',         4, 'EQUITY',    130.00),
    -- Consumer (sector 5)
    ('AMZN',  'Amazon.com',            5, 'EQUITY',    195.00),
    ('TSLA',  'Tesla Inc.',            5, 'EQUITY',    175.00),
    ('WMT',   'Walmart Inc.',          5, 'EQUITY',     85.00),
    ('PG',    'Procter & Gamble',      5, 'EQUITY',    165.00),
    ('KO',    'Coca-Cola Co.',         5, 'EQUITY',     62.00),
    -- Industrials (sector 6)
    ('CAT',   'Caterpillar Inc.',      6, 'EQUITY',    380.00),
    ('HON',   'Honeywell Intl.',       6, 'EQUITY',    205.00),
    -- Materials (sector 7)
    ('LIN',   'Linde PLC',            7, 'EQUITY',    470.00),
    ('NEM',   'Newmont Corp.',         7, 'EQUITY',     42.00),
    -- Utilities / Bond ETF (sector 8)
    ('NEE',   'NextEra Energy',        8, 'EQUITY',     70.00);

-- ── Seed: initial market prices ──────────────────────────────────────────────
-- Start mid = base_price with 0.05% bid/ask spread.

INSERT INTO market_prices (instrument_id, bid, ask)
SELECT
    id,
    ROUND(base_price * 0.9995, 4),
    ROUND(base_price * 1.0005, 4)
FROM instruments;
