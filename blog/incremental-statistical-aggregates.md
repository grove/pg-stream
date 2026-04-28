[← Back to Blog Index](README.md)

# Incremental Statistical Aggregates: stddev, Percentiles, and Histograms

## Which higher-order statistics can be maintained incrementally, which need approximations, and what the trade-offs are

---

SUM and COUNT are easy to maintain incrementally. Add a row, increment the sum. Remove a row, decrement it. The math is trivial and the result is exact. But real analytics need more: standard deviations, percentiles, histograms, median values, entropy measures. These statistics have different mathematical properties, and not all of them decompose as cleanly as addition.

This post explores the landscape of statistical aggregates from the perspective of incremental maintenance. For each class of statistic, we'll cover: can it be maintained exactly? If not, what approximation is used? What's the space-accuracy trade-off? And how does pg_trickle handle it in practice?

---

## The Incrementability Spectrum

Statistical aggregates fall on a spectrum from "trivially incremental" to "fundamentally requires full data":

| Category | Examples | Incremental? |
|----------|----------|-------------|
| Decomposable | SUM, COUNT, MIN, MAX | Exact, O(1) per update |
| Algebraic | AVG, VARIANCE, STDDEV | Exact, O(1) per update (with auxiliary state) |
| Holistic | MEDIAN, MODE, arbitrary percentiles | Not exact without full data |
| Approximate | HyperLogLog (distinct count), t-digest (percentiles) | Bounded error, O(1) per update |

The key insight is that many aggregates that seem expensive are actually algebraic — they can be maintained with a fixed amount of auxiliary state, updated in constant time per change.

---

## Variance and Standard Deviation

Standard deviation looks like it requires a full pass over the data:

$$\sigma = \sqrt{\frac{1}{n} \sum_{i=1}^{n} (x_i - \bar{x})^2}$$

But expand the formula:

$$\sigma^2 = \frac{\sum x_i^2}{n} - \left(\frac{\sum x_i}{n}\right)^2$$

This means variance can be computed from three running aggregates: `SUM(x)`, `SUM(x*x)`, and `COUNT(*)`. All three are trivially incremental. When a row is inserted with value $v$:

- `sum_x += v`
- `sum_x2 += v*v`
- `count += 1`
- `variance = sum_x2/count - (sum_x/count)^2`
- `stddev = sqrt(variance)`

pg_trickle maintains STDDEV and VARIANCE using this decomposition. The stream table stores the auxiliary aggregates internally and derives the final statistic:

```sql
SELECT pgtrickle.create_stream_table(
    'price_volatility',
    $$
    SELECT
        product_category,
        AVG(price) AS avg_price,
        STDDEV(price) AS price_stddev,
        VARIANCE(price) AS price_variance,
        COUNT(*) AS sample_count
    FROM products
    GROUP BY product_category
    $$
);
```

Each product insert or price update adjusts the running sums for the affected category. The standard deviation is recomputed from the auxiliary state in O(1). No full scan required.

---

## Covariance and Correlation

Correlation between two variables is computed from their covariance:

$$r = \frac{\text{Cov}(X,Y)}{\sigma_X \cdot \sigma_Y}$$

And covariance decomposes similarly:

$$\text{Cov}(X,Y) = \frac{\sum x_i y_i}{n} - \frac{\sum x_i}{n} \cdot \frac{\sum y_i}{n}$$

Five running aggregates — `SUM(x)`, `SUM(y)`, `SUM(x*y)`, `SUM(x*x)`, `SUM(y*y)`, and `COUNT(*)` — give you everything needed for correlation. All are incremental.

```sql
SELECT pgtrickle.create_stream_table(
    'feature_correlations',
    $$
    SELECT
        sensor_type,
        CORR(temperature, humidity) AS temp_humidity_corr,
        COVAR_SAMP(temperature, pressure) AS temp_pressure_covar,
        REGR_SLOPE(power_output, wind_speed) AS power_wind_slope
    FROM sensor_readings
    GROUP BY sensor_type
    $$
);
```

Linear regression coefficients (`REGR_SLOPE`, `REGR_INTERCEPT`, `REGR_R2`) are all derived from the same five auxiliary aggregates. They're maintained incrementally at the same cost as a simple SUM.

---

## The Percentile Problem

Percentiles (including the median, which is the 50th percentile) are fundamentally different. The median of a set of numbers depends on the ordering of the entire set. You can't compute it from running sums — you need to know which value is at position n/2 in the sorted order.

When a new value is inserted, the median might shift. But determining whether it shifts (and to what) requires knowing the values around the current median. This makes exact incremental maintenance of percentiles expensive — it requires maintaining a sorted data structure (like a B-tree or skip list) with O(log n) insert and O(1) median lookup.

PostgreSQL's `PERCENTILE_CONT` and `PERCENTILE_DISC` are ordered-set aggregates that require full data access. pg_trickle cannot maintain them incrementally in the general case.

**The workaround: approximate percentiles with t-digest or quantile sketches.**

A t-digest is a compact data structure (~1KB) that estimates percentiles with bounded relative error. It supports incremental insertion and merging. You can maintain a t-digest per group, insert new values in O(log k) where k is the compression factor, and query any percentile in O(1).

```sql
-- Using pg_trickle with approximate percentiles (via extension)
SELECT pgtrickle.create_stream_table(
    'response_time_percentiles',
    $$
    SELECT
        endpoint,
        COUNT(*) AS request_count,
        AVG(latency_ms) AS avg_latency,
        STDDEV(latency_ms) AS stddev_latency,
        -- Exact aggregates maintained incrementally ↑
        -- For p50/p95/p99, use the materialized data with periodic full refresh
        MIN(latency_ms) AS min_latency,
        MAX(latency_ms) AS max_latency
    FROM requests
    GROUP BY endpoint
    $$
);
```

