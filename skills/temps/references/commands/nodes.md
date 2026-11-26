<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `nodes` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`nodes capability`](#nodes-capability)
- [`nodes mesh`](#nodes-mesh)
- [`nodes pair`](#nodes-pair)
- [`nodes ssh`](#nodes-ssh)
- [`nodes resources`](#nodes-resources)
- [`nodes state-status`](#nodes-state-status)
- [`nodes show`](#nodes-show)
- [`nodes containers`](#nodes-containers)
- [`nodes drain`](#nodes-drain)
- [`nodes undrain`](#nodes-undrain)
- [`nodes remove`](#nodes-remove-alias-rm)
- [`nodes build-limits`](#nodes-build-limits)
- [`nodes public-ingress`](#nodes-public-ingress)
- [`nodes workload-mode`](#nodes-workload-mode)

## `nodes`

Worker nodes and workload placement

**Subcommands:**

- `capability` - Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued
- `mesh` - WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected
- `pair` - Pair nodes this control plane dials: for a control plane nodes cannot reach (a laptop, a server behind NAT). Lists recent pairings and their progress
- `ssh` - Add servers over SSH: the control plane logs in, installs temps if needed, pairs the server and starts its agent. Lists recent ones and their progress
- `resources` - What the scheduler keeps for the OS and Temps on a node, and whether a placement that does not fit the remaining capacity is refused
- `state-status` - Show the workload plane's desired-state stream: whether it runs here, its lag and counters
- `show` - Show one node: status, addresses, labels and capacity
- `containers` - List the containers running on a node
- `drain` - Stop scheduling on a node and move its replicas elsewhere (redeploys the affected environments)
- `undrain` - Let a drained node accept new deployments again
- `remove` (`rm`) - Remove a drained node from the cluster and from every placement that targets it
- `build-limits` - Show a worker node's build limits, or change them with flags
- `public-ingress` - Show or change whether a node serves public HTTP(S) traffic itself, and on which ports and addresses
- `workload-mode` - Show an environment's workload mode, or set it (legacy, shadow, enforce); operator-only

### `nodes capability`

Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes mesh`

WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `enable` - Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it can be turned off again only while no node is on it
- `disable` - Turn the WireGuard mesh off. Refused, with the reason, while a node is on it, a pairing is in progress or the cluster network runs over it. The control plane stops tending its end; the interface itself goes away when `temps serve` restarts
- `hub` - Mesh hub: a member that relays between members that cannot reach each other (two nodes behind NAT). Shows the hub and every pair it carries
- `doctor` - Check the mesh from the control plane: its end, every node link and every pairing in progress, each failure with what fixes it. Exits 1 when a check fails. For a node's own end, run `temps doctor mesh` on it

#### `nodes mesh enable`

Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it can be turned off again only while no node is on it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cidr <cidr>` | Mesh address pool (private IPv4, clear of the compute pool) | - | No |
| `--port <port>` | UDP port every node must accept from the others | - | No |
| `--node-api-port <port>` | TCP port nodes reach this control plane on over the mesh (default: the mesh port) | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes mesh disable`

Turn the WireGuard mesh off. Refused, with the reason, while a node is on it, a pairing is in progress or the cluster network runs over it. The control plane stops tending its end; the interface itself goes away when `temps serve` restarts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes mesh hub`

Mesh hub: a member that relays between members that cannot reach each other (two nodes behind NAT). Shows the hub and every pair it carries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `set` - Make <member> the hub: control-plane, or a node name. Pairs that never connect move onto it within a few minutes. The hub can read the traffic it relays: pick your own machine
- `unset` - Remove the hub: relayed pairs go back to trying the direct path

##### `nodes mesh hub set`

Make <member> the hub: control-plane, or a node name. Pairs that never connect move onto it within a few minutes. The hub can read the traffic it relays: pick your own machine

**Usage:** `nodes mesh hub set <member> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

##### `nodes mesh hub unset`

Remove the hub: relayed pairs go back to trying the direct path

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes mesh doctor`

Check the mesh from the control plane: its end, every node link and every pairing in progress, each failure with what fixes it. Exits 1 when a check fails. For a node's own end, run `temps doctor mesh` on it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes pair`

Pair nodes this control plane dials: for a control plane nodes cannot reach (a laptop, a server behind NAT). Lists recent pairings and their progress

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `create` - Start pairing the node at --address. Prints the one command to run on it; the control plane then dials it on the mesh port until it answers (30 minutes)
- `cancel` - Cancel a pending pairing: its command stops working and its address is released

#### `nodes pair create`

Start pairing the node at --address. Prints the one command to run on it; the control plane then dials it on the mesh port until it answers (30 minutes)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--address <ip[:port]>` | The node's public IP (and mesh port, if not the default) | - | Yes |
| `--name <name>` | Name the node registers under (default: worker-<random>) | - | No |
| `--json` | Output in JSON format (includes the command, which holds a secret) | - | No |

#### `nodes pair cancel`

Cancel a pending pairing: its command stops working and its address is released

**Usage:** `nodes pair cancel <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes ssh`

Add servers over SSH: the control plane logs in, installs temps if needed, pairs the server and starts its agent. Lists recent ones and their progress

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page to show (default 1) | - | No |
| `--per-page <n>` | Servers per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `add` - Add the server at --host. Shows its SSH host key to confirm first (or pass --host-key). Credentials are used for this enrollment only and never stored
- `host-key` - Read the SSH host key of the server at --host, to verify it before `nodes ssh add --host-key`. Nothing is logged in to or changed
- `show` - Show one enrollment: its progress and the log with the server output

#### `nodes ssh add`

Add the server at --host. Shows its SSH host key to confirm first (or pass --host-key). Credentials are used for this enrollment only and never stored

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--host <host>` | Hostname or IP address of the server | - | Yes |
| `--port <port>` | SSH port | `22` | No |
| `--user <user>` | User to log in as: root, or a user with sudo | `root` | No |
| `--identity-file <path>` | Log in with this private key | - | No |
| `--ask-passphrase` | Prompt for the private key passphrase | - | No |
| `--passphrase-stdin` | Read the private key passphrase from stdin (not with --password-stdin) | - | No |
| `--password-stdin` | Read the password from stdin (default: prompt for it) | - | No |
| `--host-key <fingerprint>` | The SHA256:… host key fingerprint you verified (see `nodes ssh host-key`) | - | No |
| `--name <name>` | Name the node registers under (default: worker-<random>) | - | No |
| `--node-address <ip[:port]>` | The server's public address for WireGuard, if not the one SSH connects to | - | No |
| `--no-wait` | Return once started instead of following the progress | - | No |
| `--json` | Output in JSON format | - | No |

#### `nodes ssh host-key`

Read the SSH host key of the server at --host, to verify it before `nodes ssh add --host-key`. Nothing is logged in to or changed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--host <host>` | Hostname or IP address of the server | - | Yes |
| `--port <port>` | SSH port | `22` | No |
| `--json` | Output in JSON format | - | No |

#### `nodes ssh show`

Show one enrollment: its progress and the log with the server output

**Usage:** `nodes ssh show <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes resources`

What the scheduler keeps for the OS and Temps on a node, and whether a placement that does not fit the remaining capacity is refused

**Subcommands:**

- `show` - Show a node's reservation and resource policy (node id; 0 or "control-plane" for the control plane)
- `set` - Change a node's resource policy or reservation. Giving only one of --reserve-cpu and --reserve-memory keeps the node's current value for the other

#### `nodes resources show`

Show a node's reservation and resource policy (node id; 0 or "control-plane" for the control plane)

**Usage:** `nodes resources show <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `nodes resources set`

Change a node's resource policy or reservation. Giving only one of --reserve-cpu and --reserve-memory keeps the node's current value for the other

**Usage:** `nodes resources set <node> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--policy <policy>` | best_effort (reservations not checked) or enforced (placements that do not fit are refused) | - | No |
| `--reserve-cpu <millis>` | CPU kept for the OS and Temps, in millicores (1000 = one core) | - | No |
| `--reserve-memory <mb>` | Memory kept for the OS and Temps, in MB | - | No |
| `--reset-reserved` | Go back to the default reservation for the node's role | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes state-status`

Show the workload plane's desired-state stream: whether it runs here, its lag and counters

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes show`

Show one node: status, addresses, labels and capacity

**Usage:** `nodes show <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes containers`

List the containers running on a node

**Usage:** `nodes containers <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes drain`

Stop scheduling on a node and move its replicas elsewhere (redeploys the affected environments)

**Usage:** `nodes drain <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--status` | Only show drain progress | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes undrain`

Let a drained node accept new deployments again

**Usage:** `nodes undrain <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `nodes remove` (alias: `rm`)

Remove a drained node from the cluster and from every placement that targets it

**Usage:** `nodes remove <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--force` | Remove even if the node is not drained (its containers are not moved first) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes build-limits`

Show a worker node's build limits, or change them with flags

**Usage:** `nodes build-limits <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--timeout <seconds>` | Longest a build may run | - | No |
| `--concurrency <n>` | Builds that may run at once | - | No |
| `--queue-wait <seconds>` | Longest a build may wait for a slot | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes public-ingress`

Show or change whether a node serves public HTTP(S) traffic itself, and on which ports and addresses

**Usage:** `nodes public-ingress <node-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--enable` | Serve public traffic on this node | - | No |
| `--disable` | Stop serving public traffic on this node | - | No |
| `--http-port <port>` | HTTP port to listen on | - | No |
| `--https-port <port>` | HTTPS port to listen on | - | No |
| `--listen-address <addr>` | Local address to bind ("" for automatic) | - | No |
| `--public-address <addr>` | Address DNS points at ("" for the listen address) | - | No |
| `--toggle-only` | Only flip enabled with --enable/--disable, without saving ports or addresses | - | No |
| `--check` | Check from the control plane that the node answers on its public address | - | No |
| `-y, --yes` | Do not ask before disabling | - | No |
| `--json` | Output in JSON format | - | No |

### `nodes workload-mode`

Show an environment's workload mode, or set it (legacy, shadow, enforce); operator-only

**Usage:** `nodes workload-mode <environment-id> [mode] [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask before changing it | - | No |
| `--json` | Output in JSON format | - | No |
