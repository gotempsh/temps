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
- `update-managed-domain` - Update a managed domain's settings (hostname mode, sync opt-in, auto-manage)
- `apply-hostname-mode` - Apply a hostname mode to a managed domain (persist + optional DNS sync + route reload)
- `preview-hostname-mode` - Preview the impact of switching a managed domain's hostname mode

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
| `-t, --type <type>` | Provider type (cloudflare, bunny, route53, digitalocean, namecheap, gcp, azure, manual, pebble) | - | No |
| `-d, --description <description>` | Provider description | - | No |
| `--api-token <token>` | API token (Cloudflare, DigitalOcean; prefer --api-token-stdin to keep it out of shell history) | - | No |
| `--api-token-stdin` | Read the API token from stdin | - | No |
| `--account-id <id>` | Cloudflare account ID (optional) | - | No |
| `--access-key-id <key>` | AWS access key ID | - | No |
| `--secret-access-key <secret>` | AWS secret access key (prefer --secret-access-key-stdin to keep it out of shell history) | - | No |
| `--secret-access-key-stdin` | Read the AWS secret access key from stdin | - | No |
| `--region <region>` | AWS region | - | No |
| `--api-user <user>` | Namecheap API user | - | No |
| `--api-key <key>` | API key (Bunny, Namecheap; prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the Bunny or Namecheap API key from stdin | - | No |
| `--username <username>` | Namecheap username | - | No |
| `--client-ip <ip>` | Namecheap whitelisted client IP | - | No |
| `--project-id <id>` | GCP project ID | - | No |
| `--service-account-email <email>` | GCP service account email | - | No |
| `--private-key-id <id>` | GCP private key ID | - | No |
| `--private-key <key>` | GCP private key (prefer --private-key-stdin to keep it out of shell history) | - | No |
| `--private-key-stdin` | Read the GCP private key from stdin | - | No |
| `--tenant-id <id>` | Azure tenant ID | - | No |
| `--client-id <id>` | Azure client ID | - | No |
| `--client-secret <secret>` | Azure client secret (prefer --client-secret-stdin to keep it out of shell history) | - | No |
| `--client-secret-stdin` | Read the Azure client secret from stdin | - | No |
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
| `--api-key <key>` | New API key/token (prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the new API key/token from stdin | - | No |
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
| `--generated-hostname-mode <value>` | Layout of generated hostnames (one of: standard, flat) | - | No |
| `--proxied-by-default` | Create records proxied through the provider (Cloudflare) by default | - | No |
| `--no-proxied-by-default` | Set proxied_by_default to false | - | No |
| `--sync-generated-records` | Keep generated hostnames' DNS records in this zone in sync | - | No |
| `--no-sync-generated-records` | Set sync_generated_records to false | - | No |

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

### `dns-provider update-managed-domain`

Update a managed domain's settings (hostname mode, sync opt-in, auto-manage)

**Usage:** `dns-provider update-managed-domain <provider-id> <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--auto-manage` | Toggle automatic DNS management for this domain | - | No |
| `--no-auto-manage` | Set auto_manage to false | - | No |
| `--generated-hostname-mode <value>` | '"standard"' or '"flat"' | - | No |
| `--proxied-by-default` | Default proxy mode for newly managed records; 'false' is an explicit override | - | No |
| `--no-proxied-by-default` | Set proxied_by_default to false | - | No |
| `--sync-generated-records` | Toggle DNS record sync for this domain | - | No |
| `--no-sync-generated-records` | Set sync_generated_records to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `dns-provider apply-hostname-mode`

Apply a hostname mode to a managed domain (persist + optional DNS sync + route reload)

**Usage:** `dns-provider apply-hostname-mode <provider-id> <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--adopt-records <json>` | Conflicting records, from the preview's 'conflicts', that the user confirmed the sync may adopt — one... (JSON) | - | No |
| `--mode <value>` | Target mode to apply: '"standard"' or '"flat"' [required] | - | No |
| `--skip-records <json>` | Conflicting hostnames, from the preview's 'conflicts', that the user chose to leave untouched: the sync... (JSON) | - | No |
| `--sync-dns` | Also reconcile the provider's DNS zone for the affected hostnames | - | No |
| `--no-sync-dns` | Set sync_dns to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `dns-provider preview-hostname-mode`

Preview the impact of switching a managed domain's hostname mode

**Usage:** `dns-provider preview-hostname-mode <provider-id> <domain> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--mode <value>` | Target mode: standard\|flat [required] | - | No |
| `--sync` | Include DNS record changes | - | No |
| `--no-sync` | Set sync to false | - | No |
| `--json` | Output in JSON format | - | No |