For many practical purposes, min, max, average, and standard deviation (all exactly incremental) give you enough to characterize the distribution. If you need percentiles, consider whether the approximation from assuming a normal distribution (mean ± 2σ ≈ 95th percentile) is sufficient for your use case.

---

## Histograms and Frequency Distributions

A histogram bins values into ranges and counts the frequency per bin. If the bin boundaries are fixed, a histogram is trivially incremental:

```sql
SELECT pgtrickle.create_stream_table(
    'latency_histogram',
    $$
    SELECT
        endpoint,
        CASE
            WHEN latency_ms < 10 THEN '0-10ms'
            WHEN latency_ms < 50 THEN '10-50ms'
            WHEN latency_ms < 100 THEN '50-100ms'
            WHEN latency_ms < 500 THEN '100-500ms'
            ELSE '500ms+'
        END AS bucket,
        COUNT(*) AS frequency
    FROM requests
    GROUP BY endpoint,
        CASE
            WHEN latency_ms < 10 THEN '0-10ms'
            WHEN latency_ms < 50 THEN '10-50ms'
            WHEN latency_ms < 100 THEN '50-100ms'
            WHEN latency_ms < 500 THEN '100-500ms'
            ELSE '500ms+'
        END
    $$
);
```

Each new request increments exactly one bucket for its endpoint. The cost is O(1) per insert. This gives you a live histogram that updates with every request — perfect for observability dashboards that show latency distributions in real time.

The key requirement is that bin boundaries are deterministic from the row values. If you use `width_bucket()` or a CASE expression with fixed thresholds, the histogram is incrementally maintainable. If you use adaptive binning (where boundaries shift based on the data distribution), you need a full recompute when boundaries change.

---

## Distinct Counts with HyperLogLog

Exact distinct counts (`COUNT(DISTINCT x)`) are maintained by pg_trickle using reference counting — tracking how many times each distinct value appears. When the count drops to zero, the distinct count decreases. This is exact but requires space proportional to the number of distinct values.

For very high cardinality (millions of distinct values), this becomes expensive. HyperLogLog (HLL) approximates distinct counts in a fixed ~1.3KB of space with ~2% relative error. It supports incremental insertion (add a value to the sketch in O(1)) but not deletion (you can't remove a value from an HLL sketch).

For stream tables where rows are only inserted (append-only workloads), HLL is a perfect fit. For workloads with deletions, exact reference counting is needed for correctness, and pg_trickle provides it.

```sql
-- Exact distinct counts (incrementally maintained with reference counting)
SELECT pgtrickle.create_stream_table(
    'daily_unique_visitors',
    $$
    SELECT
        date_trunc('day', visited_at) AS day,
        COUNT(DISTINCT visitor_id) AS unique_visitors,
        COUNT(*) AS total_pageviews
    FROM pageviews
    GROUP BY date_trunc('day', visited_at)
    $$
);
```

---

## Exponentially Weighted Moving Averages

EWMA is used extensively in monitoring (Prometheus uses it) and financial analysis. The formula is:

$$\text{EWMA}_t = \alpha \cdot x_t + (1 - \alpha) \cdot \text{EWMA}_{t-1}$$

This is inherently sequential — each value depends on the previous EWMA. It's not decomposable, but it is naturally incremental: each new value updates the EWMA in O(1) with no historical data access.

In SQL, EWMA can be expressed as a window function or, for the latest value only, as a recursive computation. Stream tables can maintain the latest EWMA per entity:

```sql
SELECT pgtrickle.create_stream_table(
    'sensor_ewma',
    $$
    SELECT
        sensor_id,
        -- Latest reading weighted against historical exponential average
        SUM(value * POWER(0.1, ROW_NUMBER() OVER (PARTITION BY sensor_id ORDER BY ts DESC) - 1))
            / SUM(POWER(0.1, ROW_NUMBER() OVER (PARTITION BY sensor_id ORDER BY ts DESC) - 1))
            AS ewma_value
    FROM sensor_readings
    GROUP BY sensor_id
    $$
);
```

---

## Decision Guide

When choosing how to implement a statistical aggregate as a stream table:

| Statistic | Strategy | Accuracy | Space per group |
|-----------|----------|----------|-----------------|
| SUM, COUNT, MIN, MAX | Direct incremental | Exact | O(1) |
| AVG, VARIANCE, STDDEV | Auxiliary sums | Exact | O(1) |
| CORR, COVAR, REGR_* | Auxiliary sums | Exact | O(1) |
| COUNT(DISTINCT) | Reference counting | Exact | O(distinct values) |
| Histogram (fixed bins) | GROUP BY bucket | Exact | O(bins) |
| PERCENTILE | Full refresh or approximation | Exact or ~2% error | O(n) or O(1) |
| MODE | Full refresh | Exact | O(distinct values) |
| EWMA | Sequential update | Exact | O(1) |

The takeaway: most statistics that data engineers use daily (mean, variance, stddev, correlation, histograms) are exactly maintainable with O(1) cost per update. The exceptions are order statistics (percentiles, median, mode) which require either full data access or approximation data structures.

---

*pg_trickle gives you exact, live statistics for the aggregates that matter most — and honest about the ones that need approximation. Know which is which, and design your analytics accordingly.*
