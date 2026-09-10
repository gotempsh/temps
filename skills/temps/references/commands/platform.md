<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `platform` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `platform` (alias: `plat`)

View platform and server information

**Subcommands:**

- `info` - Get platform information
- `access` - Get access and networking information
- `private-ip` - Get the server private IP address
- `public-ip` - Get the server public IP address
- `update` - Check for and apply temps releases on the server
- `alert-rules` - Inspect and retune the control-plane's own monitoring alert rules

### `platform info`

Get platform information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform access`

Get access and networking information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `platform private-ip`

Get the server private IP address

### `platform public-ip`

Get the server public IP address

### `platform update`

Check for and apply temps releases on the server

**Subcommands:**

- `status` - Show the available release and whether it can be applied from here
- `check` - Ask the release API for the newest version on this channel now
- `channel` - Show or set the release channel: stable, beta, nightly, or "auto" to follow the installed version
- `apply` - Install a release on the server and restart it

#### `platform update status`

Show the available release and whether it can be applied from here

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `platform update check`

Ask the release API for the newest version on this channel now

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `platform update channel`

Show or set the release channel: stable, beta, nightly, or "auto" to follow the installed version

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `platform update apply`

Install a release on the server and restart it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--version <version>` | Release tag to install (default: newest on this channel) | - | No |
| `-y, --yes` | Skip the confirmation prompt | - | No |
| `--json` | Output in JSON format | - | No |

### `platform alert-rules`

Inspect and retune the control-plane's own monitoring alert rules

**Subcommands:**

- `list` - List the alert rules watching this node (proxy health, socket exhaustion)
- `set` - Retune, enable, or disable an alert rule on this node

#### `platform alert-rules list`

List the alert rules watching this node (proxy health, socket exhaustion)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (default: 0, the control plane) | - | No |
| `--json` | Output in JSON format | - | No |

#### `platform alert-rules set`

Retune, enable, or disable an alert rule on this node

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node ID (default: 0, the control plane) | - | No |
| `--threshold <n>` | Value the metric must cross to fire | - | No |
| `--comparator <op>` | Comparison operator: >, >=, <, <= | - | No |
| `--severity <level>` | Alert severity: warning or critical | - | No |
| `--for-duration <secs>` | Seconds the condition must hold before firing | - | No |
| `--enable` | Enable the rule | - | No |
| `--disable` | Disable the rule (survives the startup re-seed; deleting does not) | - | No |
| `--json` | Output in JSON format | - | No |
