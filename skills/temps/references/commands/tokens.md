<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `tokens` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `tokens` (alias: `token`)

Manage deployment tokens for project API access (KV, Blob, etc.)

**Subcommands:**

- `list` (`ls`) - List deployment tokens for a project
- `create` (`add`) - Create a new deployment token
- `show` (`get`) - Show deployment token details
- `delete` (`rm`) - Delete a deployment token
- `permissions` - List available deployment token permissions

### `tokens list` (alias: `ls`)

List deployment tokens for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `tokens create` (alias: `add`)

Create a new deployment token

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Token name | - | No |
| `--permissions <permissions>` | Comma-separated permissions (e.g., "visitors:enrich,emails:send" or "*" for full access) | - | No |
| `-e, --expires-in <days>` | Expires in N days (7, 30, 90, 365, or "never") | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `tokens show` (alias: `get`)

Show deployment token details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--id <id>` | Token ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `tokens delete` (alias: `rm`)

Delete a deployment token

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--id <id>` | Token ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `tokens permissions`

List available deployment token permissions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
