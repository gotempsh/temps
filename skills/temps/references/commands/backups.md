<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `backups` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`backups schedules`](#backups-schedules)
- [`backups sources`](#backups-sources)
- [`backups list`](#backups-list)
- [`backups show`](#backups-show)
- [`backups delete`](#backups-delete)
- [`backups cleanup`](#backups-cleanup)
- [`backups run-service`](#backups-run-service)
- [`backups list-alerts`](#backups-list-alerts)
- [`backups get-external-service-capability`](#backups-get-external-service-capability)
- [`backups list-external-service`](#backups-list-external-service)
- [`backups list-service-schedules`](#backups-list-service-schedules)
- [`backups test-s3-connection-preview`](#backups-test-s3-connection-preview)
- [`backups set-default-s3-source`](#backups-set-default-s3-source)
- [`backups test-s3-source-connection`](#backups-test-s3-source-connection)
- [`backups cancel-schedule-run`](#backups-cancel-schedule-run)
- [`backups list-schedule-run-jobs`](#backups-list-schedule-run-jobs)
- [`backups update-schedule`](#backups-update-schedule)
- [`backups run-schedule-now`](#backups-run-schedule-now)
- [`backups list-schedule-runs`](#backups-list-schedule-runs)
- [`backups list-schedule-services`](#backups-list-schedule-services)
- [`backups attach-schedule-services`](#backups-attach-schedule-services)
- [`backups detach-schedule-service`](#backups-detach-schedule-service)
- [`backups cancel`](#backups-cancel)
- [`backups list-children`](#backups-list-children)

## `backups` (alias: `backup`)

Manage backup schedules and backups

**Subcommands:**

- `schedules` (`schedule`) - Manage backup schedules
- `sources` (`source`) - Manage S3 backup sources
- `list` (`ls`) - List backups for a schedule
- `show` - Show backup details
- `delete` (`rm`) - Permanently delete one terminal backup
- `cleanup` - Delete backups expired by their schedule retention policy
- `run-service` - Run a backup for an external service
- `list-alerts` - List open backup alerts
- `get-external-service-capability` - Report whether an existing service can produce a Cloud-restorable backup
- `list-external-service` - List all backups for a specific external service (DB-only, no S3 scan)
- `list-service-schedules` - List the schedules that target a specific external service
- `test-s3-connection-preview` - Test S3 connectivity against a prospective source configuration (before creating it)
- `set-default-s3-source` - Mark an S3 source as the default
- `test-s3-source-connection` - Test connectivity to an existing S3 source using its stored credentials
- `cancel-schedule-run` - Cancel every non-terminal child backup belonging to a schedule run
- `list-schedule-run-jobs` - List the individual backup jobs for a single scheduler run
- `update-schedule` - Update a backup schedule (partial update)
- `run-schedule-now` - Immediately fan-out a run for the given schedule (Run Now)
- `list-schedule-runs` - Paginated run history for a backup schedule (one row per scheduler tick)
- `list-schedule-services` - List the external services attached to a backup schedule
- `attach-schedule-services` - Attach one or more external services to a backup schedule
- `detach-schedule-service` - Detach a single external service from a backup schedule
- `cancel` - Cancel a single in-flight backup
- `list-children` - List the external-service child backups that belong to a parent backup

### `backups schedules` (alias: `schedule`)

Manage backup schedules

**Subcommands:**

- `list` (`ls`) - List backup schedules
- `create` - Create a backup schedule
- `show` - Show backup schedule details
- `enable` - Enable a backup schedule
- `disable` - Disable a backup schedule
- `delete` (`rm`) - Delete a backup schedule

#### `backups schedules list` (alias: `ls`)

List backup schedules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `backups schedules create`

Create a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Schedule name | - | No |
| `-t, --type <type>` | Backup type (full, incremental) | - | No |
| `-s, --schedule <cron>` | Schedule expression: six-field cron with seconds first, e.g. "0 0 2 * * *" | - | No |
| `-r, --retention <days>` | Retention period in days | - | No |
| `-d, --description <desc>` | Description | - | No |
| `--s3-source-id <id>` | S3 Source ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--target-all-services` | Back up every external service (the default when no --service-ids) | - | No |
| `--no-target-all-services` | Set target_all_services to false | - | No |
| `--service-ids <n>` | Back up only these external services (repeatable) (repeatable) | - | No |
| `--include-control-plane` | Also back up Temps' own database | - | No |
| `--no-include-control-plane` | Set include_control_plane to false | - | No |
| `--max-runtime-secs <n>` | Cancel a run that takes longer than this | - | No |

#### `backups schedules show`

Show backup schedule details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `backups schedules enable`

Enable a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |

#### `backups schedules disable`

Disable a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |

#### `backups schedules delete` (alias: `rm`)

Delete a backup schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Schedule ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `backups sources` (alias: `source`)

Manage S3 backup sources

**Subcommands:**

- `list` (`ls`) - List S3 sources
- `create` - Create an S3 source
- `show` - Show S3 source details
- `update` - Update an S3 source
- `remove` (`rm`) - Delete an S3 source
- `backups` - List backups for an S3 source
- `run` - Trigger a backup for an S3 source

#### `backups sources list` (alias: `ls`)

List S3 sources

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `backups sources create`

Create an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Source name | - | No |
| `--bucket <bucket>` | S3 bucket name | - | No |
| `--region <region>` | S3 region | - | No |
| `--endpoint <endpoint>` | S3 endpoint (for S3-compatible services) | - | No |
| `--access-key <key>` | Access key ID | - | No |
| `--secret-key <key>` | Secret access key | - | No |
| `--prefix <prefix>` | Bucket path/prefix | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--use-tls` | Connect to the endpoint over TLS (default true) | - | No |
| `--no-use-tls` | Set use_tls to false | - | No |
| `--worker-credentials <json>` | Separate credentials the backup workers use, as JSON (JSON) | - | No |
| `--backing-service-id <n>` | The external service (S3/MinIO) that hosts this bucket | - | No |
| `--is-default` | Make this the default backup destination | - | No |
| `--no-is-default` | Set is_default to false | - | No |
| `--secret-key-file <path>` | Read the S3 secret access key from a file (- reads stdin) instead of --secret-key | - | No |
| `--secret-key-stdin` | Read the S3 secret access key from stdin instead of --secret-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `backups sources show`

Show S3 source details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `backups sources update`

Update an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `-n, --name <name>` | New source name | - | No |
| `--bucket <bucket>` | New S3 bucket name | - | No |
| `--region <region>` | New S3 region | - | No |
| `--endpoint <endpoint>` | New S3 endpoint | - | No |
| `--access-key <key>` | New access key ID | - | No |
| `--secret-key <key>` | New secret access key | - | No |
| `--prefix <prefix>` | New bucket path/prefix | - | No |
| `--use-tls` | Connect to the endpoint over TLS (default true) | - | No |
| `--no-use-tls` | Set use_tls to false | - | No |
| `--worker-credentials <json>` | Separate credentials the backup workers use, as JSON (JSON) | - | No |
| `--secret-key-file <path>` | Read the S3 secret access key from a file (- reads stdin) instead of --secret-key | - | No |
| `--secret-key-stdin` | Read the S3 secret access key from stdin instead of --secret-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `backups sources remove` (alias: `rm`)

Delete an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `backups sources backups`

List backups for an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--include-s3-scan` | Also list backups found in the bucket that this server has no record of | - | No |
| `--no-include-s3-scan` | Set include_s3_scan to false | - | No |

#### `backups sources run`

Trigger a backup for an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | S3 source ID | - | Yes |

### `backups list` (alias: `ls`)

List backups for a schedule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--schedule-id <id>` | Schedule ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `backups show`

Show backup details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Backup ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `backups delete` (alias: `rm`)

Permanently delete one terminal backup

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Backup UUID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `backups cleanup`

Delete backups expired by their schedule retention policy

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--dry-run` | Preview expired backups without deleting them | - | No |
| `--schedule-id <id>` | Limit cleanup to one schedule | - | No |
| `-y, --yes` | Skip confirmation prompt | - | No |
| `--json` | Output the cleanup report as JSON | - | No |

### `backups run-service`

Run a backup for an external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | External service ID | - | Yes |
| `--s3-source-id <id>` | S3 source ID to store the backup | - | Yes |
| `-t, --type <type>` | Backup type (e.g., full, incremental) | - | No |
| `--confirm-restart` | Allow the backup to restart the database (PostgreSQL with WAL archiving off restarts once to turn it on) | - | No |

### `backups list-alerts`

List open backup alerts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups get-external-service-capability`

Report whether an existing service can produce a Cloud-restorable backup

**Usage:** `backups get-external-service-capability <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups list-external-service`

List all backups for a specific external service (DB-only, no S3 scan)

**Usage:** `backups list-external-service <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (1-based) | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-service-schedules`

List the schedules that target a specific external service

**Usage:** `backups list-service-schedules <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups test-s3-connection-preview`

Test S3 connectivity against a prospective source configuration (before creating it)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--access-key-id <value>` | Access key ID of the bucket's credentials, for example 'AKIA...' on AWS [required] | - | No |
| `--backing-service-id <n>` | Managed RustFS/S3 service that supplies this destination | - | No |
| `--bucket-name <value>` | Body field bucket_name [required] | - | No |
| `--bucket-path <value>` | Body field bucket_path [required] | - | No |
| `--endpoint <value>` | Optional endpoint for S3-compatible services like MinIO or RustFS, with or without a scheme | - | No |
| `--force-path-style` | Whether to use path-style addressing (default: true) | - | No |
| `--no-force-path-style` | Set force_path_style to false | - | No |
| `--is-default` | When true, make this the default source (will swap out any existing default) | - | No |
| `--no-is-default` | Set is_default to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--region <value>` | Body field region [required] | - | No |
| `--secret-key-stdin` | Secret access key that goes with the access key ID [required] (read from stdin) | - | No |
| `--secret-key-file <path>` | Secret access key that goes with the access key ID [required] (read from a file) | - | No |
| `--use-tls` | Whether the endpoint is reached over TLS | - | No |
| `--no-use-tls` | Set use_tls to false | - | No |
| `--worker-credentials <value>` | What a worker node may receive from an S3 source (ADR-102) (one of: scoped, allow_full) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups set-default-s3-source`

Mark an S3 source as the default

**Usage:** `backups set-default-s3-source <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups test-s3-source-connection`

Test connectivity to an existing S3 source using its stored credentials

**Usage:** `backups test-s3-source-connection <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups cancel-schedule-run`

Cancel every non-terminal child backup belonging to a schedule run

**Usage:** `backups cancel-schedule-run <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-schedule-run-jobs`

List the individual backup jobs for a single scheduler run

**Usage:** `backups list-schedule-run-jobs <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups update-schedule`

Update a backup schedule (partial update)

**Usage:** `backups update-schedule <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--description <value>` | New human-readable description | - | No |
| `--enabled` | Enable or disable the schedule | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--include-control-plane` | Toggle whether the control-plane backup is produced on every run | - | No |
| `--no-include-control-plane` | Set include_control_plane to false | - | No |
| `--max-runtime-secs <n>` | Per-schedule wall-clock timeout override (seconds) | - | No |
| `--name <value>` | New schedule name | - | No |
| `--retention-period <n>` | Days to retain backups produced by this schedule | - | No |
| `--schedule-expression <value>` | New cron expression with a leading seconds field ('sec min hour day-of-month month day-of-week', e.g. '0 0... | - | No |
| `--service-ids <n>` | Replace the explicit external-service selection atomically with the schedule update (repeatable) | - | No |
| `--tags <value>` | Replace the full tag list (repeatable) | - | No |
| `--target-all-services` | Toggle between "back up every database" ('true') and "back up only the explicit list" ('false') | - | No |
| `--no-target-all-services` | Set target_all_services to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups run-schedule-now`

Immediately fan-out a run for the given schedule (Run Now)

**Usage:** `backups run-schedule-now <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups list-schedule-runs`

Paginated run history for a backup schedule (one row per scheduler tick)

**Usage:** `backups list-schedule-runs <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (1-based, defaults to 1, clamped to 1 if < 1) | - | No |
| `--page-size <n>` | Items per page (defaults to 20, clamped to 100 if > 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-schedule-services`

List the external services attached to a backup schedule

**Usage:** `backups list-schedule-services <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `backups attach-schedule-services`

Attach one or more external services to a backup schedule

**Usage:** `backups attach-schedule-services <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--service-ids <n>` | External service ids to attach (repeatable) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `backups detach-schedule-service`

Detach a single external service from a backup schedule

**Usage:** `backups detach-schedule-service <id> <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `backups cancel`

Cancel a single in-flight backup

**Usage:** `backups cancel <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `backups list-children`

List the external-service child backups that belong to a parent backup

**Usage:** `backups list-children <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
