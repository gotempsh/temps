<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `logs` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `logs` (alias: `glogs`)

Search collected logs across every project and database you can access

**Subcommands:**

- `search` - Search log lines across projects (newest first)
- `facets` - Distinct values and counts per field for the same filter scope
- `attributes` - Attribute keys observed in the window, most common first (requires the ClickHouse line index)
- `histogram` - Line counts bucketed over time, optionally split by a label or attribute (requires the ClickHouse line index)
- `aggregate` - Group-by aggregation over lines (requires the ClickHouse line index)
- `capabilities` - Whether container logs are being collected, and whether attribute facets, histograms and aggregates are available

### `logs search`

Search log lines across projects (newest first)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Lines per request (default: 200, max: 1000) | - | No |
| `--cursor <token>` | Resume from a previous next_cursor | - | No |
| `--all` | Follow next_cursor automatically until the results run out (or --max-pages is reached, default 20 pages) | - | No |
| `--max-pages <n>` | Page ceiling for --all (default: 20) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id|slug|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs facets`

Distinct values and counts per field for the same filter scope

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--field <name>` | Facet field, repeatable: env, service, level, stream, project, external_service, node, deploy, container (default: env, service, level, node, deploy) | `` | No |
| `--attr-keys <keys>` | Comma-separated label names and/or attr:<name> (e.g. worker,attr:cache) — routes to the attribute-aware endpoint, which requires the ClickHouse line index | - | No |
| `--limit <n>` | Max values per key when using --attr-keys or --attr (default 50, cap 1000) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id|slug|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs attributes`

Attribute keys observed in the window, most common first (requires the ClickHouse line index)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max keys returned (default 100, cap 1000) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id|slug|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs histogram`

Line counts bucketed over time, optionally split by a label or attribute (requires the ClickHouse line index)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--bucket-secs <n>` | Bucket width in seconds (default 60) | - | No |
| `--group-by <key>` | One label name or attr:<name> to split series by | - | No |
| `--max-groups <n>` | Max series before folding the rest into "other" (default 8) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id|slug|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs aggregate`

Group-by aggregation over lines (requires the ClickHouse line index)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group-by <keys>` | Comma-separated label names and/or attr:<name> | - | Yes |
| `--metric <spec>` | count \| count_distinct:<k> \| avg:<k> \| p50:<k> \| p95:<k> \| p99:<k> \| max:<k> \| sum:<k> | - | Yes |
| `--limit <n>` | Max rows returned (default 50, cap 1000) | - | No |
| `--since <duration>` | Relative window ending now, e.g. 30m, 6h, 7d (default: 1h) | - | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--source <kind>` | collected (default), application, or service | - | No |
| `--project <id|slug|name>` | Restrict to a project, repeatable | `` | No |
| `--external-service <id|name>` | Restrict to a managed database/service, repeatable | `` | No |
| `--scope <kind:id>` | Explicit resource identity, e.g. application:12, repeatable | `` | No |
| `--level <level>` | TRACE\|DEBUG\|INFO\|WARN\|ERROR, repeatable | `` | No |
| `--env <name>` | Environment, repeatable | `` | No |
| `--service <name>` | Container service label (web, worker, …), repeatable | `` | No |
| `--container <id>` | Container ID, repeatable | `` | No |
| `--node <id>` | Worker node ID, repeatable | `` | No |
| `--deploy <id>` | Deployment ID | - | No |
| `--text <substring>` | Case-insensitive message substring match | - | No |
| `--attr <pred>` | Attribute predicate, repeatable: <key>=<value>, <key>!=<value>, <key>^=<prefix>, <key>><value>, <key><<value>, or <key>? for "exists" (requires the ClickHouse line index) | `` | No |
| `--json` | Output in JSON format | - | No |

### `logs capabilities`

Whether container logs are being collected, and whether attribute facets, histograms and aggregates are available

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
