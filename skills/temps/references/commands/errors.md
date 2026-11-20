<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `errors` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`errors list`](#errors-list)
- [`errors assign`](#errors-assign)
- [`errors show`](#errors-show)
- [`errors update`](#errors-update)
- [`errors bulk`](#errors-bulk)
- [`errors merge`](#errors-merge)
- [`errors unmerge`](#errors-unmerge)
- [`errors split`](#errors-split)
- [`errors fingerprints`](#errors-fingerprints)
- [`errors filters`](#errors-filters)
- [`errors comment`](#errors-comment)
- [`errors activity`](#errors-activity)
- [`errors searches`](#errors-searches)
- [`errors performance`](#errors-performance)
- [`errors releases`](#errors-releases)
- [`errors release`](#errors-release)
- [`errors events`](#errors-events)
- [`errors event`](#errors-event)
- [`errors stats`](#errors-stats)
- [`errors timeline`](#errors-timeline)
- [`errors dashboard`](#errors-dashboard)
- [`errors sourcemaps`](#errors-sourcemaps)
- [`errors source-files`](#errors-source-files)

## `errors` (alias: `error`)

Manage error tracking and error groups

**Subcommands:**

- `list` (`ls`) - List error groups for a project
- `assign` - Assign an error group to a user or team, or unassign it
- `show` - Show error group details
- `update` - Update error group status
- `bulk` - Resolve, reopen or ignore many error groups in one request
- `merge` - Merge other error groups into one surviving group
- `unmerge` - Take an error group back out of the merge it is part of
- `split` - Split fingerprints out of an error group into a new one
- `fingerprints` - List the fingerprints routing to an error group
- `filters` - Drop noisy error events before they are stored (browser extensions, crawlers, localhost, message or release patterns)
- `comment` - Comment on an error group, or edit or delete a comment
- `activity` - Show the comments and history of an error group, newest first
- `searches` - Saved searches over error groups
- `performance` - Show whether traces are checked for performance issues, and what is detected
- `releases` - Show the health of each release: crash-free sessions and users, adoption and deployment
- `release` - Show one release's sessions and crash-free rate per hour
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
| `--assignee <who>` | Only groups assigned to: me (you or your teams), none, user:<id> or team:<id> | - | No |
| `--search <text>` | Only groups whose title, type or message contains this text | - | No |
| `--saved-search <id>` | Apply a saved search; flags given here override its filters | - | No |
| `--type <type>` | Only errors, or only performance issues found in traces: error or performance | - | No |
| `--json` | Output in JSON format | - | No |

### `errors assign`

Assign an error group to a user or team, or unassign it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--user-id <id>` | Assign to this user | - | No |
| `--team-id <id>` | Assign to this team | - | No |
| `--me` | Assign to yourself | - | No |
| `--none` | Unassign the group | - | No |
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
| `--assigned-to <email>` | Assign to the user with this email ("" unassigns); see also errors assign | - | No |
| `--ignore-for <duration>` | With ignore: come back after this long (30m, 12h, 7d, 2w) | - | No |
| `--ignore-until <date>` | With ignore: come back at this time (ISO 8601) | - | No |
| `--ignore-count <n>` | With ignore: come back after this many more occurrences | - | No |
| `--ignore-users <n>` | With ignore: come back once this many new users are affected | - | No |
| `--ignore-forever` | With ignore: never come back on its own | - | No |
| `--in-next-release` | With resolve: stay resolved until an event arrives from a release newer than the current one | - | No |

### `errors bulk`

Resolve, reopen or ignore many error groups in one request

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--action <action>` | Transition to apply (resolve, unresolve, ignore) | - | Yes |
| `--group-ids <ids>` | Comma-separated group IDs (at most 1000) | - | No |
| `--status <status>` | Select every group in this status instead of listing IDs | - | No |
| `--environment-id <id>` | Select every group of this environment | - | No |
| `--start-date <date>` | With --end-date: only groups with events in the window (ISO 8601) | - | No |
| `--end-date <date>` | With --start-date: only groups with events in the window (ISO 8601) | - | No |
| `--search <text>` | Select groups whose title, type or message contains this text | - | No |
| `--json` | Output in JSON format | - | No |
| `--ignore-for <duration>` | With ignore: come back after this long (30m, 12h, 7d, 2w) | - | No |
| `--ignore-until <date>` | With ignore: come back at this time (ISO 8601) | - | No |
| `--ignore-count <n>` | With ignore: come back after this many more occurrences | - | No |
| `--ignore-users <n>` | With ignore: come back once this many new users are affected | - | No |
| `--ignore-forever` | With ignore: never come back on its own | - | No |
| `--in-next-release` | With resolve: stay resolved until an event arrives from a release newer than the current one | - | No |

### `errors merge`

Merge other error groups into one surviving group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Surviving error group ID | - | Yes |
| `--source-group-ids <ids>` | Comma-separated group IDs to merge in (at most 50) | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors unmerge`

Take an error group back out of the merge it is part of

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Merged-away error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors split`

Split fingerprints out of an error group into a new one

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--fingerprints <hashes>` | Comma-separated fingerprint hashes to move out | - | Yes |
| `--title <title>` | Title for the new group | - | No |
| `--json` | Output in JSON format | - | No |

### `errors fingerprints`

List the fingerprints routing to an error group

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors filters`

Drop noisy error events before they are stored (browser extensions, crawlers, localhost, message or release patterns)

**Subcommands:**

- `list` - List a project's inbound filters and how many events each dropped
- `add` - Add an inbound filter
- `update` - Change a filter's pattern, or switch it on or off
- `remove` (`delete`) - Delete an inbound filter; matching events are stored again

#### `errors filters list`

List a project's inbound filters and how many events each dropped

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors filters add`

Add an inbound filter

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--kind <kind>` | browser-extensions, web-crawlers, localhost, error-message or release | - | Yes |
| `--pattern <glob>` | For error-message and release: a case-insensitive glob, * and ? wildcards | - | No |
| `--disabled` | Create the filter switched off | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors filters update`

Change a filter's pattern, or switch it on or off

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--filter-id <id>` | Filter ID | - | Yes |
| `--pattern <glob>` | New pattern | - | No |
| `--enable` | Switch the filter on | - | No |
| `--disable` | Switch the filter off | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors filters remove` (alias: `delete`)

Delete an inbound filter; matching events are stored again

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--filter-id <id>` | Filter ID | - | Yes |

### `errors comment`

Comment on an error group, or edit or delete a comment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--body <text>` | The comment (at most 10,000 characters) | - | No |
| `--edit <comment-id>` | Replace the text of your comment with --body | - | No |
| `--delete <comment-id>` | Delete a comment (yours, or any as an administrator) | - | No |
| `--json` | Output in JSON format | - | No |

### `errors activity`

Show the comments and history of an error group, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--group-id <id>` | Error group ID | - | Yes |
| `--page <n>` | Page number | `1` | No |
| `--page-size <n>` | Entries per page (max 100) | `20` | No |
| `--json` | Output in JSON format | - | No |

### `errors searches`

Saved searches over error groups

**Subcommands:**

- `list` (`ls`) - List your saved searches and the ones shared with the project
- `save` - Save a set of list filters under a name
- `delete` - Delete a saved search (yours, or a shared one as an administrator)

#### `errors searches list` (alias: `ls`)

List your saved searches and the ones shared with the project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `errors searches save`

Save a set of list filters under a name

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--name <name>` | Name, unique among your saved searches in the project | - | Yes |
| `--shared` | Share with everyone who can open the project | - | No |
| `--status <status>` | unresolved, resolved or ignored | - | No |
| `--environment-id <id>` | An environment of the project | - | No |
| `--assignee <who>` | me, none, user:<id> or team:<id> | - | No |
| `--search <text>` | Text in the title, type or message | - | No |
| `--sort-by <field>` | last_seen, first_seen, total_count or events_in_range | - | No |
| `--sort-order <order>` | asc or desc | - | No |
| `--json` | Output in JSON format | - | No |

#### `errors searches delete`

Delete a saved search (yours, or a shared one as an administrator)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--id <id>` | Saved search ID | - | Yes |

### `errors performance`

Show whether traces are checked for performance issues, and what is detected

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `errors releases`

Show the health of each release: crash-free sessions and users, adoption and deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment <name>` | SDK environment (as the SDK sends it) | - | No |
| `--days <n>` | Window in days, 1 to 14 | `7` | No |
| `--page <n>` | Page | `1` | No |
| `--json` | Output in JSON format | - | No |

### `errors release`

Show one release's sessions and crash-free rate per hour

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | Yes |
| `--environment <name>` | SDK environment (as the SDK sends it) | - | No |
| `--days <n>` | Window in days, 1 to 14 | `7` | No |
| `--json` | Output in JSON format | - | No |

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
