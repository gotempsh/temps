<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `functions` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `functions` (alias: `fn`)

Manage scale-to-zero functions (WASM modules or OCI images) in a project

**Subcommands:**

- `capability` - Show whether this Temps instance can run functions, and what is missing
- `list` (`ls`) - List the functions of a project
- `show` - Show a function (ID or name), including why it is not serving
- `create` - Create a function from a WASM module or an OCI image
- `rm` (`delete`) - Delete a function (ID or name)
- `sleep` - Stop a running function now; the next request wakes it
- `wake` - Start a function now instead of on its next request

### `functions capability`

Show whether this Temps instance can run functions, and what is missing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `functions list` (alias: `ls`)

List the functions of a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `functions show`

Show a function (ID or name), including why it is not serving

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `functions create`

Create a function from a WASM module or an OCI image

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--name <name>` | Function name; part of its hostname | - | No |
| `--wasm <file>` | WASI preview 1 module (wasm32-wasip1) serving HTTP CGI-style | - | No |
| `--image <ref>` | OCI image reference serving HTTP on --port | - | No |
| `--port <port>` | Port the function serves on (default 8080) | - | No |
| `--memory <mb>` | Memory limit in MiB (1-4096) | - | No |
| `--idle-timeout <seconds>` | Stop after this many idle seconds (10-86400, default 300) | - | No |
| `--wake-timeout <seconds>` | How long the first request waits for a start (1-300, default 30) | - | No |
| `--json` | Output in JSON format | - | No |

### `functions rm` (alias: `delete`)

Delete a function (ID or name)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `functions sleep`

Stop a running function now; the next request wakes it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `functions wake`

Start a function now instead of on its next request

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |
