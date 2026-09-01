<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `notifications` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `notifications` (alias: `notify`)

Manage notification providers (Slack, Email, Webhook, etc.)

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

### `notifications list` (alias: `ls`)

List configured notification providers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `notifications add`

Add a new notification provider

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Provider type (slack, email, webhook) | - | No |
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
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

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
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Skip confirmation prompts | - | No |

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

#### `notifications routes list` (alias: `ls`)

List notification routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

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
