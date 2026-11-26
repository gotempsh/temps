<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `email-providers` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `email-providers` (alias: `eprov`)

Manage email providers (SES, Scaleway) for transactional email

**Subcommands:**

- `list` (`ls`) - List all email providers
- `create` (`add`) - Create a new email provider
- `show` - Show email provider details
- `remove` (`rm`) - Remove an email provider
- `test` - Test an email provider by sending a test email
- `discoverable-domains` (`discover-domains`) - List domain identities already registered on the provider's side, for importing
- `update` - Update an email provider
- `setup-tracking` - One-click AWS-side setup of SES event tracking (SNS topic + webhook subscription + SESv2 event..
- `get-tracking-status` - Live status of SES event tracking for a provider

### `email-providers list` (alias: `ls`)

List all email providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `email-providers create` (alias: `add`)

Create a new email provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Provider name | - | No |
| `-t, --type <type>` | Provider type (ses, scaleway, smtp) | - | No |
| `-r, --region <region>` | Cloud region | - | No |
| `--access-key-id <key>` | AWS access key ID (for SES) | - | No |
| `--secret-access-key <secret>` | AWS secret access key (for SES) | - | No |
| `--api-key <key>` | Scaleway API key | - | No |
| `--project-id <id>` | Scaleway project ID | - | No |
| `--sns-topic-arn <arn>` | SES: the only SNS topic allowed to deliver bounce and complaint events | - | No |
| `--smtp-host <host>` | SMTP server host name | - | No |
| `--smtp-port <port>` | SMTP port (default: 587, or 465 with --smtp-encryption tls) | - | No |
| `--smtp-encryption <mode>` | starttls (default), tls or none | - | No |
| `--smtp-username <user>` | SMTP user name, when the server requires a sign-in | - | No |
| `--smtp-password <password>` | SMTP password | - | No |
| `--smtp-accept-invalid-certs` | Accept a self-signed or otherwise invalid server certificate | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--secret-access-key-file <path>` | Read the AWS secret access key from a file (- reads stdin) instead of --secret-access-key | - | No |
| `--secret-access-key-stdin` | Read the AWS secret access key from stdin instead of --secret-access-key | - | No |
| `--api-key-file <path>` | Read the Scaleway API key from a file (- reads stdin) instead of --api-key | - | No |
| `--api-key-stdin` | Read the Scaleway API key from stdin instead of --api-key | - | No |
| `--smtp-password-file <path>` | Read the SMTP password from a file (- reads stdin) instead of --smtp-password | - | No |
| `--smtp-password-stdin` | Read the SMTP password from stdin instead of --smtp-password | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `email-providers show`

Show email provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-providers remove` (alias: `rm`)

Remove an email provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `email-providers test`

Test an email provider by sending a test email

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--from <email>` | Sender email address (must be verified) | - | No |
| `--from-name <name>` | Sender display name | - | No |

### `email-providers discoverable-domains` (alias: `discover-domains`)

List domain identities already registered on the provider's side, for importing

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `email-providers update`

Update an email provider

**Usage:** `email-providers update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--is-active` | Body field is_active | - | No |
| `--no-is-active` | Set is_active to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--region <value>` | Body field region | - | No |
| `--scaleway-credentials <json>` | Body field scaleway_credentials (JSON) | - | No |
| `--ses-credentials <json>` | Body field ses_credentials (JSON) | - | No |
| `--smtp-credentials <json>` | Generic SMTP credentials request body (JSON) | - | No |
| `--sns-topic-arn <value>` | Rotate or clear the exact SNS topic allowed for this SES provider | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `email-providers setup-tracking`

One-click AWS-side setup of SES event tracking (SNS topic + webhook subscription + SESv2 event..

**Usage:** `email-providers setup-tracking <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `email-providers get-tracking-status`

Live status of SES event tracking for a provider

**Usage:** `email-providers get-tracking-status <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
