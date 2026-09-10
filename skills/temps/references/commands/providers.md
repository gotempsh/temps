<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `providers` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `providers` (alias: `provider`)

Manage Git providers

**Subcommands:**

- `list` (`ls`) - List configured Git providers
- `add` - Add a new Git provider
- `remove` (`rm`) - Remove a Git provider
- `show` - Show Git provider details
- `activate` - Activate a Git provider
- `deactivate` - Deactivate a Git provider
- `safe-delete` - Safely delete a Git provider (checks dependencies first)
- `deletion-check` - Check if a Git provider can be safely deleted
- `git` - Manage Git providers
- `connections` (`conn`) - Manage Git provider connections

### `providers list` (alias: `ls`)

List configured Git providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `providers add`

Add a new Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --provider <provider>` | Provider type (github, gitlab, bitbucket, gitea, generic) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-t, --token <token>` | Personal access token (or Bitbucket access token / app password) | - | No |
| `--base-url <url>` | Instance base URL (GitLab/Gitea self-hosted; required for gitea) | - | No |
| `--username <username>` | Bitbucket username (selects app-password auth) | - | No |
| `--password <password>` | Bitbucket app password (used with --username) | - | No |
| `--clone-url <url>` | HTTPS clone URL (generic provider) | - | No |
| `--token-username <username>` | HTTP Basic username for the token (generic; default x-access-token) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `providers remove` (alias: `rm`)

Remove a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `providers show`

Show Git provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `providers activate`

Activate a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `providers deactivate`

Deactivate a Git provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `providers safe-delete`

Safely delete a Git provider (checks dependencies first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `providers deletion-check`

Check if a Git provider can be safely deleted

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `providers git`

Manage Git providers

**Subcommands:**

- `connect` - Connect a Git provider (github, gitlab, bitbucket, gitea, generic)
- `repos` - List available repositories

#### `providers git connect`

Connect a Git provider (github, gitlab, bitbucket, gitea, generic)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --provider <provider>` | Provider type (github, gitlab, bitbucket, gitea, generic) | - | Yes |
| `-n, --name <name>` | Provider name | - | No |
| `-t, --token <token>` | Personal access token (or Bitbucket access token / app password) | - | No |
| `--base-url <url>` | Instance base URL (GitLab/Gitea self-hosted; required for gitea) | - | No |
| `--username <username>` | Bitbucket username (selects app-password auth) | - | No |
| `--password <password>` | Bitbucket app password (used with --username) | - | No |
| `--clone-url <url>` | HTTPS clone URL (generic provider) | - | No |
| `--token-username <username>` | HTTP Basic username for the token (generic; default x-access-token) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

#### `providers git repos`

List available repositories

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID (optional, lists all if not provided) | - | No |
| `--json` | Output in JSON format | - | No |
| `--search <term>` | Search repositories by name | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (max: 100) | - | No |
| `--sort <field>` | Sort by field (name, created_at, updated_at, stars) | - | No |
| `--direction <dir>` | Sort direction: asc or desc | - | No |
| `--language <lang>` | Filter by programming language | - | No |
| `--owner <owner>` | Filter by repository owner | - | No |

### `providers connections` (alias: `conn`)

Manage Git provider connections

**Subcommands:**

- `list` (`ls`) - List all Git connections
- `show` - Show connection details for a provider
- `delete` (`rm`) - Delete a Git connection
- `activate` - Activate a Git connection
- `deactivate` - Deactivate a Git connection
- `sync` - Sync repositories for a Git connection
- `update-token` - Update access token for a Git connection
- `validate` - Validate a Git connection

#### `providers connections list` (alias: `ls`)

List all Git connections

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--per-page <n>` | Items per page (default: 30, max: 100) | - | No |
| `--sort <field>` | Sort by field (created_at, updated_at, account_name) | - | No |
| `--direction <dir>` | Sort direction: asc or desc (default: desc) | - | No |

#### `providers connections show`

Show connection details for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `providers connections delete` (alias: `rm`)

Delete a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `providers connections activate`

Activate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections deactivate`

Deactivate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections sync`

Sync repositories for a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |

#### `providers connections update-token`

Update access token for a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `-t, --token <token>` | New access token | - | Yes |

#### `providers connections validate`

Validate a Git connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Connection ID | - | Yes |
| `--json` | Output in JSON format | - | No |
