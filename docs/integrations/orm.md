# ORM Integration Guides

pg_trickle stream tables are read-only materialized views that refresh
automatically. This page documents how to use stream tables from popular
Python ORMs — SQLAlchemy and Django ORM.

## Key Principles

1. **Stream tables are read-only.** All writes go to the source tables;
   pg_trickle refreshes stream tables in the background.
2. **Model stream tables as views**, not regular tables. ORMs should never
   attempt `INSERT`, `UPDATE`, or `DELETE` on a stream table.
3. **Internal columns are hidden.** The `__pgt_row_id` column used for
   incremental maintenance is excluded from `SELECT *` queries.

---

## SQLAlchemy

### Read-Only Model Definition

Map a stream table as a read-only model using `__table_args__`:

```python
from sqlalchemy import Column, Integer, Numeric, String, BigInteger
from sqlalchemy.orm import DeclarativeBase

class Base(DeclarativeBase):
    pass

class OrderTotals(Base):
    """Read-only model backed by pg_trickle stream table."""
    __tablename__ = "order_totals"
    
    # Map the stream table's row ID as primary key for ORM identity
    __pgt_row_id = Column("__pgt_row_id", BigInteger, primary_key=True)
    
    region = Column(String, nullable=False)
    order_count = Column(BigInteger, nullable=False)
    total = Column(Numeric(10, 2), nullable=False)
    
    __table_args__ = {
        "info": {"readonly": True},  # Convention marker
    }
```

### Querying

Query stream tables like any other SQLAlchemy model:

```python
from sqlalchemy import select

# All regions
stmt = select(OrderTotals).order_by(OrderTotals.total.desc())
results = session.execute(stmt).scalars().all()

# Filtered
stmt = (
    select(OrderTotals)
    .where(OrderTotals.order_count > 10)
    .where(OrderTotals.region == "East")
)
row = session.execute(stmt).scalar_one_or_none()
```

### Preventing Accidental Writes

Use SQLAlchemy events to block write operations:

```python
from sqlalchemy import event

READONLY_TABLES = {"order_totals", "daily_revenue", "customer_stats"}

@event.listens_for(session, "before_flush")
def block_stream_table_writes(session, flush_context, instances):
    for obj in session.new | session.dirty | session.deleted:
        table_name = obj.__class__.__tablename__
        if table_name in READONLY_TABLES:
            raise RuntimeError(
                f"Cannot write to stream table '{table_name}'. "
                f"Write to the source table instead."
            )
```

### Reflecting Stream Tables

If you prefer reflection over explicit models:

```python
from sqlalchemy import MetaData, Table, create_engine

engine = create_engine("postgresql://...")
metadata = MetaData()

# Reflect the stream table (treated as a regular table by PostgreSQL)
order_totals = Table("order_totals", metadata, autoload_with=engine)

# Query
with engine.connect() as conn:
    result = conn.execute(order_totals.select().limit(10))
    for row in result:
        print(row)
```

### Checking Freshness

Query the stream table's metadata to check when it was last refreshed:

```python
from sqlalchemy import text

def get_staleness(session, st_name: str) -> dict:
    """Return freshness info for a stream table."""
    result = session.execute(
        text("SELECT * FROM pgtrickle.get_staleness(:name)"),
        {"name": st_name},
    ).mappings().one()
    return dict(result)

# Usage
staleness = get_staleness(session, "order_totals")
print(f"Last refresh: {staleness['data_timestamp']}")
print(f"Stale for: {staleness['staleness_seconds']}s")
```

### Async SQLAlchemy (2.0+)

Works identically with `async_session`:

```python
from sqlalchemy.ext.asyncio import AsyncSession

async def get_top_regions(session: AsyncSession, limit: int = 10):
    stmt = (
        select(OrderTotals)
        .order_by(OrderTotals.total.desc())
        .limit(limit)
    )
    result = await session.execute(stmt)
    return result.scalars().all()
```

---

## Django ORM

### Read-Only Model Definition

Use `managed = False` so Django never creates, alters, or drops the table:

```python
# models.py
from django.db import models

class OrderTotals(models.Model):
    """Read-only model backed by pg_trickle stream table."""
    
    region = models.CharField(max_length=255)
    order_count = models.BigIntegerField()
    total = models.DecimalField(max_digits=10, decimal_places=2)
    
    class Meta:
        managed = False        # Django will not create/alter this table
        db_table = "order_totals"
    
    def save(self, *args, **kwargs):
        raise NotImplementedError("Stream tables are read-only")
    
    def delete(self, *args, **kwargs):
        raise NotImplementedError("Stream tables are read-only")
```

### Querying

Standard Django QuerySet operations work:

