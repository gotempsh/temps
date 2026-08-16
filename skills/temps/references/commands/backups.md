<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `backups` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

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
| `-s, --schedule <cron>` | Schedule expression (cron format) | - | No |
| `-r, --retention <days>` | Retention period in days | - | No |
| `-d, --description <desc>` | Description | - | No |
| `--s3-source-id <id>` | S3 Source ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

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
