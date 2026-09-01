// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  getProjectBySlug,
  getAiPageBreakdown,
  getAiAgentBreakdown,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, colors, info } from '../../ui/output.js'
import { parsePeriod } from './period.js'

interface AiPagesOptions {
  project?: string
  period?: string
  limit?: string
  path?: string
  withAgents?: boolean
  json?: boolean
}

function formatNumber(n: number): string {
  return n.toLocaleString('en-US')
}

export async function aiPages(options: AiPagesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const period = options.period ?? '24h'
  const limit = options.limit ? parseInt(options.limit, 10) : 20
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

  const data = await withSpinner('Fetching AI crawled pages...', async () => {
    const { data, error } = await getAiPageBreakdown({
      client,
      query: {
        project_id: projectId,
        start_time: startDate,
        end_time: endDate,
        limit,
        path: options.path,
      },
    })

    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  const items: any[] = (data as any)?.items ?? []

  // When the caller asks for the per-page agent split, fan out one
  // path-scoped breakdown per page. This mirrors the web UI's expanded row.
  let perPageAgents: Record<string, any[]> | undefined
  if (options.withAgents && items.length > 0) {
    perPageAgents = await withSpinner('Fetching per-page agents...', async () => {
      const out: Record<string, any[]> = {}
      const results = await Promise.all(
        items.map(async (p) => {
          const { data: ad, error: aerr } = await getAiAgentBreakdown({
            client,
            query: {
              project_id: projectId,
              start_time: startDate,
              end_time: endDate,
              limit: 50,
              path: p.path,
            },
          })
          if (aerr) throw new Error(getErrorMessage(aerr))
          return [p.path, (ad as any)?.items ?? []] as const
        })
      )
      for (const [path, agents] of results) out[path] = agents
      return out
    })
  }

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      period,
      path: options.path,
      total_pages: items.length,
      items: items.map((p) => ({
        path: p.path,
        request_count: p.request_count,
        agent_count: p.agent_count,
        last_seen: p.last_seen,
        agents: perPageAgents?.[p.path],
      })),
    })
    return
  }

  const line = chalk.cyan('━'.repeat(64))
  const title = options.path
    ? `AI Crawls on ${options.path}`
    : 'Pages Crawled by AI Agents'

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white(title)} ${chalk.gray(`— ${resolved.slug} (${label})`)}`
  )
  console.log(line)
  newline()

  if (items.length === 0) {
    console.log(`  ${chalk.gray('No AI crawler page hits in this period.')}`)
    newline()
    console.log(line)
    newline()
    return
  }

  const maxCount = Math.max(...items.map((p) => p.request_count), 1)
  const maxBarWidth = 22

  console.log(
    `  ${chalk.gray('#'.padEnd(4))}` +
      `${chalk.gray('Path'.padEnd(40))}` +
      `${chalk.gray('Agents'.padStart(8))}` +
      `${chalk.gray('Requests'.padStart(10))}` +
      `  ${chalk.gray('Share')}`
  )
  console.log(`  ${chalk.gray('─'.repeat(72))}`)

  items.forEach((p, i) => {
    const display = p.path.length > 38 ? p.path.slice(0, 35) + '...' : p.path
    const barWidth = Math.max(1, Math.round((p.request_count / maxCount) * maxBarWidth))
    const bar = chalk.cyan('█'.repeat(barWidth))
    console.log(
      `  ${chalk.gray(String(i + 1).padEnd(4))}` +
        `${chalk.white(display.padEnd(40))}` +
        `${formatNumber(p.agent_count).padStart(8)}` +
        `${formatNumber(p.request_count).padStart(10)}` +
        `  ${bar}`
    )

    if (perPageAgents) {
      const agents = perPageAgents[p.path] ?? []
      if (agents.length === 0) {
        console.log(`      ${chalk.gray('└─ no agent detail')}`)
      } else {
        const aMax = Math.max(...agents.map((a) => a.request_count), 1)
        agents.slice(0, 8).forEach((a, j) => {
          const isLast = j === Math.min(agents.length, 8) - 1
          const branch = isLast ? '└─' : '├─'
          const aw = Math.max(1, Math.round((a.request_count / aMax) * 16))
          const ab = chalk.gray('▌'.repeat(aw))
          const aLabel = `${a.agent} (${a.provider})`
          const display = aLabel.length > 32 ? aLabel.slice(0, 29) + '...' : aLabel
          console.log(
            `      ${chalk.gray(branch)} ${chalk.white(display.padEnd(34))}` +
              `${formatNumber(a.request_count).padStart(8)}  ${ab}`
          )
        })
        if (agents.length > 8) {
          console.log(`      ${chalk.gray(`   + ${agents.length - 8} more`)}`)
        }
      }
    }
  })

  newline()
  console.log(
    `  ${chalk.gray('Distinct pages:')} ${chalk.bold(formatNumber(items.length))}`
  )
  newline()
  console.log(line)
  newline()
}
