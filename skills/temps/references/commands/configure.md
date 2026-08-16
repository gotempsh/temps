<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `configure` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `configure`

Configure CLI settings (AWS-style wizard)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--api-url <url>` | API URL | - | No |
| `--api-token <token>` | API token for authentication | - | No |
| `--output-format <format>` | Output format (table, json, minimal) | - | No |
| `--enable-colors` | Enable colored output in config | - | No |
| `--disable-colors` | Disable colored output in config | - | No |
| `-i, --interactive` | Force interactive mode even in non-TTY | - | No |
| `-y, --no-interactive` | Non-interactive mode (uses defaults for unspecified options) | - | No |

**Subcommands:**

- `get` - Get a configuration value
- `set` - Set a configuration value
- `list` - List all configuration values
- `show` - Show current configuration and authentication status
- `reset` - Reset configuration to defaults

### `configure get`

Get a configuration value

### `configure set`

Set a configuration value

### `configure list`

List all configuration values

### `configure show`

Show current configuration and authentication status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `configure reset`

Reset configuration to defaults
