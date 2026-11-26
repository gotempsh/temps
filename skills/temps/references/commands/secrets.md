<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `secrets` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `secrets` (alias: `secret`)

Manage agent secrets. env-type: reference as ${TEMPS_SECRET:name} in MCP config. file-type: written to --mount-path in sandbox; reference that path.

**Subcommands:**

- `list` (`ls`) - List all secrets (values are masked)
- `create` (`add`) - Create or update a secret (upsert by name). Prefer --value-file or --value-stdin, which keep the value out of shell history
- `update` - Update an existing secret (alias for create — upserts). The value must be re-supplied: the server never returns it
- `delete` (`rm`) - Delete a secret

### `secrets list` (alias: `ls`)

List all secrets (values are masked)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `secrets create` (alias: `add`)

Create or update a secret (upsert by name). Prefer --value-file or --value-stdin, which keep the value out of shell history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | Secret value on the command line (visible in shell history and `ps`). `-v @path` reading a file is deprecated: use --value-file | - | No |
| `-t, --type <type>` | Secret type: "env" (default) or "file" | `env` | No |
| `-m, --mount-path <path>` | Absolute path inside sandbox where file-type secret is written (required for --type file) | - | No |
| `-d, --description <description>` | Human-readable description | - | No |
| `-p, --project <slug-or-id>` | Scope the secret to one project (default: instance-wide) | - | No |
| `--value-file <path>` | Read the secret value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the secret value from stdin, e.g. piped from a secret manager | - | No |
| `--literal` | Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `secrets update`

Update an existing secret (alias for create — upserts). The value must be re-supplied: the server never returns it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Secret name | - | Yes |
| `-v, --value <value>` | New value on the command line (visible in shell history). `-v @path` is deprecated: use --value-file | - | No |
| `-t, --type <type>` | Secret type: "env" or "file" | - | No |
| `-m, --mount-path <path>` | New mount path (file type only) | - | No |
| `-d, --description <description>` | New description | - | No |
| `-p, --project <slug-or-id>` | The project the secret is scoped to (default: instance-wide) | - | No |
| `--value-file <path>` | Read the new secret value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the new secret value from stdin, e.g. piped from a secret manager | - | No |
| `--literal` | Send a -v value that starts with @ as is, instead of reading it as a file path (deprecated form) | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `secrets delete` (alias: `rm`)

Delete a secret

**Usage:** `secrets delete <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Delete the secret of this project (default: the instance-wide one) | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
