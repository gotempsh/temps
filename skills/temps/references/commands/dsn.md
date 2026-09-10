<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `dsn` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `dsn`

Manage Data Source Names (DSNs) for error tracking and analytics

**Subcommands:**

- `list` (`ls`) - List all DSNs for a project
- `create` (`add`) - Create a new DSN for a project
- `get-or-create` - Get an existing DSN or create one if none exists
- `regenerate` - Regenerate DSN keys (rotate keys)
- `revoke` - Revoke (deactivate) a DSN

### `dsn list` (alias: `ls`)

List all DSNs for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dsn create` (alias: `add`)

Create a new DSN for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | DSN name | - | No |
| `--environment-id <id>` | Environment ID | - | No |
| `--deployment-id <id>` | Deployment ID | - | No |
| `--base-url <url>` | Base URL for the DSN | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `dsn get-or-create`

Get an existing DSN or create one if none exists

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID | - | No |
| `--deployment-id <id>` | Deployment ID | - | No |
| `--base-url <url>` | Base URL for the DSN | - | No |
| `--json` | Output in JSON format | - | No |

### `dsn regenerate`

Regenerate DSN keys (rotate keys)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--dsn-id <id>` | DSN ID | - | Yes |
| `--base-url <url>` | New base URL for the DSN | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `dsn revoke`

Revoke (deactivate) a DSN

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--dsn-id <id>` | DSN ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
