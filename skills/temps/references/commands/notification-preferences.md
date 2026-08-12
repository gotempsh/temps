<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `notification-preferences` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `notification-preferences` (alias: `notif-prefs`)

Manage notification preferences

**Subcommands:**

- `show` (`get`) - Show current notification preferences
- `update` (`set`) - Update a notification preference
- `reset` - Reset notification preferences to defaults

### `notification-preferences show` (alias: `get`)

Show current notification preferences

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notification-preferences update` (alias: `set`)

Update a notification preference

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-k, --key <key>` | Preference key to update | - | Yes |
| `-v, --value <value>` | Value for the preference | - | Yes |

### `notification-preferences reset`

Reset notification preferences to defaults

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |
