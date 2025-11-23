import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, keyValue, formatDate } from '../../ui/output.js'
import { detailsTable, statusBadge } from '../../ui/table.js'
import { getClient } from '../../api/client.js'

interface Deployment {
  id: number
  project_name?: string
  environment?: string
  status: string
  branch?: string
  commit_sha?: string
  commit_message?: string
  created_at: string
  started_at?: string
  finished_at?: string
  error_message?: string
}

interface StatusOptions {
  json?: boolean
}

export async function status(deploymentId: string, options: StatusOptions): Promise<void> {
  await requireAuth()

  const client = getClient()

  const deployment = await withSpinner('Fetching deployment status...', async () => {
    const response = await client.get('/api/deployments/{id}' as never, {
      params: { path: { id: deploymentId } },
    })

    if (response.error || !response.data) {
      throw new Error(`Deployment #${deploymentId} not found`)
    }

    return response.data as Deployment
  })

  if (options.json) {
    json(deployment)
    return
  }

  newline()
  header(`${icons.rocket} Deployment #${deployment.id}`)

  const duration =
    deployment.started_at && deployment.finished_at
      ? calculateDuration(deployment.started_at, deployment.finished_at)
      : deployment.started_at
        ? 'In progress...'
        : 'Not started'

  detailsTable({
    Project: deployment.project_name ?? 'Unknown',
    Environment: deployment.environment ?? 'production',
    Status: statusBadge(deployment.status),
    Branch: deployment.branch ?? '-',
    Commit: deployment.commit_sha?.substring(0, 7) ?? '-',
    Message: deployment.commit_message ?? '-',
    Duration: duration,
    Created: formatDate(deployment.created_at),
    Started: deployment.started_at ? formatDate(deployment.started_at) : '-',
    Finished: deployment.finished_at ? formatDate(deployment.finished_at) : '-',
  })

  if (deployment.error_message) {
    newline()
    console.log(colors.error(`Error: ${deployment.error_message}`))
  }

  newline()
}

function calculateDuration(start: string, end: string): string {
  const startDate = new Date(start)
  const endDate = new Date(end)
  const diffMs = endDate.getTime() - startDate.getTime()

  const seconds = Math.floor(diffMs / 1000)
  const minutes = Math.floor(seconds / 60)

  if (minutes > 0) {
    return `${minutes}m ${seconds % 60}s`
  }
  return `${seconds}s`
}
