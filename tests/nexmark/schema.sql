-- =============================================================================
-- Nexmark Streaming Benchmark — Schema
--
-- Models an online auction system with three event types:
--   person  — registered users (sellers/bidders)
--   auction — items listed for sale
--   bid     — bids placed on auctions
--
-- Adapted from the Nexmark benchmark specification:
--   https://datalab.cs.pdx.edu/niagara/NEXMark/
--   https://github.com/nexmark/nexmark
--
-- Fair Use: This workload is derived from the Nexmark Benchmark specification
-- and is used for correctness validation, not competitive benchmarking.
-- =============================================================================

-- ── Persons ─────────────────────────────────────────────────────────────
CREATE TABLE person (
    id          BIGINT PRIMARY KEY,
    name        TEXT        NOT NULL,
    email       TEXT        NOT NULL,
    city        TEXT        NOT NULL,
    state       TEXT        NOT NULL,
    date_time   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ── Auctions ────────────────────────────────────────────────────────────
CREATE TABLE auction (
    id          BIGINT PRIMARY KEY,
    item_name   TEXT        NOT NULL,
    description TEXT        NOT NULL DEFAULT '',
    initial_bid BIGINT      NOT NULL,
    reserve     BIGINT      NOT NULL,
    seller      BIGINT      NOT NULL REFERENCES person(id),
    category    INT         NOT NULL,
    date_time   TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires     TIMESTAMPTZ NOT NULL
);

-- ── Bids ────────────────────────────────────────────────────────────────
CREATE TABLE bid (
    auction     BIGINT      NOT NULL REFERENCES auction(id),
    bidder      BIGINT      NOT NULL REFERENCES person(id),
    price       BIGINT      NOT NULL,
    channel     TEXT        NOT NULL DEFAULT 'web',
    url         TEXT        NOT NULL DEFAULT '',
    date_time   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Synthetic PK for pg_trickle row identity tracking
    bid_id      BIGSERIAL   PRIMARY KEY
);

-- Indexes for join performance
CREATE INDEX idx_bid_auction ON bid(auction);
CREATE INDEX idx_bid_bidder  ON bid(bidder);
CREATE INDEX idx_auction_seller ON auction(seller);
CREATE INDEX idx_auction_category ON auction(category);
