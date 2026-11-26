<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `cluster` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `cluster`

Cluster-wide multi-node operations

**Subcommands:**

- `dns` - Cluster DNS resolver (ADR-024) operations
- `network` - The compute network workloads get their addresses from
- `join-token` - The shared token worker nodes join with
- `enrollment-tokens` - Short-lived, single-use tokens a new node enrolls with
- `rotate-ca` - Replace a compromised cluster CA: every node re-enrolls and outstanding enrollment tokens are revoked

### `cluster dns`

Cluster DNS resolver (ADR-024) operations

**Subcommands:**

- `status` - Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs
- `resolvers` - Each node's resolver: health, version, records served and last sync
- `records` - The internal zone's records and who owns them, newest first
- `queries` - Recent queries the internal resolvers answered, newest first
- `traffic` - Queries per outcome over time for one node's resolver
- `resolve` - Resolve a name from one node's resolver, optionally as a project or sandbox would see it

#### `cluster dns status`

Show whether cluster DNS is healthy across every node — resolver status, last sync, and errors — without SSHing into a node to read logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster dns resolvers`

Each node's resolver: health, version, records served and last sync

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster dns records`

The internal zone's records and who owns them, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--search <text>` | Only records whose name contains this | - | No |
| `--node <id>` | Only records served by this node | - | No |
| `--owner-kind <kind>` | Only records owned by this kind (e.g. environment, service, sandbox) | - | No |
| `--page <n>` | Page (default 1) | - | No |
| `--page-size <n>` | Records per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster dns queries`

Recent queries the internal resolvers answered, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Only this node | - | No |
| `--outcome <outcome>` | Only this outcome (e.g. answered, nxdomain, refused) | - | No |
| `--zone <zone>` | Only this zone | - | No |
| `--name <name>` | Only this queried name | - | No |
| `--project <id>` | Only queries from this project ID | - | No |
| `--page <n>` | Page (default 1) | - | No |
| `--page-size <n>` | Queries per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster dns traffic`

Queries per outcome over time for one node's resolver

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node (default: the control plane) | - | No |
| `--range <range>` | Time range, e.g. 1h, 24h, 7d | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster dns resolve`

Resolve a name from one node's resolver, optionally as a project or sandbox would see it

**Usage:** `cluster dns resolve <name> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <id>` | Node whose resolver answers | - | Yes |
| `--type <type>` | Record type (A, AAAA, CNAME, SRV, TXT) | - | No |
| `--project <id>` | Ask as this project ID | - | No |
| `--sandbox <id>` | Ask as this sandbox | - | No |
| `--json` | Output in JSON format | - | No |

### `cluster network`

The compute network workloads get their addresses from

**Subcommands:**

- `show` - Show the compute pool, whether it can change, and each node's allocation
- `set` - Change the compute pool (only while no allocation uses it)
- `new-network-id` - Give this cluster its own VXLAN network ID, so it cannot collide with another cluster on the same network

#### `cluster network show`

Show the compute pool, whether it can change, and each node's allocation

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster network set`

Change the compute pool (only while no allocation uses it)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--compute-pool <cidr>` | CIDR workloads get addresses from | - | Yes |
| `--overlay-network <name>` | Docker overlay network name | - | No |
| `--subnet-prefix <len>` | Per-node subnet prefix length | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster network new-network-id`

Give this cluster its own VXLAN network ID, so it cannot collide with another cluster on the same network

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `cluster join-token`

The shared token worker nodes join with

**Subcommands:**

- `status` - Show whether a join token is configured
- `generate` - Generate a new join token (replaces the current one; shown once)
- `revoke` - Revoke the join token; no new node can join with it

#### `cluster join-token status`

Show whether a join token is configured

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster join-token generate`

Generate a new join token (replaces the current one; shown once)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask before replacing an existing token | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster join-token revoke`

Revoke the join token; no new node can join with it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `cluster enrollment-tokens`

Short-lived, single-use tokens a new node enrolls with

**Subcommands:**

- `list` (`ls`) - List valid enrollment tokens (the tokens themselves are never returned)
- `create` (`mint`) - Mint an enrollment token (shown once)
- `revoke` - Revoke an enrollment token

#### `cluster enrollment-tokens list` (alias: `ls`)

List valid enrollment tokens (the tokens themselves are never returned)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `cluster enrollment-tokens create` (alias: `mint`)

Mint an enrollment token (shown once)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--ttl <seconds>` | How long it is valid | - | No |
| `--max-uses <n>` | How many nodes may enroll with it | - | No |
| `--node <name>` | Only a node with this name may use it | - | No |
| `--json` | Output in JSON format | - | No |

#### `cluster enrollment-tokens revoke`

Revoke an enrollment token

**Usage:** `cluster enrollment-tokens revoke <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `cluster rotate-ca`

Replace a compromised cluster CA: every node re-enrolls and outstanding enrollment tokens are revoked

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--expected-fingerprint <sha256>` | Fingerprint of the current CA, read through a trusted channel just before | - | Yes |
| `-y, --yes` | Do not ask; sends the confirmation "ROTATE CLUSTER CA" for you | - | No |
| `--json` | Output in JSON format | - | No |
