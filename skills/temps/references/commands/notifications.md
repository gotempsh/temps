<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `notifications` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`notifications list`](#notifications-list)
- [`notifications add`](#notifications-add)
- [`notifications update`](#notifications-update)
- [`notifications enable`](#notifications-enable)
- [`notifications disable`](#notifications-disable)
- [`notifications show`](#notifications-show)
- [`notifications remove`](#notifications-remove)
- [`notifications test`](#notifications-test)
- [`notifications routes`](#notifications-routes)
- [`notifications create-cloudflare-provider`](#notifications-create-cloudflare-provider)
- [`notifications update-cloudflare-provider`](#notifications-update-cloudflare-provider)
- [`notifications create-email-provider`](#notifications-create-email-provider)
- [`notifications create-webhook-provider`](#notifications-create-webhook-provider)
- [`notifications update-webhook-provider`](#notifications-update-webhook-provider)
- [`notifications reveal-provider-config`](#notifications-reveal-provider-config)

## `notifications` (alias: `notify`)

Manage notification providers (Slack, Email, Webhook, Telegram, etc.)

**Subcommands:**

- `list` (`ls`) - List configured notification providers
- `add` - Add a new notification provider
- `update` - Update a notification provider
- `enable` - Enable a notification provider
- `disable` - Disable a notification provider
- `show` - Show notification provider details
- `remove` (`rm`) - Remove a notification provider
- `test` - Send a test notification
- `routes` - Manage severity-based notification routes (which providers receive which severities)
- `create-cloudflare-provider` - Create a new Cloudflare Email Sending notification provider
- `update-cloudflare-provider` - Update a Cloudflare Email Sending notification provider
- `create-email-provider` - Create a new Email notification provider
- `create-webhook-provider` - Create a new Webhook notification provider
- `update-webhook-provider` - Update a Webhook notification provider
- `reveal-provider-config` - Reveal notification provider config

### `notifications list` (alias: `ls`)

List configured notification providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

### `notifications add`

Add a new notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Provider type (slack, email, webhook, telegram) | - | No |
| `-n, --name <name>` | Provider name | - | No |
| `-w, --webhook-url <url>` | Webhook URL (for slack) | - | No |
| `-c, --channel <channel>` | Channel name (for slack, optional) | - | No |
| `--smtp-host <host>` | SMTP host (for email) | - | No |
| `--smtp-port <port>` | SMTP port (for email) | - | No |
| `--username <username>` | SMTP username (for email) | - | No |
| `--password <password>` | SMTP password (for email) | - | No |
| `--from-address <address>` | From email address (for email) | - | No |
| `--from-name <name>` | From display name (for email, optional) | - | No |
| `--to-addresses <addresses>` | Comma-separated recipient addresses (for email) | - | No |
| `--url <url>` | Webhook URL (for webhook) | - | No |
| `--method <method>` | HTTP method: POST, PUT, PATCH (for webhook, default: POST) | - | No |
| `--header <header>` | Request header "Name: value" (for webhook, repeatable) | - | No |
| `--bot-token <token>` | Bot token from @BotFather (for telegram) | - | No |
| `--chat-id <id>` | Chat ID, e.g. -1001234567890 or @channelname (for telegram) | - | No |
| `--topic-id <id>` | Forum topic ID (for telegram, optional) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--webhook-url-file <path>` | Read the Slack webhook URL from a file (- reads stdin) instead of --webhook-url | - | No |
| `--webhook-url-stdin` | Read the Slack webhook URL from stdin instead of --webhook-url | - | No |
| `--password-file <path>` | Read the SMTP password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the SMTP password from stdin instead of --password | - | No |
| `--bot-token-file <path>` | Read the Telegram bot token from a file (- reads stdin) instead of --bot-token | - | No |
| `--bot-token-stdin` | Read the Telegram bot token from stdin instead of --bot-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `notifications update`

Update a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-n, --name <name>` | New provider name | - | No |
| `--enabled <enabled>` | Enable or disable (true/false) | - | No |
| `-w, --webhook-url <url>` | Webhook URL (for slack) | - | No |
| `-c, --channel <channel>` | Channel name (for slack) | - | No |
| `--smtp-host <host>` | SMTP host (for email) | - | No |
| `--smtp-port <port>` | SMTP port (for email) | - | No |
| `--username <username>` | SMTP username (for email) | - | No |
| `--password <password>` | SMTP password (for email) | - | No |
| `--from-address <address>` | From email address (for email) | - | No |
| `--from-name <name>` | From display name (for email) | - | No |
| `--to-addresses <addresses>` | Comma-separated recipient addresses (for email) | - | No |
| `--url <url>` | Webhook URL (for webhook) | - | No |
| `--method <method>` | HTTP method: POST, PUT, PATCH (for webhook) | - | No |
| `--header <header>` | Request header "Name: value" (for webhook, repeatable; replaces the stored headers) | - | No |
| `--bot-token <token>` | Bot token from @BotFather (for telegram) | - | No |
| `--chat-id <id>` | Chat ID, e.g. -1001234567890 or @channelname (for telegram) | - | No |
| `--topic-id <id>` | Forum topic ID (for telegram; empty string clears it) | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |
| `--webhook-url-file <path>` | Read the Slack webhook URL from a file (- reads stdin) instead of --webhook-url | - | No |
| `--webhook-url-stdin` | Read the Slack webhook URL from stdin instead of --webhook-url | - | No |
| `--password-file <path>` | Read the SMTP password from a file (- reads stdin) instead of --password | - | No |
| `--password-stdin` | Read the SMTP password from stdin instead of --password | - | No |
| `--bot-token-file <path>` | Read the Telegram bot token from a file (- reads stdin) instead of --bot-token | - | No |
| `--bot-token-stdin` | Read the Telegram bot token from stdin instead of --bot-token | - | No |
| `--trim-trailing-newline` | Drop exactly one trailing newline (\n or \r\n) from a value read from a file or stdin (kept byte-for-byte by default) | - | No |

### `notifications enable`

Enable a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `notifications disable`

Disable a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `notifications show`

Show notification provider details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `notifications remove` (alias: `rm`)

Remove a notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `notifications test`

Send a test notification

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Provider ID | - | Yes |

### `notifications routes`

Manage severity-based notification routes (which providers receive which severities)

**Subcommands:**

- `list` (`ls`) - List notification routes
- `show` - Show notification route details
- `create` - Create a notification route
- `update` - Update a notification route
- `remove` (`rm`) - Remove a notification route
- `get-delivery-coverage` - Check whether a notification of the given severity would be delivered
- `test` - Send a test notification through a route to each of its providers

#### `notifications routes list` (alias: `ls`)

List notification routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--sort-by <value>` | Sort column (default created_at) | - | No |
| `--sort-order <value>` | Sort direction (one of: asc, desc) | - | No |

#### `notifications routes show`

Show notification route details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Route ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `notifications routes create`

Create a notification route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Route name | - | No |
| `--min-severity <severity>` | Minimum severity: debug, info, warning, error, critical, emergency | - | No |
| `--max-severity <severity>` | Maximum severity: debug, info, warning, error, critical, emergency | - | No |
| `--provider-ids <ids>` | Comma-separated notification provider IDs | - | No |
| `--enabled <enabled>` | Enable or disable (true/false, default: true) | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

#### `notifications routes update`

Update a notification route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Route ID | - | Yes |
| `-n, --name <name>` | New route name | - | No |
| `--min-severity <severity>` | Minimum severity: debug, info, warning, error, critical, emergency | - | No |
| `--max-severity <severity>` | Maximum severity: debug, info, warning, error, critical, emergency | - | No |
| `--provider-ids <ids>` | Comma-separated notification provider IDs (replaces the current set) | - | No |
| `--enabled <enabled>` | Enable or disable (true/false) | - | No |
| `--json` | Output in JSON format | - | No |

#### `notifications routes remove` (alias: `rm`)

Remove a notification route

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Route ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

#### `notifications routes get-delivery-coverage`

Check whether a notification of the given severity would be delivered

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--severity <value>` | Severity of the notification to check: debug, info, warning, error, critical or emergency [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `notifications routes test`

Send a test notification through a route to each of its providers

**Usage:** `notifications routes test <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notifications create-cloudflare-provider`

Create a new Cloudflare Email Sending notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a Cloudflare Email Sending notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications update-cloudflare-provider`

Update a Cloudflare Email Sending notification provider

**Usage:** `notifications update-cloudflare-provider <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a Cloudflare Email Sending notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications create-email-provider`

Create a new Email notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Request-side SMTP config (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications create-webhook-provider`

Create a new Webhook notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a generic webhook notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications update-webhook-provider`

Update a Webhook notification provider

**Usage:** `notifications update-webhook-provider <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--config <json>` | Configuration for a generic webhook notification provider (JSON) [required] | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `notifications reveal-provider-config`

Reveal notification provider config

**Usage:** `notifications reveal-provider-config <id> <field> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
