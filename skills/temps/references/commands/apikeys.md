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
- `get-ai-harness-status` - Whether the caller's external AI harness has connected
- `update-api-key` - Update api key
- `rotate-api-key` - Rotate api key

### `apikeys list` (alias: `ls`)

List all API keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Keys per page (max 100) | - | No |

### `apikeys create` (alias: `add`)

Create a new API key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | API key name | - | No |
| `-r, --role <role>` | Role type (admin, platform_admin, user, reader, api_reader, custom, metrics_ingest) | - | No |
| `-e, --expires-in <days>` | Expires in N days (7, 30, 90, 365) | - | No |
| `-p, --permissions <permissions>` | Comma-separated list of the key's exact permissions (the key uses the custom role) | - | No |
| `--project-id <id>` | Confine to one project’s deployment and environment APIs | - | No |
| `--environment-id <id>` | Confine to one environment (requires --project-id) | - | No |
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

### `apikeys get-ai-harness-status`

Whether the caller's external AI harness has connected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `apikeys update-api-key`

Update api key

**Usage:** `apikeys update-api-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expires-at <value>` | Body field expires_at | - | No |
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--permissions <value>` | Body field permissions (repeatable) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `apikeys rotate-api-key`

Rotate api key

**Usage:** `apikeys rotate-api-key <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |
