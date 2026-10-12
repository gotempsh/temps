# Docker image build contexts

This directory contains the Dockerfiles and entrypoints for the container
images Temps uses to run managed database services. It is **not** a folder of pictures —
repo screenshots and media live in [`assets/`](../assets/).

| Directory | Image | Purpose |
|---|---|---|
| `postgres-walg/` | `ghcr.io/gotempsh/postgres-walg` | PostgreSQL with WAL-G for PITR backups |
| `pgvector-walg/` | `ghcr.io/gotempsh/pgvector-walg` | PostgreSQL + pgvector with WAL-G |
| `timescaledb-walg/` | `ghcr.io/gotempsh/timescaledb-walg` | TimescaleDB with WAL-G |
| `postgres-ha/` | `ghcr.io/gotempsh/postgres-ha` | PostgreSQL high-availability cluster image |
| `redis-walg/` | `ghcr.io/gotempsh/redis-walg` | Redis with WAL-G-based backups |
| `mongodb-walg/` | `ghcr.io/gotempsh/mongodb-walg` | MongoDB with WAL-G-based backups |
| `mariadb-walg/` | `ghcr.io/gotempsh/mariadb-walg` | MariaDB physical streaming backups through WAL-G |

The default image tags used by the platform are set in
`crates/temps-providers/src/externalsvc/`. Those defaults still reference the
Docker Hub `gotempsh/*` copies, which are pushed by hand.

## Publishing

All images except MariaDB are published for `linux/amd64` and `linux/arm64`
by `.github/workflows/database-images.yml`; what it builds is planned by
`.github/scripts/database_image_matrix.py`, which holds the list of supported
majors per image.

| Tag kind | Example | When it is built |
|---|---|---|
| Floating major | `postgres-walg:18-bookworm`, `pgvector-walg:pg18`, `mongodb-walg:8.0` | Every run, so upstream base-image security rebuilds are picked up |
| Pinned PostgreSQL minor | `postgres-walg:18.3-bookworm`, `postgres-ha:18.3-bookworm` | The newest 4 upstream minors per major, discovered from `docker.io/library/postgres`. The newest is rebuilt every run; older ones only when missing or when their build context changes |

The workflow runs on every push to `main` that touches an image, weekly on a
schedule, and on demand (`workflow_dispatch`, with `rebuild_all` to rebuild
frozen minors too). Pull requests that touch an image build and verify it
without publishing. Before anything is pushed, each image must report a
working `wal-g --version`, and PostgreSQL images must report the exact server
version their tag claims.

Releases call the same workflow from `release.yml` in "ensure" mode: it
publishes only tags that are missing (a release never moves a tag under a
running database), then blocks the release unless every required tag can be
pulled anonymously from GHCR, the way a fresh install pulls it. Keeping these
images up to date is the weekly schedule's job, not the release's.

Two cases follow from "never moves a tag". An image that is published under
some of its tags but not all (a newly added alias) gets the missing tags
pointed at the existing digest instead of being rebuilt. And if GHCR cannot be
asked whether a tag exists (rate limit, outage), the run fails rather than
treating the tag as missing.

To support a new major, add it to `CATALOG` in the planner script.

The MariaDB image is published by `.github/workflows/mariadb-walg-image.yml`.

After the first publication of a new package, an organization owner must set
its visibility to **Public** under GitHub organization package settings.
Public visibility lets a fresh Temps installation pull the default image
without configuring GHCR credentials.
