<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `cloud` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `cloud`

Temps Cloud

**Subcommands:**

- `login` - Login to Temps Cloud
- `logout` - Logout from Temps Cloud
- `whoami` - Show current Temps Cloud account
- `vps` - Manage cloud VPS instances
- `billing` - Manage Temps Cloud billing and subscription

### `cloud login`

Login to Temps Cloud

### `cloud logout`

Logout from Temps Cloud

### `cloud whoami`

Show current Temps Cloud account

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
