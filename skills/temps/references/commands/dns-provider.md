<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `dns-provider` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `dns-provider` (alias: `dnsp`)

Manage DNS providers and managed domains

**Subcommands:**

- `list` (`ls`) - List all DNS providers
- `create` (`add`) - Create a new DNS provider
- `show` - Show DNS provider details
- `update` - Update a DNS provider
- `remove` (`rm`) - Delete a DNS provider
- `test` - Test DNS provider connection
- `zones` - List DNS zones for a provider
- `domains` - Manage domains associated with a DNS provider
- `lookup` - Lookup DNS A records for a domain

### `dns-provider list` (alias: `ls`)

List all DNS providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `dns-provider create` (alias: `add`)

Create a new DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Provider name | - | No |
| `-t, --type <type>` | Provider type (cloudflare, route53, digitalocean, namecheap, gcp, azure, manual, pebble) | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | API token (Cloudflare, DigitalOcean) | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | Namecheap API key | - | No |
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
| `--management-url <url>` | pebble-challtestsrv management API URL (local ACME test server only) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `dns-provider show`

Show DNS provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns-provider update`

Update a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-n, --name <name>` | New provider name | - | No |
| `-d, --description <description>` | New description | - | No |
| `--api-key <key>` | New API key/token | - | No |
| `--active <boolean>` | Set active status (true/false) | - | No |

### `dns-provider remove` (alias: `rm`)

Delete a DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `dns-provider test`

Test DNS provider connection

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `dns-provider zones`

List DNS zones for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `dns-provider domains`

Manage domains associated with a DNS provider

**Subcommands:**

- `list` (`ls`) - List managed domains for a provider
- `add` - Add a managed domain to a provider
- `remove` (`rm`) - Remove a managed domain from a provider
- `verify` - Verify a managed domain

#### `dns-provider domains list` (alias: `ls`)

List managed domains for a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `dns-provider domains add`

Add a managed domain to a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--auto-manage` | Enable auto-management for DNS records | - | No |

#### `dns-provider domains remove` (alias: `rm`)

Remove a managed domain from a provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--provider-id <id>` | Provider ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `dns-provider domains verify`

Verify a managed domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--provider-id <id>` | Provider ID | - | Yes |
| `-d, --domain <domain>` | Domain name | - | Yes |

### `dns-provider lookup`

Lookup DNS A records for a domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name to lookup | - | Yes |
| `--json` | Output in JSON format | - | No |
