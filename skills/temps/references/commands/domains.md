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
- `get-by-host` - Get domain details by hostname
- `get-on-demand-cert-status` - Get on-demand TLS certificate status for a hostname
- `list-on-demand-certs` - List on-demand TLS certificate attempts
- `get-certificate-settings` - Get certificate settings
- `update-certificate-settings` - Update certificate settings
- `get-by-id` - Get domain by ID
- `get-challenge-token` - Get challenge token for a domain (returns plain text token)

### `domains list` (alias: `ls`)

List domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--search <value>` | Only domains whose name contains this | - | No |
| `--sort-by <value>` | Sort column (default created_at) (one of: created_at, domain, status, expiration) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

### `domains add`

Add a custom domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-c, --challenge <type>` | Challenge type (http-01 or dns-01) | `http-01` | No |
| `--manual-dns` | With dns-01, print the TXT record to add by hand even when a DNS provider in Temps manages the zone | - | No |
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
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

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

### `domains get-by-host`

Get domain details by hostname

**Usage:** `domains get-by-host <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains get-on-demand-cert-status`

Get on-demand TLS certificate status for a hostname

**Usage:** `domains get-on-demand-cert-status <hostname> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains list-on-demand-certs`

List on-demand TLS certificate attempts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (1-indexed) | - | No |
| `--page-size <n>` | Number of items per page (max 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `domains get-certificate-settings`

Get certificate settings

**Usage:** `domains get-certificate-settings <domain-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains update-certificate-settings`

Update certificate settings

**Usage:** `domains update-certificate-settings <domain-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--alert-days <n>` | Override the instance SSL expiry warning threshold; null inherits it | - | No |
| `--alerts-enabled` | Override instance SSL expiry alerts; null inherits it | - | No |
| `--no-alerts-enabled` | Set alerts_enabled to false | - | No |
| `--challenge-type <value>` | Validation method for future ACME orders [required] | - | No |
| `--renewal-days <n>` | Start ACME renewal this many days before expiry (1–90) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `domains get-by-id`

Get domain by ID

**Usage:** `domains get-by-id <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `domains get-challenge-token`

Get challenge token for a domain (returns plain text token)

**Usage:** `domains get-challenge-token <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-o, --output <path>` | Where to write the reply (default: stdout) | `-` | No |
| `--json` | Output in JSON format | - | No |
