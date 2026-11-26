<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `sandbox` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`sandbox create`](#sandbox-create)
- [`sandbox list`](#sandbox-list-alias-ls)
- [`sandbox nodes`](#sandbox-nodes)
- [`sandbox show`](#sandbox-show)
- [`sandbox rm`](#sandbox-rm-alias-stop-destroy)
- [`sandbox pause`](#sandbox-pause)
- [`sandbox resume`](#sandbox-resume)
- [`sandbox restart`](#sandbox-restart)
- [`sandbox clone`](#sandbox-clone)
- [`sandbox shell`](#sandbox-shell-alias-attach)
- [`sandbox extend`](#sandbox-extend)
- [`sandbox exec`](#sandbox-exec)
- [`sandbox logs`](#sandbox-logs)
- [`sandbox domain`](#sandbox-domain)
- [`sandbox password`](#sandbox-password)
- [`sandbox preview-access`](#sandbox-preview-access)
- [`sandbox egress`](#sandbox-egress)
- [`sandbox fs`](#sandbox-fs)
- [`sandbox snapshots`](#sandbox-snapshots)
- [`sandbox snapshot`](#sandbox-snapshot)
- [`sandbox service-access`](#sandbox-service-access)
- [`sandbox internal-access`](#sandbox-internal-access)
- [`sandbox services`](#sandbox-services)
- [`sandbox get-creation`](#sandbox-get-creation)
- [`sandbox get-image-preparation`](#sandbox-get-image-preparation)
- [`sandbox prepare-images`](#sandbox-prepare-images)
- [`sandbox update-host-image-preparation`](#sandbox-update-host-image-preparation)
- [`sandbox get-resource-defaults`](#sandbox-get-resource-defaults)
- [`sandbox rootfs-report`](#sandbox-rootfs-report)
- [`sandbox rootfs-gc`](#sandbox-rootfs-gc)
- [`sandbox get-cmd`](#sandbox-get-cmd)
- [`sandbox cmd-logs`](#sandbox-cmd-logs)
- [`sandbox cmd`](#sandbox-cmd)
- [`sandbox get-creation-by`](#sandbox-get-creation-by)
- [`sandbox list-dir`](#sandbox-list-dir)
- [`sandbox write-files`](#sandbox-write-files)
- [`sandbox list-jobs`](#sandbox-list-jobs)
- [`sandbox job-status`](#sandbox-job-status)
- [`sandbox kill-job`](#sandbox-kill-job)
- [`sandbox create-preview-link`](#sandbox-create-preview-link)
- [`sandbox resize`](#sandbox-resize)
- [`sandbox issue-runtime-environment`](#sandbox-issue-runtime-environment)
- [`sandbox cmd-kill`](#sandbox-cmd-kill)

## `sandbox`

Manage standalone sandboxes (/v1/sandbox API)

**Subcommands:**

- `create` - Create a new sandbox
- `list` (`ls`) - List your sandboxes
- `nodes` - List nodes that can run sandboxes and control which ones take new sandboxes
- `show` - Show a sandbox: status, image, node, expiry, preview access and outbound policy
- `rm` (`stop`, `destroy`) - Remove a sandbox permanently (aliases: stop, destroy)
- `pause` - Pause a running sandbox (non-destructive — resume later with `sandbox resume`)
- `resume` - Resume a paused sandbox
- `restart` - Restart a running sandbox (preserves filesystem)
- `clone` - Clone a git repo or extract a tarball into a running sandbox
- `shell` (`attach`) - Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab
- `extend` - Extend a sandbox's idle timeout
- `exec` - Run a command inside a sandbox, print its output and exit with its exit code. Put `--` before the command: `exec ID -- ls -la`
- `logs` - Print the output of a job started with `exec --detach`, following it until the job ends
- `domain` - Resolve the preview URL for a port inside a sandbox
- `password` - Generate, rotate, or clear the preview-URL password for a sandbox
- `preview-access` - Show or set who may open the preview URLs: Temps login (default), the preview password, or anyone, optionally from listed networks only
- `egress` - Show or set where the sandbox may connect to: the internet, nothing, or only listed hosts and CIDRs. Private, loopback and metadata addresses are always refused
- `fs` - Filesystem operations inside a sandbox
- `snapshots` - Manage sandbox snapshots (ADR-037)
- `snapshot` - Take a snapshot of a sandbox
- `service-access` - Service access commands
- `internal-access` - Internal access commands
- `services` - Services commands
- `get-creation` - Read a create's steps by the 'creation_id' its 'POST /v1/sandboxes' sent
- `get-image-preparation` - Every host that can run sandboxes, with whether its default sandbox image is pulled (with download..
- `prepare-images` - Pull the default sandbox image on every eligible host (or one), so the first sandbox there starts without..
- `update-host-image-preparation` - Turn keeping one host's sandbox image prepared on or off
- `get-resource-defaults` - Effective resource defaults and input ceilings for the requested placement
- `rootfs-report` - Inspect rootfs storage: the Firecracker digest-keyed cache (with which sandboxes reference each entry) and..
- `rootfs-gc` - Reclaim rootfs cache entries not backing any live sandbox
- `get-cmd` - Get cmd
- `cmd-logs` - Stream a command's stdout/stderr as 'application/x-ndjson' ('@vercel/sandbox'-compatible)
- `cmd` - Run a command the '@vercel/sandbox' way: without --wait it returns the started command's id at once (read its output with cmd-logs); with --wait it prints the running and finished envelopes as JSON lines. For an interactive run use `temps sandbox exec`
- `get-creation-by` - Read the steps of the create that made a sandbox, while this server still tracks it (it is running, or..
- `list-dir` - List dir
- `write-files` - Batch-write multiple files in a single request
- `list-jobs` - List jobs
- `job-status` - Job status
- `kill-job` - Terminate a detached job
- `create-preview-link` - Mint a shareable link to a sandbox preview
- `resize` - Grow a Firecracker sandbox's root disk
- `issue-runtime-environment` - Issue the same scoped service variables used by a deployment runtime for the project attached to this sandbox
- `cmd-kill` - Kill a running command ('@vercel/sandbox'-compatible)

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
| `--pids-limit <n>` | Maximum number of processes (default 512) | - | No |
| `--creation-id <id>` | Your own id for this create (8-64 letters, digits, - or _), to follow its steps with `sandbox get-creation` | - | No |
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
| `--egress-allow <host\|cidr[:ports]>` | Restrict outbound traffic to this destination (repeatable; no ports = 80 and 443). IPv6 with ports: [addr/prefix]:port | - | No |
| `--egress-deny-all` | Allow no outbound traffic at all | - | No |
| `--from-snapshot <snap-id>` | Create sandbox from a snapshot (mutually exclusive with --image) | - | No |
| `--node <name\|id>` | Node to run the sandbox on (name, id, or 'control-plane'). Omit to let Temps place it; see `sandbox nodes` | - | No |
| `--backend <backend>` | Isolation backend: docker, firecracker, microsandbox. Omit for the host default. An unavailable backend fails instead of downgrading | - | No |
| `--allow-docker` | Allow installing/running Docker inside microsandbox; relaxes guest privilege restrictions. Requires --backend microsandbox | - | No |
| `--json` | Output as JSON | - | No |
| `--git-password-file <path>` | Read the Git HTTP password or token from a file (- reads stdin) instead of --git-password | - | No |
| `--git-password-stdin` | Read the Git HTTP password or token from stdin instead of --git-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

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

### `sandbox nodes`

List nodes that can run sandboxes and control which ones take new sandboxes

**Subcommands:**

- `list` - List nodes with their sandbox placement state (the default)
- `show` - Show a node's placement state and every live sandbox on it, from all users (name, id, or 'control-plane'). Admin only
- `set` - Allow exactly these nodes to run new sandboxes, replacing the list (names, ids, or 'control-plane'). Admin only
- `allow` - Add nodes to the list allowed to run new sandboxes. Admin only
- `deny` - Stop nodes from taking new sandboxes (existing ones keep running). Admin only
- `allow-all` - Allow every node, including the control plane, to run sandboxes (the default). Admin only
- `deny-all` - Stop every node from taking new sandboxes (existing ones keep running). Admin only
- `evict` - Destroy every sandbox on a worker node, from all users, so the node can be removed. Works on a node that is offline for good. Admin only

#### `sandbox nodes list`

List nodes with their sandbox placement state (the default)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes show`

Show a node's placement state and every live sandbox on it, from all users (name, id, or 'control-plane'). Admin only

**Usage:** `sandbox nodes show <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default 1) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--json` | Output as JSON | - | No |

#### `sandbox nodes set`

Allow exactly these nodes to run new sandboxes, replacing the list (names, ids, or 'control-plane'). Admin only

**Usage:** `sandbox nodes set <nodes...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes allow`

Add nodes to the list allowed to run new sandboxes. Admin only

**Usage:** `sandbox nodes allow <nodes...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes deny`

Stop nodes from taking new sandboxes (existing ones keep running). Admin only

**Usage:** `sandbox nodes deny <nodes...> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes allow-all`

Allow every node, including the control plane, to run sandboxes (the default). Admin only

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes deny-all`

Stop every node from taking new sandboxes (existing ones keep running). Admin only

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox nodes evict`

Destroy every sandbox on a worker node, from all users, so the node can be removed. Works on a node that is offline for good. Admin only

**Usage:** `sandbox nodes evict <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox show`

Show a sandbox: status, image, node, expiry, preview access and outbound policy

**Usage:** `sandbox show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `sandbox rm` (alias: `stop`, `destroy`)

Remove a sandbox permanently (aliases: stop, destroy)

**Usage:** `sandbox rm <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |

### `sandbox pause`

Pause a running sandbox (non-destructive — resume later with `sandbox resume`)

**Usage:** `sandbox pause <id>`

### `sandbox resume`

Resume a paused sandbox

**Usage:** `sandbox resume <id>`

### `sandbox restart`

Restart a running sandbox (preserves filesystem)

**Usage:** `sandbox restart <id>`

### `sandbox clone`

Clone a git repo or extract a tarball into a running sandbox

**Usage:** `sandbox clone <id> [options]`

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
| `--strip-git-metadata` | Remove the .git directory after cloning | - | No |
| `--git-password-file <path>` | Read the Git HTTP password or token from a file (- reads stdin) instead of --git-password | - | No |
| `--git-password-stdin` | Read the Git HTTP password or token from stdin instead of --git-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `sandbox shell` (alias: `attach`)

Open an interactive terminal in a sandbox. Detach with Ctrl-P Ctrl-Q to leave the program running; `exit` ends it. Reattach with the same --tab

**Usage:** `sandbox shell <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tab <name>` | Tab to attach to; reusing a name reattaches to the program already running in it | `main` | No |
| `--cmd <command>` | Program to start when the tab is created, e.g. "claude" (default: login shell) | - | No |

### `sandbox extend`

Extend a sandbox's idle timeout

**Usage:** `sandbox extend <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--secs <seconds>` | Extra seconds to add to the current expiry | - | Yes |

### `sandbox exec`

Run a command inside a sandbox, print its output and exit with its exit code. Put `--` before the command: `exec ID -- ls -la`

**Usage:** `sandbox exec <id> [args...] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--detach` | Start in background and print a job ID instead of waiting | - | No |
| `--cwd <path>` | Working directory inside the sandbox | - | No |
| `-e, --env <KEY=VAL>` | Env var for this exec (repeatable) | - | No |

### `sandbox logs`

Print the output of a job started with `exec --detach`, following it until the job ends

**Usage:** `sandbox logs <id> <jobId>`

### `sandbox domain`

Resolve the preview URL for a port inside a sandbox

**Usage:** `sandbox domain <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--port <port>` | Port inside the sandbox (1..=65535) | - | Yes |

### `sandbox password`

Generate, rotate, or clear the preview-URL password for a sandbox

**Usage:** `sandbox password <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--rotate` | Generate a new random password and set it (default when no flag is given) | - | No |
| `--length <n>` | Length of the generated password (8..=256, default 24) | - | No |
| `--clear` | Remove the preview password — preview URLs become open again | - | No |

### `sandbox preview-access`

Show or set who may open the preview URLs: Temps login (default), the preview password, or anyone, optionally from listed networks only

**Usage:** `sandbox preview-access <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--access <mode>` | temps_login (alias 'login'), password, or public. 'password' needs a preview password first (`sandbox password`) | - | No |
| `--allow-ip <ip\|cidr>` | Only these client networks may open previews, in every mode (repeatable; replaces the list) | - | No |
| `--any-ip` | Clear the allow-list: any client address may open previews | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox egress`

Show or set where the sandbox may connect to: the internet, nothing, or only listed hosts and CIDRs. Private, loopback and metadata addresses are always refused

**Usage:** `sandbox egress <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--internet` | Remove the restriction | - | No |
| `--deny-all` | Allow no outbound traffic at all | - | No |
| `--allow <host\|cidr[:ports]>` | Allow only these destinations (repeatable; replaces the policy; no ports = 80 and 443). IPv6 with ports: [addr/prefix]:port | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox fs`

Filesystem operations inside a sandbox

**Subcommands:**

- `read` - Read a file from the sandbox
- `write` - Write a file to the sandbox
- `stat` - Stat a path inside the sandbox
- `mkdir` - Create a directory inside the sandbox (mkdir -p)

#### `sandbox fs read`

Read a file from the sandbox

**Usage:** `sandbox fs read <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute file path inside the sandbox | - | Yes |
| `--out <localPath>` | Write to this local file (stdout when omitted) | - | No |

#### `sandbox fs write`

Write a file to the sandbox

**Usage:** `sandbox fs write <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute target path inside the sandbox | - | Yes |
| `--file <localPath>` | Local source file to upload (mutually exclusive with --content) | - | No |
| `--content <string>` | Inline string content to write | - | No |
| `--mode <octal>` | Unix permission mask (default: 0644) | - | No |

#### `sandbox fs stat`

Stat a path inside the sandbox

**Usage:** `sandbox fs stat <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |
| `--json` | Output as JSON | - | No |

#### `sandbox fs mkdir`

Create a directory inside the sandbox (mkdir -p)

**Usage:** `sandbox fs mkdir <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <path>` | Absolute path inside the sandbox | - | Yes |

### `sandbox snapshots`

Manage sandbox snapshots (ADR-037)

**Subcommands:**

- `list` (`ls`) - List your snapshots
- `show` - Show details for a snapshot
- `delete` (`rm`) - Delete a snapshot permanently
- `storage` - Show snapshot storage usage and quota

#### `sandbox snapshots list` (alias: `ls`)

List your snapshots

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project <id>` | Filter by project ID | - | No |
| `--status <status>` | Filter by status: creating \| ready \| failed \| deleted | - | No |
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Items per page (default 20, max 100) | - | No |
| `--json` | Output as JSON | - | No |

#### `sandbox snapshots show`

Show details for a snapshot

**Usage:** `sandbox snapshots show <snapId> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `sandbox snapshots delete` (alias: `rm`)

Delete a snapshot permanently

**Usage:** `sandbox snapshots delete <snapId> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation prompt | - | No |

#### `sandbox snapshots storage`

Show snapshot storage usage and quota

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

### `sandbox snapshot`

Take a snapshot of a sandbox

**Usage:** `sandbox snapshot <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--label <label>` | Human-readable label for the snapshot | - | No |
| `--wait` | Wait until the snapshot reaches ready or failed status | - | No |
| `--json` | Output as JSON | - | No |

### `sandbox service-access`

Service access commands

**Subcommands:**

- `list` - List access services
- `create` - Create access service
- `get` - Get access service
- `update` - Update access service
- `list-grants` - List service grants
- `request-grant` - Request service grant
- `approve-grant` - Approve service grant
- `revoke-grant` - Revoke service grant
- `capability` - Access capability

#### `sandbox service-access list`

List access services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access create`

Create access service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access get`

Get access service

**Usage:** `sandbox service-access get <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access update`

Update access service

**Usage:** `sandbox service-access update <service-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--clear-credential` | Body field clear_credential | - | No |
| `--no-clear-credential` | Set clear_credential to false | - | No |
| `--credential-stdin` | A credential (read from stdin) | - | No |
| `--credential-file <path>` | A credential (read from a file) | - | No |
| `--enabled` | Allows requests to be saved; does not enable a network connection [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--expected-version <n>` | Required for updates; omit for creation | - | No |
| `--max-ttl-seconds <n>` | Maximum lifetime of a request, between 60 and 86400 seconds [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--policy <json>` | Exact destination and request scope (JSON) [required] | - | No |
| `--vault-id <value>` | Add a newly created service to this vault in the same transaction | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access list-grants`

List service grants

**Usage:** `sandbox service-access list-grants <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access request-grant`

Request service grant

**Usage:** `sandbox service-access request-grant <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--service-id <value>` | Body field service_id [required] | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--ttl-seconds <n>` | Absolute expiry is calculated at creation, even if the broker is unavailable [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access approve-grant`

Approve service grant

**Usage:** `sandbox service-access approve-grant <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--request-id <value>` | Body field request_id [required] | - | No |
| `--service-id <value>` | Body field service_id [required] | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--ttl-seconds <n>` | Body field ttl_seconds [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access revoke-grant`

Revoke service grant

**Usage:** `sandbox service-access revoke-grant <sandbox-id> <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox service-access capability`

Access capability

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox internal-access`

Internal access commands

**Subcommands:**

- `list-grants` - List the projects allowed to call this project's internal addresses
- `create-grant` - Allow another project to call this project's internal addresses
- `list-internal-access-sandbox-grants` - List the sandboxes allowed to call this project's internal addresses
- `create-internal-access-sandbox-grant` - Allow one of your sandboxes to call this project's internal addresses
- `delete-grant` - Stop a sandbox from calling this project's internal addresses
- `delete-internal-access-grant` - Stop another project from calling this project's internal addresses
- `list-addresses` - List the internal addresses one of your sandboxes may call

#### `sandbox internal-access list-grants`

List the projects allowed to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Only grants that cover this environment of the project: its own and the project-wide ones | - | No |
| `--page <n>` | 1-based page, default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access create-grant`

Allow another project to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--caller-environment-id <n>` | Only this environment of the calling project; omit for every environment, present and future | - | No |
| `--caller-project-id <n>` | The project whose workloads may call [required] | - | No |
| `--description <value>` | Why the grant exists, up to 200 characters | - | No |
| `--target-environment-id <n>` | The environment of this project that may be called; omit for every environment, present and future | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access list-internal-access-sandbox-grants`

List the sandboxes allowed to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Only grants that cover this environment of the project: its own and the project-wide ones | - | No |
| `--page <n>` | 1-based page, default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access create-internal-access-sandbox-grant`

Allow one of your sandboxes to call this project's internal addresses

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--description <value>` | Why the grant exists, up to 200 characters | - | No |
| `--sandbox-id <value>` | Public id of one of your sandboxes [required] | - | No |
| `--target-environment-id <n>` | The environment of this project that may be called; omit for every environment, present and future | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access delete-grant`

Stop a sandbox from calling this project's internal addresses

**Usage:** `sandbox internal-access delete-grant <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access delete-internal-access-grant`

Stop another project from calling this project's internal addresses

**Usage:** `sandbox internal-access delete-internal-access-grant <grant-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox internal-access list-addresses`

List the internal addresses one of your sandboxes may call

**Usage:** `sandbox internal-access list-addresses <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | 1-based page, default 1 | - | No |
| `--page-size <n>` | Default 20, at most 100 | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox services`

Services commands

**Subcommands:**

- `list` - List sandbox services
- `request` - Request sandbox service
- `approve` - Approve sandbox service
- `revoke` - Revoke sandbox service

#### `sandbox services list`

List sandbox services

**Usage:** `sandbox services list <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--page-size <n>` | Query parameter page_size | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox services request`

Request sandbox service

**Usage:** `sandbox services request <sandbox-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--service-id <value>` | Body field service_id [required] | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox services approve`

Approve sandbox service

**Usage:** `sandbox services approve <sandbox-id> <access-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version [required] | - | No |
| `--expires-at <value>` | Omit for the sandbox lifetime | - | No |
| `--service-version <n>` | Body field service_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `sandbox services revoke`

Revoke sandbox service

**Usage:** `sandbox services revoke <sandbox-id> <access-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--expected-version <n>` | Body field expected_version [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox get-creation`

Read a create's steps by the 'creation_id' its 'POST /v1/sandboxes' sent

**Usage:** `sandbox get-creation <creation-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox get-image-preparation`

Every host that can run sandboxes, with whether its default sandbox image is pulled (with download..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox prepare-images`

Pull the default sandbox image on every eligible host (or one), so the first sandbox there starts without..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node-id <n>` | The host to prepare: a node id, or '0' for the control plane | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox update-host-image-preparation`

Turn keeping one host's sandbox image prepared on or off

**Usage:** `sandbox update-host-image-preparation <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--keep-prepared` | Keep this host's default sandbox image pulled, re-pulling it when an upgrade changes the default. 'false'... [required] | - | No |
| `--no-keep-prepared` | Set keep_prepared to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox get-resource-defaults`

Effective resource defaults and input ceilings for the requested placement

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <value>` | Query parameter node | - | No |
| `--backend <value>` | Which isolation backend a sandbox runs on (ADR-029, ADR-050) (one of: docker, firecracker, microsandbox, local) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox rootfs-report`

Inspect rootfs storage: the Firecracker digest-keyed cache (with which sandboxes reference each entry) and..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox rootfs-gc`

Reclaim rootfs cache entries not backing any live sandbox

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox get-cmd`

Get cmd

**Usage:** `sandbox get-cmd <id> <cmd-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox cmd-logs`

Stream a command's stdout/stderr as 'application/x-ndjson' ('@vercel/sandbox'-compatible)

**Usage:** `sandbox cmd-logs <id> <cmd-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `sandbox cmd`

Run a command the '@vercel/sandbox' way: without --wait it returns the started command's id at once (read its output with cmd-logs); with --wait it prints the running and finished envelopes as JSON lines. For an interactive run use `temps sandbox exec`

**Usage:** `sandbox cmd <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--command <value>` | Program to run [required] | - | No |
| `--args <value>` | Argument (repeatable, in order) (repeatable) | - | No |
| `--cwd <value>` | Working directory inside the sandbox | - | No |
| `--env <json>` | Environment variables as a JSON object, e.g. {"NODE_ENV":"test"} (JSON) | - | No |
| `--sudo` | Run the command privileged | - | No |
| `--no-sudo` | Set sudo to false | - | No |
| `--wait` | Wait for the command to finish | - | No |
| `--no-wait` | Set wait to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |

### `sandbox get-creation-by`

Read the steps of the create that made a sandbox, while this server still tracks it (it is running, or..

**Usage:** `sandbox get-creation-by <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox list-dir`

List dir

**Usage:** `sandbox list-dir <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Absolute path of a directory inside the sandbox [required] | - | No |
| `--page <n>` | 1-based page (default 1) | - | No |
| `--page-size <n>` | Entries per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox write-files`

Batch-write multiple files in a single request

**Usage:** `sandbox write-files <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--files <json>` | List of files to write (JSON) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox list-jobs`

List jobs

**Usage:** `sandbox list-jobs <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox job-status`

Job status

**Usage:** `sandbox job-status <id> <job-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox kill-job`

Terminate a detached job

**Usage:** `sandbox kill-job <id> <job-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--force` | When true, sends SIGKILL immediately | - | No |
| `--no-force` | Set force to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox create-preview-link`

Mint a shareable link to a sandbox preview

**Usage:** `sandbox create-preview-link <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--path <value>` | Path the recipient lands on | - | No |
| `--port <n>` | Port inside the sandbox the preview serves on [required] | - | No |
| `--ttl-seconds <n>` | How long the link stays usable, in seconds | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox resize`

Grow a Firecracker sandbox's root disk

**Usage:** `sandbox resize <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--disk-size-mb <n>` | New root disk size in MB [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `sandbox issue-runtime-environment`

Issue the same scoped service variables used by a deployment runtime for the project attached to this sandbox

**Usage:** `sandbox issue-runtime-environment <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `sandbox cmd-kill`

Kill a running command ('@vercel/sandbox'-compatible)

**Usage:** `sandbox cmd-kill <id> <cmd-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--force` | Optional: when true, SIGKILL instead of SIGTERM | - | No |
| `--no-force` | Set force to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
