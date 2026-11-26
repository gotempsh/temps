<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `blob` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `blob`

Upload, download and list a project's files in Blob storage, and manage the Blob service

**Subcommands:**

- `list` (`ls`) - List blobs, optionally under a pathname prefix
- `put` (`upload`) - Upload a local file
- `get` (`download`) - Download a blob to a file, or to stdout with -o -
- `head` (`stat`) - Show a blob's metadata (size, content type, last modified) without downloading it
- `copy` (`cp`) - Copy a blob (by pathname or URL) to a new pathname
- `del` (`delete`) - Delete one or more blobs
- `status` - Show whether the Blob service is enabled and healthy on this instance
- `enable` - Enable the Blob service on this instance (starts its S3-compatible container)
- `update` - Change the Docker image the Blob service runs (recreates its container)
- `disable` - Disable the Blob service on this instance (stops its container; every project loses Blob access)
- `container-name` - Show, or set with --container-name/--reset, the Docker container name of the Blob service

### `blob list` (alias: `ls`)

List blobs, optionally under a pathname prefix

**Usage:** `blob list [prefix] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--limit <n>` | Blobs per page | - | No |
| `--cursor <token>` | Continue from the cursor a previous page returned | - | No |
| `--all` | Follow cursors to list everything (at most 100 pages) | - | No |
| `--json` | Output in JSON format | - | No |

### `blob put` (alias: `upload`)

Upload a local file

**Usage:** `blob put <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--pathname <path>` | Where to store it (default: the file name) | - | No |
| `--content-type <type>` | Content type (default: guessed from the extension) | - | No |
| `--random-suffix` | Add a random suffix to the pathname so it never overwrites | - | No |
| `--json` | Output in JSON format | - | No |

### `blob get` (alias: `download`)

Download a blob to a file, or to stdout with -o -

**Usage:** `blob get <pathname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `-o, --output <path>` | Where to write it (default: its file name in this directory; - for stdout) | - | No |

### `blob head` (alias: `stat`)

Show a blob's metadata (size, content type, last modified) without downloading it

**Usage:** `blob head <pathname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `blob copy` (alias: `cp`)

Copy a blob (by pathname or URL) to a new pathname

**Usage:** `blob copy <from> <to> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `blob del` (alias: `delete`)

Delete one or more blobs

**Usage:** `blob del <pathnames...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (defaults to the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `blob status`

Show whether the Blob service is enabled and healthy on this instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `blob enable`

Enable the Blob service on this instance (starts its S3-compatible container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run (default: the one this Temps release pins) | - | No |
| `--root-user <user>` | Root user for S3 access (default: generated) | - | No |
| `--root-password <password>` | Root password for S3 access (prefer --root-password-stdin) | - | No |
| `--json` | Output in JSON format | - | No |
| `--root-password-file <path>` | Read the root password from a file (- reads stdin) instead of --root-password | - | No |
| `--root-password-stdin` | Read the root password from stdin instead of --root-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `blob update`

Change the Docker image the Blob service runs (recreates its container)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <ref>` | Docker image to run | - | Yes |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `blob disable`

Disable the Blob service on this instance (stops its container; every project loses Blob access)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `blob container-name`

Show, or set with --container-name/--reset, the Docker container name of the Blob service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--container-name <name>` | New container name (must keep the required prefix) | - | No |
| `--reset` | Go back to the default container name | - | No |
| `--json` | Output in JSON format | - | No |
