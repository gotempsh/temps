<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `cloud` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `cloud`

Temps Cloud

**Subcommands:**

- `login` - Login to Temps Cloud
- `logout` - Logout from Temps Cloud
- `whoami` - Show current Temps Cloud account
- `status` - Show this self-hosted instance's Temps Cloud link
- `connect` - Connect this self-hosted instance using an enrollment code
- `disconnect` - Disconnect this self-hosted instance from Temps Cloud
- `vps` - Manage cloud VPS instances
- `billing` - Manage Temps Cloud billing and subscription
- `telemetry` - Where a project’s spans are written — this instance, or Temps Cloud (ADR-041)

### `cloud login`

Login to Temps Cloud

### `cloud logout`

Logout from Temps Cloud

### `cloud whoami`

Show current Temps Cloud account

### `cloud status`

Show this self-hosted instance's Temps Cloud link

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output JSON | - | No |

### `cloud connect`

Connect this self-hosted instance using an enrollment code

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--code <code>` | Single-use enrollment code from Temps Cloud | - | Yes |

### `cloud disconnect`

Disconnect this self-hosted instance from Temps Cloud

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `cloud vps`

Manage cloud VPS instances

**Subcommands:**

- `list` - List VPS instances
- `create` - Provision a new VPS instance
- `show` - Show VPS instance details and provisioning logs
- `destroy` - Destroy a VPS instance
- `retry` - Retry failed VPS provisioning
- `credentials` - Show VPS panel credentials
- `images` - List available OS images
- `locations` - List available datacenter locations
- `types` - List available server types with pricing

#### `cloud vps list`

List VPS instances

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps create`

Provision a new VPS instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--image <image>` | OS image ID | - | No |
| `--location <location>` | Datacenter location ID | - | No |
| `--type <type>` | Server type ID | - | No |
| `--json` | Output as JSON | - | No |

#### `cloud vps show`

Show VPS instance details and provisioning logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps destroy`

Destroy a VPS instance

#### `cloud vps retry`

Retry failed VPS provisioning

#### `cloud vps credentials`

Show VPS panel credentials

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps images`

List available OS images

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps locations`

List available datacenter locations

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud vps types`

List available server types with pricing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--location <location>` | Filter by datacenter location | - | No |
| `--json` | Output as JSON | - | No |

### `cloud billing`

Manage Temps Cloud billing and subscription

**Subcommands:**

- `overview` - Show billing overview
- `usage` - Show usage and limits
- `upgrade` - Upgrade your plan

#### `cloud billing overview`

Show billing overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud billing usage`

Show usage and limits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as JSON | - | No |

#### `cloud billing upgrade`

Upgrade your plan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--yearly` | Use yearly billing cycle (default: monthly) | - | No |
| `--no-browser` | Don't open browser, just show the URL | - | No |

### `cloud telemetry`

Where a project’s spans are written — this instance, or Temps Cloud (ADR-041)

**Subcommands:**

- `write-mode` - Read or change a project’s telemetry write mode
- `status` - Instance-wide Cloud telemetry write status: queue depth, gaps, and whether the local span store is still required
- `bulk-switch` - Switch many projects to Temps Cloud and ship their history in one job — estimates first, then asks
- `bulk-status` - Show the Temps Cloud activation running on this instance — progress, ETA, skips and failures
- `bulk-cancel` - Stop a Temps Cloud activation at its next chunk boundary

#### `cloud telemetry write-mode`

Read or change a project’s telemetry write mode

**Subcommands:**

- `get` - Show where a project’s spans are written, what is queued, and any gaps
- `set` - Set the write mode to "local" (stored on this instance) or "cloud" (written to Temps Cloud, not stored here)

##### `cloud telemetry write-mode get`

Show where a project’s spans are written, what is queued, and any gaps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug | - | No |
| `--json` | Output in JSON format | - | No |

##### `cloud telemetry write-mode set`

Set the write mode to "local" (stored on this instance) or "cloud" (written to Temps Cloud, not stored here)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug | - | No |
| `--fidelity <tier>` | Also set Cloud telemetry fidelity: metered or queryable. "cloud" requires "queryable". | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry status`

Instance-wide Cloud telemetry write status: queue depth, gaps, and whether the local span store is still required

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry bulk-switch`

Switch many projects to Temps Cloud and ship their history in one job — estimates first, then asks

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--all` | Every project still storing its spans on this instance. Projects already on Temps Cloud are not included. | - | No |
| `-p, --project <id>` | A project id to switch. Repeatable. Cannot be combined with --all. | `` | No |
| `--from <timestamp>` | Start of the history window to ship (RFC 3339). Defaults to the oldest span local retention can still be holding. | - | No |
| `--to <timestamp>` | End of the history window to ship (RFC 3339). Defaults to now. | - | No |
| `-y, --yes` | Skip the confirmation. The estimate is still computed and printed. | - | No |
| `--watch` | Follow the job until it finishes | - | No |
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry bulk-status`

Show the Temps Cloud activation running on this instance — progress, ETA, skips and failures

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--watch` | Follow the job until it finishes | - | No |
| `--json` | Output in JSON format | - | No |

#### `cloud telemetry bulk-cancel`

Stop a Temps Cloud activation at its next chunk boundary

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation | - | No |
| `--json` | Output in JSON format | - | No |
