<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `load-balancer` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `load-balancer` (alias: `lb`)

Manage load balancer routes

**Subcommands:**

- `list` (`ls`) - List load balancer routes
- `create` (`add`) - Create a load balancer route
- `show` - Show route details
- `update` - Update a load balancer route
- `remove` (`rm`) - Delete a load balancer route

### `load-balancer list` (alias: `ls`)

List load balancer routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `load-balancer create` (alias: `add`)

Create a load balancer route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain for the route | - | No |
| `-t, --target <target>` | Target upstream URL | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `load-balancer show`

Show route details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain of the route | - | Yes |
| `--json` | Output in JSON format | - | No |

### `load-balancer update`

Update a load balancer route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain of the route | - | Yes |
| `-t, --target <target>` | New target upstream URL | - | No |

### `load-balancer remove` (alias: `rm`)

Delete a load balancer route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain of the route | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |
