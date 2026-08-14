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
  apiSummary,
} from './api-traffic.js'

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
      '24h'
    )
    .option('--json', 'Output in JSON format')
    .action(overview)

  analytics
    .command('top <dimension>')
    .description(
      'Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h'
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
      '7d'
    )
    .option('--json', 'Output in JSON format')
    .action(funnelsOverview)

  analytics
    .command('ai-agents')
    .description(
      'Show AI crawler / provider breakdown (web /analytics/ai-agents)'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d)',
      '24h'
    )
    .option('--limit <n>', 'Number of rows to fetch (default: 20, max: 100)')
    .option(
      '--group-by <mode>',
      'Group rows by "agent" (default) or "provider"',
      'agent'
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
      '24h'
    )
    .option('--limit <n>', 'Number of pages to fetch (default: 20, max: 100)')
    .option('--path <path>', 'Restrict to one URL path (returns just that row)')
    .option(
      '--with-agents',
      'Also fetch and render the per-agent split for each page (slower)'
    )
    .option('--json', 'Output in JSON format')
    .action(aiPages)

  analytics
    .command('ai-page <path>')
    .description(
      'Show which agents/providers crawled a single page (e.g. /docs)'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d)',
      '24h'
    )
    .option('--limit <n>', 'Number of rows to fetch (default: 50, max: 100)')
    .option(
      '--group-by <mode>',
      'Group rows by "agent" (default) or "provider"',
      'agent'
    )
    .option('--json', 'Output in JSON format')
    .action(aiPage)

  analytics
    .command('api-overview')
    .description(
      'Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h'
    )
    .option('--json', 'Output in JSON format')
    .action(apiOverview)

  analytics
    .command('api-routes')
    .description(
      'Show top API routes by request count from /api-analytics/routes'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h'
    )
    .option('--limit <n>', 'Number of routes to return (default: 20, max: 100)')
    .option('--offset <n>', 'Number of ranked routes to skip (default: 0)')
    .option('--json', 'Output in JSON format')
    .action(apiRoutes)

  analytics
    .command('api-callers')
    .description(
      'Show top API callers by client IP from /api-analytics/callers'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h'
    )
    .option(
      '--limit <n>',
      'Number of callers to return (default: 20, max: 100)'
    )
    .option('--offset <n>', 'Number of ranked callers to skip (default: 0)')
    .option('--json', 'Output in JSON format')
    .action(apiCallers)

  analytics
    .command('api-summary')
    .description(
      'Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)'
    )
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--environment-id <id>', 'Restrict traffic to one environment ID')
    .option(
      '--period <period>',
      'Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m)',
      '24h'
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
  $ temps analytics api-summary  -p my-app --period 24h`
  )
}
