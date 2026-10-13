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

## Import an existing database into a managed service

Use this to give a project environment the data it had elsewhere: a
database on another server, a hosted provider, or an older install. It copies
one source database into one database of a running, standalone managed
service on the control-plane host. PostgreSQL, MariaDB/MySQL, MongoDB, and
Redis are supported; check before assuming:

```bash
bunx @temps-sdk/cli@0.1.37 --target-context production services import-data-availability --id <service-id>
```

It reports whether the engine supports imports, whether one can start now
(and why not), the accepted source schemes and options, whether a failed
import is all-or-nothing, and the target-name limit. If the command is
unknown, the pinned CLI predates this feature: say so instead of falling back
to raw API calls.

1. **Pick the target.** A project environment reads the database it is linked
   to, usually `<project>_<environment>`. For Redis, the target is that same
   resource name; Temps maps it to the environment's logical database.
2. **Keep the source secret.** Put the connection string in an environment
   variable and pass its name; never place it on the command line, in chat, or
   in a file you commit. Ask the user to export it themselves when it is not
   already available to you.
3. **Confirm before writing.** Name the service, target database, masked
   source, and whether existing data will be dropped. Without `--replace` an
   import into a non-empty database is refused, which is the safe default.
   With `--replace` the target is dropped first: get explicit approval, then
   repeat the name with `--confirm-target`.
4. **Run and wait for the result:**

   ```bash
   bunx @temps-sdk/cli@0.1.37 --target-context production services import-data \
     --id <service-id> --target <database> --source-url-env SOURCE_DATABASE_URL --yes
   ```

   PostgreSQL imports are atomic: a failure leaves nothing behind. MariaDB,
   MongoDB, and Redis imports are not; after a failure the target may hold part
   of the data, and the fix is to run again with `--replace`. The exception is
   a failed Redis import into a name that had no logical database yet: it
   releases the one it allocated, unless a linked environment resolves to that
   name or a deployment started using it meanwhile, and the run's message says
   which happened.
5. **Report from the run, not from assumptions:**

   ```bash
   bunx @temps-sdk/cli@0.1.37 --target-context production services import-data-run --id <service-id> --run <run-id>
   ```

   A succeeded run reports the tables/collections/keys and size it measured
   in the target. A failed run gives a one-line cause (wrong password, source
   unreachable, source newer than the service, ...) and the tool output.
   Relay the cause and the suggested fix; do not retry blindly.

The source must be reachable from the internet: private, loopback, and
cloud-metadata addresses are refused. MongoDB sources take one host (a
replica-set member is fine) and not `mongodb+srv://`; Redis Cluster and
Sentinel sources are not supported. Cancel a running import with
`services import-data-cancel --id <service-id> --run <run-id>`; it is refused
once the data has been copied.
