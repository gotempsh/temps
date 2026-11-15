<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `network-policy` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `network-policy` (alias: `netpol`)

Manage network policy for workloads: rules, egress/ingress profiles and the effective policy

**Subcommands:**

- `rules` - Manage network policy rules
- `effective` - Show the policy in force for a workload: profile, rules, inbound links and where it is enforced. <kind> is one of deployment, sandbox, service, function, job; <id> is the environment id for a deployment
- `profile` - Manage a workload's egress and ingress profile

### `network-policy rules`

Manage network policy rules

**Subcommands:**

- `list` (`ls`) - List network policy rules
- `show` - Show one network policy rule
- `create` (`add`) - Create a network policy rule
- `rm` (`remove`) - Delete a network policy rule (link rules are removed by unlinking instead)

#### `network-policy rules list` (alias: `ls`)

List network policy rules

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--subject-kind <kind>` | Only rules for this kind (deployment, sandbox, service, function, job) | - | No |
| `--subject-id <id>` | Only rules for this subject (environment id for deployments) | - | No |
| `--origin <origin>` | Only rules of this origin (link, operator, system) | - | No |
| `--page <n>` | Page number (default 1) | - | No |
| `--page-size <n>` | Rules per page (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `network-policy rules show`

Show one network policy rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `network-policy rules create` (alias: `add`)

Create a network policy rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--subject-kind <kind>` | Workload kind (deployment, sandbox, service, function, job) | - | Yes |
| `--subject-id <id>` | Environment id for a deployment; the row id for other kinds | - | Yes |
| `--direction <direction>` | Traffic direction (egress, ingress) | - | Yes |
| `--peer-kind <kind>` | Peer type: service (by id), workload (<kind>/<id>) or cidr. host rules are refused until a node runs the shared egress proxy | - | Yes |
| `--peer <ref>` | Peer reference: a service id, <kind>/<id>, or a CIDR | - | Yes |
| `--action <action>` | What to do with matching traffic (allow, deny) | - | Yes |
| `--protocol <protocol>` | tcp, udp, any (default tcp) | - | No |
| `--port <port>` | Port, or the start of a range. Omit for every port | - | No |
| `--port-end <port>` | End of the port range (needs --port) | - | No |
| `--description <text>` | Why this rule exists | - | No |
| `--json` | Output the created rule in JSON format | - | No |

#### `network-policy rules rm` (alias: `remove`)

Delete a network policy rule (link rules are removed by unlinking instead)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `network-policy effective`

Show the policy in force for a workload: profile, rules, inbound links and where it is enforced. <kind> is one of deployment, sandbox, service, function, job; <id> is the environment id for a deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `network-policy profile`

Manage a workload's egress and ingress profile

**Subcommands:**

- `set` - Set a workload's profile. Each direction is open, restricted, none; a direction left out is reset to the kind's default

#### `network-policy profile set`

Set a workload's profile. Each direction is open, restricted, none; a direction left out is reset to the kind's default

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--egress <profile>` | Outbound profile (open, restricted, none). Omit for the kind's default | - | No |
| `--ingress <profile>` | Inbound profile (open, restricted, none). Omit for the kind's default | - | No |
| `--json` | Output in JSON format | - | No |
