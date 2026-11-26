<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `funnels` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `funnels` (alias: `funnel`)

Manage analytics funnels for projects

**Subcommands:**

- `list` (`ls`) - List the funnels of a project, newest first
- `create` (`add`) - Create a new funnel for a project
- `update` - Update a funnel
- `remove` (`rm`) - Delete a funnel
- `metrics` - Get funnel metrics
- `preview` - Preview funnel metrics without saving
- `get-unique-events` - Get all unique/distinct event types for a project (paginated)

### `funnels list` (alias: `ls`)

List the funnels of a project, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `funnels create` (alias: `add`)

Create a new funnel for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | Funnel name | - | No |
| `-s, --steps <json>` | Funnel steps as JSON array (e.g. '[{"event_name":"page_view"},{"event_name":"signup"}]') | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `funnels update`

Update a funnel

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--funnel-id <id>` | Funnel ID | - | Yes |
| `-n, --name <name>` | New funnel name | - | No |
| `-s, --steps <json>` | New funnel steps as JSON array | - | No |

### `funnels remove` (alias: `rm`)

Delete a funnel

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--funnel-id <id>` | Funnel ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `funnels metrics`

Get funnel metrics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--funnel-id <id>` | Funnel ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--start-date <value>` | Window start (RFC 3339) | - | No |
| `--end-date <value>` | Window end (RFC 3339) | - | No |
| `--environment-id <n>` | Only this environment (an ID) | - | No |
| `--country-code <value>` | Only visitors from this country (ISO code, e.g. DE) | - | No |

### `funnels preview`

Preview funnel metrics without saving

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-s, --steps <json>` | Funnel steps as JSON array | - | Yes |
| `--json` | Output in JSON format | - | No |

### `funnels get-unique-events`

Get all unique/distinct event types for a project (paginated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 50, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
