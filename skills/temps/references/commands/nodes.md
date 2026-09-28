<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `nodes` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `nodes`

Worker nodes and workload placement

**Subcommands:**

- `capability` - Show whether this install can run workloads at all — local workloads plus joined worker nodes — so a deploy that could never be scheduled is visible before it is queued
- `mesh` - WireGuard mesh: lets nodes that only share the internet with the control plane join it, encrypted. Shows whether it is on and how each node is connected

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

#### `nodes mesh enable`

Turn the WireGuard mesh on. The control plane and every agent move onto it within a minute; it cannot be turned off again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--cidr <cidr>` | Mesh address pool (private IPv4, clear of the compute pool) | - | No |
| `--port <port>` | UDP port every node must accept from the others | - | No |
| `-y, --yes` | Skip the confirmation prompt (for automation) | - | No |
| `--json` | Output in JSON format | - | No |
