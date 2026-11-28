<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `environments` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`environments list`](#environments-list-alias-ls)
- [`environments create`](#environments-create)
- [`environments delete`](#environments-delete-alias-rm)
- [`environments vars`](#environments-vars)
- [`environments resources`](#environments-resources)
- [`environments timeouts`](#environments-timeouts)
- [`environments force-https`](#environments-force-https)
- [`environments scale`](#environments-scale)
- [`environments placement`](#environments-placement)
- [`environments isolation`](#environments-isolation)
- [`environments volumes`](#environments-volumes)
- [`environments ingress`](#environments-ingress)
- [`environments recording`](#environments-recording)
- [`environments outbound`](#environments-outbound)
- [`environments crons`](#environments-crons)
- [`environments reachability`](#environments-reachability)
- [`environments create-ingress-login-grant`](#environments-create-ingress-login-grant)
- [`environments sleep`](#environments-sleep)
- [`environments wake`](#environments-wake)
- [`environments health`](#environments-health)
- [`environments settings`](#environments-settings)

## `environments` (alias: `envs`, `env`)

Manage environments and environment variables

**Subcommands:**

- `list` (`ls`) - List environments for a project
- `create` - Create a new environment
- `delete` (`rm`) - Delete an environment
- `vars` - Manage environment variables
- `resources` - View or set CPU/memory resources for an environment
- `timeouts` - View or set upstream request/idle timeouts for an environment
- `force-https` - View or set the HTTP to HTTPS redirect override for an environment
- `scale` - View or set the number of replicas for an environment
- `placement` - Where the scheduler would run an environment's replicas, and why
- `isolation` - How an environment's replicas are isolated: a container (process) or a Firecracker microVM (micro_vm). Honoured only for environments whose deployments run on the workload plane
- `volumes` - Persistent volumes mounted into an environment's replicas; a replica that mounts one is pinned to its node
- `ingress` - Whether visitors must sign in to Temps to open an environment
- `recording` - Show or set whether the environment's deployments record their outbound connections (destination, outcome, bytes; never headers or bodies). Takes effect on the next deployment
- `outbound` - List the outbound connections a recording environment's deployments made, newest first
- `crons` - Manage cron jobs
- `reachability` - Reachability commands
- `create-ingress-login-grant` - Create ingress login grant
- `sleep` - Sleep an on-demand environment
- `wake` - Wake a sleeping on-demand environment
- `health` - Health commands
- `settings` - Change any setting of one environment (by its numeric ID); flags left out keep their value. `resources`, `timeouts` and `force-https` cover the common ones by name

### `environments list` (alias: `ls`)

List environments for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `environments create`

Create a new environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Environment name | - | No |
| `-b, --branch <branch>` | Git branch | - | No |
| `--preview` | Set as preview environment | - | No |

### `environments delete` (alias: `rm`)

Delete an environment

**Usage:** `environments delete <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-f, --force` | Skip confirmation | - | No |

### `environments vars`

Manage environment variables

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |

**Subcommands:**

- `list` (`ls`) - List environment variables
- `get` - Get a specific environment variable
- `set` - Set an environment variable. For a secret, read the value from a file or stdin (--value-file, --value-stdin) so it stays out of shell history and `ps`; the CLI never prints it
- `delete` (`rm`, `unset`) - Delete an environment variable
- `import` - Import environment variables from a .env file
- `export` - Export environment variables to .env format

#### `environments vars list` (alias: `ls`)

List environment variables

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Filter by environment name | - | No |
| `--show-values` | Show actual values (hidden by default) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments vars get`

Get a specific environment variable

**Usage:** `environments vars get <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Specify environment (if variable exists in multiple) | - | No |

#### `environments vars set`

Set an environment variable. For a secret, read the value from a file or stdin (--value-file, --value-stdin) so it stays out of shell history and `ps`; the CLI never prints it

**Usage:** `environments vars set <key> [value] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names (interactive if not provided) | - | No |
| `--preview` | Also include in current and future preview environments | - | No |
| `--no-preview` | Exclude from preview environments | - | No |
| `--update` | Update existing variable instead of creating new | - | No |
| `--secret` | Store as a secret: the value is masked in the UI and never returned by the API. One-way — to make a secret readable again you must delete the variable and create it anew | - | No |
| `--generate-secret [bytes\|laravel]` | Have the server generate a random secret of this many bytes (16 to 64, 32 by default, hex), stored as a secret and never shown; for a new variable, instead of a value. `laravel`, the default for APP_KEY, makes a Laravel application key (base64:, 32 bytes) | - | No |
| `--value-file <path>` | Read the value from a file, byte-for-byte (- reads stdin). Keeps it out of shell history | - | No |
| `--value-stdin` | Read the value from stdin, e.g. piped from a secret manager | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

#### `environments vars delete` (alias: `rm`, `unset`)

Delete an environment variable

**Usage:** `environments vars delete <key> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Delete only from specific environment | - | No |
| `-f, --force` | Skip confirmation | - | No |

#### `environments vars import`

Import environment variables from a .env file

**Usage:** `environments vars import [file] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environments <names>` | Comma-separated environment names | - | No |
| `--overwrite` | Overwrite existing variables | - | No |

#### `environments vars export`

Export environment variables to .env format

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-e, --environment <name>` | Export from specific environment | - | No |
| `-o, --output <file>` | Write to file instead of stdout | - | No |

### `environments resources`

View or set CPU/memory resources for an environment

**Usage:** `environments resources <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--cpu <millicores>` | CPU limit in millicores (1000 = 1 core, e.g., 500 = 0.5 CPU) | - | No |
| `--memory <mb>` | Memory limit in MB (e.g., 512) | - | No |
| `--cpu-request <millicores>` | CPU request in millicores (recorded; not currently enforced) | - | No |
| `--memory-request <mb>` | Memory request in MB (recorded; not currently enforced) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments timeouts`

View or set upstream request/idle timeouts for an environment

**Usage:** `environments timeouts <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--request <seconds>` | Timeout for regular (non-streaming) HTTP requests, in seconds | - | No |
| `--sse-idle <seconds>` | Idle timeout for Server-Sent Events streams, in seconds | - | No |
| `--websocket-idle <seconds>` | Idle timeout for WebSocket connections, in seconds | - | No |
| `--inherit` | Clear all three overrides (inherit the project/global defaults) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments force-https`

View or set the HTTP to HTTPS redirect override for an environment

**Usage:** `environments force-https <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--enable` | Always redirect plain HTTP to HTTPS, even without a local certificate | - | No |
| `--disable` | Never redirect: keep serving this environment over plain HTTP | - | No |
| `--inherit` | Clear the override and follow the proxy default | - | No |
| `--json` | Output in JSON format | - | No |

### `environments scale`

View or set the number of replicas for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name or slug | `production` | No |
| `-r, --replicas <count>` | Number of replicas to set | - | No |
| `--load-balancing <json>` | Replica routing policy as JSON; null restores project inheritance | - | No |
| `--json` | Output in JSON format | - | No |

### `environments placement`

Where the scheduler would run an environment's replicas, and why

**Subcommands:**

- `explain` - Show the nodes each replica would run on now, the eligible nodes ranked, and the ones excluded with the reason
- `recommend` - Recommend target nodes from where the environment actually ran (recommended, never applied)

#### `environments placement explain`

Show the nodes each replica would run on now, the eligible nodes ranked, and the ones excluded with the reason

**Usage:** `environments placement explain <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments placement recommend`

Recommend target nodes from where the environment actually ran (recommended, never applied)

**Usage:** `environments placement recommend <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--window-days <days>` | How many days of history to consider (1-90, default 7) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments isolation`

How an environment's replicas are isolated: a container (process) or a Firecracker microVM (micro_vm). Honoured only for environments whose deployments run on the workload plane

**Subcommands:**

- `show` - Show the isolation and whether it takes effect
- `set` - Set the isolation: process or micro_vm (a microVM replica only runs on a node that provides one)

#### `environments isolation show`

Show the isolation and whether it takes effect

**Usage:** `environments isolation show <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments isolation set`

Set the isolation: process or micro_vm (a microVM replica only runs on a node that provides one)

**Usage:** `environments isolation set <environment> <isolation> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

### `environments volumes`

Persistent volumes mounted into an environment's replicas; a replica that mounts one is pinned to its node

**Subcommands:**

- `list` (`ls`) - List the environment's volumes, newest first
- `create` - Create a volume (attach it to mount it)
- `status` - Show whether the next deploy mounts the attached volumes, and what to fix if not
- `attach` - Mount a volume (id or name) into every replica at --mount-path
- `detach` - Stop mounting a volume (id or name); its data is kept
- `rm` (`delete`) - Delete a volume (id or name), and its data unless it was created with --retain

#### `environments volumes list` (alias: `ls`)

List the environment's volumes, newest first

**Usage:** `environments volumes list <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--page <page>` | Page number (default 1) | - | No |
| `--page-size <size>` | Results per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes create`

Create a volume (attach it to mount it)

**Usage:** `environments volumes create <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-n, --name <name>` | Volume name: 2-64 characters of a-z, 0-9, "_", "." or "-" | - | Yes |
| `--size-mb <mb>` | Size in MB, when the backend supports a size | - | No |
| `--retain` | Keep the data when the volume is deleted | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes status`

Show whether the next deploy mounts the attached volumes, and what to fix if not

**Usage:** `environments volumes status <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes attach`

Mount a volume (id or name) into every replica at --mount-path

**Usage:** `environments volumes attach <environment> <volume> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--mount-path <path>` | Absolute path inside each replica | - | Yes |
| `--read-only` | Mount it read-only | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes detach`

Stop mounting a volume (id or name); its data is kept

**Usage:** `environments volumes detach <environment> <volume> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments volumes rm` (alias: `delete`)

Delete a volume (id or name), and its data unless it was created with --retain

**Usage:** `environments volumes rm <environment> <volume> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments ingress`

Whether visitors must sign in to Temps to open an environment

**Subcommands:**

- `show` - Show the login requirement and whether the proxy enforces it now
- `set` - Require visitors to sign in to Temps, or stop requiring it
- `delete-connector` - Delete a tunnel at the provider and unpublish every hostname it carries
- `get-connector` - Get one tunnel
- `retry-hostname-dns` - Retry DNS setup for a failed hostname publication on its current tunnel
- `list-providers` - List the tunnel providers this control plane offers

#### `environments ingress show`

Show the login requirement and whether the proxy enforces it now

**Usage:** `environments ingress show <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments ingress set`

Require visitors to sign in to Temps, or stop requiring it

**Usage:** `environments ingress set <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--require-login` | Require sign-in only when ingress mode is enforce. Check login_enforced in JSON output or Enforced now in text output | - | No |
| `--no-require-login` | Visitors do not need to sign in | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments ingress delete-connector`

Delete a tunnel at the provider and unpublish every hostname it carries

**Usage:** `environments ingress delete-connector <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments ingress get-connector`

Get one tunnel

**Usage:** `environments ingress get-connector <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments ingress retry-hostname-dns`

Retry DNS setup for a failed hostname publication on its current tunnel

**Usage:** `environments ingress retry-hostname-dns <id> <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments ingress list-providers`

List the tunnel providers this control plane offers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `environments recording`

Show or set whether the environment's deployments record their outbound connections (destination, outcome, bytes; never headers or bodies). Takes effect on the next deployment

**Usage:** `environments recording <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--on` | Record outbound connections | - | No |
| `--off` | Stop recording | - | No |
| `--json` | Output in JSON format | - | No |

### `environments outbound`

List the outbound connections a recording environment's deployments made, newest first

**Usage:** `environments outbound <environment> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug | - | No |
| `--page <n>` | Page number (default 1) | - | No |
| `--per-page <n>` | Records per page (default 20, max 100) | - | No |
| `--outcome <outcome>` | Only allowed, refused or failed connections | - | No |
| `--json` | Output in JSON format | - | No |

### `environments crons`

Manage cron jobs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-e, --environment <env>` | Environment name or slug | - | Yes |

**Subcommands:**

- `list` (`ls`) - List cron jobs for an environment
- `show` - Show cron job details
- `executions` (`execs`) - Show cron job execution history

#### `environments crons list` (alias: `ls`)

List cron jobs for an environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments crons show`

Show cron job details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Cron job ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `environments crons executions` (alias: `execs`)

Show cron job execution history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Cron job ID | - | Yes |
| `--page <page>` | Page number | `1` | No |
| `--per-page <count>` | Items per page | `20` | No |
| `--json` | Output in JSON format | - | No |

### `environments reachability`

Reachability commands

**Subcommands:**

- `get-certificate-methods` - Which ways to give a domain HTTPS work here
- `check-cloudflare-token` - Check which permissions a pasted Cloudflare API token has
- `get-connector-hosts` - What a tunnel publishes for environments, or would once turned on
- `set-connector-hosts` - Turn a tunnel's environment hosts on or off
- `get` - Whether visitors can reach each address of an environment
- `apply` - Carry out one option of a stored plan
- `get-choices` - Which audiences, and which ways in, can work for an environment
- `plan` - What *Make it reachable* would do for an address
- `list-runs` - The newest runs of an environment
- `get-run` - One run, with the progress of each step and what it created
- `decide-conflict` - Answer a conflict: a DNS record someone else changed while the run was setting it, or after it did (ADR..
- `resume-run` - Carry on a waiting or interrupted run from its first step not done
- `undo-run` - Remove exactly what a run created, newest first
- `get-preview-domain-status` - Whether the preview domain can be reached

#### `environments reachability get-certificate-methods`

Which ways to give a domain HTTPS work here

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain <value>` | The domain to judge, such as 'my-app.example.com' or '*.example.com' | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability check-cloudflare-token`

Check which permissions a pasted Cloudflare API token has

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--hostname <value>` | A hostname the token is meant to serve, when the zone is not known: the longest zone the token reaches... | - | No |
| `--token-stdin` | The token, as pasted [required] (read from stdin) | - | No |
| `--token-file <path>` | The token, as pasted [required] (read from a file) | - | No |
| `--zone <value>` | The zone the token is meant for | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-connector-hosts`

What a tunnel publishes for environments, or would once turned on

**Usage:** `environments reachability get-connector-hosts <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `environments reachability set-connector-hosts`

Turn a tunnel's environment hosts on or off

**Usage:** `environments reachability set-connector-hosts <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--confirm <value>` | The 'confirm_token' of the list that was shown and accepted: turning on publishes every host in it,... | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get`

Whether visitors can reach each address of an environment

**Usage:** `environments reachability get <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability apply`

Carry out one option of a stored plan

**Usage:** `environments reachability apply <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--answers <json>` | Answers to the option's fields by key. 'address', when given, must be the plan's; 'dns_provider_id' takes... (JSON) | - | No |
| `--confirmed` | The user saw and accepted the option's 'confirmation' | - | No |
| `--no-confirmed` | Set confirmed to false | - | No |
| `--option <value>` | Which option of the plan to carry out (one of: existing_tunnel, direct_managed_dns, new_tunnel, new_tunnel_with_token, direct_manual_dns, private_worker, tailscale_serve, tailscale_service, tailscale_service_with_credential, tunnel_environment_hosts) [required] | - | No |
| `--plan-id <value>` | The plan this acts on, from 'POST …/reachability/plan' [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-choices`

Which audiences, and which ways in, can work for an environment

**Usage:** `environments reachability get-choices <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability plan`

What *Make it reachable* would do for an address

**Usage:** `environments reachability plan <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--address <value>` | The hostname to make reachable; Temps suggests one without it | - | No |
| `--audience <value>` | Who an address is for (one of: public, private, signed_in, tailnet_users, unsupported) | - | No |
| `--gate <value>` | Which gate an address has (D7) (one of: temps, cloudflare_access, unsupported) | - | No |
| `--principals <json>` | For 'tailnet_users': the tailnet logins and tags it admits (ADR 065 D6) (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability list-runs`

The newest runs of an environment

**Usage:** `environments reachability list-runs <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-run`

One run, with the progress of each step and what it created

**Usage:** `environments reachability get-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability decide-conflict`

Answer a conflict: a DNS record someone else changed while the run was setting it, or after it did (ADR..

**Usage:** `environments reachability decide-conflict <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--decision <value>` | A user's answer to a conflict (ADR 064 D4), journaled with the entry (one of: keep, overwrite) [required] | - | No |
| `--key <value>` | The journal entry in conflict [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability resume-run`

Carry on a waiting or interrupted run from its first step not done

**Usage:** `environments reachability resume-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--answers <json>` | 'cloudflare_api_token', when the run had not saved the account yet (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability undo-run`

Remove exactly what a run created, newest first

**Usage:** `environments reachability undo-run <environment-id> <run-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments reachability get-preview-domain-status`

Whether the preview domain can be reached

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `environments create-ingress-login-grant`

Create ingress login grant

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--host <value>` | The hostname, without a port [required] | - | No |
| `--next <value>` | Path on the host to return to | - | No |
| `--owner-id <n>` | Body field owner_id [required] | - | No |
| `--owner-kind <value>` | 'environment' or 'sandbox' (a sandbox preview host) [required] | - | No |
| `--port <n>` | The port the visitor reached the host on, when the proxy's request carries one (it leaves out the scheme's... | - | No |
| `--scheme <value>` | 'http' or 'https': how the visitor reached the host [required] | - | No |
| `--sig <value>` | Body field sig [required] | - | No |
| `--ts <n>` | Unix seconds the proxy signed the request at [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments sleep`

Sleep an on-demand environment

**Usage:** `environments sleep <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments wake`

Wake a sleeping on-demand environment

**Usage:** `environments wake <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments health`

Health commands

**Subcommands:**

- `get-runtime` - Get runtime health
- `update-runtime` - Update runtime health
- `get-project-runtime-health` - Get project runtime health

#### `environments health get-runtime`

Get runtime health

**Usage:** `environments health get-runtime <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments health update-runtime`

Update runtime health

**Usage:** `environments health update-runtime <environment-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--paused` | Body field paused [required] | - | No |
| `--no-paused` | Set paused to false | - | No |
| `--required-healthy <n>` | Body field required_healthy | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `environments health get-project-runtime-health`

Get project runtime health

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `environments settings`

Change any setting of one environment (by its numeric ID); flags left out keep their value. `resources`, `timeouts` and `force-https` cover the common ones by name

**Usage:** `environments settings <env-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--branch <value>` | Branch this environment deploys | - | No |
| `--automatic-deploy` | Deploy automatically on push | - | No |
| `--no-automatic-deploy` | Set automatic_deploy to false | - | No |
| `--protected` | Never deploy this environment automatically from a push | - | No |
| `--no-protected` | Set protected to false | - | No |
| `--replicas <n>` | Container replicas | - | No |
| `--cpu-limit <n>` | CPU limit per container, in microcores | - | No |
| `--cpu-request <n>` | CPU reserved per container, in microcores | - | No |
| `--memory-limit <n>` | Memory limit per container, in MB | - | No |
| `--memory-request <n>` | Memory reserved per container, in MB | - | No |
| `--exposed-port <n>` | Port the container listens on (overrides the project's) | - | No |
| `--anti-affinity` | Spread replicas across different nodes | - | No |
| `--no-anti-affinity` | Set anti_affinity to false | - | No |
| `--target-nodes <n>` | Only deploy to this node ID (repeatable) (repeatable) | - | No |
| `--target-labels <json>` | Only deploy to nodes with these labels, as JSON, e.g. {"region":"eu"} (JSON) | - | No |
| `--build-location <value>` | Where a source deployment's image is built (one of: control_plane, node, ci) | - | No |
| `--cross-architecture-builds` | Build one image per architecture the eligible nodes run | - | No |
| `--no-cross-architecture-builds` | Set cross_architecture_builds to false | - | No |
| `--on-demand` | Start containers on the first request and stop them when idle | - | No |
| `--no-on-demand` | Set on_demand to false | - | No |
| `--idle-timeout-seconds <n>` | Idle seconds (60-86400) before an on-demand environment stops | - | No |
| `--wake-timeout-seconds <n>` | Seconds (5-120) to wait for an on-demand environment to wake | - | No |
| `--health <json>` | Health check settings as JSON, e.g. {"path":"/healthz"} (JSON) | - | No |
| `--health-check-timeout-seconds <n>` | Seconds (30-3600) a new deployment's containers get to become ready | - | No |
| `--stop-timeout-seconds <n>` | Seconds (1-3600) the previous deployment gets to finish in-flight requests | - | No |
| `--load-balancing <json>` | Load balancing settings as JSON (JSON) | - | No |
| `--max-concurrent-connections <n>` | Cap on concurrent requests to this environment's upstream (0 = unlimited) | - | No |
| `--request-timeout-seconds <n>` | HTTP request timeout, in seconds | - | No |
| `--sse-idle-timeout-seconds <n>` | Idle timeout of SSE streams, in seconds | - | No |
| `--websocket-idle-timeout-seconds <n>` | Idle timeout of WebSocket connections, in seconds | - | No |
| `--force-https` | Redirect HTTP to HTTPS | - | No |
| `--no-force-https` | Set force_https to false | - | No |
| `--password-stdin` | Password visitors must enter to open the environment (read from stdin) | - | No |
| `--password-file <path>` | Password visitors must enter to open the environment (read from a file) | - | No |
| `--security <json>` | Security settings (headers, rate limits) as JSON (JSON) | - | No |
| `--attack-mode` | Ask visitors to solve a CAPTCHA | - | No |
| `--no-attack-mode` | Set attack_mode to false | - | No |
| `--shield-mode <value>` | How Shield acts on its verdicts (one of: off, log, selective, attack) | - | No |
| `--shield-exempt-paths <value>` | Path Shield never challenges, e.g. /api/* (repeatable; replaces the list) (repeatable) | - | No |
| `--performance-metrics-enabled` | Collect web performance metrics | - | No |
| `--no-performance-metrics-enabled` | Set performance_metrics_enabled to false | - | No |
| `--session-recording-enabled` | Record visitor sessions | - | No |
| `--no-session-recording-enabled` | Set session_recording_enabled to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |
