<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `nodes` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `nodes`

Worker nodes and workload placement

**Subcommands:**

- `capability` - Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued
- `mesh` - WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected
- `pair` - Pair nodes this control plane dials: for a control plane nodes cannot reach (a laptop, a server behind NAT). Lists recent pairings and their progress

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
