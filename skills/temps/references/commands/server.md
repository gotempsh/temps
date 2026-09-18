<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `server` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `server`

Resource usage of the machine running the Temps control plane (what /monitoring/server shows)

**Subcommands:**

- `status` - Latest CPU, memory, disk, block I/O and network I/O sample for the control-plane host
- `metrics` - Time series of one control-plane host metric, e.g. node.cpu_percent or node.network_rx_bytes_total
- `docker-disk-usage` (`df`) - Docker disk usage by images, containers, volumes and build cache (docker system df) on the control-plane host

### `server status`

Latest CPU, memory, disk, block I/O and network I/O sample for the control-plane host

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server metrics`

Time series of one control-plane host metric, e.g. node.cpu_percent or node.network_rx_bytes_total

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--metric <name>` | Metric name (node.cpu_percent, node.memory_used_bytes, node.disk_used_bytes, node.disk_read_bytes_total, node.network_tx_bytes_total, ...) | - | Yes |
| `--range <range>` | Time window: 1h, 6h, 24h, 7d | `1h` | No |
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server docker-disk-usage` (alias: `df`)

Docker disk usage by images, containers, volumes and build cache (docker system df) on the control-plane host

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |
