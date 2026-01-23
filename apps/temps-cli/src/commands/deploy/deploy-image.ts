import { requireAuth, config, credentials } from '../../config/store.js'
import { setupClient, client } from '../../lib/api-client.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { getProjectBySlug, getProject, getEnvironments } from '../../api/sdk.gen.js'
import type { EnvironmentResponse } from '../../api/types.gen.js'
import { promptSelect, promptConfirm, promptText } from '../../ui/prompts.js'
import {
  startSpinner,
  succeedSpinner,
  failSpinner,
} from '../../ui/spinner.js'
import {
  success,
  info,
  warning,
  newline,
  icons,
  colors,
  box,
} from '../../ui/output.js'

interface DeployImageOptions {
  image: string
  project?: string
  environment?: string
  environmentId?: string
  wait?: boolean
  yes?: boolean
  metadata?: string
  timeout?: string
}

export async function deployImage(options: DeployImageOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // Get image reference
  let imageRef = options.image

  if (!imageRef && !options.yes) {
    imageRef = await promptText({
      message: 'Docker image reference',
      default: '',
      validate: (value) => {
        if (!value || value.trim() === '') {
          return 'Image reference is required'
        }
        return true
      },
    })
  }

  if (!imageRef) {
    warning('No image specified')
    info('Use: temps deploy image --image <image-ref>')
    return
  }

  // Get project name
  const projectName = options.project ?? config.get('defaultProject')

  if (!projectName) {
    warning('No project specified')
    info(
      'Use: temps deploy image --project <project> or set a default with temps configure'
    )
    return
  }

  // Fetch project details
  startSpinner('Fetching project details...')

  let projectData: { id: number; name: string; slug: string }
  let environments: EnvironmentResponse[] = []

  try {
    // Check if projectName is a numeric ID
    const isNumericId = /^\d+$/.test(projectName)

    if (isNumericId) {
      // Fetch by numeric ID
      const result = await getProject({
        client,
        path: { id: parseInt(projectName, 10) },
      })

      if (result.error || !result.data) {
        failSpinner(`Project with ID "${projectName}" not found`)
        info(`Debug: ${JSON.stringify(result)}`)
        return
      }

      // Handle potential wrapped response
      const responseData = result.data as Record<string, unknown>
      if (responseData.id !== undefined) {
        projectData = result.data as { id: number; name: string; slug: string }
      } else if (responseData.data && typeof responseData.data === 'object') {
        projectData = responseData.data as { id: number; name: string; slug: string }
      } else {
        failSpinner(`Unexpected project response format`)
        info(`Debug: ${JSON.stringify(result.data)}`)
        return
      }
    } else {
      // Fetch by slug
      const { data, error } = await getProjectBySlug({
        client,
        path: { slug: projectName },
      })

      if (error || !data) {
        failSpinner(`Project "${projectName}" not found`)
        return
      }

      projectData = data
    }

    succeedSpinner(`Found project: ${projectData.name || projectData.slug}`)

    // Fetch environments
    const { data: envData } = await getEnvironments({
      client,
      path: { project_id: projectData.id },
    })

    // Handle different response formats - could be array directly or wrapped in object
    if (Array.isArray(envData)) {
      environments = envData
    } else if (envData && typeof envData === 'object') {
      // Try common wrapper properties
      const wrapped = envData as Record<string, unknown>
      if (Array.isArray(wrapped.data)) {
        environments = wrapped.data as EnvironmentResponse[]
      } else if (Array.isArray(wrapped.environments)) {
        environments = wrapped.environments as EnvironmentResponse[]
      }
    }
  } catch (err) {
    failSpinner('Failed to fetch project')
    throw err
  }

  // Get environment
  let environmentId: number | undefined
  let environmentName = options.environment || 'production'

  // If environment ID is specified directly, use it without lookup
  if (options.environmentId) {
    environmentId = parseInt(options.environmentId, 10)
    // Try to find the name from environments list if available
    if (environments.length > 0) {
      const env = environments.find((e) => e.id === environmentId)
      if (env) {
        environmentName = env.name
      }
    } else {
      environmentName = `Environment #${environmentId}`
    }
  } else if (environments.length > 0) {
    if (options.environment) {
      // Find by name
      const env = environments.find((e) => e.name === options.environment)
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
        default: String(
          environments.find((e) => e.name === 'production')?.id ??
            environments[0]?.id ??
            ''
        ),
      })
      environmentId = parseInt(selectedEnv, 10)
      environmentName =
        environments.find((e) => e.id === environmentId)?.name ?? 'production'
    } else {
      // Non-interactive: use production or first environment
      const prodEnv = environments.find((e) => e.name === 'production')
      if (prodEnv) {
        environmentId = prodEnv.id
        environmentName = prodEnv.name
      } else if (environments[0]) {
        environmentId = environments[0].id
        environmentName = environments[0].name
      }
    }
  } else if (!options.environmentId) {
    warning('No environments found for this project')
    info('Create an environment first or specify --environment-id directly')
    return
  }

  // Show deployment preview
  newline()
  box(
    `Project: ${colors.bold(projectName)}\n` +
      `Environment: ${colors.bold(environmentName)}\n` +
      `Image: ${colors.bold(imageRef)}`,
    `${icons.rocket} Docker Image Deployment`
  )
  newline()

  // Confirm deployment (skip if --yes flag)
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

  // Trigger deployment
  startSpinner('Starting deployment...')

  const apiUrl = config.get('apiUrl')
  const apiKey = await credentials.getApiKey()

  try {
    const deployUrl = `${apiUrl}/projects/${projectData.id}/environments/${environmentId}/deploy/image`

    const deployBody: Record<string, unknown> = {
      image_ref: imageRef,
    }

    if (options.metadata) {
      try {
        deployBody.metadata = JSON.parse(options.metadata)
      } catch {
        failSpinner('Invalid metadata JSON')
        return
      }
    }

    const deployResponse = await fetch(deployUrl, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${apiKey}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(deployBody),
    })

    const deployResponseText = await deployResponse.text()

    if (!deployResponse.ok) {
      failSpinner(`Deployment failed: ${deployResponse.status}`)
      info(`URL: ${deployUrl}`)
      warning(`Response: ${deployResponseText}`)
      return
    }

    let deployment: { id: number; slug: string }
    try {
      deployment = JSON.parse(deployResponseText) as { id: number; slug: string }
    } catch (parseErr) {
      failSpinner('Failed to parse deployment response')
      info(`URL: ${deployUrl}`)
      warning(`Response: ${deployResponseText}`)
      return
    }
    succeedSpinner(`Deployment started: ${deployment.slug}`)

    // Wait for completion if requested
    if (options.wait !== false) {
      const result = await watchDeployment({
        projectId: projectData.id,
        deploymentId: deployment.id,
        timeoutSecs: parseInt(options.timeout || '300', 10),
        projectName,
      })

      if (!result.success) {
        // Exit with error code for CI/CD
        process.exitCode = 1
      }
    } else {
      newline()
      info('Deployment running in background')
      info(`Check status with: temps deployments list --project ${projectName}`)
      newline()
      success('Docker image deployment initiated successfully!')
      newline()
    }
  } catch (err) {
    failSpinner('Deployment failed')
    throw err
  }
}
