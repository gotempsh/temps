import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { getProjectBySlug, listMetricNames } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { header, list, newline, json as jsonOut, colors, info } from '../../ui/output.js'

interface NamesOptions {
  project?: string
  json?: boolean
}

export async function metricsNames(options: NamesOptions): Promise<void> {
  await requireAuth()
  await setupClient()

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

  const names = await withSpinner('Fetching metric names...', async () => {
    const { data, error } = await listMetricNames({
      client,
      path: { project_id: projectData.id },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data?.names ?? []
  })

  if (options.json) {
    jsonOut({ project: resolved.slug, names })
    return
  }

  header(`Metric Names — ${resolved.slug}`)
  newline()
  if (names.length === 0) {
    console.log(colors.muted('  No metrics have been ingested for this project yet.'))
    newline()
    return
  }
  list(names)
  newline()
  console.log(
    colors.muted(
      `  ${names.length} metric(s). Query one with: temps metrics query -p ${resolved.slug} --metric-name <name>`
    )
  )
  newline()
}
