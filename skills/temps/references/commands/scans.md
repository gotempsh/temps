<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `scans` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `scans` (alias: `scan`)

Manage vulnerability scans

**Subcommands:**

- `list` (`ls`) - List vulnerability scans for a project
- `trigger` - Trigger a new vulnerability scan
- `latest` - Get the latest scan for a project
- `environments` (`envs`) - Get latest scans per environment
- `show` - Show scan details
- `vulnerabilities` (`vulns`) - List vulnerabilities found in a scan
- `remove` (`rm`) - Delete a vulnerability scan
- `by-deployment` - Get the scan for a specific deployment

### `scans list` (alias: `ls`)

List vulnerability scans for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--page <n>` | Page number | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `scans trigger`

Trigger a new vulnerability scan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Environment ID to scan | - | Yes |

### `scans latest`

Get the latest scan for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `scans environments` (alias: `envs`)

Get latest scans per environment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `scans show`

Show scan details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Scan ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `scans vulnerabilities` (alias: `vulns`)

List vulnerabilities found in a scan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Scan ID | - | Yes |
| `--severity <level>` | Filter by severity (CRITICAL, HIGH, MEDIUM, LOW) | - | No |
| `--json` | Output in JSON format | - | No |

### `scans remove` (alias: `rm`)

Delete a vulnerability scan

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Scan ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `scans by-deployment`

Get the scan for a specific deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--deployment-id <id>` | Deployment ID | - | Yes |
| `--json` | Output in JSON format | - | No |
