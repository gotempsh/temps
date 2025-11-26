import { requireAuth, config } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProjectDeployments, getProjectBySlug } from '../../api/sdk.gen.js'
import type { DeploymentResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, formatRelativeTime, truncate } from '../../ui/output.js'

interface ListOptions {
  environment?: string
  limit: string
  json?: boolean
}

export async function list(project: string | undefined, options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectName = project ?? config.get('defaultProject')

  if (!projectName) {
    throw new Error('No project specified. Use: temps deployments list <project>')
  }

  const deployments = await withSpinner('Fetching deployments...', async () => {
    // Get project ID from slug
    const { data: projectData, error: projectError } = await getProjectBySlug({
      client,
      path: { slug: projectName },
    })

    if (projectError || !projectData) {
      throw new Error(`Project "${projectName}" not found`)
    }

    const { data, error } = await getProjectDeployments({
      client,
      path: { id: projectData.id },
    })

    if (error || !data) {
      throw new Error(getErrorMessage(error))
    }

    let result = data.deployments

    // Filter by environment if specified
    if (options.environment) {
      result = result.filter(d => d.environment?.name === options.environment)
    }

    // Apply limit
    const limit = parseInt(options.limit, 10)
    return result.slice(0, limit)
  })

  if (options.json) {
    json(deployments)
    return
  }

  newline()
  header(`${icons.rocket} Deployments for ${projectName} (${deployments.length})`)

  const columns: TableColumn<DeploymentResponse>[] = [
    { header: 'ID', key: 'id', width: 8 },
    { header: 'Environment', accessor: (d) => d.environment?.name ?? 'unknown' },
    {
      header: 'Status',
      accessor: (d) => d.status,
      color: (v) => statusBadge(v),
    },
    { header: 'Branch', accessor: (d) => d.branch ?? '-' },
    {
      header: 'Commit',
      accessor: (d) => (d.commit_hash ? truncate(d.commit_hash, 7) : '-'),
      color: (v) => colors.muted(v),
    },
    {
      header: 'Created',
      accessor: (d) => formatRelativeTime(new Date(d.created_at * 1000).toISOString()),
      color: (v) => colors.muted(v),
    },
  ]

  printTable(deployments, columns, { style: 'minimal' })
  newline()
}
