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
- `sessions` - Sessions commands

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
| `--confirm-broad-range` | Save a block range wider than /8 (IPv4) or /16 (IPv6) | - | No |
| `--acknowledge-lockout` | Save a block rule even if it matches the address you are calling from | - | No |
| `-y, --yes` | Skip prompts (for automation); does not confirm a broad range or a lockout | - | No |

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
| `--confirm-broad-range` | Save a block range wider than /8 (IPv4) or /16 (IPv6) | - | No |
| `--acknowledge-lockout` | Save a block rule even if it matches the address you are calling from | - | No |

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

### `ip-access sessions`

Sessions commands

**Subcommands:**

- `list-providers` - The saved Cloudflare Access tokens
- `create-provider` - Check a Cloudflare Access token, then save it sealed
- `check-provider` - Check which Cloudflare Access permissions a pasted token has: Access: Apps and Policies Edit (probed with..
- `delete-provider` - Delete a saved Cloudflare Access token
- `sign-out-of-apps` - Sign out of every signed-in address
- `sign-everyone-out-of-address` - Sign everyone out of a signed-in address

#### `ip-access sessions list-providers`

The saved Cloudflare Access tokens

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions create-provider`

Check a Cloudflare Access token, then save it sealed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--account-id <value>` | Body field account_id [required] | - | No |
| `--name <value>` | A name to tell tokens apart [required] | - | No |
| `--token-stdin` | Checked, then sealed; never returned [required] (read from stdin) | - | No |
| `--token-file <path>` | Checked, then sealed; never returned [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions check-provider`

Check which Cloudflare Access permissions a pasted token has: Access: Apps and Policies Edit (probed with..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--account-id <value>` | The Cloudflare account the Access applications live in (32 hex characters, from the dashboard URL or the... [required] | - | No |
| `--token-stdin` | The token, as pasted [required] (read from stdin) | - | No |
| `--token-file <path>` | The token, as pasted [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions delete-provider`

Delete a saved Cloudflare Access token

**Usage:** `ip-access sessions delete-provider <provider-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions sign-out-of-apps`

Sign out of every signed-in address

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `ip-access sessions sign-everyone-out-of-address`

Sign everyone out of a signed-in address

**Usage:** `ip-access sessions sign-everyone-out-of-address <address-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |
