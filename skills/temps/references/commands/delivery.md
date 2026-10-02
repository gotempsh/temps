<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `delivery` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `delivery`

Route project domains through a delivery provider (Cloudflare proxy, Bunny CDN)

**Subcommands:**

- `settings` - Project default delivery profile and per-environment overrides
- `bindings` (`binding`) - Domain delivery bindings: the DNS record + provider routing for a hostname

### `delivery settings`

Project default delivery profile and per-environment overrides

**Subcommands:**

- `get` (`show`) - Show the delivery profile a project and its environments use
- `set` - Change the project default profile and/or environment overrides

#### `delivery settings get` (alias: `show`)

Show the delivery profile a project and its environments use

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

#### `delivery settings set`

Change the project default profile and/or environment overrides

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--default-profile <id>` | Project default delivery profile ID, or "none" to clear it | - | No |
| `--env <environment-id=profile-id>` | Environment override, e.g. 12=3; use 12=inherit to fall back to the project default (repeatable) | `` | No |
| `--json` | Output in JSON format | - | No |

### `delivery bindings` (alias: `binding`)

Domain delivery bindings: the DNS record + provider routing for a hostname

**Subcommands:**

- `list` (`ls`) - List domain delivery bindings for a project, one page at a time (newest first)
- `preview` - Plan a delivery binding without changing DNS; prints a preview ID to apply
- `apply` - Apply a previewed delivery binding (writes DNS)
- `remove` (`rm`, `delete`) - Remove a delivery binding and the DNS record it manages

#### `delivery bindings list` (alias: `ls`)

List domain delivery bindings for a project, one page at a time (newest first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Bindings per page, 1-100 (default: 20) | - | No |
| `--sort-by <field>` | Sort field: created_at, hostname, updated_at (default: created_at) | - | No |
| `--sort-order <order>` | asc or desc (default: desc) | - | No |
| `--json` | Output the page as JSON (items, total, page, page_size) | - | No |

#### `delivery bindings preview`

Plan a delivery binding without changing DNS; prints a preview ID to apply

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Environment ID the hostname serves | - | No |
| `--hostname <hostname>` | Hostname to route, e.g. app.example.com | - | No |
| `--zone <zone>` | DNS zone that contains the hostname, e.g. example.com | - | No |
| `--dns-provider <id>` | DNS provider ID that manages the zone | - | No |
| `--origin-target <target>` | Origin the record points at (IP for A/AAAA, hostname for CNAME) | - | No |
| `--profile <id>` | Delivery profile ID (defaults to the environment or project profile) | - | No |
| `--json` | Output in JSON format | - | No |

#### `delivery bindings apply`

Apply a previewed delivery binding (writes DNS)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--preview-id <id>` | Preview ID returned by `delivery bindings preview` | - | Yes |
| `--adopt <TYPE:name>` | Adopt an existing unmanaged record the preview flagged, e.g. CNAME:www (repeatable) | `` | No |
| `--json` | Output in JSON format | - | No |

#### `delivery bindings remove` (alias: `rm`, `delete`)

Remove a delivery binding and the DNS record it manages

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--id <id>` | Binding ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
