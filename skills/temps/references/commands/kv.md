<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `kv` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `kv`

Read and write a project's keys in the KV store, and manage the KV service

**Subcommands:**

- `get` - Get the value of a key
- `set` - Set a key; the value is a string unless --json-value
- `del` (`delete`) - Delete one or more keys
- `keys` (`ls`) - List keys matching a pattern (* and ? wildcards; default *)
- `ttl` - Show how long until a key expires
- `expire` - Expire a key after a number of seconds
- `incr` - Increment a numeric key (creating it at 0 first)
- `status` - Show whether the KV service is enabled and healthy on this instance
- `enable` - Enable the KV service on this instance (starts its Redis container)
- `update` - Change the Docker image the KV service runs (recreates its container)
- `disable` - Disable the KV service on this instance (stops its container; every project loses KV access)
- `container-name` - Show, or set with --container-name/--reset, the Docker container name of the KV service

### `kv get`

Get the value of a key

**Usage:** `kv get <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv set`

Set a key; the value is a string unless --json-value

**Usage:** `kv set <key> [value] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json-value` | Parse the value as JSON (a number, object, array, true/false or null) | - | No |
| `--ex <seconds>` | Expire the key after this many seconds | - | No |
| `--px <milliseconds>` | Expire the key after this many milliseconds | - | No |
| `--nx` | Only set the key if it does not exist | - | No |
| `--xx` | Only set the key if it already exists | - | No |
| `--json` | Output in JSON format | - | No |
| `--value-file <path>` | Read the value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the value from stdin, e.g. piped from a secret manager | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `kv del` (alias: `delete`)

Delete one or more keys

**Usage:** `kv del <keys...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `kv keys` (alias: `ls`)

List keys matching a pattern (* and ? wildcards; default *)

**Usage:** `kv keys [pattern] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv ttl`

Show how long until a key expires

**Usage:** `kv ttl <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv expire`

Expire a key after a number of seconds

**Usage:** `kv expire <key> <seconds> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv incr`

Increment a numeric key (creating it at 0 first)

**Usage:** `kv incr <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--by <amount>` | Amount to add, may be negative (default 1) | - | No |
| `--json` | Output in JSON format | - | No |

### `kv status`

Show whether the KV service is enabled and healthy on this instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `kv enable`

Enable the KV service on this instance (starts its Redis container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run (default: the one this Temps release pins) | - | No |
| `--max-memory <size>` | Memory limit, e.g. 256mb or 1gb | - | No |
| `--persistence` | Persist data to disk | - | No |
| `--no-persistence` | Keep data in memory only | - | No |
| `--json` | Output in JSON format | - | No |

### `kv update`

Change the Docker image the KV service runs (recreates its container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run | - | Yes |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `kv disable`

Disable the KV service on this instance (stops its container; every project loses KV access)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `kv container-name`

Show, or set with --container-name/--reset, the Docker container name of the KV service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--container-name <name>` | New container name (must keep the required prefix) | - | No |
| `--reset` | Go back to the default container name | - | No |
| `--json` | Output in JSON format | - | No |
