<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `instantiations` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `instantiations` (alias: `inst`)

Follow and retry template creations and imports

**Subcommands:**

- `list` (`ls`) - List your recent template creations and imports, newest first
- `show` (`get`) - Show every step of a template creation or import
- `retry` - Continue from the step that failed; finished steps are not repeated

### `instantiations list` (alias: `ls`)

List your recent template creations and imports, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group <project>` | Only those that added services to this project (group slug or id) | - | No |
| `--page <n>` | Page number (default 1) | - | No |
| `--per-page <n>` | How many per page (default 20, at most 100) | - | No |
| `--limit <n>` | Same as --per-page | - | No |
| `--json` | Output in JSON format | - | No |

### `instantiations show` (alias: `get`)

Show every step of a template creation or import

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--wait` | Wait until it finishes | - | No |
| `--timeout <minutes>` | With --wait: stop waiting after this long (default 30) | - | No |
| `--json` | Output in JSON format | - | No |

### `instantiations retry`

Continue from the step that failed; finished steps are not repeated

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--no-wait` | Return right away instead of waiting until it finishes | - | No |
| `--timeout <minutes>` | Stop waiting after this long (default 30); it keeps running on the server | - | No |
| `--json` | Output in JSON format | - | No |
