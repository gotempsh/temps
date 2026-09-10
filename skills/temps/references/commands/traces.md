<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `traces` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `traces` (alias: `trace`)

Inspect distributed traces and operation latency

**Subcommands:**

- `span-stats` (`operations`, `ops`) - Rank operations by time spent, latency percentiles, or inconsistency

### `traces span-stats` (alias: `operations`, `ops`)

Rank operations by time spent, latency percentiles, or inconsistency

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--project-ids <ids>` | Comma-separated project IDs to rank across, e.g. 4,5,6 (max 50) | - | No |
| `--since <duration>` | Relative window: 30m, 24h, 7d (max 31d) | `24h` | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--service <name>` | Only this service | - | No |
| `--operation <name>` | Only this operation (exact span name) | - | No |
| `--search <text>` | Only operations whose name contains this text | - | No |
| `--kind <kind>` | Span kind (server, client, internal, producer, consumer, unspecified) | - | No |
| `--status <status>` | Span status (ok, error, unset) | - | No |
| `--environment-id <id>` | Only this environment | - | No |
| `--deployment-id <id>` | Only this deployment | - | No |
| `--attributes <pairs>` | Span attribute filters, e.g. db.system=postgresql | - | No |
| `--min-duration-ms <ms>` | Ignore spans faster than this | - | No |
| `--min-count <n>` | Drop operations with fewer samples than this | - | No |
| `--sort-by <field>` | Ranking (total_time, p50, p95, p99, max, avg, stddev, count, errors, error_rate, variability, tail_ratio) | `total_time` | No |
| `--sort-order <order>` | asc or desc | `desc` | No |
| `--limit <n>` | Rows to show (max 100) | `20` | No |
| `--offset <n>` | Page offset | - | No |
| `--json` | Output in JSON format | - | No |
