<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `otel` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `otel`

Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)

**Subcommands:**

- `ingest-errors` - Show why ingest batches were dropped, grouped by signal and failure reason
- `pipeline-history` - Show pipeline counter trends over time (received/stored/dropped per signal)

### `otel ingest-errors`

Show why ingest batches were dropped, grouped by signal and failure reason

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max failure groups to return (default: 20, server cap: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel pipeline-history`

Show pipeline counter trends over time (received/stored/dropped per signal)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--period <period>` | Time period: 1h, 6h, 24h, 7d (server presets), or today/<n>h/<n>d resolved locally | `24h` | No |
| `--start-time <iso>` | Explicit window start (RFC 3339) — overrides --period | - | No |
| `--end-time <iso>` | Explicit window end (RFC 3339) — overrides --period | - | No |
| `--json` | Output in JSON format | - | No |
