<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `webhooks` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `webhooks` (alias: `hooks`)

Manage webhooks for project events

**Subcommands:**

- `list` (`ls`) - List all webhooks for a project
- `create` (`add`) - Create a new webhook for a project
- `show` - Show webhook details
- `update` - Update a webhook
- `remove` (`rm`) - Delete a webhook
- `enable` - Enable a webhook
- `disable` - Disable a webhook
- `events` - List available webhook event types
- `deliveries` - Manage webhook deliveries

### `webhooks list` (alias: `ls`)

List all webhooks for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `webhooks create` (alias: `add`)

Create a new webhook for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-u, --url <url>` | Webhook URL | - | No |
| `-e, --events <events>` | Comma-separated event types (or "all" for all events) | - | No |
| `-s, --secret <secret>` | Webhook secret for signature verification | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `webhooks show`

Show webhook details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `webhooks update`

Update a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `-u, --url <url>` | New webhook URL | - | No |
| `-e, --events <events>` | Comma-separated event types (or "all" for all events) | - | No |
| `-s, --secret <secret>` | New webhook secret for signature verification | - | No |

### `webhooks remove` (alias: `rm`)

Delete a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `webhooks enable`

Enable a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |

### `webhooks disable`

Disable a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |

### `webhooks events`

List available webhook event types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `webhooks deliveries`

Manage webhook deliveries

**Subcommands:**

- `list` (`ls`) - List deliveries for a webhook
- `show` - Show delivery details
- `retry` - Retry a failed delivery

#### `webhooks deliveries list` (alias: `ls`)

List deliveries for a webhook

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--limit <n>` | Number of deliveries to return (default: 50) | - | No |
| `--json` | Output in JSON format | - | No |

#### `webhooks deliveries show`

Show delivery details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--delivery-id <id>` | Delivery ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `webhooks deliveries retry`

Retry a failed delivery

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--webhook-id <id>` | Webhook ID | - | Yes |
| `--delivery-id <id>` | Delivery ID | - | Yes |
