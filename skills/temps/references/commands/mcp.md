<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `mcp` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `mcp`

Configure this Temps instance as an MCP server for AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed)

**Subcommands:**

- `enable` - Enable the Temps MCP server on this instance (admin, one-time per instance)
- `disable` - Disable the Temps MCP server on this instance (admin)
- `add` - Configure an AI client to connect to this Temps instance over MCP. Clients: claude-code, claude-desktop, codex, cursor, vscode, windsurf, zed
- `remove` - Remove the Temps MCP server from an AI client
- `status` - Show whether this instance has MCP enabled and which AI clients are configured

### `mcp enable`

Enable the Temps MCP server on this instance (admin, one-time per instance)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |

### `mcp disable`

Disable the Temps MCP server on this instance (admin)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |

### `mcp add`

Configure an AI client to connect to this Temps instance over MCP. Clients: claude-code, claude-desktop, codex, cursor, vscode, windsurf, zed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-g, --groups <groups>` | Comma-separated tool groups to enable (default: all) | - | No |
| `-w, --write` | Enable write tools (deploy, delete, restart, etc). Default: read-only | - | No |
| `-k, --api-key <key>` | Use this API key instead of creating or prompting for one | - | No |
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |
| `-y, --yes` | Skip prompts and confirmation (uses defaults; requires --api-key or an existing login) | - | No |

### `mcp remove`

Remove the Temps MCP server from an AI client

### `mcp status`

Show whether this instance has MCP enabled and which AI clients are configured

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |
