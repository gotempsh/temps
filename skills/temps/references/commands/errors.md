<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `errors` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `errors` (alias: `error`)

Manage error tracking and error groups

**Subcommands:**

- `list` (`ls`) - List error groups for a project
- `show` - Show error group details
- `update` - Update error group status
- `events` - List events in an error group
- `event` - Show a specific error event
- `stats` - Get error statistics for a project
- `timeline` - Get error time series data
- `dashboard` - Get error dashboard statistics
- `sourcemaps` (`sm`) - Manage source maps for error symbolication
- `source-files` (`sf`) - Manage raw source files for native (Go/Rust/…) symbolication

### `errors list` (alias: `ls`)

List error groups for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--status <status>` | Filter by status (unresolved, resolved, ignored) | - | No |
| `--page <page>` | Page number | - | No |
| `--page-size <size>` | Page size | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--start-date <date>` | Filter by start date (ISO 8601) | - | No |
| `--end-date <date>` | Filter by end date (ISO 8601) | - | No |
| `--sort-by <field>` | Sort by field (e.g., total_count, last_seen, first_seen) | - | No |
| `--sort-order <order>` | Sort order: asc or desc | - | No |
| `--json` | Output in JSON format | - | No |

### `errors show`

Show error group details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors update`

Update error group status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--status <status>` | New status (unresolved, resolved, ignored) | - | Yes |
| `--assigned-to <user>` | Assign to user | - | No |

### `errors events`

List events in an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--page <page>` | Page number | - | No |
| `--page-size <size>` | Page size | - | No |
| `--json` | Output in JSON format | - | No |

### `errors event`

Show a specific error event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--event-id <id>` | Error event ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors stats`

Get error statistics for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors timeline`

Get error time series data

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--days <days>` | Number of days to show | `7` | No |
| `--bucket <bucket>` | Time bucket size (e.g., "1h", "15m", "1d") | `1h` | No |
| `--environment-id <id>` | Filter chart data to a specific environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `errors dashboard`

Get error dashboard statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--days <days>` | Number of days to show | `7` | No |
| `--compare` | Compare to previous period | - | No |
| `--json` | Output in JSON format | - | No |

### `errors sourcemaps` (alias: `sm`)

Manage source maps for error symbolication

**Subcommands:**

- `upload` - Upload a source map file for a release
- `list` (`ls`) - List source maps for a release
- `releases` - List all releases that have source maps
- `delete` - Delete all source maps for a release
- `delete-one` - Delete a specific source map by ID

#### `errors sourcemaps upload`

Upload a source map file for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version (e.g. commit SHA) | - | Yes |
| `--file <path>` | Path to the .map file | - | Yes |
| `--file-path <urlpath>` | URL path in stack traces (e.g. ~/assets/main.js) | - | No |
| `--dist <dist>` | Distribution identifier | - | No |

#### `errors sourcemaps list` (alias: `ls`)

List source maps for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors sourcemaps releases`

List all releases that have source maps

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors sourcemaps delete`

Delete all source maps for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |

#### `errors sourcemaps delete-one`

Delete a specific source map by ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--source-map-id <id>` | Source map ID | - | Yes |

### `errors source-files` (alias: `sf`)

Manage raw source files for native (Go/Rust/…) symbolication

**Subcommands:**

- `upload` - Upload source file(s) for a release (single --file or a --dir tree)
- `list` (`ls`) - List uploaded source files for a release
- `delete` - Delete all source files for a release

#### `errors source-files upload`

Upload source file(s) for a release (single --file or a --dir tree)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version (must match the app's SENTRY_RELEASE, e.g. the deployed commit SHA) | - | Yes |
| `--file <path>` | Path to a single source file | - | No |
| `--file-path <path>` | Path as it appears in stack frames (e.g. internal/gateway/main.go); defaults to the file name | - | No |
| `--dir <root>` | Upload every source file under this directory, recursively | - | No |
| `--ext <csv>` | Comma-separated extensions to include with --dir (default: go,rs,py,rb,js,jsx,ts,tsx,java,kt,c,h,cpp,cc,hpp,cs,php,swift,scala,ex,exs) | - | No |

#### `errors source-files list` (alias: `ls`)

List uploaded source files for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors source-files delete`

Delete all source files for a release

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--release <version>` | Release version | - | Yes |
