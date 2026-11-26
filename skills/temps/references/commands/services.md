<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `services` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`services list`](#services-list-alias-ls)
- [`services create`](#services-create-alias-add)
- [`services show`](#services-show)
- [`services remove`](#services-remove-alias-rm)
- [`services start`](#services-start)
- [`services stop`](#services-stop)
- [`services types`](#services-types)
- [`services projects`](#services-projects)
- [`services update`](#services-update)
- [`services upgrade`](#services-upgrade)
- [`services repoint-continuous-archive-source`](#services-repoint-continuous-archive-source)
- [`services import`](#services-import)
- [`services link`](#services-link)
- [`services unlink`](#services-unlink)
- [`services connect`](#services-connect)
- [`services env`](#services-env)
- [`services env-var`](#services-env-var)
- [`services logs`](#services-logs)
- [`services slow-queries`](#services-slow-queries)
- [`services enable-pg-stat-statements`](#services-enable-pg-stat-statements)
- [`services metrics`](#services-metrics)
- [`services restore-capabilities`](#services-restore-capabilities)
- [`services list-backups`](#services-list-backups)
- [`services restore`](#services-restore)
- [`services restore-runs`](#services-restore-runs)
- [`services restore-run`](#services-restore-run)
- [`services restore-cancel`](#services-restore-cancel)
- [`services import-data-availability`](#services-import-data-availability)
- [`services import-data`](#services-import-data)
- [`services import-data-runs`](#services-import-data-runs)
- [`services import-data-run`](#services-import-data-run)
- [`services import-data-cancel`](#services-import-data-cancel)
- [`services wal-health`](#services-wal-health)
- [`services list-available-containers`](#services-list-available-containers)
- [`services list-health-statuses`](#services-list-health-statuses)
- [`services get-healthcheck-defaults`](#services-get-healthcheck-defaults)
- [`services update-healthcheck-defaults`](#services-update-healthcheck-defaults)
- [`services list-project-services`](#services-list-project-services)
- [`services get-environment-variables`](#services-get-environment-variables)
- [`services get-providers-metadata`](#services-get-providers-metadata)
- [`services get-provider-metadata`](#services-get-provider-metadata)
- [`services get-cluster-health`](#services-get-cluster-health)
- [`services reveal-environment-variables`](#services-reveal-environment-variables)
- [`services trigger-health-check`](#services-trigger-health-check)
- [`services get-health-status`](#services-get-health-status)
- [`services get-healthcheck`](#services-get-healthcheck)
- [`services update-healthcheck`](#services-update-healthcheck)
- [`services apply-healthcheck`](#services-apply-healthcheck)
- [`services get-log-ingestion`](#services-get-log-ingestion)
- [`services update-log-ingestion`](#services-update-log-ingestion)
- [`services add-cluster-member`](#services-add-cluster-member)
- [`services remove-cluster-member`](#services-remove-cluster-member)
- [`services get-cluster-member`](#services-get-cluster-member)
- [`services promote-cluster-member`](#services-promote-cluster-member)
- [`services migrate-s3`](#services-migrate-s3)
- [`services reveal-parameter`](#services-reveal-parameter)
- [`services get-preview-environment-variables-masked`](#services-get-preview-environment-variables-masked)
- [`services get-preview-environment-variable-names`](#services-get-preview-environment-variable-names)
- [`services issue-runtime-credentials`](#services-issue-runtime-credentials)
- [`services update-resources`](#services-update-resources)
- [`services retry-cluster`](#services-retry-cluster)
- [`services get-runtime`](#services-get-runtime)
- [`services get-stats`](#services-get-stats)
- [`services get-tls`](#services-get-tls)
- [`services update-tls`](#services-update-tls)
- [`services get-managed-ca-certificate`](#services-get-managed-ca-certificate)
- [`services test-wal-archive-target`](#services-test-wal-archive-target)
- [`services get-workload-mode`](#services-get-workload-mode)
- [`services update-workload-mode`](#services-update-workload-mode)
- [`services get-workload-pending-changes`](#services-get-workload-pending-changes)
- [`services recreate-from-workload-spec`](#services-recreate-from-workload-spec)
- [`services external-reset-pg-stat-statements`](#services-external-reset-pg-stat-statements)
- [`services query`](#services-query)
- [`services pg-upgrade`](#services-pg-upgrade)

## `services` (alias: `svc`)

Manage external services (databases, caches, storage)

**Subcommands:**

- `list` (`ls`) - List all external services
- `create` (`add`) - Create a new external service
- `show` - Show service details
- `remove` (`rm`) - Remove a service
- `start` - Start a stopped service
- `stop` - Stop a running service
- `types` - List available service types
- `projects` - List projects linked to a service
- `update` - Update a service
- `upgrade` - Upgrade a service to a newer version
- `repoint-continuous-archive-source` - Repoint a Postgres/MariaDB service's continuous archiving (WAL-G, or MariaDB's binlog shipper) to a different S3 source. Data archived before this call stays under the previous source and will no longer be verifiable or replayable once archiving points at the new one.
- `import` - Import an existing external service
- `link` - Link a service to a project
- `unlink` - Unlink a service from a project
- `connect` - Get connection info for a service by name or slug
- `env` - Show environment variables for a linked service
- `env-var` - Get a specific environment variable
- `logs` - View persisted logs for an external service
- `slow-queries` - Show slowest PostgreSQL queries from pg_stat_statements
- `enable-pg-stat-statements` - Enable pg_stat_statements on a standalone Postgres service by restarting its container (drops active connections briefly)
- `metrics` - Resource and engine metrics for a database/cache/storage service
- `restore-capabilities` - Show what restore modes a service supports (in-place / new service / PITR)
- `list-backups` - List backups stored on an S3 source
- `restore` - Restore a service from a backup (in-place, new service, or PITR)
- `restore-runs` - List recent restore runs for a service
- `restore-run` - Show a single restore run
- `restore-cancel` - Cancel a restore run that has not started writing data yet (refused once it has)
- `import-data-availability` - Show whether a service can receive data imported from an external database, and what source it accepts
- `import-data` - Copy a database from an external server into a database of this service (PostgreSQL, MariaDB/MySQL, MongoDB, Redis)
- `import-data-runs` - List data imports into a service, newest first
- `import-data-run` - Show one data import: outcome, phase, timings, who started it and the transfer output
- `import-data-cancel` - Cancel a running data import (refused once the data has been copied)
- `wal-health` - Probe a PostgreSQL service's WAL / archive_command health right now (archiver failures, backlog, stale replication slots) — diagnoses "Cloud backup mirror unavailable ... check that PostgreSQL's archive_command is succeeding" warnings
- `list-available-containers` - List available Docker containers that can be imported as services
- `list-health-statuses` - Current health status for many services at once
- `get-healthcheck-defaults` - Get the instance-wide default healthcheck of managed service containers
- `update-healthcheck-defaults` - Replace the instance-wide default healthcheck of managed service containers
- `list-project-services` - List services linked to a project
- `get-environment-variables` - Get all environment variables for all services linked to a project
- `get-providers-metadata` - Get provider metadata (display names, icons, descriptions)
- `get-provider-metadata` - Get metadata for a specific provider
- `get-cluster-health` - Per-member health for a Postgres HA cluster
- `reveal-environment-variables` - Reveal a service's basic environment variables in plaintext, before it is linked to any project
- `trigger-health-check` - Run a health check for one service right now
- `get-health-status` - Persisted health status for an external service
- `get-healthcheck` - Get a managed service's healthcheck
- `update-healthcheck` - Replace a managed service's healthcheck overrides
- `apply-healthcheck` - Recreate a managed service's container to apply its healthcheck now
- `get-log-ingestion` - Get this service's own container log ingestion budget
- `update-log-ingestion` - Replace this service's container log ingestion budget
- `add-cluster-member` - Begin adding a single new member to a running cluster
- `remove-cluster-member` - Remove a single member from a running cluster
- `get-cluster-member` - Get a single cluster member's current state
- `promote-cluster-member` - Promote a replica to primary by triggering a pg_auto_failover failover
- `migrate-s3` - Copy every bucket of another managed S3 service into this one
- `reveal-parameter` - Reveal one sensitive service parameter
- `get-preview-environment-variables-masked` - Get environment variables preview with masked sensitive values
- `get-preview-environment-variable-names` - Get environment variable names preview (safe - no sensitive values)
- `issue-runtime-credentials` - Issue live connection credentials for a service in an environment
- `update-resources` - Update a service's resource limits (memory, CPU caps)
- `retry-cluster` - Retry a failed cluster service initialization
- `get-runtime` - Inspect a service's container(s): status, restart count, OOM-killed flag, exit code, and the cgroup limits..
- `get-stats` - Sample current CPU/memory usage from each of a service's containers
- `get-tls` - Get service tls
- `update-tls` - Update service tls
- `get-managed-ca-certificate` - Get managed ca certificate
- `test-wal-archive-target` - Test wal archive target
- `get-workload-mode` - Get a database's workload mode (internal, operator-only)
- `update-workload-mode` - Change a database's workload mode (internal, operator-only)
- `get-workload-pending-changes` - List what a recreate from the workload spec would change (internal, operator-only)
- `recreate-from-workload-spec` - Recreate a database's container from its workload spec (internal, operator-only)
- `external-reset-pg-stat-statements` - Reset all statistics accumulated by 'pg_stat_statements' for a Postgres service
- `query` - Query commands
- `pg-upgrade` - Pg upgrade commands

### `services list` (alias: `ls`)

List all external services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services create` (alias: `add`)

Create a new external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Service type (postgres, mongodb, redis, s3) | - | No |
| `-n, --name <name>` | Service name | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |
| `--backend <backend>` | S3 storage engine: rustfs (default) or garage (s3/blob only) | - | No |
| `--bucket <name>` | Bucket to create (repeatable, s3/blob only; the first is S3_BUCKET) | `` | No |
| `--node <id>` | Worker node to run the service on (default: the control plane) | - | No |
| `--topology <topology>` | standalone (default) or cluster (highly available, several members) | - | No |
| `--member <role[@node]>` | With --topology cluster: a member, monitor, primary or replica, optionally on a node id (repeatable; exactly one monitor) | `` | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `services show`

Show service details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services remove` (alias: `rm`)

Remove a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `services start`

Start a stopped service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

### `services stop`

Stop a running service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

### `services types`

List available service types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `info` - Show parameters schema for a service type (useful for automation)

#### `services types info`

Show parameters schema for a service type (useful for automation)

**Usage:** `services types info <type> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as raw JSON schema (default) | - | No |

### `services projects`

List projects linked to a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services update`

Update a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-n, --name <name>` | Docker image name (e.g., postgres:18-alpine) | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |

### `services upgrade`

Upgrade a service to a newer version

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-v, --version <version>` | Docker image to upgrade to (e.g., postgres:18-alpine) | - | No |

### `services repoint-continuous-archive-source`

Repoint a Postgres/MariaDB service's continuous archiving (WAL-G, or MariaDB's binlog shipper) to a different S3 source. Data archived before this call stays under the previous source and will no longer be verifiable or replayable once archiving points at the new one.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--s3-source <id>` | S3 source ID to point continuous archiving at | - | Yes |

### `services import`

Import an existing external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Service type (postgres, mongodb, redis, s3) | - | No |
| `-n, --name <name>` | Service name | - | No |
| `--container-id <id>` | Container ID or name to import | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |
| `--version <version>` | Optional version override | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `services link`

Link a service to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--database-mode <mode>` | Which database deployments get: project (one per project), project_environment (one per environment) or custom | - | No |
| `--database-name <name>` | With --database-mode custom: the exact database name | - | No |
| `--env-alias <APP_NAME=INJECTED_NAME>` | Also inject a variable under the name the app reads, e.g. DATABASE_URL=POSTGRES_URL (repeatable) | - | No |

### `services unlink`

Unlink a service from a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `services connect`

Get connection info for a service by name or slug

**Usage:** `services connect <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--json` | Output in JSON format | - | No |

### `services env`

Show environment variables for a linked service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--json` | Output in JSON format | - | No |

### `services env-var`

Get a specific environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--var <name>` | Environment variable name | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services logs`

View persisted logs for an external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--from <datetime>` | Start of time range. ISO 8601 timestamp or a relative duration like "1h", "24h", "7d" (default: 24h ago) | - | No |
| `--to <datetime>` | End of time range. ISO 8601 timestamp (default: now) | - | No |
| `-l, --level <levels>` | Comma-separated log levels to include: ERROR,WARN,INFO,DEBUG,TRACE | - | No |
| `-n, --tail <lines>` | Maximum number of log lines to fetch (default: 200, max: 1000) | `200` | No |
| `-t, --text <query>` | Filter log lines by text (case-insensitive) | - | No |
| `--json` | Output raw JSON instead of formatted lines | - | No |

### `services slow-queries`

Show slowest PostgreSQL queries from pg_stat_statements

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--page <n>` | Page number (1-based, default: 1) | `1` | No |
| `--page-size <n>` | Rows per page (1–100, default: 20) | `20` | No |
| `--sort-by <column>` | Sort column: calls, total_exec_time_ms, mean_exec_time_ms, rows, cache_hit_ratio (default: mean_exec_time_ms) | - | No |
| `--sort-order <order>` | Sort direction: asc or desc (default: desc) | - | No |
| `--json` | Output raw JSON instead of a formatted table | - | No |

### `services enable-pg-stat-statements`

Enable pg_stat_statements on a standalone Postgres service by restarting its container (drops active connections briefly)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-y, --yes` | Skip the restart confirmation prompt (for automation) | - | No |

### `services metrics`

Resource and engine metrics for a database/cache/storage service

**Subcommands:**

- `latest` - Show the most recent value of every tracked metric
- `range` - Show a time-series range for a single metric
- `status` - Show when metrics were last received for a service
- `by-database` - Per-database metric breakdown (PostgreSQL services only)
- `enable` - Enable metric collection for a service (seeds default alert rules)
- `disable` - Disable metric collection for a service
- `alert-rules` - Manage monitoring alert rules for a service

#### `services metrics latest`

Show the most recent value of every tracked metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics range`

Show a time-series range for a single metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-m, --metric <name>` | Metric name, e.g. "pg.connections_active" | - | Yes |
| `-r, --range <window>` | Time window: 1h, 6h, 24h, 7d (default: 24h) | - | No |
| `-p, --percentile <n>` | Histogram percentile (0-100) instead of a plain average | - | No |
| `--json` | Output raw JSON instead of a formatted table | - | No |

#### `services metrics status`

Show when metrics were last received for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics by-database`

Per-database metric breakdown (PostgreSQL services only)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics enable`

Enable metric collection for a service (seeds default alert rules)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

#### `services metrics disable`

Disable metric collection for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

#### `services metrics alert-rules`

Manage monitoring alert rules for a service

**Subcommands:**

- `list` (`ls`) - List alert rules for a service
- `create` (`add`) - Create an alert rule for a service
- `update` - Update an existing alert rule
- `remove` (`rm`) - Delete an alert rule

##### `services metrics alert-rules list` (alias: `ls`)

List alert rules for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules create` (alias: `add`)

Create an alert rule for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-n, --name <name>` | Alert rule name | - | Yes |
| `-m, --metric <name>` | Metric name, e.g. "pg.connections_active" | - | Yes |
| `-c, --comparator <op>` | Comparator: >, <, >=, <= | - | Yes |
| `-t, --threshold <n>` | Threshold value that triggers the alert | - | Yes |
| `-s, --severity <level>` | warning or critical (default: warning) | - | No |
| `--for-duration <secs>` | Seconds the breach must persist before firing (default: 0) | - | No |
| `--disabled` | Create the rule disabled | - | No |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules update`

Update an existing alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--rule-id <id>` | Alert rule ID | - | Yes |
| `-n, --name <name>` | Alert rule name | - | No |
| `-m, --metric <name>` | Metric name | - | No |
| `-c, --comparator <op>` | Comparator: >, <, >=, <= | - | No |
| `-t, --threshold <n>` | Threshold value | - | No |
| `-s, --severity <level>` | warning or critical | - | No |
| `--for-duration <secs>` | Seconds the breach must persist before firing | - | No |
| `--enable` | Enable the rule | - | No |
| `--disable` | Disable the rule | - | No |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules remove` (alias: `rm`)

Delete an alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--rule-id <id>` | Alert rule ID | - | Yes |
| `-y, --yes` | Skip confirmation prompt | - | No |

### `services restore-capabilities`

Show what restore modes a service supports (in-place / new service / PITR)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services list-backups`

List backups stored on an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--s3-source-id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore`

Restore a service from a backup (in-place, new service, or PITR)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Source service ID (the service the backup came from) | - | Yes |
| `--backup-id <id>` | Backup ID to restore from (see `list-backups`) | - | No |
| `--backup-location <s3-key>` | Instead of --backup-id: the S3 key of a backup this instance did not record (one found by an S3 scan, made by another Temps instance) | - | No |
| `--backup-engine <engine>` | With --backup-location: postgres, redis, mongodb or s3 | - | No |
| `--s3-source <id>` | With --backup-location: the S3 source the backup is in | - | No |
| `--new-service [name]` | Clone into a new service. Omit the value or pass "auto" to accept the auto-suggested name. | - | No |
| `--pitr <iso>` | Point-in-time recovery target, ISO 8601 timestamp (requires a PITR-capable backup). Combine with --new-service to route PITR into a new service. | - | No |
| `--confirm-cross-service` | Confirm an in-place (or PITR in-place) restore of a backup produced by a different service. Required by the server for such restores; recorded in the audit log. | - | No |
| `-y, --yes` | Skip confirmation | - | No |
| `--no-wait` | Return immediately without polling run status | - | No |
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `plan` - Plan restore

#### `services restore plan`

Plan restore

**Usage:** `services restore plan <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Body field mode (one of: in_place, new_service, pitr) [required] | - | No |
| `--name <value>` | Name for the new service | - | No |
| `--parameter-overrides <json>` | Optional parameter overrides (port, docker_image, database) (JSON) | - | No |
| `--new-service-name <value>` | Required when 'to_new_service' is true | - | No |
| `--target <json>` | Recovery target kind + value (JSON) | - | No |
| `--to-new-service` | Whether PITR restores in place or creates a new service | - | No |
| `--no-to-new-service` | Set to_new_service to false | - | No |
| `--backup-engine <value>` | Engine of the backup when specified by 'backup_location' ("postgres", "redis", "mongodb", "s3") | - | No |
| `--backup-id <n>` | DB id of the backup to restore from | - | No |
| `--backup-location <value>` | Raw S3 URL / key of the backup — used when restoring a backup discovered by S3 scan (i.e., produced by... | - | No |
| `--confirm-cross-service` | Explicit confirmation for a destructive cross-service restore | - | No |
| `--no-confirm-cross-service` | Set confirm_cross_service to false | - | No |
| `--s3-source-id <n>` | S3 source the 'backup_location' lives in | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services restore-runs`

List recent restore runs for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore-run`

Show a single restore run

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Restore run ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore-cancel`

Cancel a restore run that has not started writing data yet (refused once it has)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Restore run ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services import-data-availability`

Show whether a service can receive data imported from an external database, and what source it accepts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services import-data`

Copy a database from an external server into a database of this service (PostgreSQL, MariaDB/MySQL, MongoDB, Redis)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID to import into | - | Yes |
| `--target <name>` | Database that receives the data (created if missing; for Redis, the resource name, usually <project>_<environment>) | - | Yes |
| `--source-url-env <var>` | Name of an environment variable holding the source connection string (recommended: keeps the password out of shell history) | - | No |
| `--source-url <url>` | Source connection string (visible in shell history and the process list; prefer --source-url-env) | - | No |
| `--replace` | Drop the target database first if it already holds data (DESTRUCTIVE) | - | No |
| `--confirm-target <name>` | Required with --replace when not interactive: repeat the target name | - | No |
| `--timeout <minutes>` | Stop the copy after this many minutes (server default when omitted) | - | No |
| `-y, --yes` | Skip the confirmation prompt | - | No |
| `--no-wait` | Return after starting instead of waiting for the result | - | No |
| `--json` | Output in JSON format | - | No |

### `services import-data-runs`

List data imports into a service, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--page <n>` | Page number (default 1) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `services import-data-run`

Show one data import: outcome, phase, timings, who started it and the transfer output

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--run <id>` | Data import ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services import-data-cancel`

Cancel a running data import (refused once the data has been copied)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--run <id>` | Data import ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services wal-health`

Probe a PostgreSQL service's WAL / archive_command health right now (archiver failures, backlog, stale replication slots) — diagnoses "Cloud backup mirror unavailable ... check that PostgreSQL's archive_command is succeeding" warnings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services list-available-containers`

List available Docker containers that can be imported as services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services list-health-statuses`

Current health status for many services at once

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ids <value>` | Comma-separated service IDs | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-healthcheck-defaults`

Get the instance-wide default healthcheck of managed service containers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-healthcheck-defaults`

Replace the instance-wide default healthcheck of managed service containers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--interval-secs <n>` | Seconds between checks once healthy (5–3600) | - | No |
| `--retries <n>` | Consecutive failures that make the container unhealthy (1–10) | - | No |
| `--start-interval-secs <n>` | Seconds between checks while starting (1–60) | - | No |
| `--start-period-secs <n>` | Seconds after start during which failures do not count (0–600) | - | No |
| `--timeout-secs <n>` | Seconds one check may run (1–60, and shorter than the interval) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services list-project-services`

List services linked to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of items per page (max 100) | - | No |
| `--sort-by <value>` | Query parameter sort_by | - | No |
| `--sort-order <value>` | Query parameter sort_order | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-environment-variables`

Get all environment variables for all services linked to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-providers-metadata`

Get provider metadata (display names, icons, descriptions)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-provider-metadata`

Get metadata for a specific provider

**Usage:** `services get-provider-metadata <service-type> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-cluster-health`

Per-member health for a Postgres HA cluster

**Usage:** `services get-cluster-health <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services reveal-environment-variables`

Reveal a service's basic environment variables in plaintext, before it is linked to any project

**Usage:** `services reveal-environment-variables <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services trigger-health-check`

Run a health check for one service right now

**Usage:** `services trigger-health-check <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-health-status`

Persisted health status for an external service

**Usage:** `services get-health-status <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max number of recent checks (default 50, max 200) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-healthcheck`

Get a managed service's healthcheck

**Usage:** `services get-healthcheck <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-healthcheck`

Replace a managed service's healthcheck overrides

**Usage:** `services update-healthcheck <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--interval-secs <n>` | Seconds between checks once healthy (5–3600) | - | No |
| `--retries <n>` | Consecutive failures that make the container unhealthy (1–10) | - | No |
| `--start-interval-secs <n>` | Seconds between checks while starting (1–60) | - | No |
| `--start-period-secs <n>` | Seconds after start during which failures do not count (0–600) | - | No |
| `--timeout-secs <n>` | Seconds one check may run (1–60, and shorter than the interval) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services apply-healthcheck`

Recreate a managed service's container to apply its healthcheck now

**Usage:** `services apply-healthcheck <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-log-ingestion`

Get this service's own container log ingestion budget

**Usage:** `services get-log-ingestion <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-log-ingestion`

Replace this service's container log ingestion budget

**Usage:** `services update-log-ingestion <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--log-ingest-kib-per-sec <n>` | KiB of log text per second per container (0–1,048,576). null = platform default | - | No |
| `--log-ingest-lines-per-sec <n>` | Lines per second per container (0–1,000,000). null = platform default | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services add-cluster-member`

Begin adding a single new member to a running cluster

**Usage:** `services add-cluster-member <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node-id <n>` | Target worker node ID | - | No |
| `--role <value>` | Member role [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services remove-cluster-member`

Remove a single member from a running cluster

**Usage:** `services remove-cluster-member <id> <member-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-cluster-member`

Get a single cluster member's current state

**Usage:** `services get-cluster-member <id> <member-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services promote-cluster-member`

Promote a replica to primary by triggering a pg_auto_failover failover

**Usage:** `services promote-cluster-member <id> <member-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services migrate-s3`

Copy every bucket of another managed S3 service into this one

**Usage:** `services migrate-s3 <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--source-service-id <n>` | Managed S3 service to copy from (RustFS or Garage) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services reveal-parameter`

Reveal one sensitive service parameter

**Usage:** `services reveal-parameter <id> <param-name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-preview-environment-variables-masked`

Get environment variables preview with masked sensitive values

**Usage:** `services get-preview-environment-variables-masked <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-preview-environment-variable-names`

Get environment variable names preview (safe - no sensitive values)

**Usage:** `services get-preview-environment-variable-names <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services issue-runtime-credentials`

Issue live connection credentials for a service in an environment

**Usage:** `services issue-runtime-credentials <id> <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `services update-resources`

Update a service's resource limits (memory, CPU caps)

**Usage:** `services update-resources <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cpu-shares <n>` | Relative CPU weight (default 1024) | - | No |
| `--memory-mb <n>` | Hard memory limit in MiB | - | No |
| `--memory-swap-mb <n>` | Memory + swap limit in MiB | - | No |
| `--nano-cpus <n>` | CPU quota in nano-cpus. 1_000_000_000 = 1 full CPU core | - | No |
| `--shm-size-mb <n>` | Shared memory (/dev/shm) size in MiB | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services retry-cluster`

Retry a failed cluster service initialization

**Usage:** `services retry-cluster <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--members <json>` | Cluster member specifications (same format as create) (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-runtime`

Inspect a service's container(s): status, restart count, OOM-killed flag, exit code, and the cgroup limits..

**Usage:** `services get-runtime <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-stats`

Sample current CPU/memory usage from each of a service's containers

**Usage:** `services get-stats <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-tls`

Get service tls

**Usage:** `services get-tls <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-tls`

Update service tls

**Usage:** `services update-tls <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | The new mode. 'required' is rejected until it is supported (one of: off, available, required) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-managed-ca-certificate`

Get managed ca certificate

**Usage:** `services get-managed-ca-certificate <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services test-wal-archive-target`

Test wal archive target

**Usage:** `services test-wal-archive-target <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services get-workload-mode`

Get a database's workload mode (internal, operator-only)

**Usage:** `services get-workload-mode <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services update-workload-mode`

Change a database's workload mode (internal, operator-only)

**Usage:** `services update-workload-mode <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Body field mode (one of: legacy, shadow, enforce) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services get-workload-pending-changes`

List what a recreate from the workload spec would change (internal, operator-only)

**Usage:** `services get-workload-pending-changes <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services recreate-from-workload-spec`

Recreate a database's container from its workload spec (internal, operator-only)

**Usage:** `services recreate-from-workload-spec <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--confirmed-fingerprint <value>` | The 'fingerprint' of the pending changes being confirmed | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services external-reset-pg-stat-statements`

Reset all statistics accumulated by 'pg_stat_statements' for a Postgres service

**Usage:** `services external-reset-pg-stat-statements <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--confirm` | Must be 'true' to acknowledge the global, irreversible reset [required] | - | No |
| `--no-confirm` | Set confirm to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `services query`

Query commands

**Subcommands:**

- `data` - Query data from an entity with optional filters, pagination, and sorting
- `download-object` - Download an object (S3 only) as a streaming response
- `get-container-info` - Get information about a specific container

#### `services query data`

Query data from an entity with optional filters, pagination, and sorting

**Usage:** `services query data <service-id> <path> <entity> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--filters <json>` | JSON filters (backend-specific format) (JSON) | - | No |
| `--limit <n>` | Maximum number of rows to return | - | No |
| `--offset <n>` | Number of rows to skip | - | No |
| `--sort-by <value>` | Sort by field name | - | No |
| `--sort-order <value>` | Sort order (asc/desc) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `services query download-object`

Download an object (S3 only) as a streaming response

**Usage:** `services query download-object <service-id> <path> <entity> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

#### `services query get-container-info`

Get information about a specific container

**Usage:** `services query get-container-info <service-id> <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services pg-upgrade`

Pg upgrade commands

**Subcommands:**

- `list` - List recent upgrades for a single service (newest first, page size 50)
- `start` - Start a new PostgreSQL major-version upgrade for a service
- `get` - Get a single upgrade by id, scoped to a service
- `cancel` - Cancel an in-flight upgrade
- `get-logs` - Get the accumulated JSONL log content for an upgrade (for dashboard display)
- `retry` - Retry a failed upgrade
- `rollback` - Roll a completed upgrade back to its pre-upgrade PGDATA volume and old image

#### `services pg-upgrade list`

List recent upgrades for a single service (newest first, page size 50)

**Usage:** `services pg-upgrade list <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade start`

Start a new PostgreSQL major-version upgrade for a service

**Usage:** `services pg-upgrade start <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--from-image <value>` | Body field from_image [required] | - | No |
| `--from-version <value>` | Body field from_version [required] | - | No |
| `--to-image <value>` | Body field to_image [required] | - | No |
| `--to-version <value>` | Body field to_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade get`

Get a single upgrade by id, scoped to a service

**Usage:** `services pg-upgrade get <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade cancel`

Cancel an in-flight upgrade

**Usage:** `services pg-upgrade cancel <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade get-logs`

Get the accumulated JSONL log content for an upgrade (for dashboard display)

**Usage:** `services pg-upgrade get-logs <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade retry`

Retry a failed upgrade

**Usage:** `services pg-upgrade retry <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `services pg-upgrade rollback`

Roll a completed upgrade back to its pre-upgrade PGDATA volume and old image

**Usage:** `services pg-upgrade rollback <service-id> <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |
