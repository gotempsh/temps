<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `instances` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `instances` (alias: `instance`)

Manage Temps server instances

**Subcommands:**

- `list` (`ls`) - List configured instances
- `add` - Add a new instance
- `remove` (`rm`) - Remove an instance
- `switch` (`use`) - Switch to a different instance
- `show` - Show instance details (or current instance)

### `instances list` (alias: `ls`)

List configured instances

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `instances add`

Add a new instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Instance name | - | No |
| `-u, --url <url>` | Instance URL | - | No |

### `instances remove` (alias: `rm`)

Remove an instance

### `instances switch` (alias: `use`)

Switch to a different instance

### `instances show`

Show instance details (or current instance)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
