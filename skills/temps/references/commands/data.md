<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `data` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `data`

Browse the data inside a service (tables, collections, keys, objects) — read-only

**Subcommands:**

- `info` - Show what a service supports and how its containers nest
- `containers` (`databases`) - List top-level containers (databases, or buckets for S3)
- `tables` (`entities`) - List tables, collections, keys or objects in a container
- `schema` (`columns`) - Show an entity's columns, types and row count
- `rows` (`select`) - Read rows from an entity
- `ai-access` - Show or set whether the built-in AI assistant may read this service's rows

### `data info`

Show what a service supports and how its containers nest

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `data containers` (alias: `databases`)

List top-level containers (databases, or buckets for S3)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | List containers nested under this path instead of the root | - | No |
| `--json` | Output in JSON format | - | No |

### `data tables` (alias: `entities`)

List tables, collections, keys or objects in a container

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--limit <n>` | Maximum entities to return (default: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `data schema` (alias: `columns`)

Show an entity's columns, types and row count

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `data rows` (alias: `select`)

Read rows from an entity

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Container path, slash-separated (e.g. mydb/public) | - | Yes |
| `--filter <json>` | Backend-specific filter as JSON (SQL: '{"where":"id > 5"}'). See: temps data info <service> | - | No |
| `--limit <n>` | Maximum rows to return (default: 20) | - | No |
| `--offset <n>` | Rows to skip (default: 0) | - | No |
| `--sort-by <field>` | Field to sort by | - | No |
| `--sort-order <order>` | asc or desc (default: asc) | - | No |
| `--json` | Output in JSON format | - | No |

### `data ai-access`

Show or set whether the built-in AI assistant may read this service's rows

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Allow the built-in assistant to read row data | - | No |
| `--disable` | Stop the built-in assistant reading row data | - | No |
| `--json` | Output in JSON format | - | No |
