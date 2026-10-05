<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `nodes` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `nodes`

Worker nodes and workload placement

**Subcommands:**

- `capability` - Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued
- `mesh` - WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected
- `pair` - Pair nodes this control plane dials: for a control plane nodes cannot reach (a laptop, a server behind NAT). Lists recent pairings and their progress
- `ssh` - Add servers over SSH: the control plane logs in, installs temps if needed, pairs the server and starts its agent. Lists recent ones and their progress

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

- `enable` - Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it cannot be turned off again
- `hub` - Mesh hub: a member that relays between members that cannot reach each other (two nodes behind NAT). Shows the hub and every pair it carries
- `doctor` - Check the mesh from the control plane: its end, every node link and every pairing in progress, each failure with what fixes it. Exits 1 when a check fails. For a node's own end, run `temps doctor mesh` on it

#### `nodes mesh enable`

Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it cannot be turned off again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cidr <cidr>` | Mesh address pool (private IPv4, clear of the compute pool) | - | No |
| `--port <port>` | UDP port every node must accept from the others | - | No |
| `--node-api-port <port>` | TCP port nodes reach this control plane on over the mesh (default: the mesh port) | - | No |
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
| `--agent` | Log in with the SSH agent of the control plane's temps serve process | - | No |
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

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
