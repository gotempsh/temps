<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `monitors` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `monitors` (alias: `monitoring`)

Manage uptime monitors for status pages

**Subcommands:**

- `list` (`ls`) - List all monitors for a project
- `create` (`add`) - Create a new monitor for a project
- `show` - Show monitor details and current status
- `remove` (`rm`) - Delete a monitor
- `status` - Get current status — all monitors for a project, or a single monitor by ID
- `history` - Get monitor uptime history

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
