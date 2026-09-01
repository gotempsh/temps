// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getAiAgentBreakdown,
  getAiPageBreakdown,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, colors, info } from '../../ui/output.js'
import { parsePeriod } from './period.js'

interface AiPageOptions {
  project?: string
  period?: string
  limit?: string
  groupBy?: 'agent' | 'provider'
  json?: boolean
}

function formatNumber(n: number): string {
  return n.toLocaleString('en-US')
}

export interface AiAgentRow {
  label: string
  provider: string
  agent: string
  purpose: string
  requestCount: number
  uniqueIps: number
  percentage: number
}

export interface AiAgentBreakdownItem {
  provider: string
  agent: string
  purpose?: string
  request_count: number
  unique_ips: number
}

/**
 * Shapes the agents-on-this-path breakdown into display rows, optionally
 * aggregating by provider. Mirrors ai-agents.ts's buildAiAgentRows for the
 * single-page drill-down view.
 */
export function buildAiAgentRows(
  items: AiAgentBreakdownItem[],
  groupBy: 'agent' | 'provider'
): AiAgentRow[] {
  const total = items.reduce((sum, r) => sum + (r.request_count ?? 0), 0)

  if (groupBy === 'provider') {
    const byProvider = new Map<string, { count: number; uniqueIps: number; sample: string }>()
    for (const row of items) {
      const prev = byProvider.get(row.provider)
      if (prev) {
        prev.count += row.request_count
        prev.uniqueIps += row.unique_ips
      } else {
        byProvider.set(row.provider, {
          count: row.request_count,
          uniqueIps: row.unique_ips,
          sample: row.agent,
        })
      }
    }
    return Array.from(byProvider.entries())
      .map(([provider, v]) => ({
        label: provider,
        provider,
        agent: v.sample,
        purpose: '',
        requestCount: v.count,
        uniqueIps: v.uniqueIps,
        percentage: total > 0 ? (v.count / total) * 100 : 0,
      }))
      .sort((a, b) => b.requestCount - a.requestCount)
  }

  return items
    .map((row) => ({
      label: row.agent,
      provider: row.provider,
      agent: row.agent,
      purpose: row.purpose ?? '',
      requestCount: row.request_count,
      uniqueIps: row.unique_ips,
      percentage: total > 0 ? (row.request_count / total) * 100 : 0,
    }))
    .sort((a, b) => b.requestCount - a.requestCount)
}

/**
 * `temps analytics ai-page <path>` — drill into one URL path and show the
 * agents (default) or providers that hit it. This is the CLI equivalent of
 * expanding a row in the web "Pages crawled" tab.
 */
export async function aiPage(
  path: string,
  options: AiPageOptions
): Promise<void> {
  if (!path || !path.startsWith('/')) {
    throw new Error('Path must be a URL path starting with "/", e.g. /docs')
  }

  const groupBy = options.groupBy ?? 'agent'
  if (groupBy !== 'agent' && groupBy !== 'provider') {
    throw new Error(`Invalid --group-by "${groupBy}". Use: agent, provider`)
  }

  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const limit = options.limit ? parseInt(options.limit, 10) : 50
  const { startDate, endDate, label } = parsePeriod(period)

  const resolved = await requireProjectSlug(options.project)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }

  const { data: projectData, error: projectError } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })

  if (projectError || !projectData) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }

  const projectId = projectData.id

  // Fetch the agents-on-this-path and the path summary in parallel so we can
  // report total requests + unique agent count in the header.
  const { agents, pageSummary } = await withSpinner(
    `Fetching agents for ${path}...`,
    async () => {
      const [agentsRes, pageRes] = await Promise.all([
        getAiAgentBreakdown({
          client,
          query: {
            project_id: projectId,
            start_time: startDate,
            end_time: endDate,
            limit,
            path,
          },
        }),
        getAiPageBreakdown({
          client,
          query: {
            project_id: projectId,
            start_time: startDate,
            end_time: endDate,
            path,
            limit: 1,
          },
        }),
      ])

      if (agentsRes.error) throw new Error(getErrorMessage(agentsRes.error))
      if (pageRes.error) throw new Error(getErrorMessage(pageRes.error))

      return {
        agents: (agentsRes.data as any)?.items ?? [],
        pageSummary: (pageRes.data as any)?.items?.[0],
      }
    }
  )

  const totalRequests = agents.reduce(
    (sum: number, r: any) => sum + (r.request_count ?? 0),
    0
  )

  const rows = buildAiAgentRows(agents, groupBy)

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      period,
      path,
      group_by: groupBy,
      total_requests: totalRequests,
      distinct_agents: agents.length,
      page: pageSummary
        ? {
            request_count: pageSummary.request_count,
            agent_count: pageSummary.agent_count,
            last_seen: pageSummary.last_seen,
          }
        : null,
      items: rows,
    })
    return
  }

  const line = chalk.cyan('━'.repeat(64))

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white('AI Agents on')} ${chalk.bold.cyan(path)} ${chalk.gray(`— ${resolved.slug} (${label})`)}`
  )
  console.log(line)
  newline()

  if (rows.length === 0) {
    console.log(`  ${chalk.gray('No AI crawler hits for this path.')}`)
    newline()
    console.log(line)
    newline()
    return
  }

  const maxCount = Math.max(...rows.map((r) => r.requestCount), 1)
  const maxBarWidth = 22
  const labelWidth = groupBy === 'agent' ? 24 : 20

  console.log(
    `  ${chalk.gray('#'.padEnd(4))}` +
      `${chalk.gray(groupBy === 'agent' ? 'Agent'.padEnd(labelWidth) : 'Provider'.padEnd(labelWidth))}` +
      `${groupBy === 'agent' ? chalk.gray('Provider'.padEnd(14)) : ''}` +
      `${chalk.gray('Requests'.padStart(10))}` +
      `${chalk.gray('IPs'.padStart(8))}` +
      `${chalk.gray('%'.padStart(7))}` +
      `  ${chalk.gray('Share')}`
  )
  console.log(`  ${chalk.gray('─'.repeat(72))}`)

  rows.forEach((r, i) => {
    const display =
      r.label.length > labelWidth - 1
        ? r.label.slice(0, labelWidth - 4) + '...'
        : r.label
    const barWidth = Math.max(
      1,
      Math.round((r.requestCount / maxCount) * maxBarWidth)
    )
    const bar = chalk.cyan('█'.repeat(barWidth))
    const providerCol =
      groupBy === 'agent'
        ? chalk.gray(
            (r.provider.length > 12
              ? r.provider.slice(0, 12)
              : r.provider
            ).padEnd(14)
          )
        : ''

    console.log(
      `  ${chalk.gray(String(i + 1).padEnd(4))}` +
        `${chalk.white(display.padEnd(labelWidth))}` +
        `${providerCol}` +
        `${formatNumber(r.requestCount).padStart(10)}` +
        `${formatNumber(r.uniqueIps).padStart(8)}` +
        `${(r.percentage.toFixed(1) + '%').padStart(7)}` +
        `  ${bar}`
    )
  })

  newline()
  console.log(
    `  ${chalk.gray('Total requests:')} ${chalk.bold(formatNumber(totalRequests))}   ` +
      `${chalk.gray(groupBy === 'agent' ? 'Distinct agents:' : 'Distinct providers:')} ${chalk.bold(formatNumber(rows.length))}`
  )
  newline()
  console.log(line)
  newline()
}
