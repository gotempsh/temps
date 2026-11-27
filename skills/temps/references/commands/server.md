<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `server` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `server`

Resource usage of the machine running the Temps control plane (what /monitoring/server shows)

**Subcommands:**

- `status` - Latest CPU, memory, disk, block I/O and network I/O sample for the control-plane host
- `metrics` - Time series of one control-plane host metric, e.g. node.cpu_percent or node.network_rx_bytes_total
- `cluster-metrics` - Time series of one metric combined across nodes (counters summed, ratios averaged)
- `cluster-latest` - Latest value of every metric, combined across nodes
- `reporting` - Whether each worker reports metrics, and why not
- `disk-health` - Latest software RAID and drive SMART health for a node
- `alert-rules` - CPU, memory and disk alerts on a node
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
| `--start <time>` | Start of an explicit window (ISO 8601); with --end, instead of --range | - | No |
| `--end <time>` | End of an explicit window (ISO 8601) | - | No |
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server cluster-metrics`

Time series of one metric combined across nodes (counters summed, ratios averaged)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--metric <name>` | Metric name, e.g. node.cpu_percent | - | Yes |
| `--nodes <ids>` | Comma-separated node IDs, e.g. 0,1,2 | - | Yes |
| `--range <range>` | Time window, e.g. 1h, 24h, 7d | - | No |
| `--start <time>` | Start of an explicit window (ISO 8601) | - | No |
| `--end <time>` | End of an explicit window (ISO 8601) | - | No |
| `--json` | Output in JSON format | - | No |

### `server cluster-latest`

Latest value of every metric, combined across nodes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--nodes <ids>` | Comma-separated node IDs | - | Yes |
| `--json` | Output in JSON format | - | No |

### `server reporting`

Whether each worker reports metrics, and why not

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `server disk-health`

Latest software RAID and drive SMART health for a node

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |

### `server alert-rules`

CPU, memory and disk alerts on a node

**Subcommands:**

- `get` - Show one of a node's resource alerts
- `create` - Alert when a node metric crosses a threshold
- `delete` (`rm`) - Delete a node's resource alert

#### `server alert-rules get`

Show one of a node's resource alerts

**Usage:** `server alert-rules get <id> <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `server alert-rules create`

Alert when a node metric crosses a threshold

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--name <name>` | Rule name | - | Yes |
| `--metric <name>` | Metric, e.g. node.cpu_percent or node.disk_used_percent | - | Yes |
| `--comparator <op>` | One of >, <, >=, <=, or gt, lt, gte, lte | - | Yes |
| `--threshold <value>` | Threshold value | - | Yes |
| `--severity <level>` | One of warning, critical | `warning` | No |
| `--for <seconds>` | How long the condition must hold before it fires | - | No |
| `--disabled` | Create it turned off | - | No |
| `--json` | Output in JSON format | - | No |

#### `server alert-rules delete` (alias: `rm`)

Delete a node's resource alert

**Usage:** `server alert-rules delete <rule-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `-y, --yes` | Do not ask for confirmation | - | No |

### `server docker-disk-usage` (alias: `df`)

Docker disk usage by images, containers, volumes and build cache (docker system df) on the control-plane host

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (0 = control plane) | `0` | No |
| `--json` | Output in JSON format | - | No |
