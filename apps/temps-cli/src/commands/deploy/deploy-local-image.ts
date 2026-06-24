import { requireAuth, config, credentials } from '../../config/store.js'
import { setupClient, client, normalizeApiUrl } from '../../lib/api-client.js'
import { resolveProjectSlug } from '../../config/resolve-project.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { getProjectBySlug, getProject, getEnvironments, generatePresetDockerfile } from '../../api/sdk.gen.js'
import type { EnvironmentResponse } from '../../api/types.gen.js'
import { promptSelect } from '../../ui/prompts.js'
import {
  startSpinner,
  succeedSpinner,
  failSpinner,
  updateSpinner,
  isQuietMode,
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
import { spawn } from 'node:child_process'
import { existsSync, createWriteStream, writeFileSync, mkdirSync } from 'node:fs'
import { unlink } from 'node:fs/promises'
import { resolve, basename, join } from 'node:path'
import { tmpdir } from 'node:os'

interface DeployLocalImageOptions {
  image?: string
  dockerfile?: string
  context?: string
  buildArg?: string[]
  noBuild?: boolean
  project?: string
  environment?: string
  environmentId?: string
  tag?: string
  wait?: boolean
  yes?: boolean
  metadata?: string
  healthCheckPath?: string
  timeout?: string
}

export async function deployLocalImage(options: DeployLocalImageOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()

  // ─── Step 1: Resolve project and environment ─────────────────────────────
  const resolved = await resolveProjectSlug(options.project)

  if (!resolved) {
    warning('No project specified')
    info('Use: bunx @temps-sdk/cli deploy:local-image --project <slug>')
    info('Or link this directory: bunx @temps-sdk/cli link <slug>')
    return
  }

  const projectName = resolved.slug

  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(projectName)} (from ${resolved.source})`)
  }

  startSpinner('Fetching project details...')

  let projectData: { id: number; name: string; slug: string }
  let environments: EnvironmentResponse[] = []

  try {
    const isNumericId = /^\d+$/.test(projectName)

    if (isNumericId) {
      const result = await getProject({
        client,
        path: { id: parseInt(projectName, 10) },
      })

      if (result.error || !result.data) {
        failSpinner(`Project with ID "${projectName}" not found`)
        info(`Debug: ${JSON.stringify(result)}`)
        return
      }

      const responseData = result.data as Record<string, unknown>
      if (responseData.id !== undefined) {
        projectData = result.data as { id: number; name: string; slug: string }
      } else if (responseData.data && typeof responseData.data === 'object') {
        projectData = responseData.data as { id: number; name: string; slug: string }
      } else {
        failSpinner('Unexpected project response format')
        info(`Debug: ${JSON.stringify(result.data)}`)
        return
      }
    } else {
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

    if (Array.isArray(envData)) {
      environments = envData
    } else if (envData && typeof envData === 'object') {
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

  // ─── Step 2: Select environment ──────────────────────────────────────────
  let environmentId: number | undefined
  let environmentName = options.environment || 'production'

  if (options.environmentId) {
    environmentId = parseInt(options.environmentId, 10)
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
      const env = environments.find((e) => e.name === options.environment)
      if (env) {
        environmentId = env.id
        environmentName = env.name
      }
    } else if (!options.yes) {
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

  info(`Environment: ${colors.bold(environmentName)}`)

  // ─── Step 3: Resolve Dockerfile and build image ──────────────────────────
  let imageName: string
  let didBuild = false

  const shouldBuild = !options.noBuild && !options.image

  if (shouldBuild) {
    let dockerfilePath = options.dockerfile || 'Dockerfile'
    const contextPath = options.context || '.'
    let resolvedDockerfile = resolve(dockerfilePath)
    const resolvedContext = resolve(contextPath)
    let generatedBuildArgs: string[] = []

    // Check if Dockerfile exists — if not, try to generate one from the project's preset
    if (!existsSync(resolvedDockerfile)) {
      const generated = await tryGenerateDockerfile(options.project)

      if (generated) {
        resolvedDockerfile = generated.dockerfilePath
        generatedBuildArgs = generated.buildArgs
        success(`Dockerfile generated from ${colors.bold(generated.preset)} preset`)
      } else {
        warning(`Dockerfile not found: ${resolvedDockerfile}`)
        info('Options:')
        info('  1. Create a Dockerfile in the current directory')
        info('  2. Specify a Dockerfile path: --dockerfile <path>')
        info('  3. Skip build and use existing image: --image <image-name>')
        return
      }
    }

    const allBuildArgs = [...generatedBuildArgs, ...(options.buildArg || [])]

    const dirName = basename(process.cwd())
    const timestamp = Date.now()
    imageName = options.tag || `${projectName || dirName}:local-${timestamp}`

    newline()
    box(
      `Dockerfile: ${colors.bold(resolvedDockerfile)}\n` +
        `Context: ${colors.bold(resolvedContext)}\n` +
        `Image Tag: ${colors.bold(imageName)}` +
        (allBuildArgs.length ? `\nBuild Args: ${colors.bold(allBuildArgs.join(', '))}` : ''),
      `${icons.package} Docker Build`
    )
    newline()

    // Stream the full Docker build log instead of collapsing it into a single
    // spinner line. A spinner can't coexist with multi-line streamed output —
    // it would fight the build lines for the cursor — so we stop it and print
    // each line dimmed, like subordinate log output. Quiet mode stays silent.
    const quiet = isQuietMode()
    if (!quiet) {
      info('Building Docker image...')
      newline()
    }

    try {
      await dockerBuild({
        dockerfile: resolvedDockerfile,
        context: resolvedContext,
        tag: imageName,
        buildArgs: allBuildArgs,
        onOutput: (line) => {
          const trimmed = line.trimEnd()
          if (trimmed && !quiet) {
            console.log(colors.dim(`  ${trimmed}`))
          }
        },
      })
      if (!quiet) newline()
      success(`Image built: ${imageName}`)
      didBuild = true
    } catch (err) {
      if (!quiet) newline()
      warning('Docker build failed')
      if (err instanceof Error) {
        warning(err.message)
      }
      return
    }
  } else if (options.image) {
    imageName = options.image

    startSpinner('Verifying local Docker image...')

    const imageExists = await checkImageExists(imageName)
    if (!imageExists) {
      failSpinner(`Image "${imageName}" not found locally`)
      info('Make sure the image exists by running: docker images')
      info('Or remove --image to build from Dockerfile')
      return
    }

    succeedSpinner(`Found local image: ${imageName}`)
  } else {
    warning('No image specified and build is disabled')
    info('Use: temps deploy:local-image --image <image-name>')
    info('Or remove --no-build to build from Dockerfile')
    return
  }

  // ─── Step 4: Show deployment preview ─────────────────────────────────────
  const imageSize = await getImageSize(imageName)
  info(`Image size: ${formatFileSize(imageSize)}`)

  newline()
  box(
    `Project: ${colors.bold(projectName)}\n` +
      `Environment: ${colors.bold(environmentName)}\n` +
      `Image: ${colors.bold(imageName)}\n` +
      `Size: ${colors.bold(formatFileSize(imageSize))}` +
      (didBuild ? `\n${colors.dim('(freshly built)')}` : ''),
    `${icons.rocket} Deploy Local Image`
  )
  newline()

  // ─── Step 5: Export, upload, and deploy ──────────────────────────────────
  const tempFilename = `temps-image-${Date.now()}.tar`
  const tempFilePath = join(tmpdir(), tempFilename)

  startSpinner('Exporting image with docker save...')

  let exportedSize: number
  try {
    exportedSize = await dockerSaveToFile(imageName, tempFilePath, (progress) => {
      updateSpinner(`Exporting image... ${formatFileSize(progress)} saved`)
    })
    succeedSpinner(`Image exported: ${formatFileSize(exportedSize)}`)
  } catch (err) {
    failSpinner('Failed to export image')
    if (err instanceof Error) {
      warning(err.message)
    }
    return
  }

  const MAX_SIZE = 1024 * 1024 * 1024
  if (exportedSize > MAX_SIZE) {
    warning(`Image size (${formatFileSize(exportedSize)}) exceeds maximum allowed (1 GB)`)
    await unlink(tempFilePath).catch(() => {})
    return
  }

  startSpinner('Uploading image to server...')

  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()

  try {
    const uploadUrl = `${apiUrl}/projects/${projectData.id}/environments/${environmentId}/deploy/image-upload`

    const queryParams = new URLSearchParams()
    if (options.tag) {
      queryParams.set('tag', options.tag)
    }
    const healthCheckPath = options.healthCheckPath?.trim()
    if (healthCheckPath) {
      if (!healthCheckPath.startsWith('/')) {
        throw new Error('--health-check-path must start with "/" (e.g. /api/healthz)')
      }
      queryParams.set('health_check_path', healthCheckPath)
    }
    const finalUrl = queryParams.toString()
      ? `${uploadUrl}?${queryParams.toString()}`
      : uploadUrl

    const file = Bun.file(tempFilePath)
    const filename = `${imageName.replace(/[/:]/g, '-')}.tar`

    const boundary = `----BunFormBoundary${Date.now().toString(16)}`

    const fileStream = file.stream()

    const header = [
      `--${boundary}`,
      `Content-Disposition: form-data; name="file"; filename="${filename}"`,
      'Content-Type: application/x-tar',
      '',
      '',
    ].join('\r\n')

    const footer = `\r\n--${boundary}--\r\n`

    const headerBytes = new TextEncoder().encode(header)
    const footerBytes = new TextEncoder().encode(footer)

    let uploadedBytes = 0

    const combinedStream = new ReadableStream({
      async start(controller) {
        controller.enqueue(headerBytes)

        const reader = fileStream.getReader()
        try {
          while (true) {
            const { done, value } = await reader.read()
            if (done) break
            uploadedBytes += value.byteLength
            updateSpinner(`Uploading... ${formatFileSize(uploadedBytes)} / ${formatFileSize(exportedSize)}`)
            controller.enqueue(value)
          }
        } finally {
          reader.releaseLock()
        }

        controller.enqueue(footerBytes)
        controller.close()
      },
    })

    const uploadResponse = await fetch(finalUrl, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${apiKey}`,
        'Content-Type': `multipart/form-data; boundary=${boundary}`,
      },
      body: combinedStream,
      duplex: 'half',
    } as RequestInit)

    await unlink(tempFilePath).catch(() => {})

    if (!uploadResponse.ok) {
      const errorText = await uploadResponse.text()
      failSpinner(`Upload failed: ${uploadResponse.status}`)
      info(`URL: ${finalUrl}`)
      warning(`Response: ${errorText}`)
      return
    }

    const responseText = await uploadResponse.text()
    let deployment: { id: number; slug: string }
    try {
      deployment = JSON.parse(responseText) as { id: number; slug: string }
    } catch (parseErr) {
      failSpinner('Failed to parse deployment response')
      info(`URL: ${finalUrl}`)
      warning(`Response: ${responseText}`)
      return
    }

    succeedSpinner(`Deployment started: ${deployment.slug}`)

    if (options.wait !== false) {
      const result = await watchDeployment({
        projectId: projectData.id,
        deploymentId: deployment.id,
        timeoutSecs: parseInt(options.timeout || '600', 10),
        projectName,
      })

      if (!result.success) {
        process.exitCode = 1
      }
    } else {
      newline()
      info('Deployment running in background')
      info(`Check status with: temps deployments list --project ${projectName}`)
      newline()
      success('Local image deployment initiated successfully!')
      newline()
    }
  } catch (err) {
    failSpinner('Deployment failed')
    throw err
  }
}

