<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `incidents` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `incidents` (alias: `incident`)

Manage incidents for status pages and monitoring

**Subcommands:**

- `list` (`ls`) - List incidents for a project
- `create` (`add`) - Create a new incident
- `show` - Show incident details
- `update-status` - Update an incident status
- `set-visibility` - Show or hide an incident update on published status pages
- `updates` - List status updates for an incident
- `bucketed` - Get bucketed incident data for a project

### `incidents list` (alias: `ls`)

List incidents for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--status <status>` | Filter by status (investigating, identified, monitoring, resolved) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--page <n>` | Page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--json` | Output in JSON format | - | No |

### `incidents create` (alias: `add`)

Create a new incident

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-t, --title <title>` | Incident title | - | No |
| `-d, --description <description>` | Incident description | - | No |
| `-s, --severity <severity>` | Severity level (critical, major, minor) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `incidents show`

Show incident details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `incidents update-status`

Update an incident status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `-s, --status <status>` | New status (investigating, identified, monitoring, resolved) | - | No |
| `-m, --message <message>` | Status update message | - | No |
| `--public` | Show this update on published status pages (private by default) | - | No |

### `incidents set-visibility`

Show or hide an incident update on published status pages

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--update-id <id>` | Incident update ID (see `incidents updates`) | - | Yes |
| `--public` | Show the update on published status pages | - | No |
| `--private` | Hide the update from published status pages | - | No |

### `incidents updates`

List status updates for an incident

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Incident ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `incidents bucketed`

Get bucketed incident data for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-i, --interval <interval>` | Bucket interval: 5min, hourly, daily (default: hourly) | - | No |
| `--start-time <time>` | Start time (ISO 8601) | - | No |
| `--end-time <time>` | End time (ISO 8601) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |
