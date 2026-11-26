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
- `acknowledge-alarm` - Acknowledge a firing alarm (marks it as seen but not resolved)
- `resolve-alarm` - Resolve an alarm
- `unsilence` - Lift an alarm's silence before it expires, so the next breach of the same alert notifies again
- `silence` - Silence an alarm (and future re-fires of the same type/scope) for a chosen duration, without permanently..
- `system` - System commands

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
| `--service-id <id>` | Only alarms of this external service | - | No |
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

**Usage:** `alarms ack [alarmIds...] [options]`

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

**Usage:** `alarms resolve [alarmIds...] [options]`

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

### `alarms acknowledge-alarm`

Acknowledge a firing alarm (marks it as seen but not resolved)

**Usage:** `alarms acknowledge-alarm <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms resolve-alarm`

Resolve an alarm

**Usage:** `alarms resolve-alarm <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms unsilence`

Lift an alarm's silence before it expires, so the next breach of the same alert notifies again

**Usage:** `alarms unsilence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms silence`

Silence an alarm (and future re-fires of the same type/scope) for a chosen duration, without permanently..

**Usage:** `alarms silence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--duration-hours <n>` | How long to mute this alarm (and future re-fires of the same type/scope) for, in hours [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `alarms system`

System commands

**Subcommands:**

- `acknowledge` - Acknowledge a firing system alarm
- `resolve` - Resolve a system alarm
- `unsilence` - Lift a system alarm's silence before it expires
- `silence` - Silence a system alarm (and future re-fires of the same type/scope) for a chosen duration, without..

#### `alarms system acknowledge`

Acknowledge a firing system alarm

**Usage:** `alarms system acknowledge <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `alarms system resolve`

Resolve a system alarm

**Usage:** `alarms system resolve <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `alarms system unsilence`

Lift a system alarm's silence before it expires

**Usage:** `alarms system unsilence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `alarms system silence`

Silence a system alarm (and future re-fires of the same type/scope) for a chosen duration, without..

**Usage:** `alarms system silence <alarm-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--duration-hours <n>` | How long to mute this alarm (and future re-fires of the same type/scope) for, in hours [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
