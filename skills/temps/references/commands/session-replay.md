<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `session-replay` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `session-replay` (alias: `sessions`, `replay`)

Manage session replay recordings

**Subcommands:**

- `list` (`ls`) - List session replays for a project
- `visitor` - List session replays for a specific visitor
- `show` - Show session metadata (use numeric session ID from list)
- `events` - Download or page through all rrweb events for a session
- `delete` (`rm`) - Delete a session replay

### `session-replay list` (alias: `ls`)

List session replays for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--page <n>` | Page number (default: 1) | `1` | No |
| `--per-page <n>` | Sessions per page (default: 25, max: 100) | `25` | No |
| `--json` | Output raw JSON | - | No |

### `session-replay visitor`

List session replays for a specific visitor

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page number (default: 1) | `1` | No |
| `--per-page <n>` | Sessions per page (default: 25) | `25` | No |
| `--json` | Output raw JSON | - | No |

### `session-replay show`

Show session metadata (use numeric session ID from list)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output raw JSON | - | No |

### `session-replay events`

Download or page through all rrweb events for a session

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page <n>` | Page of events to display (default: 1) | `1` | No |
| `--limit <n>` | Events per page (default: 50) | `50` | No |
| `--output <file>` | Write all events as JSON to a file (skips paged display) | - | No |
| `--json` | Print all events as JSON to stdout | - | No |

### `session-replay delete` (alias: `rm`)

Delete a session replay

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-y, --yes` | Skip confirmation prompt | - | No |
