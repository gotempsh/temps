<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `secrets` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `secrets` (alias: `secret`)

Manage agent secrets. env-type: reference as ${TEMPS_SECRET:name} in MCP config. file-type: written to --mount-path in sandbox; reference that path.

**Subcommands:**

- `list` (`ls`) - List all secrets (values are masked)
- `create` (`add`) - Create or update a secret (upsert by name)
- `update` - Update an existing secret (alias for create — upserts)
- `delete` (`rm`) - Delete a secret

### `secrets list` (alias: `ls`)

List all secrets (values are masked)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `secrets create` (alias: `add`)

Create or update a secret (upsert by name)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | Secret value. Prefix with @ to read from file (e.g. @./creds.json) | - | Yes |
| `-t, --type <type>` | Secret type: "env" (default) or "file" | `env` | No |
| `-m, --mount-path <path>` | Absolute path inside sandbox where file-type secret is written (required for --type file) | - | No |
| `-d, --description <description>` | Human-readable description | - | No |

### `secrets update`

Update an existing secret (alias for create — upserts)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | New value. Prefix with @ to read from file | - | No |
| `-t, --type <type>` | Secret type: "env" or "file" | - | No |
| `-m, --mount-path <path>` | New mount path (file type only) | - | No |
| `-d, --description <description>` | New description | - | No |

### `secrets delete` (alias: `rm`)

Delete a secret

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
