<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `tunnels` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `tunnels`

Ingress tunnels: publish apps on workers with no open ports or certificates. Apps under a tunnel's domain go live by adding the hostname to an environment

**Subcommands:**

- `list` - List tunnels, what each publishes and the workers it runs on
- `workers` - List worker nodes, for choosing where a tunnel runs (create --node)
- `create` - Create a tunnel for a managed zone and run it on workers. The zone must already be managed by the DNS provider (dns-providers domains add); workers must allow ingress
- `hostnames` - List hostnames tunnels publish or could publish, with their state and why
- `explain` - Explain how a hostname reaches visitors: which tunnel carries it, or why none does
- `route` - Choose how a hostname reaches visitors: through a tunnel (--tunnel <id>), whichever tunnel covers it (--automatic), or no tunnel (--off)
- `publish` - Set what a tunnel publishes: every app under its domain (--all) or only chosen hostnames (--chosen)
- `nodes` - Set the workers a tunnel runs on (an empty list stops it everywhere)
- `links` - Show a tunnel's live connections to the provider: data centre, age, worker
- `sync` - Re-check a tunnel with its provider and recreate it if it was deleted there

### `tunnels list`

List tunnels, what each publishes and the workers it runs on

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <page>` | Page number (1-indexed) | `1` | No |
| `--page-size <pageSize>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `tunnels workers`

List worker nodes, for choosing where a tunnel runs (create --node)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels create`

Create a tunnel for a managed zone and run it on workers. The zone must already be managed by the DNS provider (dns-providers domains add); workers must allow ingress

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--name <name>` | Lowercase letters, digits and hyphens | - | Yes |
| `--dns-provider <id>` | Cloudflare DNS provider id | - | Yes |
| `--domain <domain>` | Zone (or subdomain of one) whose apps it publishes | - | Yes |
| `--node <ids...>` | Worker node ids to run it on | - | Yes |
| `--provider <kind>` | Tunnel provider kind | `cloudflare_tunnel` | No |
| `--json` | Output in JSON format | - | No |

### `tunnels hostnames`

List hostnames tunnels publish or could publish, with their state and why

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels explain`

Explain how a hostname reaches visitors: which tunnel carries it, or why none does

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels route`

Choose how a hostname reaches visitors: through a tunnel (--tunnel <id>), whichever tunnel covers it (--automatic), or no tunnel (--off)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--tunnel <id>` | Pin it to this tunnel | - | No |
| `--automatic` | Let the most specific covering tunnel publish it | - | No |
| `--off` | Keep every tunnel away from it (DNS managed elsewhere) | - | No |
| `--json` | Output in JSON format | - | No |

### `tunnels publish`

Set what a tunnel publishes: every app under its domain (--all) or only chosen hostnames (--chosen)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--all` | Publish every app under its domain | - | No |
| `--chosen` | Publish only hostnames routed to it | - | No |

### `tunnels nodes`

Set the workers a tunnel runs on (an empty list stops it everywhere)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--node <ids...>` | Worker node ids | - | Yes |

### `tunnels links`

Show a tunnel's live connections to the provider: data centre, age, worker

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `tunnels sync`

Re-check a tunnel with its provider and recreate it if it was deleted there
