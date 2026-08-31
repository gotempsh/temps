<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `traefik-discovery` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `traefik-discovery`

Route containers Temps did not deploy by reading their Traefik labels (an existing docker-compose / Coolify / Dokploy stack)

**Subcommands:**

- `status` - Show whether Traefik label discovery is enabled on this server, which Docker network it watches, and what the last reconciliation found
- `routes` - Inspect and suppress individual auto-discovered routes

### `traefik-discovery status`

Show whether Traefik label discovery is enabled on this server, which Docker network it watches, and what the last reconciliation found

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `traefik-discovery routes`

Inspect and suppress individual auto-discovered routes

**Subcommands:**

- `list` (`ls`) - List every route discovered from Traefik labels, including the labelled containers that were found but not routed, and why
- `enable` - Restore a previously suppressed discovered route
- `disable` - Stop routing one discovered host without touching the container labels; the route stays listed so you can see what was found

#### `traefik-discovery routes list` (alias: `ls`)

List every route discovered from Traefik labels, including the labelled containers that were found but not routed, and why

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery routes enable`

Restore a previously suppressed discovered route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery routes disable`

Stop routing one discovered host without touching the container labels; the route stays listed so you can see what was found

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
