import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getDeployment, getProjectBySlug } from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, formatDate } from '../../ui/output.js'
import { detailsTable, statusBadge } from '../../ui/table.js'

interface StatusOptions {
  json?: boolean
}

export async function status(deploymentId: string, options: StatusOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  // Parse project and deployment from input
  // Format: project:deployment_id or just deployment_id (requires project context)
  let projectId: number
  let depId: number

  if (deploymentId.includes(':')) {
    const [projectSlug, depIdStr] = deploymentId.split(':')
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectSlug },
    })
    if (error || !data) {
      throw new Error(`Project "${projectSlug}" not found`)
    }
    projectId = data.id
    depId = parseInt(depIdStr, 10)
  } else {
    // Assume deploymentId is in format "projectId:deploymentId" as number:number
    throw new Error('Please specify deployment as project:deployment_id (e.g., my-project:123)')
  }

  const deployment = await withSpinner('Fetching deployment status...', async () => {
    const { data, error } = await getDeployment({
      client,
      path: { project_id: projectId, deployment_id: depId },
    })

    if (error || !data) {
      throw new Error(`Deployment #${depId} not found`)
    }

    return data
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
    Project: `Project ID ${deployment.project_id}`,
    Environment: deployment.environment?.name ?? 'unknown',
    Status: statusBadge(deployment.status),
    Branch: deployment.branch ?? '-',
    Commit: deployment.commit_hash?.substring(0, 7) ?? '-',
    Message: deployment.commit_message ?? '-',
    Duration: duration,
    URL: deployment.url ?? '-',
    Created: formatDate(new Date(deployment.created_at * 1000).toISOString()),
    Started: deployment.started_at ? formatDate(new Date(deployment.started_at * 1000).toISOString()) : '-',
    Finished: deployment.finished_at ? formatDate(new Date(deployment.finished_at * 1000).toISOString()) : '-',
  })

  if (deployment.cancelled_reason) {
    newline()
    console.log(colors.error(`Cancelled: ${deployment.cancelled_reason}`))
  }

  newline()
}

function calculateDuration(startMs: number, endMs: number): string {
  const diffMs = (endMs - startMs) * 1000
  const seconds = Math.floor(diffMs / 1000)
  const minutes = Math.floor(seconds / 60)

  if (minutes > 0) {
    return `${minutes}m ${seconds % 60}s`
  }
  return `${seconds}s`
}
