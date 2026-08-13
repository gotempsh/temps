<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `revenue` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `revenue`

Manage revenue integrations and import historical data

**Subcommands:**

- `import` - Import historical revenue data from a CSV export

### `revenue import`

Import historical revenue data from a CSV export

**Subcommands:**

- `subscriptions` - Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)
- `invoices` - Import paid invoices CSV to backfill the revenue chart

#### `revenue import subscriptions`

Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |

#### `revenue import invoices`

Import paid invoices CSV to backfill the revenue chart

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (defaults to linked project) | - | No |
| `--integration-id <id>` | Target integration ID (auto-detected if only one exists) | - | No |
| `--provider <name>` | Target provider name (e.g., stripe) | - | No |
| `--json` | Output the import outcome as JSON (suppresses spinners) | - | No |
