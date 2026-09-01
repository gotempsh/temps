// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { overview } from './overview.js'
import { breakdown } from './breakdown.js'
import { funnelsOverview } from './funnels.js'
import { aiAgents } from './ai-agents.js'
import { aiPages } from './ai-pages.js'
import { aiPage } from './ai-page.js'
import {
  apiOverview,
  apiRoutes,
  apiCallers,
  apiIp,
  apiPath,
  apiQuery,
  apiSummary,
} from './api-traffic.js'
import { performanceInsights } from './performance.js'

function collect(value: string, previous: string[]): string[] {
  return [...previous, value]
}

export function registerAnalyticsCommands(program: Command): void {
  const analytics = program
    .command('analytics')
    .alias('stats')
    .description('View project analytics')

  analytics
    .command('overview')
    .alias('o')
    .description('Show analytics dashboard overview')
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h',
    )
    .option('--json', 'Output in JSON format')
    .action(overview)

  analytics
    .command('top <dimension>')
    .description(
      'Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h',
    )
    .option('--limit <n>', 'Number of results (default: 20, max: 100)')
    .option('--json', 'Output in JSON format')
    .action(breakdown)

  analytics
    .command('funnels')
    .description('Show funnel conversion metrics for all funnels')
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '7d',
    )
    .option('--json', 'Output in JSON format')
    .action(funnelsOverview)

  analytics
    .command('performance')
    .alias('speed')
    .description('Show real-user Web Vitals and optional dimension breakdowns')
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (ignored with --start-date/--end-date)',
      '7d',
    )
    .option(
      '--start-date <date>',
      'Explicit window start (RFC 3339; requires --end-date)',
    )
    .option(
      '--end-date <date>',
      'Explicit window end (RFC 3339; requires --start-date)',
    )
    .option('--environment-id <id>', 'Restrict samples to one environment ID')
    .option('--deployment-id <id>', 'Restrict samples to one deployment ID')
    .option('--device <device>', 'Device filter: desktop or mobile')
    .option('--include-bots', 'Include crawler and datacenter bot samples')
    .option(
      '--group-by <dimension>',
      'Break down by path, country, region, city, device_type, browser, or operating_system',
    )
    .option('--path <path>', 'Restrict samples to one page pathname')
    .option('--country <country>', 'Restrict samples to one country')
    .option('--region <region>', 'Restrict samples to one region')
    .option('--city <city>', 'Restrict samples to one city')
    .option('--browser <browser>', 'Restrict samples to one browser')
    .option(
      '--os <operating-system>',
      'Restrict samples to one operating system',
    )
    .option('--json', 'Output in JSON format')
    .action(performanceInsights)

  analytics
    .command('ai-agents')
    .description(
      'Show AI crawler / provider breakdown (web /analytics/ai-agents)',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d)',
      '24h',
    )
    .option('--limit <n>', 'Number of rows to fetch (default: 20, max: 100)')
    .option(
      '--group-by <mode>',
      'Group rows by "agent" (default) or "provider"',
      'agent',
    )
    .option('--path <path>', 'Restrict to one URL path (e.g. /docs)')
    .option('--json', 'Output in JSON format')
    .action(aiAgents)

  analytics
    .command('ai-pages')
    .description('Show pages crawled by AI agents, with distinct-agent counts')
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d)',
      '24h',
    )
    .option('--limit <n>', 'Number of pages to fetch (default: 20, max: 100)')
    .option('--path <path>', 'Restrict to one URL path (returns just that row)')
    .option(
      '--with-agents',
      'Also fetch and render the per-agent split for each page (slower)',
    )
    .option('--json', 'Output in JSON format')
    .action(aiPages)

  analytics
    .command('ai-page <path>')
    .description(
      'Show which agents/providers crawled a single page (e.g. /docs)',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d)',
      '24h',
    )
    .option('--limit <n>', 'Number of rows to fetch (default: 50, max: 100)')
    .option(
      '--group-by <mode>',
      'Group rows by "agent" (default) or "provider"',
      'agent',
    )
    .option('--json', 'Output in JSON format')
    .action(aiPage)

  analytics
    .command('api-overview')
    .description(
      'Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h',
    )
    .option('--json', 'Output in JSON format')
    .action(apiOverview)

  analytics
    .command('api-routes')
    .description(
      'Show top API routes by request count from /api-analytics/routes',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h',
    )
    .option('--limit <n>', 'Number of routes to return (default: 20, max: 100)')
    .option('--offset <n>', 'Number of ranked routes to skip (default: 0)')
    .option(
      '--sort-by <metric>',
      'Sort by requests, latency_avg, or error_rate',
      'requests',
    )
    .option('--order <direction>', 'Sort direction: asc or desc', 'desc')
    .option('--include-synthetic', 'Include Temps status-monitor checks')
    .option('--json', 'Output in JSON format')
    .action(apiRoutes)

  analytics
    .command('api-callers')
    .description(
      'Show top API callers by client IP from /api-analytics/callers',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h',
    )
    .option(
      '--limit <n>',
      'Number of callers to return (default: 20, max: 100)',
    )
    .option('--offset <n>', 'Number of ranked callers to skip (default: 0)')
    .option('--sort-by <metric>', 'Sort by requests or error_rate', 'requests')
    .option('--order <direction>', 'Sort direction: asc or desc', 'desc')
    .option('--include-synthetic', 'Include Temps status-monitor checks')
    .option('--json', 'Output in JSON format')
    .action(apiCallers)

  analytics
    .command('api-ip <ip>')
    .description(
      'Show routes called by one client IP with latency and error analytics',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option('--period <period>', 'Time period (e.g. 1h, 24h, 7d, 30d)', '24h')
    .option('--limit <n>', 'Rows per page (default: 20, max: 100)')
    .option('--page <n>', 'Page number', '1')
    .option(
      '--sort-by <metric>',
      'Sort by requests, latency_avg, or error_rate',
      'requests',
    )
    .option('--order <direction>', 'Sort direction: asc or desc', 'desc')
    .option('--include-synthetic', 'Include Temps status-monitor checks')
    .option('--json', 'Output in JSON format')
    .action(apiIp)

  analytics
    .command('api-path <path>')
    .description(
      'Show client IPs calling one path with latency and error analytics',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option('--period <period>', 'Time period (e.g. 1h, 24h, 7d, 30d)', '24h')
    .option('--limit <n>', 'Rows per page (default: 20, max: 100)')
    .option('--page <n>', 'Page number', '1')
    .option('--sort-by <metric>', 'Sort by requests or error_rate', 'requests')
    .option('--order <direction>', 'Sort direction: asc or desc', 'desc')
    .option('--include-synthetic', 'Include Temps status-monitor checks')
    .option('--json', 'Output in JSON format')
    .action(apiPath)

  analytics
    .command('api-query')
    .description('Run a typed multi-dimensional API traffic aggregation')
    .option(
      '--group-by <dimensions>',
      'Comma-separated dimensions (omit for one overall rollup)',
    )
    .requiredOption(
      '--metrics <metrics>',
      'Comma-separated metrics (e.g. requests,error_rate,latency_p95)',
    )
    .option(
      '--filter <dimension:operator:value>',
      'Repeatable filter (operators: eq, not_eq, contains, starts_with, in)',
      collect,
      [],
    )
    .option('--sort-by <field>', 'Requested dimension or metric to sort by')
    .option('--order <direction>', 'Sort direction: asc or desc', 'desc')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option('--period <period>', 'Time period (e.g. 1h, 24h, 7d, 30d)', '24h')
    .option('--page <n>', 'Page number', '1')
    .option('--limit <n>', 'Rows per page (default: 20, max: 100)')
    .option('--include-synthetic', 'Include Temps status-monitor checks')
    .option('--json', 'Output in JSON format')
    .action(apiQuery)

  analytics
    .command('api-summary')
    .description(
      'Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)',
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h',
    )
    .option('--json', 'Output in JSON format')
    .action(apiSummary)

  // Default: no subcommand shows help with available commands
  analytics.addHelpText(
    'after',
    `
Examples:
  $ temps analytics                              Show overview (last 24h)
  $ temps analytics overview -p my-app --period 7d
  $ temps analytics funnels --period 7d           Show funnel metrics
  $ temps analytics top pages -p my-app --period 30d
  $ temps analytics top referrers --period 1h
  $ temps analytics top browsers --period 48h --json
  $ temps analytics top countries --period 3m --limit 50

  Performance Insights (real-user Web Vitals):
  $ temps analytics performance -p my-app --period 7d --device desktop
  $ temps analytics speed -p my-app --period 7d --device mobile
  $ temps analytics performance -p my-app --group-by path --device mobile
  $ temps analytics performance -p my-app --start-date 2026-08-01T00:00:00Z --end-date 2026-08-08T00:00:00Z --json

  AI agents (mirrors /analytics/ai-agents):
  $ temps analytics ai-agents -p my-app --period 24h
  $ temps analytics ai-agents -p my-app --group-by provider --period 7d
  $ temps analytics ai-agents -p my-app --path /docs --json
  $ temps analytics ai-pages   -p my-app --period 24h
  $ temps analytics ai-pages   -p my-app --period 7d --with-agents --limit 10
  $ temps analytics ai-pages   -p my-app --path /pricing --json
  $ temps analytics ai-page /docs -p my-app --period 24h
  $ temps analytics ai-page /pricing -p my-app --group-by provider

  API traffic (mirrors /api-analytics/*, built on proxy_logs):
  $ temps analytics api-overview -p my-app --period 24h
  $ temps analytics api-routes   -p my-app --period 7d --limit 50
  $ temps analytics api-callers  -p my-app --period 24h --json
  $ temps analytics api-ip 203.0.113.10 -p my-app --period 7d
  $ temps analytics api-path /api/health -p my-app --sort-by error_rate
  $ temps analytics api-query -p my-app --group-by client_ip,path --metrics requests,error_rate,latency_p95 --filter method:eq:GET --json
  $ temps analytics api-summary  -p my-app --period 24h`,
  )
}
