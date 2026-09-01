// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { metricsQuery } from './query.js'
import { metricsNames } from './names.js'
import { metricsLabelKeys } from './label-keys.js'
import { metricsLabelValues } from './label-values.js'

export function registerMetricsCommands(program: Command): void {
  const metrics = program
    .command('metrics')
    .alias('metric')
    .description('Query OTel application metrics for debugging (not container/docker stats — see "temps containers metrics" for those)')

  metrics
    .command('names')
    .description('List distinct metric names ingested for a project — start here if you don\'t know what to query')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(metricsNames)

  metrics
    .command('query')
    .description('Query a metric with time bucketing and aggregation')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--metric-name <name>', 'Metric to query (see "temps metrics names")')
    .option('--service-name <name>', 'Filter by service name')
    .option('--environment <name>', 'Filter by deployment environment name')
    .option('--period <period>', 'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 24h, 7d)', '24h')
    .option('--start-time <iso>', 'Explicit window start (RFC 3339) — overrides --period')
    .option('--end-time <iso>', 'Explicit window end (RFC 3339) — overrides --period')
    .option('--bucket-interval <interval>', 'Bucket size, e.g. "5 minutes", "1 hour"')
    .option(
      '--aggregation <mode>',
      'Per-bucket aggregation: avg (default), sum, min, max, count, rate, p50/p95/p99, quantile:0.95'
    )
    .option('--metric-type <type>', 'Filter by metric type: gauge, sum, histogram, exponential_histogram, summary')
    .option('--label-filters <pairs>', 'Comma-separated key=value data-point label filters, e.g. http.method=GET,http.status_code=200')
    .option('--group-by <keys>', 'Comma-separated label keys to group series by, e.g. http.method,http.route')
    .option('--limit <n>', 'Max buckets to return (default: 500, server cap: 1000)')
    .option('--json', 'Output in JSON format')
    .action(metricsQuery)

  metrics
    .command('label-keys')
    .description('List the label keys observed on a metric — powers filter/group-by discovery')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--metric-name <name>', 'Metric to inspect')
    .option('--start-time <iso>', 'Window start (RFC 3339); defaults to 24h before end')
    .option('--end-time <iso>', 'Window end (RFC 3339); defaults to now')
    .option('--json', 'Output in JSON format')
    .action(metricsLabelKeys)

  metrics
    .command('label-values')
    .description('List the distinct values seen for a label key on a metric')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--metric-name <name>', 'Metric to inspect')
    .requiredOption('--label-key <key>', 'Label key whose values to list')
    .option('--start-time <iso>', 'Window start (RFC 3339); defaults to 24h before end')
    .option('--end-time <iso>', 'Window end (RFC 3339); defaults to now')
    .option('--json', 'Output in JSON format')
    .action(metricsLabelValues)

  metrics.addHelpText(
    'after',
    `
Discovery workflow — find a metric, then narrow it down:
  $ temps metrics names -p my-app
  $ temps metrics label-keys -p my-app --metric-name go.goroutine.count
  $ temps metrics label-values -p my-app --metric-name go.goroutine.count --label-key http.method

Examples:
  $ temps metrics query -p my-app --metric-name go.goroutine.count --period 24h
  $ temps metrics query -p my-app --metric-name http.server.duration --aggregation p95 --period 7d
  $ temps metrics query -p my-app --metric-name http.server.duration \\
      --label-filters http.method=GET,http.status_code=200 --group-by http.route
  $ temps metrics query -p my-app --metric-name go.goroutine.count \\
      --start-time 2026-08-12T10:01:12.053Z --end-time 2026-08-13T10:01:12.053Z \\
      --bucket-interval "15 minutes" --aggregation avg --limit 500 --json`
  )
}
