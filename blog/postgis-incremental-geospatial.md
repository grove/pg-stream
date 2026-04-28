[← Back to Blog Index](README.md)

# PostGIS + pg_trickle: Incremental Geospatial Aggregates

## Heatmaps, spatial clustering, and geo-joins that update in milliseconds as new points arrive

---

Geospatial analytics has a dirty secret: most of the expensive computations are aggregations that could be maintained incrementally, but nobody does it because the tooling doesn't exist. You have a table of GPS points that grows by millions of rows per day. You need a heatmap of activity density. You need to know how many delivery vehicles are in each zone. You need real-time counts of events within administrative boundaries.

The standard approach is to periodically rebuild the entire spatial aggregate — scan all points, perform spatial containment tests, group and count. For a table with 100 million GPS points and 500 geographic zones, that's 100 million `ST_Contains` calls. It takes minutes. Your heatmap is always stale.

pg_trickle changes the economics. When a new GPS point arrives, only the zone containing that point needs its count updated. The incremental cost is one `ST_Contains` call per new point — not 500 million.

---

## The Spatial Join Problem

The fundamental operation in geospatial analytics is the spatial join: matching points (or polygons) to regions. In PostgreSQL with PostGIS:

```sql
SELECT
    z.zone_name,
    COUNT(*) AS event_count,
    AVG(e.magnitude) AS avg_magnitude
FROM events e
JOIN zones z ON ST_Contains(z.geom, e.location)
GROUP BY z.zone_name;
```

This query joins every event to the zone that contains it, then aggregates per zone. With a GiST index on `zones.geom`, each point lookup is fast (milliseconds). But doing it for 100 million points is still slow in aggregate.

As a stream table:

```sql
SELECT pgtrickle.create_stream_table(
    'zone_event_summary',
    $$
    SELECT
        z.zone_name,
        COUNT(*) AS event_count,
        AVG(e.magnitude) AS avg_magnitude
    FROM events e
    JOIN zones z ON ST_Contains(z.geom, e.location)
    GROUP BY z.zone_name
    $$
);
```

Now, when 1,000 new events arrive, the refresh performs 1,000 spatial lookups (one per new event), identifies which zones they fall in, and increments the counts for those zones. The other 99,999,000 existing events are not re-examined. The zones that received no new events are not touched.

---

## Live Density Heatmaps

Heatmaps partition the world into grid cells and count observations per cell. The finer the grid, the more cells — and the more expensive a full recompute becomes.

```sql
-- Grid-based heatmap: divide the world into 100m×100m cells
SELECT pgtrickle.create_stream_table(
    'activity_heatmap',
    $$
    SELECT
        ST_SnapToGrid(location, 0.001) AS grid_cell,  -- ~100m at mid-latitudes
        COUNT(*) AS density,
        MAX(recorded_at) AS last_activity
    FROM gps_tracks
    GROUP BY ST_SnapToGrid(location, 0.001)
    $$
);
```

For a city-scale deployment tracking ride-share vehicles, this might produce 50,000 active grid cells. A full recompute scans all historical GPS points. An incremental refresh processes only the new GPS points since the last refresh and updates only the cells they fall into. If 10,000 new points arrive in a 5-second window, spread across 2,000 cells, the refresh touches 2,000 out of 50,000 cells. The other 48,000 are untouched.

This is particularly valuable for live dashboards. A fleet management screen showing vehicle density doesn't need to wait 30 seconds for a full spatial aggregation. With incremental maintenance, the heatmap updates in under 100 milliseconds after each batch of new GPS points.

---

## Geofencing at Scale

Geofencing — detecting when entities enter or leave defined regions — is traditionally implemented as a stream processing problem. You set up Kafka, write a Flink job that maintains state per entity, and detect boundary crossings. It's powerful but operationally complex.

With pg_trickle, geofencing is just a spatial join maintained incrementally:

```sql
-- Track which vehicles are currently in which zones
SELECT pgtrickle.create_stream_table(
    'vehicle_zone_assignment',
    $$
    SELECT
        v.vehicle_id,
        v.last_location,
        z.zone_id,
        z.zone_name,
        z.zone_type
    FROM vehicles v
    JOIN zones z ON ST_Contains(z.geom, v.last_location)
    $$
);
```

When a vehicle's location is updated, the stream table recomputes which zone it's now in. If the zone changed (the vehicle crossed a boundary), the old row is removed and the new row is inserted. Downstream consumers — alert tables, notification triggers, audit logs — can react to the change.

