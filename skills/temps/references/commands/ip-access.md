<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `ip-access` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `ip-access` (alias: `ipa`)

Manage instance-wide IP access control rules, and per-project lists (ip-access project)

**Subcommands:**

- `list` (`ls`) - List all IP access control rules
- `create` (`add`) - Create a new IP access control rule
- `show` - Show IP access control rule details
- `update` - Update an IP access control rule
- `remove` (`rm`) - Delete an IP access control rule
- `check` - Check if an IP address is blocked
- `project` - Allow or deny IP ranges for one project or environment

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
| `--ip <ip_or_cidr>` | IPv4 or IPv6 address or CIDR range (e.g., "192.168.1.1", "10.0.0.0/24", or "2001:db8::/32") | - | No |
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

### `ip-access project`

Allow or deny IP ranges for one project or environment

**Subcommands:**

- `show` - Show the IP access list
- `set` - Replace the IP access list. A list that would lock out this machine needs --acknowledge-lockout
- `clear` - Remove the list (an environment then follows the project list)
- `test` - Check whether an address would be allowed

#### `ip-access project show`

Show the IP access list

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access project set`

Replace the IP access list. A list that would lock out this machine needs --acknowledge-lockout

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `--mode <mode>` | disabled, allow (only listed) or deny (block listed) | - | Yes |
| `--cidr <cidr>` | Address or CIDR range; repeat or comma-separate | `` | No |
| `--tailscale` | Include the Tailscale ranges (100.64.0.0/10 and fd7a:115c:a1e0::/48) | - | No |
| `--acknowledge-lockout` | Apply even if it would block the address you are calling from | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access project clear`

Remove the list (an environment then follows the project list)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `-y, --yes` | Skip confirmation | - | No |

#### `ip-access project test`

Check whether an address would be allowed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID (omit for the project-wide list) | - | No |
| `--ip <ip>` | Address to evaluate | - | Yes |
| `--json` | Output in JSON format | - | No |
