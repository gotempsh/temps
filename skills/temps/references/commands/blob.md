<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `blob` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `blob`

Blob storage commands (coming soon)

**Subcommands:**

- `list` (`ls`) - List blobs in a project
- `upload` (`put`) - Upload a file as a blob
- `delete` (`rm`) - Delete a blob
- `copy` (`cp`) - Copy a blob to a new key
- `download` (`get`) - Download a blob to a local file
- `head` - Get blob metadata (size, content type, etc.)
- `enable` - Enable blob storage for a project
- `disable` - Disable blob storage for a project
- `status` - Get blob storage status for a project

### `blob list` (alias: `ls`)

List blobs in a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--prefix <prefix>` | Filter by key prefix | - | No |
| `--json` | Output in JSON format | - | No |

### `blob upload` (alias: `put`)

Upload a file as a blob

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key (path) | - | Yes |
| `--file <path>` | Local file path to upload | - | Yes |

### `blob delete` (alias: `rm`)

Delete a blob

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key to delete | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `blob copy` (alias: `cp`)

Copy a blob to a new key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--source <key>` | Source blob key | - | Yes |
| `--dest <key>` | Destination blob key | - | Yes |

### `blob download` (alias: `get`)

Download a blob to a local file

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key to download | - | Yes |
| `--output <path>` | Local file path to save to | - | Yes |

### `blob head`

Get blob metadata (size, content type, etc.)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--key <key>` | Blob key | - | Yes |
| `--json` | Output in JSON format | - | No |

### `blob enable`

Enable blob storage for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `blob disable`

Disable blob storage for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |

### `blob status`

Get blob storage status for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |
