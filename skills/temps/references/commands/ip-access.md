<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `ip-access` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `ip-access` (alias: `ipa`)

Manage IP access control rules

**Subcommands:**

- `list` (`ls`) - List all IP access control rules
- `create` (`add`) - Create a new IP access control rule
- `show` - Show IP access control rule details
- `update` - Update an IP access control rule
- `remove` (`rm`) - Delete an IP access control rule
- `check` - Check if an IP address is blocked

### `ip-access list` (alias: `ls`)

List all IP access control rules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `ip-access create` (alias: `add`)

Create a new IP access control rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ip <ip_or_cidr>` | IP address or CIDR range (e.g., "192.168.1.1" or "10.0.0.0/24") | - | No |
| `--action <action>` | Action to take: "allow" or "deny" | - | No |
| `--description <desc>` | Optional description/reason for the rule | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `ip-access show`

Show IP access control rule details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Rule ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `ip-access update`

Update an IP access control rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Rule ID | - | Yes |
| `--ip <ip>` | New IP address or CIDR range | - | No |
| `--action <action>` | New action: "allow" or "deny" | - | No |
| `--description <desc>` | New description/reason | - | No |

### `ip-access remove` (alias: `rm`)

Delete an IP access control rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Rule ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `ip-access check`

Check if an IP address is blocked

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ip <ip>` | IP address to check | - | No |
| `--json` | Output in JSON format | - | No |
