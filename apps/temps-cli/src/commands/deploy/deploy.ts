import { requireAuth, config } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getProjectBySlug,
  getEnvironments,
  triggerProjectPipeline,
  getLastDeployment,
} from '../../api/sdk.gen.js'
import type { EnvironmentResponse, DeploymentResponse } from '../../api/types.gen.js'
import { promptSelect, promptText, promptConfirm } from '../../ui/prompts.js'
import { startSpinner, succeedSpinner, failSpinner, updateSpinner } from '../../ui/spinner.js'
import { success, info, warning, newline, icons, colors, header, keyValue, box } from '../../ui/output.js'

interface DeployOptions {
  project?: string
  environment?: string
  environmentId?: string
  branch?: string
  wait?: boolean
  yes?: boolean
}

export async function deploy(options: DeployOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // Get project name
  const projectName = options.project ?? config.get('defaultProject')

  if (!projectName) {
    warning('No project specified')
    info('Use: temps deploy --project <project> or set a default with temps configure')
    return
  }

  // Fetch project details
  startSpinner('Fetching project details...')

  let projectData: { id: number; name: string }
  let environments: EnvironmentResponse[] = []

  try {
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectName },
    })

    if (error || !data) {
      failSpinner(`Project "${projectName}" not found`)
      return
    }

    projectData = data
    succeedSpinner(`Found project: ${projectData.name}`)

    // Fetch environments
    const { data: envData } = await getEnvironments({
      client,
      path: { project_id: projectData.id },
    })
    environments = envData ?? []
  } catch (err) {
    failSpinner('Failed to fetch project')
    throw err
  }

  // Get environment
  let environmentId: number | undefined
  let environmentName = options.environment || 'production'

  if (environments.length > 0) {
    // If environment ID is specified directly, use it
    if (options.environmentId) {
      environmentId = parseInt(options.environmentId, 10)
      const env = environments.find(e => e.id === environmentId)
      if (env) {
        environmentName = env.name
      }
    } else if (options.environment) {
      // Find by name
      const env = environments.find(e => e.name === options.environment)
      if (env) {
        environmentId = env.id
        environmentName = env.name
      }
    } else if (!options.yes) {
      // Interactive: prompt for environment selection
      const selectedEnv = await promptSelect({
        message: 'Select environment',
        choices: environments.map((env) => ({
          name: env.name,
          value: String(env.id),
          description: env.is_preview ? 'Preview environment' : undefined,
        })),
        default: String(environments.find(e => e.name === 'production')?.id ?? environments[0]?.id ?? ''),
      })
      environmentId = parseInt(selectedEnv, 10)
      environmentName = environments.find(e => e.id === environmentId)?.name ?? 'production'
    } else {
      // Non-interactive: use production or first environment
      const prodEnv = environments.find(e => e.name === 'production')
      if (prodEnv) {
        environmentId = prodEnv.id
        environmentName = prodEnv.name
      } else if (environments[0]) {
        environmentId = environments[0].id
        environmentName = environments[0].name
      }
    }
  }

  // Get branch - use flag value, or prompt if interactive mode
  let branch = options.branch
  if (!branch) {
    if (options.yes) {
      branch = 'main' // Default for automation
    } else {
      branch = await promptText({
        message: 'Branch to deploy',
        default: 'main',
      })
    }
  }

  // Confirm deployment (skip if --yes flag)
  newline()
  box(
    `Project: ${colors.bold(projectName)}\n` +
      `Environment: ${colors.bold(environmentName)}\n` +
      `Branch: ${colors.bold(branch)}`,
    `${icons.rocket} Deployment Preview`
  )
  newline()

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: 'Start deployment?',
      default: true,
    })

    if (!confirmed) {
      info('Deployment cancelled')
      return
    }
  }

  // Start deployment
  startSpinner('Starting deployment...')

  try {
    const { data, error } = await triggerProjectPipeline({
      client,
      path: { id: projectData.id },
      body: {
        branch,
        environment_id: environmentId,
      },
    })

    if (error || !data) {
      failSpinner('Failed to start deployment')
      return
    }

    succeedSpinner(`Deployment started`)
    info(data.message ?? 'Pipeline triggered successfully')

    if (options.wait !== false) {
      await waitForDeployment(projectData.id, environmentId)
    } else {
      newline()
      info('Deployment running in background')
      info(`Check status with: temps deployments list --project ${projectName}`)
    }
  } catch (err) {
    failSpinner('Deployment failed')
    throw err
  }
}

async function waitForDeployment(projectId: number, environmentId?: number): Promise<void> {
  const statusMessages: Record<string, string> = {
    pending: 'Waiting in queue...',
    building: 'Building application...',
    deploying: 'Deploying to servers...',
    running: 'Starting containers...',
  }

  startSpinner('Waiting for deployment...')

  let lastStatus = ''
  let attempts = 0
  const maxAttempts = 180 // 6 minutes with 2s intervals

  while (attempts < maxAttempts) {
    attempts++

    const { data: deployment, error } = await getLastDeployment({
      client,
      path: { id: projectId },
    })

    if (error || !deployment) {
      await new Promise((resolve) => setTimeout(resolve, 2000))
      continue
    }

    // Check if this is the right deployment (for the selected environment)
    if (environmentId && deployment.environment_id !== environmentId) {
      await new Promise((resolve) => setTimeout(resolve, 2000))
      continue
    }

    if (deployment.status !== lastStatus) {
      lastStatus = deployment.status
      updateSpinner(statusMessages[deployment.status] ?? `Status: ${deployment.status}`)
    }

    if (deployment.status === 'success' || deployment.status === 'completed' || deployment.status === 'deployed') {
      succeedSpinner(`${icons.rocket} Deployment successful!`)
      newline()
      header(`${icons.check} Deployment Complete`)
      keyValue('Deployment ID', deployment.id)
      keyValue('Commit', deployment.commit_hash?.substring(0, 7) ?? '-')
      if (deployment.url) {
        keyValue('URL', colors.primary(deployment.url))
      }
      newline()
      return
    }

    if (deployment.status === 'failed' || deployment.status === 'error' || deployment.status === 'cancelled') {
      failSpinner('Deployment failed')
      newline()
      if (deployment.cancelled_reason) {
        info(`Reason: ${deployment.cancelled_reason}`)
      }
      info(`View logs with: temps logs ${projectId}`)
      return
    }

    // Wait before checking again
    await new Promise((resolve) => setTimeout(resolve, 2000))
  }

  failSpinner('Deployment timed out')
  info('Deployment is still running. Check status with: temps deployments list')
}
