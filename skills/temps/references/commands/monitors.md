<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `monitors` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`monitors list`](#monitors-list)
- [`monitors create`](#monitors-create)
- [`monitors add-target`](#monitors-add-target)
- [`monitors update-target`](#monitors-update-target)
- [`monitors locations`](#monitors-locations)
- [`monitors show`](#monitors-show)
- [`monitors remove`](#monitors-remove)
- [`monitors status`](#monitors-status)
- [`monitors history`](#monitors-history)
- [`monitors get-health`](#monitors-get-health)
- [`monitors update`](#monitors-update)
- [`monitors get-bucketed-status`](#monitors-get-bucketed-status)
- [`monitors crons`](#monitors-crons)
- [`monitors http-checks`](#monitors-http-checks)

## `monitors` (alias: `monitoring`)

Manage uptime monitors for status pages

**Subcommands:**

- `list` (`ls`) - List all monitors for a project
- `create` (`add`) - Create a new monitor for a project
- `add-target` - Monitor a URL, port, TLS certificate or DNS record outside Temps
- `update-target` - Change a monitor of a target outside Temps; omitted flags keep their value
- `locations` - List where monitors checked from worker nodes are checked from
- `show` - Show monitor details and current status
- `remove` (`rm`) - Delete a monitor
- `status` - Get current status — all monitors for a project, or a single monitor by ID
- `history` - Get monitor uptime history
- `get-health` - Get monitor-based health summaries for multiple projects in a single query
- `update` - Update a monitor's name, check interval or check path
- `get-bucketed-status` - Get bucketed status data for a monitor using TimescaleDB
- `crons` - Crons commands
- `http-checks` - Http checks commands

### `monitors list` (alias: `ls`)

List all monitors for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors create` (alias: `add`)

Create a new monitor for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | Monitor name | - | No |
| `-t, --type <type>` | Monitor type (http, tcp, ping) | - | No |
| `-i, --interval <seconds>` | Check interval in seconds (60, 300, 600, 900, 1800) | - | No |
| `--check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Defaults to "/" for HTTP monitors. | - | No |
| `--environment-id <id>` | Environment ID (default: 0 for production) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `monitors add-target`

Monitor a URL, port, TLS certificate or DNS record outside Temps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--kind <kind>` | http, tcp, tls or dns | - | Yes |
| `--target <target>` | URL (http), host:port (tcp, tls) or host name (dns) | - | Yes |
| `-n, --name <name>` | Monitor name | - | Yes |
| `-i, --interval <seconds>` | Check interval in seconds (default 60, at least 30) | - | No |
| `--keyword-absent` | Fail when --keyword is present instead of missing | - | No |
| `--from-nodes` | Also check from online worker nodes; down only when most locations agree | - | No |
| `--method <method>` | HTTP method (default GET) | - | No |
| `--expect-status <codes>` | Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx) | - | No |
| `--keyword <text>` | Text the response body must contain (first 256 KiB; never stored) | - | No |
| `--degraded-after-ms <ms>` | Slower than this is degraded (http, tcp) | - | No |
| `--warn-days <days>` | Degraded when the certificate expires within this many days (tls, default 14) | - | No |
| `--record <type>` | DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A) | - | No |
| `--expect <values>` | Comma-separated DNS values; any match is up (default: any answer) | - | No |
| `--header-file <path>` | File of "Name: value" request headers; stored encrypted, never shown | - | No |
| `--body-file <path>` | File with the request body; stored encrypted | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors update-target`

Change a monitor of a target outside Temps; omitted flags keep their value

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--monitor-id <id>` | Monitor ID | - | Yes |
| `--kind <kind>` | Change the check kind (pass the check flags for it) | - | No |
| `--target <target>` | New target | - | No |
| `-n, --name <name>` | New name | - | No |
| `-i, --interval <seconds>` | Check interval in seconds | - | No |
| `--keyword-absent` | Fail when --keyword is present instead of missing | - | No |
| `--pause` | Stop checking | - | No |
| `--resume` | Start checking again | - | No |
| `--from-nodes` | Also check from online worker nodes | - | No |
| `--no-from-nodes` | Check from this server only | - | No |
| `--method <method>` | HTTP method (default GET) | - | No |
| `--expect-status <codes>` | Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx) | - | No |
| `--keyword <text>` | Text the response body must contain (first 256 KiB; never stored) | - | No |
| `--degraded-after-ms <ms>` | Slower than this is degraded (http, tcp) | - | No |
| `--warn-days <days>` | Degraded when the certificate expires within this many days (tls, default 14) | - | No |
| `--record <type>` | DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A) | - | No |
| `--expect <values>` | Comma-separated DNS values; any match is up (default: any answer) | - | No |
| `--header-file <path>` | File of "Name: value" request headers; stored encrypted, never shown | - | No |
| `--body-file <path>` | File with the request body; stored encrypted | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors locations`

List where monitors checked from worker nodes are checked from

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors show`

Show monitor details and current status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors remove` (alias: `rm`)

Delete a monitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `monitors status`

Get current status — all monitors for a project, or a single monitor by ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID (omit to show all monitors for the project) | - | No |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors history`

Get monitor uptime history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--days <days>` | Number of days to show | `7` | No |

### `monitors get-health`

Get monitor-based health summaries for multiple projects in a single query

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated list of project IDs [required] | - | No |
| `--environment-id <n>` | Select one environment within the authorized projects; defaults to production | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors update`

Update a monitor's name, check interval or check path

**Usage:** `monitors update <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--check-interval-seconds <n>` | How often to probe, in seconds. **Clamped, not rejected**, exactly like on create: values below 30 are... | - | No |
| `--check-path <value>` | Path probed on the monitored environment; must start with '/' | - | No |
| `--name <value>` | Display name; 1 to 255 characters after trimming | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors get-bucketed-status`

Get bucketed status data for a monitor using TimescaleDB

**Usage:** `monitors get-bucketed-status <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--interval <value>` | Bucket interval: '5min', 'hourly', or 'daily' (default: hourly) | - | No |
| `--start-time <value>` | Start time (ISO 8601) (default: 24 hours ago) | - | No |
| `--end-time <value>` | End time (ISO 8601) (default: now) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors crons`

Crons commands

**Subcommands:**

- `list` - List cron monitors
- `create` - Create cron monitor
- `delete` - Delete cron monitor
- `get` - Get cron monitor
- `update` - Update cron monitor
- `list-runs` - List cron monitor runs

#### `monitors crons list`

List cron monitors

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons create`

Create cron monitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) [required] | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--slug <value>` | Body field slug [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons delete`

Delete cron monitor

**Usage:** `monitors crons delete <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons get`

Get cron monitor

**Usage:** `monitors crons get <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons update`

Update cron monitor

**Usage:** `monitors crons update <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Body field config (JSON) | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors crons list-runs`

List cron monitor runs

**Usage:** `monitors crons list-runs <monitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors http-checks`

Http checks commands

**Subcommands:**

- `detect-env-credential` - Detect env credential
- `list-variable-history` - List variable history
- `list` - List http checks
- `create` - Create http check
- `get-capabilities` - Get http checks capabilities
- `list-presets` - List http check presets
- `delete` - Delete http check
- `set-enabled` - Set http check enabled
- `update` - Update http check
- `run` - Run http check
- `detect-secret-credential` - Detect secret credential
- `list-secret-history` - List secret history

#### `monitors http-checks detect-env-credential`

Detect env credential

**Usage:** `monitors http-checks detect-env-credential <env-var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list-variable-history`

List variable history

**Usage:** `monitors http-checks list-variable-history <env-var-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list`

List http checks

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks create`

Create http check

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--credential-stdin` | Body field credential (read from stdin) | - | No |
| `--credential-file <path>` | Body field credential (read from a file) | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--env-var-id <n>` | Body field env_var_id | - | No |
| `--interval-seconds <n>` | Body field interval_seconds | - | No |
| `--kind <value>` | 'http' (default) calls an endpoint; 'local' reads expiring items (certificates, SSH certificates, OpenPGP... (one of: http, local) | - | No |
| `--local <json>` | Body field local (JSON) | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--secret-id <n>` | Project secret to check | - | No |
| `--spec <json>` | This is a declarative HTTP recipe, never executable code (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks get-capabilities`

Get http checks capabilities

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list-presets`

List http check presets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks delete`

Delete http check

**Usage:** `monitors http-checks delete <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks set-enabled`

Set http check enabled

**Usage:** `monitors http-checks set-enabled <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks update`

Update http check

**Usage:** `monitors http-checks update <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--credential-stdin` | Body field credential (read from stdin) | - | No |
| `--credential-file <path>` | Body field credential (read from a file) | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--env-var-id <n>` | Body field env_var_id | - | No |
| `--interval-seconds <n>` | Body field interval_seconds | - | No |
| `--kind <value>` | 'http' (default) calls an endpoint; 'local' reads expiring items (certificates, SSH certificates, OpenPGP... (one of: http, local) | - | No |
| `--local <json>` | Body field local (JSON) | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--secret-id <n>` | Project secret to check | - | No |
| `--spec <json>` | This is a declarative HTTP recipe, never executable code (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks run`

Run http check

**Usage:** `monitors http-checks run <check-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks detect-secret-credential`

Detect secret credential

**Usage:** `monitors http-checks detect-secret-credential <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `monitors http-checks list-secret-history`

List secret history

**Usage:** `monitors http-checks list-secret-history <secret-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |
