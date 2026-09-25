<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `alarms` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `alarms` (alias: `alarm`)

List, acknowledge, and resolve alarms (container crashes, uptime, metrics, databases)

**Subcommands:**

- `list` (`ls`) - List alarms, newest first
- `summary` - Show active alarm counts by status, severity, and type
- `ack` (`acknowledge`) - Acknowledge alarms by ID, or every alarm matching the filters with --all
- `resolve` - Resolve alarms by ID, or every alarm matching the filters with --all

### `alarms list` (alias: `ls`)

List alarms, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--status <status>` | Filter by status (firing, acknowledged, resolved) | - | No |
| `--severity <severity>` | Filter by severity (info, warning, critical) | - | No |
| `--type <type>` | Filter by alarm type (e.g. container_crash) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--deployment-id <id>` | Filter by deployment ID | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms summary`

Show active alarm counts by status, severity, and type

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms ack` (alias: `acknowledge`)

Acknowledge alarms by ID, or every alarm matching the filters with --all

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--status <status>` | Filter by status (firing, acknowledged, resolved) | - | No |
| `--severity <severity>` | Filter by severity (info, warning, critical) | - | No |
| `--type <type>` | Filter by alarm type (e.g. container_crash) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--deployment-id <id>` | Filter by deployment ID | - | No |
| `--all` | Target every alarm matching the filters instead of explicit IDs | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms resolve`

Resolve alarms by ID, or every alarm matching the filters with --all

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--project-id <id>` | Project ID (instead of --project) | - | No |
| `--system` | Target host-wide system alarms (disk space, worker nodes) instead of a project | - | No |
| `--status <status>` | Filter by status (firing, acknowledged, resolved) | - | No |
| `--severity <severity>` | Filter by severity (info, warning, critical) | - | No |
| `--type <type>` | Filter by alarm type (e.g. container_crash) | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--deployment-id <id>` | Filter by deployment ID | - | No |
| `--all` | Target every alarm matching the filters instead of explicit IDs | - | No |
| `--json` | Output in JSON format | - | No |
