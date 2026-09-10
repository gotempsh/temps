<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `context` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `context`

Manage CLI contexts (one set of credentials per Temps server)

**Subcommands:**

- `list` (`ls`) - List all configured contexts
- `use` (`switch`) - Switch the active context
- `remove` (`rm`) - Remove a context (does NOT revoke the key on the server)
- `rename` - Rename a context
- `current` - Print the active context name

### `context list` (alias: `ls`)

List all configured contexts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `context use` (alias: `switch`)

Switch the active context

### `context remove` (alias: `rm`)

Remove a context (does NOT revoke the key on the server)

### `context rename`

Rename a context

### `context current`

Print the active context name

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format with full details | - | No |
