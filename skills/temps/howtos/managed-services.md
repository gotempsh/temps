# Managed databases, caches, and object storage

Use [the services reference](../references/commands/services.md) for lifecycle
operations and [the data reference](../references/commands/data.md) for bounded,
read-only inspection.

1. Inspect service types and existing services in the explicit target context.
2. Confirm engine, version, storage, network exposure, project/environment
   links, and backup expectations before creation or import.
3. Treat connection strings and generated credentials as secrets. Do not print
   or persist them; inject them into the linked environment through the
   platform.
4. Explain and confirm operations that restart, upgrade, unlink, remove, or
   restore a service.
5. Verify health and project linkage with read-only service queries. For data
   checks, request the smallest useful sample and avoid returning personal or
   credential-bearing columns.

PostgreSQL, MariaDB/MySQL, MongoDB, Redis, and S3-compatible stores have
different backup and restore contracts. Do not infer support from another
engine; inspect capabilities before promising recovery behavior.
