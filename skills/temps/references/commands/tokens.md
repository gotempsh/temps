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
- `deployment` - Deployment commands

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

### `tokens deployment`

Deployment commands

**Subcommands:**

- `list` - List all deployment tokens for a project
- `create` - Create a new deployment token for a project
- `delete` - Delete a deployment token
- `get` - Get a specific deployment token
- `update` - Update a deployment token
- `rotate` - Rotate a deployment token, invalidating its old secret and issuing a new one

#### `tokens deployment list`

List all deployment tokens for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment create`

Create a new deployment token for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--deployment-id <n>` | Optional deployment ID - if set, token is scoped to a specific deployment | - | No |
| `--environment-id <n>` | Optional environment ID - if not set, token applies to all environments | - | No |
| `--expires-at <value>` | Body field expires_at | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--permissions <value>` | List of permissions (e.g., ["visitors:enrich", "emails:send"]) If not provided, defaults to full access (repeatable) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment delete`

Delete a deployment token

**Usage:** `tokens deployment delete <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment get`

Get a specific deployment token

**Usage:** `tokens deployment get <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment update`

Update a deployment token

**Usage:** `tokens deployment update <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expires-at <value>` | Body field expires_at | - | No |
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--permissions <value>` | Body field permissions (repeatable) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `tokens deployment rotate`

Rotate a deployment token, invalidating its old secret and issuing a new one

**Usage:** `tokens deployment rotate <token-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |
