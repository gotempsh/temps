<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `domains` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `domains` (alias: `domain`)

Manage custom domains

**Subcommands:**

- `list` (`ls`) - List domains
- `add` - Add a custom domain
- `verify` - Verify domain and provision SSL certificate
- `remove` (`rm`) - Remove a domain
- `ssl` - Manage SSL certificate
- `status` - Check domain status
- `renewal-attempts` - Show the certificate renewal-attempt history for a domain
- `orders` (`order`) - Manage ACME orders for SSL certificate provisioning
- `dns-challenge` - Setup DNS challenge records automatically using a DNS provider
- `http-debug` - Debug HTTP-01 challenge for a domain

### `domains list` (alias: `ls`)

List domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains add`

Add a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-c, --challenge <type>` | Challenge type (http-01 or dns-01) | `http-01` | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

### `domains verify`

Verify domain and provision SSL certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |

### `domains remove` (alias: `rm`)

Remove a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `domains ssl`

Manage SSL certificate

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--renew` | Force certificate renewal | - | No |

### `domains status`

Check domain status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |

### `domains renewal-attempts`

Show the certificate renewal-attempt history for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--page <page>` | Page number (1-indexed) | `1` | No |
| `--page-size <pageSize>` | Items per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `domains orders` (alias: `order`)

Manage ACME orders for SSL certificate provisioning

**Subcommands:**

- `list` (`ls`) - List all ACME orders
- `show` - Show ACME order for a domain
- `create` - Create or recreate an ACME order for a domain
- `finalize` - Finalize an ACME order (complete challenge validation)
- `cancel` - Cancel an ACME order for a domain

#### `domains orders list` (alias: `ls`)

List all ACME orders

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `domains orders show`

Show ACME order for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `domains orders create`

Create or recreate an ACME order for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |

#### `domains orders finalize`

Finalize an ACME order (complete challenge validation)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |

#### `domains orders cancel`

Cancel an ACME order for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `domains dns-challenge`

Setup DNS challenge records automatically using a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--domain-id <id>` | Domain ID | - | Yes |
| `--provider-id <id>` | DNS provider ID | - | Yes |

### `domains http-debug`

Debug HTTP-01 challenge for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--json` | Output in JSON format | - | No |