interface DockerBuildOptions {
  dockerfile: string
  context: string
  tag: string
  buildArgs: string[]
  onOutput?: (line: string) => void
}

async function dockerBuild(options: DockerBuildOptions): Promise<void> {
  return new Promise((resolve, reject) => {
    const args = [
      'build',
      // Force the plain, line-oriented BuildKit renderer. The default `auto`
      // renderer collapses into a single rewriting line when stdout isn't a
      // TTY (which it isn't — we pipe it), so the per-step build log is lost.
      // `plain` emits every step/line, which we stream through to the user.
      '--progress=plain',
      '-f', options.dockerfile,
      '-t', options.tag,
    ]

    for (const arg of options.buildArgs) {
      args.push('--build-arg', arg)
    }

    args.push(options.context)

    const docker = spawn('docker', args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      // BuildKit's plain renderer keys off whether *its* stdout is a TTY;
      // setting this env makes it deterministic regardless of how docker is
      // invoked, and keeps colour codes out of the piped stream.
      env: { ...process.env, BUILDKIT_PROGRESS: 'plain' },
    })

    let stderr = ''

    docker.stdout.on('data', (chunk: Buffer) => {
      const lines = chunk.toString().split('\n')
      for (const line of lines) {
        if (line.trim() && options.onOutput) {
          options.onOutput(line)
        }
      }
    })

    docker.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString()
      const lines = chunk.toString().split('\n')
      for (const line of lines) {
        if (line.trim() && options.onOutput) {
          options.onOutput(line)
        }
      }
    })

    docker.on('close', (code) => {
      if (code === 0) {
        resolve()
      } else {
        reject(new Error(`docker build failed with code ${code}:\n${stderr}`))
      }
    })

    docker.on('error', (err) => {
      reject(new Error(`Failed to spawn docker: ${err.message}`))
    })
  })
}

