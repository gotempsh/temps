<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `audit` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `audit`

View audit logs

**Subcommands:**

- `list` (`ls`) - List audit logs
- `show` - Show audit log details

### `audit list` (alias: `ls`)

List audit logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--limit <n>` | Maximum number of logs to return | `50` | No |
| `--offset <n>` | Number of logs to skip | - | No |
| `--operation-type <type>` | Filter by operation type | - | No |
| `--user-id <id>` | Filter by user ID | - | No |
| `--from <timestamp>` | Start timestamp (ISO 8601 or epoch ms) | - | No |
| `--to <timestamp>` | End timestamp (ISO 8601 or epoch ms) | - | No |

### `audit show`

Show audit log details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Audit log ID | - | Yes |
| `--json` | Output in JSON format | - | No |
