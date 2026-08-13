<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `proxy-logs` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `proxy-logs` (alias: `plogs`)

View proxy request logs and statistics

**Subcommands:**

- `list` (`ls`) - List proxy logs
- `show` - Show proxy log details
- `by-request` - Get proxy log by request ID
- `stats` - Get time bucket statistics (last 24 hours)
- `today` - Get today's request statistics

### `proxy-logs list` (alias: `ls`)

List proxy logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--limit <n>` | Items per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | - | No |
| `--project-id <id>` | Filter by project ID | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--method <method>` | Filter by HTTP method (GET, POST, etc.) | - | No |
| `--status-code <code>` | Filter by HTTP status code | - | No |
| `--host <host>` | Filter by host | - | No |
| `--path <path>` | Filter by path (partial match) | - | No |
| `--start-date <date>` | Start date (ISO 8601) | - | No |
| `--end-date <date>` | End date (ISO 8601) | - | No |
| `--sort-by <field>` | Sort by field (default: timestamp) | - | No |
| `--sort-order <order>` | Sort order: asc or desc (default: desc) | - | No |
| `--is-bot` | Filter for bot requests only | - | No |
| `--has-error` | Filter for requests with errors only | - | No |

### `proxy-logs show`

Show proxy log details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Proxy log ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `proxy-logs by-request`

Get proxy log by request ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--request-id <id>` | Request ID | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs stats`

Get time bucket statistics (last 24 hours)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `proxy-logs today`

Get today's request statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
