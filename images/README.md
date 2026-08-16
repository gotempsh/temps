# Docker image build contexts

This directory contains the Dockerfiles and entrypoints for the container
images Temps uses to run managed database services (published as
`gotempsh/<name>` on Docker Hub). It is **not** a folder of pictures —
repo screenshots and media live in [`assets/`](../assets/).

| Directory | Image | Purpose |
|---|---|---|
| `postgres-walg/` | `gotempsh/postgres-walg` | PostgreSQL with WAL-G for PITR backups |
| `pgvector-walg/` | `gotempsh/pgvector-walg` | PostgreSQL + pgvector with WAL-G |
| `timescaledb-walg/` | `gotempsh/timescaledb-walg` | TimescaleDB with WAL-G |
| `postgres-ha/` | `gotempsh/postgres-ha` | PostgreSQL high-availability cluster image |
| `redis-walg/` | `gotempsh/redis-walg` | Redis with WAL-G-based backups |
| `mongodb-walg/` | `gotempsh/mongodb-walg` | MongoDB with WAL-G-based backups |

The default image tags used by the platform are set in
`crates/temps-providers/src/externalsvc/`.
