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
- `metrics` - Get container resource metrics (all containers if no container ID specified)

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
