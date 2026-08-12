<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `mcp-servers` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `mcp-servers` (alias: `mcp`)

Manage MCP server definitions (global or project-scoped)

**Subcommands:**

- `list` (`ls`) - List MCP server definitions
- `create` (`add`) - Create a new MCP server definition
- `update` - Update an existing MCP server definition
- `delete` (`rm`) - Delete an MCP server definition

### `mcp-servers list` (alias: `ls`)

List MCP server definitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | List global (platform-wide) MCP servers | - | No |
| `--project <slug>` | List MCP servers for a specific project | - | No |
| `--json` | Output in JSON format | - | No |

### `mcp-servers create` (alias: `add`)

Create a new MCP server definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | MCP server name | - | Yes |
| `-s, --slug <slug>` | MCP server slug (URL-safe identifier) | - | Yes |
| `-c, --config <config>` | MCP server config (JSON). Prefix with @ to read from file (e.g. @./mcp.json) | - | Yes |
| `-d, --description <description>` | MCP server description | - | No |
| `--global` | Create as global (platform-wide) MCP server | - | No |
| `--project <slug>` | Create MCP server for a specific project | - | No |

### `mcp-servers update`

Update an existing MCP server definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | New name | - | No |
| `-c, --config <config>` | New config (JSON). Prefix with @ to read from file | - | No |
| `-d, --description <description>` | New description | - | No |
| `--global` | Update a global MCP server | - | No |
| `--project <slug>` | Update a project-scoped MCP server | - | No |

### `mcp-servers delete` (alias: `rm`)

Delete an MCP server definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--global` | Delete a global MCP server | - | No |
| `--project <slug>` | Delete a project-scoped MCP server | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
