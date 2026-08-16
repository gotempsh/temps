<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `kv` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `kv`

KV store commands (coming soon)

**Subcommands:**

- `get` - Get a value by key
- `set` - Set a key-value pair
- `del` (`delete`) - Delete a key
- `keys` (`ls`) - List keys
- `ttl` - Get the TTL (time-to-live) for a key
- `expire` - Set expiry on an existing key
- `incr` - Increment a numeric value
- `enable` - Enable KV store for a project
- `disable` - Disable KV store for a project
- `status` - Get KV store status for a project

### `kv get`

Get a value by key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to retrieve | - | Yes |

### `kv set`

Set a key-value pair

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to set | - | Yes |
| `--value <value>` | Value to set | - | Yes |
| `--ttl <seconds>` | Time-to-live in seconds | - | No |

### `kv del` (alias: `delete`)

Delete a key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to delete | - | Yes |

### `kv keys` (alias: `ls`)

List keys

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--pattern <pattern>` | Key pattern to filter by (e.g., "user:*") | - | No |
| `--json` | Output in JSON format | - | No |

### `kv ttl`

Get the TTL (time-to-live) for a key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to check | - | Yes |

### `kv expire`

Set expiry on an existing key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to set expiry on | - | Yes |
| `--ttl <seconds>` | Time-to-live in seconds | - | Yes |

### `kv incr`

Increment a numeric value

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Key to increment | - | Yes |

### `kv enable`

Enable KV store for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `kv disable`

Disable KV store for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `kv status`

Get KV store status for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