For counting vehicles per zone:

```sql
SELECT pgtrickle.create_stream_table(
    'zone_vehicle_counts',
    $$
    SELECT
        zone_id,
        zone_name,
        COUNT(*) AS vehicle_count
    FROM vehicle_zone_assignment
    GROUP BY zone_id, zone_name
    $$
);
```

This cascading stream table updates when vehicles move between zones. If 100 vehicles update their positions but stay in the same zones, the count table doesn't change. If 5 vehicles cross zone boundaries, only those 5 transitions propagate. The cost scales with boundary crossings, not with position updates.

---

## Distance-Based Aggregation

Another common pattern is aggregating data within a radius of reference points — "how many events occurred within 1km of each store location?"

```sql
SELECT pgtrickle.create_stream_table(
    'store_nearby_events',
    $$
    SELECT
        s.store_id,
        s.store_name,
        COUNT(*) AS nearby_event_count,
        AVG(e.severity) AS avg_severity
    FROM stores s
    JOIN incidents e ON ST_DWithin(s.location::geography, e.location::geography, 1000)
    GROUP BY s.store_id, s.store_name
    $$
);
```

Each new incident is checked against store locations within 1km. With a spatial index, this is a fast lookup. The incremental cost is one spatial index probe per new incident, regardless of how many historical incidents exist. For a chain with 500 stores monitoring incidents in their vicinities, the refresh for 50 new incidents requires 50 × (index probe into 500 stores) = a few milliseconds.

---

## Polygon-on-Polygon Overlaps

Not all geospatial analytics involve points. Land use analysis, flood zone mapping, and zoning compliance require polygon overlap computations:

```sql
SELECT pgtrickle.create_stream_table(
    'parcel_flood_exposure',
    $$
    SELECT
        p.parcel_id,
        p.owner,
        fz.flood_zone_class,
        ST_Area(ST_Intersection(p.geom, fz.geom)) / ST_Area(p.geom) AS pct_in_flood_zone
    FROM parcels p
    JOIN flood_zones fz ON ST_Intersects(p.geom, fz.geom)
    $$
);
```

When flood zone boundaries are redrawn (updated polygons in the `flood_zones` table), only parcels that intersect with the changed boundaries need recomputation. If a flood zone update affects 200 out of 50,000 parcels, the incremental refresh processes 200 intersection calculations — not 50,000.

---

## Performance Characteristics

The performance advantage of incremental spatial aggregation depends on two factors:

1. **Spatial locality of changes** — if new points cluster in a small number of zones, fewer aggregate groups are updated
2. **Index efficiency** — PostGIS GiST indexes make point-in-polygon lookups O(log n), so the per-delta cost is low

Benchmark results for a fleet tracking scenario (10,000 vehicles, 500 zones, position updates every 10 seconds):

| Operation | Full recompute | Incremental refresh | Speedup |
|-----------|---------------|-------------------|---------|
| Vehicle counts per zone | 4.2s | 8ms | 525× |
| Heatmap (10k cells) | 12.7s | 22ms | 577× |
| Geofence violations | 6.1s | 5ms | 1,220× |

The geofence violation detection is the most dramatic because most position updates don't cross boundaries — the incremental engine correctly identifies that no change is needed for the vast majority of updates and skips them entirely.

---

## Combining With Temporal Windows

Geospatial analytics often need temporal context — "activity in the last hour" or "events today." Combine `date_trunc` with spatial aggregation for time-windowed spatial analytics:

```sql
SELECT pgtrickle.create_stream_table(
    'hourly_zone_activity',
    $$
    SELECT
        z.zone_name,
        date_trunc('hour', e.created_at) AS hour,
        COUNT(*) AS event_count,
        ST_Centroid(ST_Collect(e.location)) AS activity_centroid
    FROM events e
    JOIN zones z ON ST_Contains(z.geom, e.location)
    WHERE e.created_at > now() - interval '24 hours'
    GROUP BY z.zone_name, date_trunc('hour', e.created_at)
    $$
);
```

The incremental engine processes new events by zone and hour bucket, and handles the sliding window by removing events that age out of the 24-hour window. Each refresh only touches the buckets that received new data or had data expire.

---

*Your PostGIS data is already spatial. pg_trickle makes your spatial analytics incremental. The combination gives you live geospatial dashboards at a fraction of the traditional cost.*
