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
| `-t, --type <type>` | Provider type (ses, scaleway) | - | No |
| `-r, --region <region>` | Cloud region | - | No |
| `--access-key-id <key>` | AWS access key ID (for SES) | - | No |
| `--secret-access-key <secret>` | AWS secret access key (for SES) | - | No |
| `--api-key <key>` | Scaleway API key | - | No |
| `--project-id <id>` | Scaleway project ID | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

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
