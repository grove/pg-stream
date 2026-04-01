# CloudNativePG / Kubernetes

pg_trickle is designed to work with [CloudNativePG](https://cloudnative-pg.io/)
(CNPG) — the Kubernetes operator for PostgreSQL. The extension is loaded via
**Image Volume Extensions**, meaning no custom PostgreSQL image is needed.

## Prerequisites

- Kubernetes 1.33+ with the ImageVolume feature gate enabled
- CloudNativePG operator 1.28+
- The `pg_trickle-ext` OCI image available in your cluster registry

## Architecture

```
┌─────────────────────────────────────┐
│  CNPG Cluster (3 pods)              │
│                                     │
│  ┌──────────┐  ┌──────────────────┐ │
│  │ Primary  │  │ pg_trickle-ext   │ │
│  │ PG 18    │◄─┤ (ImageVolume)    │ │
│  │          │  │ .so + .sql only  │ │
│  └──────────┘  └──────────────────┘ │
│  ┌──────────┐  ┌──────────┐        │
│  │ Replica 1│  │ Replica 2│        │
│  │ (standby)│  │ (standby)│        │
│  └──────────┘  └──────────┘        │
└─────────────────────────────────────┘
```

- The **scheduler runs on the primary pod only**. Replica pods detect recovery
  mode (`pg_is_in_recovery() = true`) and sleep.
- Stream tables are replicated to standbys via physical streaming replication
  like any other heap table.
- Pod restarts are safe — the scheduler resumes from the stored frontier with
  no data loss.

## Deploying pg_trickle on CNPG

### 1. Build the extension image

The `cnpg/Dockerfile.ext` builds a scratch-based OCI image containing
only the shared library, control file, and SQL migrations:

```bash
# From the dist/ directory with pre-built artifacts:
docker build -t ghcr.io/<owner>/pg_trickle-ext:0.13.0 -f cnpg/Dockerfile.ext dist/
docker push ghcr.io/<owner>/pg_trickle-ext:0.13.0
```

### 2. Deploy the Cluster

Apply the Cluster manifest with pg_trickle configured as an Image Volume
extension:

```yaml
# cnpg/cluster-example.yaml
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: pg-trickle-demo
spec:
  instances: 3
  imageName: ghcr.io/cloudnative-pg/postgresql:18

  postgresql:
    shared_preload_libraries:
      - pg_trickle
    extensions:
      - name: pg-trickle
        image:
          reference: ghcr.io/<owner>/pg_trickle-ext:0.13.0
    parameters:
      max_worker_processes: "8"

  bootstrap:
    initdb:
      database: app
      owner: app

  storage:
    size: 10Gi
    storageClass: standard
```

```bash
kubectl apply -f cnpg/cluster-example.yaml
```

### 3. Enable the extension

Use the CNPG Database resource for declarative extension management:

```yaml
# cnpg/database-example.yaml
apiVersion: postgresql.cnpg.io/v1
kind: Database
metadata:
  name: app
spec:
  cluster:
    name: pg-trickle-demo
  name: app
  owner: app
  extensions:
    - name: pg_trickle
```

```bash
kubectl apply -f cnpg/database-example.yaml
```

### 4. Verify

```bash
kubectl exec -it pg-trickle-demo-1 -- psql -U postgres -d app -c \
  "SELECT pgtrickle.version();"
```

## Key Considerations

### Worker processes

Each database with pg_trickle needs one background worker slot. Set
`max_worker_processes` in the Cluster manifest to accommodate the launcher
(1) + one scheduler per database + any parallel refresh workers:

```yaml
parameters:
  max_worker_processes: "16"
```

### Persistent volumes

Catalog tables (`pgtrickle.pgt_stream_tables`) and change buffers
(`pgtrickle_changes.*`) are stored in regular PostgreSQL tablespaces.
Persistent volume claims preserve them across pod rescheduling.

### Backups

pg_trickle state (catalog, change buffers, stream table data) is included
in CNPG's Barman object-store backups automatically. After a restore,
the scheduler detects frontier inconsistencies and performs a full refresh
on the first cycle. See [Backup and Restore](../BACKUP_AND_RESTORE.md) for
details.

### Failover

When the primary pod fails and a replica is promoted, the new primary's
scheduler starts automatically. Since stream tables were replicated via
streaming replication, they are already up-to-date (minus replication lag).
The scheduler resumes refreshing from the stored frontier.

### Resource limits

For production deployments, set resource requests and limits in the Cluster
manifest to prevent the scheduler from starving other workloads:

```yaml
resources:
  requests:
    memory: 512Mi
    cpu: 500m
  limits:
    memory: 2Gi
    cpu: 2000m
```

## Example manifests

The repository includes ready-to-use manifests in the `cnpg/` directory:

| File | Purpose |
|------|---------|
| `cnpg/Dockerfile.ext` | Build the scratch-based extension image |
| `cnpg/Dockerfile.ext-build` | Multi-stage build for CI/CD pipelines |
| `cnpg/cluster-example.yaml` | Complete Cluster manifest with pg_trickle |
| `cnpg/database-example.yaml` | Database resource with declarative extension management |

## Further reading

- [CloudNativePG Image Volume Extensions](https://cloudnative-pg.io/docs/1.28/imagevolume_extensions/)
- [CloudNativePG Declarative Database Management](https://cloudnative-pg.io/docs/1.28/declarative_database_management/)
- [Backup and Restore](../BACKUP_AND_RESTORE.md)
- [Configuration Reference](../CONFIGURATION.md)
