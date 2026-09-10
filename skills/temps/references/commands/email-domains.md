<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `email-domains` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `email-domains` (alias: `edom`)

Manage email domains for transactional email

**Subcommands:**

- `list` (`ls`) - List all email domains
- `create` (`add`) - Create a new email domain
- `show` - Show email domain details
- `remove` (`rm`) - Remove an email domain
- `by-name` - Look up an email domain by domain name
- `dns-records` - Get DNS records for an email domain
- `setup-dns` - Setup DNS records using a configured DNS provider
- `verify` - Verify an email domain DNS configuration
- `projects` - List projects authorized to send through an email domain
- `authorize-project` - Authorize a project to send through an email domain
- `revoke-project` - Revoke a project's permission to send through an email domain

### `email-domains list` (alias: `ls`)

List all email domains

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `email-domains create` (alias: `add`)

Create a new email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name (e.g., mail.example.com) | - | No |
| `--provider-id <id>` | Email provider ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `email-domains show`

Show email domain details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains remove` (alias: `rm`)

Remove an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `email-domains by-name`

Look up an email domain by domain name

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --domain <domain>` | Domain name | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains dns-records`

Get DNS records for an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains setup-dns`

Setup DNS records using a configured DNS provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--dns-provider-id <id>` | DNS provider ID to use | - | No |

### `email-domains verify`

Verify an email domain DNS configuration

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |

### `email-domains projects`

List projects authorized to send through an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-domains authorize-project`

Authorize a project to send through an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--project-id <id>` | Project ID | - | Yes |

### `email-domains revoke-project`

Revoke a project's permission to send through an email domain

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email domain ID | - | Yes |
| `--project-id <id>` | Project ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |
