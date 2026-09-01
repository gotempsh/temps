// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { otelIngestErrors } from './ingest-errors.js'
import { otelPipelineHistory } from './pipeline-history.js'

export function registerOtelCommands(program: Command): void {
  const otel = program
    .command('otel')
    .description(
      'Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)'
    )

  otel
    .command('ingest-errors')
    .description(
      'Show why ingest batches were dropped, grouped by signal and failure reason'
    )
    .option('--limit <n>', 'Max failure groups to return (default: 20, server cap: 100)')
    .option('--json', 'Output in JSON format')
    .action(otelIngestErrors)

  otel
    .command('pipeline-history')
    .description('Show pipeline counter trends over time (received/stored/dropped per signal)')
    .option(
      '--period <period>',
      'Time period: 1h, 6h, 24h, 7d (server presets), or today/<n>h/<n>d resolved locally',
      '24h'
    )
    .option('--start-time <iso>', 'Explicit window start (RFC 3339) — overrides --period')
    .option('--end-time <iso>', 'Explicit window end (RFC 3339) — overrides --period')
    .option('--json', 'Output in JSON format')
    .action(otelPipelineHistory)

  otel.addHelpText(
    'after',
    `
Triage workflow — a drop counter moved, now find out why:
  $ temps otel pipeline-history --period 24h     # when did it happen?
  $ temps otel ingest-errors                     # what failed, and how often?

Examples:
  $ temps otel ingest-errors --limit 50
  $ temps otel ingest-errors --json
  $ temps otel pipeline-history --period 7d
  $ temps otel pipeline-history \\
      --start-time 2026-08-24T00:00:00Z --end-time 2026-08-25T00:00:00Z --json

Notes:
  Ingest errors are recorded only after a storage write exhausts its retries,
  so any entry means data was actually lost. Windows are capped at 7 days.`
  )
}
