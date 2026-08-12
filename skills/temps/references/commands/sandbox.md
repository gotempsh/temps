<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `sandbox` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `sandbox`

Manage standalone sandboxes (/v1/sandbox API)

**Subcommands:**

- `create` - Create a new sandbox
- `list` (`ls`) - List your sandboxes
- `show` - Show details for a sandbox
- `rm` (`stop`, `destroy`) - Remove a sandbox permanently (aliases: stop, destroy)
- `pause` - Pause a running sandbox (non-destructive — resume later with `sandbox resume`)
- `resume` - Resume a paused sandbox
- `restart` - Restart a running sandbox (preserves filesystem)
- `clone` - Clone a git repo or extract a tarball into a running sandbox
- `shell` (`attach`) - Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab
- `extend` - Extend a sandbox's idle timeout
- `exec` - Run a command inside a sandbox. Use `--` to pass flags: `exec ID -- ls -la`
- `logs` - Stream logs from a detached job (SSE)
- `domain` - Resolve the preview URL for a port inside a sandbox
- `password` - Generate, rotate, or clear the preview-URL password for a sandbox
- `fs` - Filesystem operations inside a sandbox

### `sandbox create`

Create a new sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | Docker image override (uses platform default when omitted) | - | No |
| `--name <name>` | Display name for the sandbox | - | No |
| `--timeout <seconds>` | Idle timeout in seconds (clamped to [60, 86400]) | - | No |
| `-e, --env <KEY=VAL>` | Env var baked into the container (repeatable) | - | No |
| `--cpu-limit <cpu>` | CPU limit (e.g., 0.5 for half a core) | - | No |
| `--memory-mb <mb>` | Memory limit in megabytes | - | No |
| `--git-url <url>` | Git repo URL to clone into the work dir | - | No |
| `--git-rev <revision>` | Git revision to check out (requires --git-url) | - | No |
| `--git-depth <n>` | Shallow clone depth (requires --git-url) | - | No |
| `--git-connection <id>` | ID of a stored git provider connection; temps injects the token server-side | - | No |
| `--git-username <user>` | HTTP Basic username for private repo clone (requires --git-password) | - | No |
| `--git-password <token>` | HTTP Basic password/token (paired with --git-username; injected via GIT_ASKPASS) | - | No |
| `--tarball-url <url>` | Tarball URL to download and extract | - | No |
| `--workspace` | Create a persistent workspace: suspends when idle, wakes automatically on the next command, and is never destroyed for you | - | No |
| `--project <slug>` | Seed from a temps project's connected repo (and attribute the sandbox to it). Defaults to the linked project in .temps/config.json | - | No |
| `--repo <owner/name>` | Seed from a repo on one of your git connections that has no temps project | - | No |
| `--branch <ref>` | Branch, tag, or SHA to check out (alias of --git-rev) | - | No |
| `--new-branch <name>` | Create and switch to a new branch after cloning, based on whatever was checked out | - | No |
| `--preview-password` | Generate a random preview-URL password and print it once on stdout | - | No |
| `--preview-password-length <n>` | Length of the generated preview password (8..=256, default 24) | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox list` (alias: `ls`)

List your sandboxes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page (1-indexed) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--workspace` | Show only persistent workspaces | - | No |
| `--lifecycle <class>` | Filter by lifecycle class: ephemeral \| workspace | - | No |
| `--project <slug>` | Show only sandboxes created from this project | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox show`

Show details for a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `sandbox rm` (alias: `stop`, `destroy`)

Remove a sandbox permanently (aliases: stop, destroy)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |

### `sandbox pause`

Pause a running sandbox (non-destructive — resume later with `sandbox resume`)

### `sandbox resume`

Resume a paused sandbox

### `sandbox restart`

Restart a running sandbox (preserves filesystem)

### `sandbox clone`

Clone a git repo or extract a tarball into a running sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--git-url <url>` | Git repo URL to clone | - | No |
| `--git-rev <revision>` | Git revision (branch/tag/SHA) to check out | - | No |
| `--git-depth <n>` | Shallow clone depth | - | No |
| `--git-connection <id>` | ID of a stored git provider connection; temps injects the token server-side | - | No |
| `--git-username <user>` | HTTP Basic username (pairs with --git-password) | - | No |
| `--git-password <token>` | HTTP Basic password/token (injected via GIT_ASKPASS) | - | No |
| `--tarball-url <url>` | Tarball URL to download and extract | - | No |

### `sandbox shell` (alias: `attach`)

Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tab <name>` | Tab to attach to; reusing a name reattaches to the program already running in it | `main` | No |
| `--cmd <command>` | Program to start when the tab is created, e.g. "claude" (default: login shell) | - | No |

### `sandbox extend`

Extend a sandbox's idle timeout

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--secs <seconds>` | Extra seconds to add to the current expiry | - | Yes |

### `sandbox exec`

Run a command inside a sandbox. Use `--` to pass flags: `exec ID -- ls -la`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--detach` | Start in background and print a job ID instead of waiting | - | No |
| `--cwd <path>` | Working directory inside the sandbox | - | No |
| `-e, --env <KEY=VAL>` | Env var for this exec (repeatable) | - | No |

### `sandbox logs`

Stream logs from a detached job (SSE)

### `sandbox domain`

Resolve the preview URL for a port inside a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--port <port>` | Port inside the sandbox (1..=65535) | - | Yes |

### `sandbox password`

Generate, rotate, or clear the preview-URL password for a sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--rotate` | Generate a new random password and set it (default when no flag is given) | - | No |
| `--length <n>` | Length of the generated password (8..=256, default 24) | - | No |
| `--clear` | Remove the preview password — preview URLs become open again | - | No |

### `sandbox fs`

Filesystem operations inside a sandbox

**Subcommands:**

- `read` - Read a file from the sandbox
- `write` - Write a file to the sandbox
- `stat` - Stat a path inside the sandbox
- `mkdir` - Create a directory inside the sandbox (mkdir -p)

#### `sandbox fs read`

Read a file from the sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute file path inside the sandbox | - | Yes |
| `--out <localPath>` | Write to this local file (stdout when omitted) | - | No |

#### `sandbox fs write`

Write a file to the sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute target path inside the sandbox | - | Yes |
| `--file <localPath>` | Local source file to upload (mutually exclusive with --content) | - | No |
| `--content <string>` | Inline string content to write | - | No |
| `--mode <octal>` | Unix permission mask (default: 0644) | - | No |

#### `sandbox fs stat`

Stat a path inside the sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |
| `--json` | Output as JSON | - | No |

#### `sandbox fs mkdir`

Create a directory inside the sandbox (mkdir -p)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |
