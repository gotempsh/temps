<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `facets` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `facets`

Manage OTel span attribute facets — attribute keys promoted to a fast-filterable column (ClickHouse or TimescaleDB, whichever backend is active; see ADR-039). Facets are platform-global, not per-project, since the underlying spans table is shared across every project. Historical backfill runs asynchronously — check `temps facets list` for status.

**Subcommands:**

- `list` (`ls`) - List registered span attribute facets
- `create` - Register an attribute key as a facet, making it fast to filter on across all traces. Backfills existing spans that carry the attribute. Capped at 20 facets platform-wide.
- `remove` (`rm`) - Remove a registered facet, freeing its slot for reuse
- `retry` - Retry a failed historical backfill. Only valid when the facet's status is "failed" — resets progress and lets the background poller re-attempt from the beginning.

### `facets list` (alias: `ls`)

List registered span attribute facets

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `facets create`

Register an attribute key as a facet, making it fast to filter on across all traces. Backfills existing spans that carry the attribute. Capped at 20 facets platform-wide.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `facets remove` (alias: `rm`)

Remove a registered facet, freeing its slot for reuse

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `facets retry`

Retry a failed historical backfill. Only valid when the facet's status is "failed" — resets progress and lets the background poller re-attempt from the beginning.

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
