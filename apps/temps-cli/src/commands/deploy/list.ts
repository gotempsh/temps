import { requireAuth, config } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { newline, header, icons, json, colors, formatRelativeTime, truncate } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface Deployment {
  id: number
  project_name?: string
  environment?: string
  status: string
  branch?: string
  commit_sha?: string
  created_at: string
  finished_at?: string
}

interface ListOptions {
  environment?: string
  limit: string
  json?: boolean
}

export async function list(project: string | undefined, options: ListOptions): Promise<void> {
  await requireAuth()

  const projectName = project ?? config.get('defaultProject')
  const client = getClient()

  const deployments = await withSpinner('Fetching deployments...', async () => {
    const endpoint = projectName
      ? '/api/projects/{project}/deployments'
      : '/api/deployments'

    const response = await client.get(endpoint as '/api/deployments', {
      params: {
        path: projectName ? { project: projectName } : undefined,
        query: {
          environment: options.environment,
          limit: parseInt(options.limit, 10),
        },
      } as never,
    })

    if (response.error) {
      throw new Error('Failed to fetch deployments')
    }

    return (response.data ?? []) as Deployment[]
  })

  if (options.json) {
    json(deployments)
    return
  }

  newline()
  const title = projectName
    ? `${icons.rocket} Deployments for ${projectName} (${deployments.length})`
    : `${icons.rocket} Recent Deployments (${deployments.length})`
  header(title)

  const columns: TableColumn<Deployment>[] = [
    { header: 'ID', key: 'id', width: 8 },
    ...(projectName
      ? []
      : [{ header: 'Project', accessor: (d: Deployment) => d.project_name ?? '-' } as TableColumn<Deployment>]),
    { header: 'Environment', accessor: (d) => d.environment ?? 'production' },
    {
      header: 'Status',
      accessor: (d) => d.status,
      color: (v) => statusBadge(v),
    },
    { header: 'Branch', accessor: (d) => d.branch ?? '-' },
    {
      header: 'Commit',
      accessor: (d) => (d.commit_sha ? truncate(d.commit_sha, 7) : '-'),
      color: (v) => colors.muted(v),
    },
    {
      header: 'Created',
      accessor: (d) => formatRelativeTime(d.created_at),
      color: (v) => colors.muted(v),
    },
  ]

  printTable(deployments, columns, { style: 'minimal' })
  newline()
}
