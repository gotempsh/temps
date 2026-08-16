<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `presets` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `presets` (alias: `preset`)

Browse available build presets

**Subcommands:**

- `list` (`ls`) - List available presets
- `show` (`get`) - Show details for a specific preset

### `presets list` (alias: `ls`)

List available presets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--type <type>` | Filter by project type (server, static) | - | No |

### `presets show` (alias: `get`)

Show details for a specific preset

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
