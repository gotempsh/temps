<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `containers` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `containers` (alias: `cts`)

Manage project containers in environments

**Subcommands:**

- `list` (`ls`) - List containers in an environment, or across all environments if -e omitted
- `show` - Show container details
- `start` - Start a stopped container
- `stop` - Stop a running container
- `restart` - Restart a container
- `history` - List containers that have run in an environment, including ones replaced by a later redeploy; every currently-running container is always included
- `metrics` - Get container resource metrics (all containers if no container ID specified)
- `get-environment-variable` - Get container environment variable
- `metrics-get-history` - Fetch a time-series range for a single container resource metric (recorded by the container health monitor..
- `stream-metrics` - Stream container metrics via Server-Sent Events (SSE)

### `containers list` (alias: `ls`)

List containers in an environment, or across all environments if -e omitted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID (optional - lists all environments if omitted) | - | No |
| `--json` | Output in JSON format | - | No |

### `containers show`

Show container details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `containers start`

Start a stopped container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |

### `containers stop`

Stop a running container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |

### `containers restart`

Restart a container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID | - | Yes |

### `containers history`

List containers that have run in an environment, including ones replaced by a later redeploy; every currently-running container is always included

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-d, --deployment-id <id>` | Only list containers belonging to this deployment | - | No |
| `-l, --limit <count>` | Max REPLACED container rows to return on top of the running ones, newest first (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `containers metrics`

Get container resource metrics (all containers if no container ID specified)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project-id <id>` | Project ID | - | Yes |
| `-e, --environment-id <id>` | Environment ID | - | Yes |
| `-c, --container-id <id>` | Container ID (optional - shows all if not specified) | - | No |
| `--json` | Output in JSON format | - | No |
| `-w, --watch` | Watch mode - continuously update metrics | - | No |
| `-i, --interval <seconds>` | Refresh interval in seconds (default: 2) | `2` | No |

### `containers get-environment-variable`

Get container environment variable

**Usage:** `containers get-environment-variable <environment-id> <container-id> <variable-name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `containers metrics-get-history`

Fetch a time-series range for a single container resource metric (recorded by the container health monitor..

**Usage:** `containers metrics-get-history <environment-id> <container-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--metric <value>` | Dotted metric name, e.g. 'container.cpu_percent' or 'container.memory_used_bytes' [required] | - | No |
| `--range <value>` | Time window: '1h', '6h', '24h', or '7d' (defaults to '1h') | - | No |
| `--json` | Output in JSON format | - | No |

### `containers stream-metrics`

Stream container metrics via Server-Sent Events (SSE)

**Usage:** `containers stream-metrics <environment-id> <container-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--interval <n>` | Update interval in milliseconds (default: 1000) | - | No |
| `--json` | Output in JSON format | - | No |
