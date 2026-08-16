import chalk from 'chalk'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, getPropertyBreakdown } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, json as jsonOut, colors, info } from '../../ui/output.js'
import { parsePeriod } from './period.js'

export const DIMENSION_MAP: Record<string, { groupBy: string; label: string }> = {
  pages: { groupBy: 'page_path', label: 'Top Pages' },
  referrers: { groupBy: 'referrer_hostname', label: 'Top Referrers' },
  browsers: { groupBy: 'browser', label: 'Browsers' },
  os: { groupBy: 'operating_system', label: 'Operating Systems' },
  devices: { groupBy: 'device_type', label: 'Devices' },
  countries: { groupBy: 'country', label: 'Countries' },
  regions: { groupBy: 'region', label: 'Regions' },
  cities: { groupBy: 'city', label: 'Cities' },
  channels: { groupBy: 'channel', label: 'Channels' },
  events: { groupBy: 'event_name', label: 'Events' },
  languages: { groupBy: 'language', label: 'Languages' },
  utm_source: { groupBy: 'utm_source', label: 'UTM Sources' },
  utm_medium: { groupBy: 'utm_medium', label: 'UTM Mediums' },
  utm_campaign: { groupBy: 'utm_campaign', label: 'UTM Campaigns' },
}

interface BreakdownOptions {
  project?: string
  period?: string
  limit?: string
  json?: boolean
}


function formatNumber(n: number): string {
  return n.toLocaleString('en-US')
}

/** Look up a `temps analytics top <dimension>` argument, rejecting anything not in DIMENSION_MAP. */
export function resolveDimension(dimension: string): { groupBy: string; label: string } {
  const dim = DIMENSION_MAP[dimension]
  if (!dim) {
    const valid = Object.keys(DIMENSION_MAP).join(', ')
    throw new Error(`Unknown dimension "${dimension}". Available: ${valid}`)
  }
  return dim
}

export async function breakdown(dimension: string, options: BreakdownOptions): Promise<void> {
  const dim = resolveDimension(dimension)

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

  const data = await withSpinner(`Fetching ${dim.label.toLowerCase()}...`, async () => {
    const { data, error } = await getPropertyBreakdown({
      client,
      path: { project_id: projectId },
      query: {
        start_date: startDate,
        end_date: endDate,
        group_by: dim.groupBy,
        limit,
      },
    })

    if (error) throw new Error(getErrorMessage(error))
    return data
  })

  if (options.json) {
    jsonOut({
      project: resolved.slug,
      period,
      dimension,
      ...data,
    })
    return
  }

  const items = (data as any)?.items ?? []
  const total = (data as any)?.total ?? 0

  const line = chalk.cyan('━'.repeat(64))

  newline()
  console.log(line)
  console.log(
    `   ${chalk.bold.white(dim.label)} ${chalk.gray(`— ${resolved.slug} (${label})`)}`
  )
  console.log(line)
  newline()

  if (items.length === 0) {
    console.log(`  ${chalk.gray('No data for this period.')}`)
    newline()
    console.log(line)
    newline()
    return
  }

  // Render bar chart
  const maxCount = Math.max(...items.map((i: any) => i.count), 1)
  const maxBarWidth = 30

  console.log(
    `  ${chalk.gray('#'.padEnd(4))}${chalk.gray('Value'.padEnd(32))}${chalk.gray('Count'.padStart(10))}${chalk.gray('%'.padStart(8))}  ${chalk.gray('Distribution')}`
  )
  console.log(`  ${chalk.gray('─'.repeat(60))}`)

  items.forEach((item: any, i: number) => {
    const value = item.value || '(unknown)'
    const display = value.length > 30 ? value.slice(0, 27) + '...' : value
    const barWidth = Math.max(1, Math.round((item.count / maxCount) * maxBarWidth))
    const bar = chalk.cyan('█'.repeat(barWidth))

    console.log(
      `  ${chalk.gray(String(i + 1).padEnd(4))}${chalk.white(display.padEnd(32))}${formatNumber(item.count).padStart(10)}${(item.percentage.toFixed(1) + '%').padStart(8)}  ${bar}`
    )
  })

  newline()
  console.log(`  ${chalk.gray('Total:')} ${formatNumber(total)}`)
  newline()
  console.log(line)
  newline()
}
