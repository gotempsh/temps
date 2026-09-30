<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `dns` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `dns`

Manage DNS providers and Temps-managed DNS records

**Subcommands:**

- `list` (`ls`) - List configured DNS providers
- `add` - Add a new DNS provider
- `show` - Show DNS provider details
- `remove` (`rm`) - Remove a DNS provider
- `test` - Test DNS provider connection
- `zones` - List available zones in a DNS provider
- `records` (`record`) - Manage DNS records on managed domains (ownership-guarded: Temps only changes records it owns)

### `dns list` (alias: `ls`)

List configured DNS providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `dns add`

Add a new DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Provider type (cloudflare, bunny, route53, digitalocean, namecheap, gcp, azure, manual) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | Cloudflare API token | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | API key (Bunny, Namecheap) | - | No |
| `--username <username>` | Namecheap username | - | No |
| `--client-ip <ip>` | Namecheap whitelisted client IP | - | No |
| `--project-id <id>` | GCP project ID | - | No |
| `--service-account-email <email>` | GCP service account email | - | No |
| `--private-key-id <id>` | GCP private key ID | - | No |
| `--private-key <key>` | GCP private key | - | No |
| `--tenant-id <id>` | Azure tenant ID | - | No |
| `--client-id <id>` | Azure client ID | - | No |
| `--client-secret <secret>` | Azure client secret | - | No |
| `--subscription-id <id>` | Azure subscription ID | - | No |
| `--resource-group <name>` | Azure resource group | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `dns show`

Show DNS provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns remove` (alias: `rm`)

Remove a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

### `dns test`

Test DNS provider connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `dns zones`

List available zones in a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns records` (alias: `record`)

Manage DNS records on managed domains (ownership-guarded: Temps only changes records it owns)

**Subcommands:**

- `ownership` (`owner`) - Show whether Temps owns a DNS record and may change it
- `set` (`create`) - Create or update a Temps-owned DNS record
- `import` (`adopt`) - Adopt an existing DNS record into Temps management
- `remove` (`rm`, `delete`) - Delete a Temps-owned DNS record

#### `dns records ownership` (alias: `owner`)

Show whether Temps owns a DNS record and may change it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME, TXT, MX, NS, SRV, CAA, PTR) | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `dns records set` (alias: `create`)

Create or update a Temps-owned DNS record

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME, TXT, MX, NS, SRV, CAA, PTR) | - | Yes |
| `--value <value>` | Record value: address (A/AAAA), target (CNAME/MX/SRV/PTR), text (TXT), nameserver (NS), CAA value | - | No |
| `--priority <n>` | Priority (MX, SRV) | - | No |
| `--weight <n>` | Weight (SRV) | - | No |
| `--port <n>` | Port (SRV) | - | No |
| `--flags <n>` | Flags (CAA, default 0) | - | No |
| `--tag <tag>` | Tag (CAA: issue, issuewild, iodef) | - | No |
| `--ttl <seconds>` | TTL in seconds (default: provider default) | - | No |
| `--proxied` | Proxy through the provider CDN (Cloudflare orange cloud) | - | No |
| `--no-proxied` | Do not proxy (DNS only) | - | No |
| `-p, --project <project>` | Project slug or ID to stamp as the record owner | - | No |
| `--environment-id <id>` | Environment ID to stamp as the record owner | - | No |
| `--json` | Output in JSON format | - | No |

#### `dns records import` (alias: `adopt`)

Adopt an existing DNS record into Temps management

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME, TXT, MX, NS, SRV, CAA, PTR) | - | Yes |
| `-p, --project <project>` | Project slug or ID to stamp as the record owner | - | No |
| `--environment-id <id>` | Environment ID to stamp as the record owner | - | No |
| `--json` | Output in JSON format | - | No |

#### `dns records remove` (alias: `rm`, `delete`)

Delete a Temps-owned DNS record

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain under a managed zone, e.g. example.com | - | Yes |
| `--name <name>` | Record name relative to the zone ("@" for apex) | - | Yes |
| `-t, --type <type>` | Record type (A, AAAA, CNAME, TXT, MX, NS, SRV, CAA, PTR) | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
