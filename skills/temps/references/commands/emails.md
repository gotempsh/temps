<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `emails` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `emails` (alias: `email`)

Manage and send emails

**Subcommands:**

- `list` (`ls`) - List sent emails
- `send` - Send an email
- `show` - Show email details
- `stats` - Get email statistics
- `validate` - Validate an email address

### `emails list` (alias: `ls`)

List sent emails

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--page <n>` | Page number | - | No |
| `--page-size <n>` | Items per page | - | No |
| `--status <status>` | Filter by status (sent, delivered, failed) | - | No |
| `--domain-id <id>` | Filter by domain ID | - | No |
| `--project-id <id>` | Filter by project ID | - | No |
| `--from-address <email>` | Filter by sender address | - | No |

### `emails send`

Send an email

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--to <email>` | Recipient email address | - | No |
| `--subject <subject>` | Email subject | - | No |
| `--body <body>` | Email body | - | No |
| `--from <email>` | Sender email address | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `emails show`

Show email details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Email ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `emails stats`

Get email statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `emails validate`

Validate an email address

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--email <email>` | Email address to validate | - | No |
| `--json` | Output in JSON format | - | No |
