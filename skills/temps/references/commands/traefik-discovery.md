<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `traefik-discovery` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `traefik-discovery`

Route containers Temps did not deploy by reading their Traefik labels (an existing docker-compose / Coolify / Dokploy stack)

**Subcommands:**

- `status` - Show whether Traefik label discovery is enabled on this server, which Docker network it watches, and what the last reconciliation found
- `routes` - Inspect and suppress individual auto-discovered routes
- `tls` - Manage HTTPS certificates for Traefik-discovered routes (ADR-041). A discovered host has cert_eligible=false by design — no container label ever causes issuance. These commands let an operator explicitly authorize it.

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

### `traefik-discovery tls`

Manage HTTPS certificates for Traefik-discovered routes (ADR-041). A discovered host has cert_eligible=false by design — no container label ever causes issuance. These commands let an operator explicitly authorize it.

**Subcommands:**

- `request` - Authorize Temps to obtain a Let's Encrypt certificate for a discovered route (Path A). The certificate renews automatically using the declared challenge type.
- `revoke` - Remove TLS authorization for a discovered route. Stops Temps from attempting renewal. Does NOT delete the certificate — use `temps domains delete <host>` to remove the certificate itself.
- `import` - Import certificates from a Traefik acme.json file (Path B). Use this to get HTTPS immediately at cutover — Traefik already holds the cert, so there is no outage window. Each host is validated (8-step X.509 chain) and a per-host result is returned. Add --dry-run to preview without writing.

#### `traefik-discovery tls request`

Authorize Temps to obtain a Let's Encrypt certificate for a discovered route (Path A). The certificate renews automatically using the declared challenge type.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--challenge-type <type>` | Challenge type: http-01 (default) or dns-01 | `http-01` | No |
| `--acknowledge-manual-dns-renewal` | Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured | - | No |
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery tls revoke`

Remove TLS authorization for a discovered route. Stops Temps from attempting renewal. Does NOT delete the certificate — use `temps domains delete <host>` to remove the certificate itself.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `traefik-discovery tls import`

Import certificates from a Traefik acme.json file (Path B). Use this to get HTTPS immediately at cutover — Traefik already holds the cert, so there is no outage window. Each host is validated (8-step X.509 chain) and a per-host result is returned. Add --dry-run to preview without writing.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--hosts <hosts>` | Comma-separated list of hostnames to import | - | Yes |
| `--renewal-method <method>` | How Temps will renew when the imported cert expires: http-01 (default) or dns-01 | `http-01` | No |
| `--acknowledge-manual-dns-renewal` | Confirm you accept manual DNS renewal when no auto-manage DNS zone is configured | - | No |
| `--dry-run` | Validate and preview; do not write any certificate | - | No |
| `--json` | Output in JSON format | - | No |
