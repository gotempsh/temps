<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `mcp` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `mcp`

Configure this Temps instance as an MCP server for AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed)

**Subcommands:**

- `enable` - Enable the Temps MCP server on this instance (admin, one-time per instance)
- `disable` - Disable the Temps MCP server on this instance (admin)
- `add` - Configure an AI client to connect to this Temps instance over MCP. Clients: claude-code, claude-desktop, codex, cursor, vscode, windsurf, zed
- `bridge` - Run the Temps MCP server on this computer over stdio, adding a tool that deploys local folders. AI apps start this themselves (see mcp add --deploy-folder); the API key comes from TEMPS_API_KEY
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

**Usage:** `mcp add [client] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-g, --groups <groups>` | Legacy tool-group filter (optional; does not restrict temps/temps_write permissions) | - | No |
| `-w, --write` | Enable write tools (deploy, delete, restart, etc). Default: read-only | - | No |
| `-k, --api-key <key>` | Use this API key instead of creating or prompting for one | - | No |
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |
| `-y, --yes` | Skip prompts and confirmation (uses defaults; requires --api-key or an existing login; creates a key for the app: read-only, or with --write one that can make changes) | - | No |
| `-d, --deploy-folder <dir>` | Let the agent deploy project folders from this computer, from inside this folder (repeatable). The app then runs the Temps bridge locally (temps mcp bridge) instead of connecting over HTTP. Implies --write | - | No |
| `--api-key-file <path>` | Read the API key from a file (- reads stdin) instead of --api-key | - | No |
| `--api-key-stdin` | Read the API key from stdin instead of --api-key | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `mcp bridge`

Run the Temps MCP server on this computer over stdio, adding a tool that deploys local folders. AI apps start this themselves (see mcp add --deploy-folder); the API key comes from TEMPS_API_KEY

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | The address of your Temps console (default: TEMPS_URL) | - | No |
| `-f, --folder <dir>` | A folder the agent may deploy from, with everything inside it (repeatable) | - | No |

### `mcp remove`

Remove the Temps MCP server from an AI client

**Usage:** `mcp remove [client]`

### `mcp status`

Show whether this instance has MCP enabled and which AI clients are configured

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-u, --url <url>` | Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted. | - | No |
