<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `apikeys` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `apikeys` (alias: `keys`)

Manage API keys for programmatic access

**Subcommands:**

- `list` (`ls`) - List all API keys
- `create` (`add`) - Create a new API key
- `show` - Show API key details
- `remove` (`rm`) - Delete an API key
- `activate` - Activate a deactivated API key
- `deactivate` - Deactivate an API key
- `permissions` - List available API key permissions

### `apikeys list` (alias: `ls`)

List all API keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `apikeys create` (alias: `add`)

Create a new API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | API key name | - | No |
| `-r, --role <role>` | Role type (admin, platform_admin, user, reader, api_reader, custom, metrics_ingest) | - | No |
| `-e, --expires-in <days>` | Expires in N days (7, 30, 90, 365) | - | No |
| `-p, --permissions <permissions>` | Comma-separated list of permissions | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `apikeys show`

Show API key details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `apikeys remove` (alias: `rm`)

Delete an API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `apikeys activate`

Activate a deactivated API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |

### `apikeys deactivate`

Deactivate an API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | API key ID | - | Yes |

### `apikeys permissions`

List available API key permissions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
