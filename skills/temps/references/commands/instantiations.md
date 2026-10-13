<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `instantiations` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `instantiations` (alias: `inst`)

Follow and retry template creations and imports

**Subcommands:**

- `list` (`ls`) - List your recent template creations and imports
- `show` - Show every step of a template creation or import
- `retry` - Continue from the step that failed; finished steps are not repeated

### `instantiations list` (alias: `ls`)

List your recent template creations and imports

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group <id>` | Only those that added services to this project (group id) | - | No |
| `--limit <n>` | At most this many (default 20) | - | No |
| `--json` | Output in JSON format | - | No |

### `instantiations show`

Show every step of a template creation or import

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--wait` | Wait until it finishes | - | No |
| `--json` | Output in JSON format | - | No |

### `instantiations retry`

Continue from the step that failed; finished steps are not repeated

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--no-wait` | Return right away instead of waiting until it finishes | - | No |
| `--json` | Output in JSON format | - | No |
