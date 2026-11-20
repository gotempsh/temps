<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `monitors` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `monitors` (alias: `monitoring`)

Manage uptime monitors for status pages

**Subcommands:**

- `list` (`ls`) - List all monitors for a project
- `create` (`add`) - Create a new monitor for a project
- `add-target` - Monitor a URL, port, TLS certificate or DNS record outside Temps
- `update-target` - Change a monitor of a target outside Temps; omitted flags keep their value
- `locations` - List where monitors checked from worker nodes are checked from
- `show` - Show monitor details and current status
- `remove` (`rm`) - Delete a monitor
- `status` - Get current status — all monitors for a project, or a single monitor by ID
- `history` - Get monitor uptime history

### `monitors list` (alias: `ls`)

List all monitors for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors create` (alias: `add`)

Create a new monitor for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `-n, --name <name>` | Monitor name | - | No |
| `-t, --type <type>` | Monitor type (http, tcp, ping) | - | No |
| `-i, --interval <seconds>` | Check interval in seconds (60, 300, 600, 900, 1800) | - | No |
| `--check-path <path>` | HTTP health-check path (must start with "/", e.g. /api/healthz). Defaults to "/" for HTTP monitors. | - | No |
| `--environment-id <id>` | Environment ID (default: 0 for production) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `monitors add-target`

Monitor a URL, port, TLS certificate or DNS record outside Temps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--kind <kind>` | http, tcp, tls or dns | - | Yes |
| `--target <target>` | URL (http), host:port (tcp, tls) or host name (dns) | - | Yes |
| `-n, --name <name>` | Monitor name | - | Yes |
| `-i, --interval <seconds>` | Check interval in seconds (default 60, at least 30) | - | No |
| `--keyword-absent` | Fail when --keyword is present instead of missing | - | No |
| `--from-nodes` | Also check from online worker nodes; down only when most locations agree | - | No |
| `--method <method>` | HTTP method (default GET) | - | No |
| `--expect-status <codes>` | Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx) | - | No |
| `--keyword <text>` | Text the response body must contain (first 256 KiB; never stored) | - | No |
| `--degraded-after-ms <ms>` | Slower than this is degraded (http, tcp) | - | No |
| `--warn-days <days>` | Degraded when the certificate expires within this many days (tls, default 14) | - | No |
| `--record <type>` | DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A) | - | No |
| `--expect <values>` | Comma-separated DNS values; any match is up (default: any answer) | - | No |
| `--header-file <path>` | File of "Name: value" request headers; stored encrypted, never shown | - | No |
| `--body-file <path>` | File with the request body; stored encrypted | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors update-target`

Change a monitor of a target outside Temps; omitted flags keep their value

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--monitor-id <id>` | Monitor ID | - | Yes |
| `--kind <kind>` | Change the check kind (pass the check flags for it) | - | No |
| `--target <target>` | New target | - | No |
| `-n, --name <name>` | New name | - | No |
| `-i, --interval <seconds>` | Check interval in seconds | - | No |
| `--keyword-absent` | Fail when --keyword is present instead of missing | - | No |
| `--pause` | Stop checking | - | No |
| `--resume` | Start checking again | - | No |
| `--from-nodes` | Also check from online worker nodes | - | No |
| `--no-from-nodes` | Check from this server only | - | No |
| `--method <method>` | HTTP method (default GET) | - | No |
| `--expect-status <codes>` | Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx) | - | No |
| `--keyword <text>` | Text the response body must contain (first 256 KiB; never stored) | - | No |
| `--degraded-after-ms <ms>` | Slower than this is degraded (http, tcp) | - | No |
| `--warn-days <days>` | Degraded when the certificate expires within this many days (tls, default 14) | - | No |
| `--record <type>` | DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A) | - | No |
| `--expect <values>` | Comma-separated DNS values; any match is up (default: any answer) | - | No |
| `--header-file <path>` | File of "Name: value" request headers; stored encrypted, never shown | - | No |
| `--body-file <path>` | File with the request body; stored encrypted | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors locations`

List where monitors checked from worker nodes are checked from

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors show`

Show monitor details and current status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `monitors remove` (alias: `rm`)

Delete a monitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `monitors status`

Get current status — all monitors for a project, or a single monitor by ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID (omit to show all monitors for the project) | - | No |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json or TEMPS_PROJECT) | - | No |
| `--json` | Output in JSON format | - | No |

### `monitors history`

Get monitor uptime history

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Monitor ID | - | Yes |
| `--json` | Output in JSON format | - | No |
| `--days <days>` | Number of days to show | `7` | No |
