<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `revenue` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `revenue`

Manage revenue integrations and import historical data

**Subcommands:**

- `import` - Import historical revenue data from a CSV export
- `recent-events` - Recent ingested events for the activity feed
- `list-integrations` - List revenue integrations for a project
- `create-integration` - Create a new revenue integration
- `delete-integration` - Delete a revenue integration (permanent — use rotate_token to refresh credentials without breaking history)
- `update-config` - Replace the typed provider config on an integration
- `import-invoices-csv` - Import a Stripe invoices CSV export
- `import-subscriptions-csv` - Import a Stripe subscriptions CSV export
- `rotate-token` - Rotate the webhook path token
- `update-secret` - Replace the stored signing secret without rotating the webhook URL
- `metrics-customers` - New + churned customers per bucket
- `metrics-mrr` - Bucketed MRR timeseries for the revenue chart
- `metrics-summary` - Current MRR / ARR / churn / ARPU for a project, in one currency
- `global-events` - Org-wide revenue events across every project
- `metrics-global-mrr` - Org-wide MRR total, summed across every project in the install
- `metrics-global-summary` - Org-wide revenue summary: MRR, paid cash (30d + all-time), refunds, active subscriptions/customers, and..
- `list-providers` - List registered providers (what the UI needs to render the "Connect" dropdown + its wizard instructions)

### `revenue import`

Import historical revenue data from a CSV export

**Subcommands:**

- `subscriptions` - Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)
- `invoices` - Import paid invoices CSV to backfill the revenue chart

#### `revenue import subscriptions`

Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)

**Usage:** `revenue import subscriptions <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

#### `revenue import invoices`

Import paid invoices CSV to backfill the revenue chart

**Usage:** `revenue import invoices <file> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

### `revenue recent-events`

Recent ingested events for the activity feed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue list-integrations`

List revenue integrations for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue create-integration`

Create a new revenue integration

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--provider <value>` | Registered provider name, e.g. "stripe" [required] | - | No |
| `--signing-secret-stdin` | Signing secret from the provider's dashboard [required] (read from stdin) | - | No |
| `--signing-secret-file <path>` | Signing secret from the provider's dashboard [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue delete-integration`

Delete a revenue integration (permanent — use rotate_token to refresh credentials without breaking history)

**Usage:** `revenue delete-integration <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue update-config`

Replace the typed provider config on an integration

**Usage:** `revenue update-config <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--config <json>` | Provider-specific integration settings persisted in 'revenue_integrations.config' (JSON) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue import-invoices-csv`

Import a Stripe invoices CSV export

**Usage:** `revenue import-invoices-csv <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue import-subscriptions-csv`

Import a Stripe subscriptions CSV export

**Usage:** `revenue import-subscriptions-csv <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue rotate-token`

Rotate the webhook path token

**Usage:** `revenue rotate-token <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue update-secret`

Replace the stored signing secret without rotating the webhook URL

**Usage:** `revenue update-secret <integration-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--signing-secret-stdin` | New signing secret from the provider's dashboard [required] (read from stdin) | - | No |
| `--signing-secret-file <path>` | New signing secret from the provider's dashboard [required] (read from a file) | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-customers`

New + churned customers per bucket

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-mrr`

Bucketed MRR timeseries for the revenue chart

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-summary`

Current MRR / ARR / churn / ARPU for a project, in one currency

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue global-events`

Org-wide revenue events across every project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter to a single project | - | No |
| `--from <value>` | Lower bound (inclusive), ISO-8601 | - | No |
| `--to <value>` | Upper bound (inclusive), ISO-8601 | - | No |
| `--event-types <value>` | Comma-separated event types (e.g. 'invoice.paid,charge.succeeded') | - | No |
| `--limit <n>` | Max rows, default 100, max 500 | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue metrics-global-mrr`

Org-wide MRR total, summed across every project in the install

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `revenue metrics-global-summary`

Org-wide revenue summary: MRR, paid cash (30d + all-time), refunds, active subscriptions/customers, and..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--currency <value>` | ISO-4217 currency code, default USD | - | No |
| `--json` | Output in JSON format | - | No |

### `revenue list-providers`

List registered providers (what the UI needs to render the "Connect" dropdown + its wizard instructions)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
