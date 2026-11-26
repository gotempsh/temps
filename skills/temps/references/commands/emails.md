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
- `tracking` - Tracking commands
- `resend` - Resend a captured email

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
| `--cc <value>` | CC recipient (repeatable) (repeatable) | - | No |
| `--bcc <value>` | BCC recipient (repeatable) (repeatable) | - | No |
| `--reply-to <value>` | Reply-to address | - | No |
| `--html <value>` | HTML body, sent alongside the text body | - | No |
| `--headers <json>` | Extra headers as a JSON object, e.g. {"X-Campaign":"spring"} (JSON) | - | No |

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

### `emails tracking`

Tracking commands

**Subcommands:**

- `get-global-events` - GET /emails/events
- `get-global-event-stats` - GET /emails/events/stats
- `get` - Get email tracking summary
- `get-events` - Get email tracking events
- `get-links` - Get tracked links for an email

#### `emails tracking get-global-events`

GET /emails/events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-type <value>` | Filter by event type (open, click) | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `emails tracking get-global-event-stats`

GET /emails/events/stats

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `emails tracking get`

Get email tracking summary

**Usage:** `emails tracking get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

#### `emails tracking get-events`

Get email tracking events

**Usage:** `emails tracking get-events <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-type <value>` | Filter by event type (open, click) | - | No |
| `--json` | Output in JSON format | - | No |

#### `emails tracking get-links`

Get tracked links for an email

**Usage:** `emails tracking get-links <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `emails resend`

Resend a captured email

**Usage:** `emails resend <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
