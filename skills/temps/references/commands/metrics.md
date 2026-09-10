<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `metrics` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `metrics` (alias: `metric`)

Query OTel application metrics for debugging (not container/docker stats — see "temps containers metrics" for those)

**Subcommands:**

- `names` - List distinct metric names ingested for a project — start here if you don't know what to query
- `query` - Query a metric with time bucketing and aggregation
- `label-keys` - List the label keys observed on a metric — powers filter/group-by discovery
- `label-values` - List the distinct values seen for a label key on a metric

### `metrics names`

List distinct metric names ingested for a project — start here if you don't know what to query

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

### `metrics query`

Query a metric with time bucketing and aggregation

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--metric-name <name>` | Metric to query (see "temps metrics names") | - | No |
| `--service-name <name>` | Filter by service name | - | No |
| `--environment <name>` | Filter by deployment environment name | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 24h, 7d) | `24h` | No |
| `--start-time <iso>` | Explicit window start (RFC 3339) — overrides --period | - | No |
| `--end-time <iso>` | Explicit window end (RFC 3339) — overrides --period | - | No |
| `--bucket-interval <interval>` | Bucket size, e.g. "5 minutes", "1 hour" | - | No |
| `--aggregation <mode>` | Per-bucket aggregation: avg (default), sum, min, max, count, rate, p50/p95/p99, quantile:0.95 | - | No |
| `--metric-type <type>` | Filter by metric type: gauge, sum, histogram, exponential_histogram, summary | - | No |
| `--label-filters <pairs>` | Comma-separated key=value data-point label filters, e.g. http.method=GET,http.status_code=200 | - | No |
| `--group-by <keys>` | Comma-separated label keys to group series by, e.g. http.method,http.route | - | No |
| `--limit <n>` | Max buckets to return (default: 500, server cap: 1000) | - | No |
| `--json` | Output in JSON format | - | No |

### `metrics label-keys`

List the label keys observed on a metric — powers filter/group-by discovery

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--metric-name <name>` | Metric to inspect | - | Yes |
| `--start-time <iso>` | Window start (RFC 3339); defaults to 24h before end | - | No |
| `--end-time <iso>` | Window end (RFC 3339); defaults to now | - | No |
| `--json` | Output in JSON format | - | No |

### `metrics label-values`

List the distinct values seen for a label key on a metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--metric-name <name>` | Metric to inspect | - | Yes |
| `--label-key <key>` | Label key whose values to list | - | Yes |
| `--start-time <iso>` | Window start (RFC 3339); defaults to 24h before end | - | No |
| `--end-time <iso>` | Window end (RFC 3339); defaults to now | - | No |
| `--json` | Output in JSON format | - | No |