async function checkImageExists(imageName: string): Promise<boolean> {
  return new Promise((resolve) => {
    const docker = spawn('docker', ['image', 'inspect', imageName], {
      stdio: ['ignore', 'pipe', 'pipe'],
    })

    docker.on('close', (code) => {
      resolve(code === 0)
    })

    docker.on('error', () => {
      resolve(false)
    })
  })
}

async function getImageSize(imageName: string): Promise<number> {
  return new Promise((resolve) => {
    const docker = spawn('docker', ['image', 'inspect', '--format', '{{.Size}}', imageName], {
      stdio: ['ignore', 'pipe', 'pipe'],
    })

    let output = ''

    docker.stdout.on('data', (chunk: Buffer) => {
      output += chunk.toString()
    })

    docker.on('close', (code) => {
      if (code === 0) {
        const size = parseInt(output.trim(), 10)
        resolve(isNaN(size) ? 0 : size)
      } else {
        resolve(0)
      }
    })

    docker.on('error', () => {
      resolve(0)
    })
  })
}

async function dockerSaveToFile(
  imageName: string,
  outputPath: string,
  onProgress?: (bytesWritten: number) => void
): Promise<number> {
  return new Promise((resolve, reject) => {
    let totalBytes = 0

    const docker = spawn('docker', ['save', imageName], {
      stdio: ['ignore', 'pipe', 'pipe'],
    })

    const writeStream = createWriteStream(outputPath)

    docker.stdout.on('data', (chunk: Buffer) => {
      writeStream.write(chunk)
      totalBytes += chunk.length
      if (onProgress) {
        onProgress(totalBytes)
      }
    })

    let stderr = ''
    docker.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString()
    })

    docker.on('close', (code) => {
      writeStream.end()
      if (code === 0) {
        resolve(totalBytes)
      } else {
        reject(new Error(`docker save failed with code ${code}: ${stderr}`))
      }
    })

    docker.on('error', (err) => {
      writeStream.end()
      reject(new Error(`Failed to spawn docker: ${err.message}`))
    })

    writeStream.on('error', (err) => {
      reject(new Error(`Failed to write to file: ${err.message}`))
    })
  })
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`
}

// ─── Dockerfile Generation ───────────────────────────────────────────────────

interface GeneratedDockerfile {
  dockerfilePath: string
  buildArgs: string[]
  preset: string
}

/**
 * Try to generate a Dockerfile from the project's preset via the API.
 * Returns null if the project has no preset or generation fails.
 */
async function tryGenerateDockerfile(
  projectSlug?: string
): Promise<GeneratedDockerfile | null> {
  let slug = projectSlug
  if (!slug) {
    const resolved = await resolveProjectSlug()
    slug = resolved?.slug
  }
  if (!slug) return null

  // Fetch the project to get its preset
  let preset: string | null | undefined
  try {
    const isNumericId = /^\d+$/.test(slug)
    if (isNumericId) {
      const { data } = await getProject({
        client,
        path: { id: parseInt(slug, 10) },
      })
      preset = (data as Record<string, unknown>)?.preset as string | undefined
    } else {
      const { data } = await getProjectBySlug({
        client,
        path: { slug },
      })
      preset = data?.preset
    }
  } catch {
    return null
  }

  if (!preset || preset === 'dockerfile') return null

  info(`No Dockerfile found. Generating from ${colors.bold(preset)} preset...`)

  // Detect local package manager
  const packageManager = detectLocalPackageManager()

  try {
    const { data, error: apiError } = await generatePresetDockerfile({
      client,
      path: { slug: preset },
      body: {
        package_manager: packageManager,
        project_name: slug,
        use_buildkit: true,
      },
    })

    if (apiError || !data) {
      const errorMsg = apiError
        ? (typeof apiError === 'object' && apiError !== null && 'detail' in apiError
            ? String((apiError as Record<string, unknown>).detail)
            : JSON.stringify(apiError))
        : 'No data returned'
      warning(`Failed to generate Dockerfile: ${errorMsg}`)
      return null
    }

    // The API client may return the response as a parsed object or as a raw
    // JSON string (when Content-Type is not application/json). Handle both.
    let parsed: Record<string, unknown>
    if (typeof data === 'string') {
      try {
        parsed = JSON.parse(data) as Record<string, unknown>
      } catch {
        warning('Failed to generate Dockerfile: could not parse API response')
        return null
      }
    } else {
      parsed = data as Record<string, unknown>
    }

    const dockerfileContent =
      typeof parsed.dockerfile === 'string' ? parsed.dockerfile : undefined
    const buildArgsObj =
      parsed.build_args && typeof parsed.build_args === 'object'
        ? (parsed.build_args as Record<string, string>)
        : {}
    const presetSlug = typeof parsed.preset === 'string' ? parsed.preset : preset

    if (!dockerfileContent) {
      warning(
        `Failed to generate Dockerfile: unexpected response (keys: ${Object.keys(parsed).join(', ')})`
      )
      return null
    }

    // Write to a temp file
    const tempDir = join(tmpdir(), `temps-dockerfile-${Date.now()}`)
    mkdirSync(tempDir, { recursive: true })
    const dockerfilePath = join(tempDir, 'Dockerfile')
    writeFileSync(dockerfilePath, dockerfileContent)

    // Convert build args to KEY=VALUE format
    const buildArgs = Object.entries(buildArgsObj).map(
      ([key, value]) => `${key}=${value}`
    )

    return { dockerfilePath, buildArgs, preset: presetSlug }
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err)
    warning(`Failed to generate Dockerfile: ${msg}`)
    return null
  }
}

/**
 * Detect the package manager by checking for lockfiles in the current directory.
 */
function detectLocalPackageManager(): string {
  const cwd = process.cwd()
  if (existsSync(join(cwd, 'pnpm-lock.yaml'))) return 'pnpm'
  if (existsSync(join(cwd, 'yarn.lock'))) return 'yarn'
  if (existsSync(join(cwd, 'bun.lockb')) || existsSync(join(cwd, 'bun.lock'))) return 'bun'
  if (existsSync(join(cwd, 'package-lock.json'))) return 'npm'
  return 'npm'
}
