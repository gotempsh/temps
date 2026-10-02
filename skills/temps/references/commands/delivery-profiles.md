<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `delivery-profiles` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `delivery-profiles` (alias: `delivery-profile`)

Manage traffic delivery profiles (Cloudflare proxy, Bunny CDN) used by project domains

**Subcommands:**

- `capabilities` (`caps`) - Show which delivery providers are available and what each one still needs
- `list` (`ls`) - List delivery profiles, one page at a time (newest first)
- `create` (`add`) - Create a delivery profile
- `remove` (`rm`, `delete`) - Delete a delivery profile

### `delivery-profiles capabilities` (alias: `caps`)

Show which delivery providers are available and what each one still needs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `delivery-profiles list` (alias: `ls`)

List delivery profiles, one page at a time (newest first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Profiles per page, 1-100 (default: 20) | - | No |
| `--sort-by <field>` | Sort field: created_at, name (default: created_at) | - | No |
| `--sort-order <order>` | asc or desc (default: desc) | - | No |
| `--search <text>` | Only profiles whose name contains this text, ignoring case (at most 100 characters) | - | No |
| `--json` | Output the page as JSON (items, total, page, page_size) | - | No |

### `delivery-profiles create` (alias: `add`)

Create a delivery profile

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-n, --name <name>` | Profile name (1-100 characters) | - | No |
| `-k, --kind <kind>` | Delivery provider (cloudflare, bunny, direct) | - | No |
| `--pull-zone-id <id>` | Bunny Pull Zone ID (required for --kind bunny) | - | No |
| `--api-key <key>` | Bunny account API key (prefer --api-key-stdin to keep it out of shell history) | - | No |
| `--api-key-stdin` | Read the Bunny API key from stdin | - | No |
| `--json` | Output in JSON format | - | No |
| `-y, --yes` | Never prompt (for automation) | - | No |

### `delivery-profiles remove` (alias: `rm`, `delete`)

Delete a delivery profile

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Profile ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