```python
# All regions sorted by total
OrderTotals.objects.all().order_by("-total")

# Filtered
OrderTotals.objects.filter(
    order_count__gt=10,
    region="East"
).first()

# Aggregation (on the stream table itself)
from django.db.models import Sum, Avg
OrderTotals.objects.aggregate(
    total_revenue=Sum("total"),
    avg_orders=Avg("order_count"),
)
```

### Django Migrations

Since `managed = False`, Django migrations won't touch stream tables.
Create stream tables in a custom migration using `RunSQL`:

```python
# migrations/0003_create_stream_tables.py
from django.db import migrations

class Migration(migrations.Migration):
    dependencies = [
        ("myapp", "0002_create_orders_table"),
    ]

    operations = [
        migrations.RunSQL(
            sql="""
                SELECT pgtrickle.create_stream_table(
                    'order_totals',
                    $pgt$SELECT region,
                                COUNT(*) AS order_count,
                                SUM(amount) AS total
                         FROM orders GROUP BY region$pgt$,
                    schedule     => '5s',
                    refresh_mode => 'DIFFERENTIAL'
                );
            """,
            reverse_sql="""
                SELECT pgtrickle.drop_stream_table('order_totals');
            """,
        ),
    ]
```

### Read-Only Mixin

Create a reusable mixin for all stream table models:

```python
class StreamTableMixin(models.Model):
    """Base class for pg_trickle stream table models."""
    
    class Meta:
        abstract = True
        managed = False
    
    def save(self, *args, **kwargs):
        raise NotImplementedError(
            f"{self.__class__.__name__} is a read-only stream table. "
            f"Write to the source table instead."
        )
    
    def delete(self, *args, **kwargs):
        raise NotImplementedError(
            f"{self.__class__.__name__} is a read-only stream table."
        )

# Usage
class OrderTotals(StreamTableMixin):
    region = models.CharField(max_length=255)
    order_count = models.BigIntegerField()
    total = models.DecimalField(max_digits=10, decimal_places=2)
    
    class Meta(StreamTableMixin.Meta):
        db_table = "order_totals"

class DailyRevenue(StreamTableMixin):
    day = models.DateField()
    revenue = models.DecimalField(max_digits=12, decimal_places=2)
    
    class Meta(StreamTableMixin.Meta):
        db_table = "daily_revenue"
```

### Checking Freshness

Use raw SQL to query pg_trickle diagnostics:

```python
from django.db import connection

def get_staleness(st_name: str) -> dict:
    """Return freshness info for a stream table."""
    with connection.cursor() as cursor:
        cursor.execute(
            "SELECT * FROM pgtrickle.get_staleness(%s)", [st_name]
        )
        columns = [col.name for col in cursor.description]
        row = cursor.fetchone()
        return dict(zip(columns, row)) if row else {}
```

### Django REST Framework

Stream table models work with DRF serializers and viewsets:

```python
from rest_framework import serializers, viewsets

class OrderTotalsSerializer(serializers.ModelSerializer):
    class Meta:
        model = OrderTotals
        fields = ["region", "order_count", "total"]

class OrderTotalsViewSet(viewsets.ReadOnlyModelViewSet):
    """Read-only API endpoint for order totals stream table."""
    queryset = OrderTotals.objects.all()
    serializer_class = OrderTotalsSerializer
```

---

## Common Patterns

### Write to Source, Read from Stream

The fundamental pattern: all writes go to source tables (normal ORM models),
reads come from stream tables (read-only models).

```python
# Write to source table (normal ORM)
order = Order(region="East", amount=Decimal("99.99"))
session.add(order)
session.commit()

# Read from stream table (auto-refreshed by pg_trickle)
totals = session.execute(
    select(OrderTotals).where(OrderTotals.region == "East")
).scalar_one()
print(f"East: {totals.order_count} orders, ${totals.total}")
```

### Handling Eventual Consistency

Stream tables refresh on a schedule (e.g., every 5 seconds). After writing
to a source table, the stream table may be briefly stale. Options:

1. **Accept staleness** — suitable for dashboards and reports.
2. **Force refresh** — call `pgtrickle.refresh_stream_table()` after critical writes.
3. **Use IMMEDIATE mode** — stream table refreshes within the same transaction.

```python
# Option 2: Force refresh after a critical write
session.execute(text(
    "SELECT pgtrickle.refresh_stream_table('order_totals')"
))
```

---

## Further Reading

- [SQL Reference](../SQL_REFERENCE.md) — Complete function reference
- [Configuration](../CONFIGURATION.md) — Schedule tuning and refresh modes
- [Getting Started](../GETTING_STARTED.md) — First stream table walkthrough
- [dbt Integration](dbt.md) — Using pg_trickle with dbt
