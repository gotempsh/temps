import { requireAuth, config, credentials } from '../../config/store.js'
import { setupClient, client } from '../../lib/api-client.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { getProjectBySlug, getProject, getEnvironments } from '../../api/sdk.gen.js'
import type { EnvironmentResponse } from '../../api/types.gen.js'
import { promptSelect } from '../../ui/prompts.js'
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
import { existsSync, statSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { basename, extname, resolve } from 'node:path'
import { spawn } from 'node:child_process'

interface DeployStaticOptions {
  path: string
  project?: string
  environment?: string
  environmentId?: string
  wait?: boolean
  yes?: boolean
  metadata?: string
  timeout?: string
}

interface StaticBundleResponse {
  id: number
  filename: string
  size: number
}

export async function deployStatic(options: DeployStaticOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // Resolve and validate path exists
  const resolvedPath = resolve(options.path)
  if (!existsSync(resolvedPath)) {
    warning(`Path does not exist: ${resolvedPath}`)
    return
  }

  const stat = statSync(resolvedPath)
  const isDirectory = stat.isDirectory()
  const isArchive = !isDirectory && isValidArchive(resolvedPath)

  if (!isDirectory && !isArchive) {
    warning('Path must be a directory or a .tar.gz/.tgz/.zip archive')
    return
  }

  // Get project name
  const projectName = options.project ?? config.get('defaultProject')

  if (!projectName) {
    warning('No project specified')
    info(
      'Use: temps deploy static --project <project> or set a default with temps configure'
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
  const fileSize = isDirectory
    ? '(will be archived)'
    : formatFileSize(stat.size)

  newline()
  box(
    `Project: ${colors.bold(projectName)}\n` +
      `Environment: ${colors.bold(environmentName)}\n` +
      `Path: ${colors.bold(resolvedPath)}\n` +
      `Size: ${colors.bold(fileSize)}`,
    `${icons.package} Static Deployment`
  )
  newline()

  // Prepare the file for upload
  startSpinner('Preparing static bundle...')

  let fileData: Buffer
  let filename: string
  let contentType: string

  try {
    if (isDirectory) {
      // Create tar.gz from directory using system tar
      const result = await createTarGzFromDirectory(resolvedPath)
      fileData = result.data
      filename = result.filename
      contentType = 'application/gzip'
    } else {
      // Read existing archive
      fileData = await readFile(resolvedPath)
      filename = basename(resolvedPath)
      contentType = getContentType(resolvedPath)
    }

    succeedSpinner(
      `Prepared bundle: ${filename} (${formatFileSize(fileData.length)}) [${contentType}]`
    )
  } catch (err) {
    failSpinner('Failed to prepare bundle')
    throw err
  }

  // Upload static bundle
  startSpinner('Uploading static bundle...')

  const apiUrl = config.get('apiUrl')
  const apiKey = await credentials.getApiKey()

  try {
    const uploadUrl = `${apiUrl}/projects/${projectData.id}/upload/static`

    const formData = new FormData()
    // Convert Buffer to Uint8Array for Blob compatibility
    const uint8Data = new Uint8Array(fileData.buffer, fileData.byteOffset, fileData.byteLength)
    formData.append(
      'file',
      new Blob([uint8Data], { type: contentType }),
      filename
    )
    formData.append('metadata', options.metadata || '{}')
    // Explicitly send content_type as a form field (more reliable than Blob type)
    formData.append('content_type', contentType)

    const uploadResponse = await fetch(uploadUrl, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${apiKey}`,
      },
      body: formData,
    })

    if (!uploadResponse.ok) {
      const errorText = await uploadResponse.text()
      failSpinner(`Upload failed: ${uploadResponse.status}`)
      info(`URL: ${uploadUrl}`)
      warning(`Response: ${errorText}`)
      return
    }

    const uploadResponseText = await uploadResponse.text()
    let bundle: StaticBundleResponse
    try {
      bundle = JSON.parse(uploadResponseText) as StaticBundleResponse
    } catch (parseErr) {
      failSpinner('Failed to parse upload response')
      info(`URL: ${uploadUrl}`)
      warning(`Response: ${uploadResponseText}`)
      return
    }
    succeedSpinner(`Bundle uploaded: id=${bundle.id}`)

    // Trigger deployment
    startSpinner('Starting deployment...')

    const deployUrl = `${apiUrl}/projects/${projectData.id}/environments/${environmentId}/deploy/static`

    const deployBody = {
      static_bundle_id: bundle.id,
      metadata: options.metadata ? JSON.parse(options.metadata) : undefined,
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
      success('Static deployment initiated successfully!')
      newline()
    }
  } catch (err) {
    failSpinner('Deployment failed')
    throw err
  }
}

function isValidArchive(path: string): boolean {
  const ext = extname(path).toLowerCase()
  const name = basename(path).toLowerCase()
  // Check for .tar.gz extension (two-part extension)
  if (name.endsWith('.tar.gz')) {
    return true
  }
  return ext === '.tgz' || ext === '.zip'
}

function getContentType(path: string): string {
  const name = basename(path).toLowerCase()
  if (name.endsWith('.tar.gz') || name.endsWith('.tgz')) {
    return 'application/gzip'
  }
  if (name.endsWith('.zip')) {
    return 'application/zip'
  }
  return 'application/octet-stream'
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`
}

async function createTarGzFromDirectory(
  dirPath: string
): Promise<{ data: Buffer; filename: string }> {
  const dirName = basename(dirPath)
  const filename = `${dirName}.tar.gz`

  // Use system tar command to create the archive
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []

    const tar = spawn('tar', ['-czf', '-', '-C', dirPath, '.'], {
      stdio: ['ignore', 'pipe', 'pipe'],
    })

    tar.stdout.on('data', (chunk: Buffer) => {
      chunks.push(chunk)
    })

    let stderr = ''
    tar.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString()
    })

    tar.on('close', (code) => {
      if (code === 0) {
        resolve({
          data: Buffer.concat(chunks),
          filename,
        })
      } else {
        reject(new Error(`tar command failed with code ${code}: ${stderr}`))
      }
    })

    tar.on('error', (err) => {
      reject(new Error(`Failed to spawn tar: ${err.message}`))
    })
  })
}
