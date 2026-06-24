/**
 * Setup wizard for `temps up` when no project is linked.
 * Guides the user through: detection → naming → git/manual → services → create → deploy.
 */
import { client, getErrorMessage } from '../../lib/api-client.js'
import {
  detectPreset,
  detectGitRemote,
  detectGitBranch,
  detectServiceHints,
  detectStaticDir,
  suggestProjectName,
  refinePythonPreset,
  isGitRepo,
  type DetectedPreset,
  type DetectedGitRemote,
} from '../../lib/detect-project.js'
import {
  selectGitConnection,
  selectRepository,
  selectBranch,
  detectAndSelectPreset,
  fetchGitConnections,
  findRepositoryByName,
} from '../../lib/git-connection.js'
import {
  selectStorageServices,
  selectServicesWithSuggestions,
  SERVICE_TYPES,
} from '../../lib/service-setup.js'
import { writeProjectConfig } from '../../config/project-config.js'
import { promptText, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, info, warning, newline, icons, colors, header, keyValue, box } from '../../ui/output.js'
import {
  createProject,
  triggerProjectPipeline,
  getLastDeployment,
  generatePresetDockerfile,
  listPresets,
} from '../../api/sdk.gen.js'
import type { RepositoryResponse } from '../../api/types.gen.js'
import { config } from '../../config/store.js'
import { watchDeployment } from '../../lib/deployment-watcher.jsx'
import { deployLocalImage } from '../deploy/deploy-local-image.js'
import { deployStatic } from '../deploy/deploy-static.js'
import { existsSync, writeFileSync, mkdirSync } from 'node:fs'
import { resolve, join } from 'node:path'
import { tmpdir } from 'node:os'

interface SetupWizardOptions {
  name?: string
  preset?: string
  branch?: string
  manual?: boolean
  /** Deploy as a pre-built static bundle (source_type: static_files). */
  static?: boolean
  /** Directory to upload as the static bundle (defaults to auto-detected). */
  staticDir?: string
  noServices?: boolean
  yes?: boolean
}

interface SetupResult {
  projectSlug: string
  projectId: number
  isGitBased: boolean
}

/**
 * Run the setup wizard — detect project, create it in Temps, and trigger first deploy.
 */
export async function runSetupWizard(options: SetupWizardOptions): Promise<SetupResult | null> {
  newline()
  console.log(colors.bold(`${icons.sparkles} Welcome to Temps!`))
  console.log(colors.muted('  Let\'s set up your project for deployment.'))
  console.log(colors.muted('─'.repeat(45)))
  newline()

  // ─── Step 1: Detect Project ──────────────────────────────────────────────
  info('Detecting project...')

  let detectedPreset: DetectedPreset | null = null
  if (options.preset) {
    detectedPreset = { slug: options.preset, label: options.preset, confidence: 'high' }
  } else {
    detectedPreset = detectPreset()

    // Refine Python preset if needed
    if (detectedPreset?.slug === 'python') {
      detectedPreset = await refinePythonPreset()
    }
  }

  if (detectedPreset) {
    success(
      `Detected: ${colors.bold(detectedPreset.label)}` +
        (detectedPreset.confidence === 'low' ? colors.muted(' (generic)') : '')
    )
  } else {
    warning('Could not auto-detect framework')
  }

  // Detect git info
  const gitRemote = detectGitRemote()
  const gitBranch = options.branch ?? detectGitBranch() ?? 'main'
  const hasGit = isGitRepo()

  if (gitRemote) {
    info(`Git remote: ${colors.muted(`${gitRemote.owner}/${gitRemote.repo}`)} (${gitRemote.host})`)
  } else if (hasGit) {
    info(`Git repository found, but no remote configured`)
  }

  // Detect service hints
  const serviceHints = await detectServiceHints()
  if (serviceHints.length > 0) {
    const serviceLabels = serviceHints.map(
      (h) => SERVICE_TYPES.find((t) => t.id === h)?.name || h
    )
    info(`Service hints: ${colors.muted(serviceLabels.join(', '))}`)
  }

  newline()

  // ─── Step 2: Project Name ────────────────────────────────────────────────
  const suggestedName = await suggestProjectName()

  const projectName = options.name ?? (options.yes
    ? suggestedName
    : await promptText({
        message: 'Project name',
        default: suggestedName,
        required: true,
        validate: (v) => (v.length >= 2 ? true : 'Name must be at least 2 characters'),
      }))

  // ─── Step 3: Deployment Method ───────────────────────────────────────────
  let isGitBased = !options.manual && !options.static
  let isStatic = !!options.static
  let connectionId: number | null = null
  let repository: RepositoryResponse | null = null
  let branch = gitBranch
  let repoName: string | null = null
  let repoOwner: string | null = null
  let gitUrl: string | null = null
  let presetSlug = detectedPreset?.slug ?? 'dockerfile'
  let directory = './'

  // Auto-detect a built static-output directory (dist/build/out/…) up front so
  // we can surface it in the deploy-method choice and pre-fill the prompt.
  const detectedStaticDir = options.staticDir ?? detectStaticDir() ?? undefined

  if (options.static) {
    isStatic = true
    isGitBased = false
    info('Static deployment mode selected')
  } else if (options.manual) {
    isGitBased = false
    info('Manual deployment mode selected')
  } else if (options.yes && gitRemote) {
    // Non-interactive with git remote: try auto-setup
    isGitBased = true
  } else if (!options.yes) {
    // Interactive: let user choose
    const deployMethod = await promptSelect({
      message: 'How would you like to deploy?',
      choices: [
        {
          name: 'Git-based (automatic deploys on push)',
          value: 'git',
          description: gitRemote ? `Using ${gitRemote.owner}/${gitRemote.repo}` : 'Requires a git connection',
        },
        {
          name: 'Static (upload a pre-built folder)',
          value: 'static',
          description: detectedStaticDir
            ? `Detected ./${detectedStaticDir}`
            : 'Upload HTML/CSS/JS — no Docker, no git',
        },
        {
          name: 'Manual (upload Docker images from CLI)',
          value: 'manual',
          description: 'Build locally, deploy with temps deploy:local-image',
        },
      ],
      default: 'git',
    })

    isGitBased = deployMethod === 'git'
    isStatic = deployMethod === 'static'
  }

  // ─── Step 3-static: Pick the folder to upload ────────────────────────────
  // Auto-detect, but always let the user override with a custom folder.
  let staticDir = detectedStaticDir ?? './'
  if (isStatic) {
    // Static bundles aren't built, so there's no real framework preset. The
    // API marks them via `project_type: 'static'`; `preset` must still be a
    // valid slug, so use the safe `dockerfile` default (mirrors `projects
    // create`). "static" is only a local detection label, not an API preset.
    presetSlug = 'dockerfile'
    if (options.staticDir) {
      staticDir = options.staticDir
    } else if (!options.yes) {
      staticDir = await promptText({
        message: 'Folder to upload (built static output)',
        default: detectedStaticDir ?? 'dist',
        required: true,
      })
    } else {
      // Non-interactive: use the detection, falling back to a sensible default.
      staticDir = detectedStaticDir ?? 'dist'
    }

    if (!existsSync(resolve(staticDir))) {
      warning(`Folder not found: ${colors.bold(staticDir)}`)
      info('Build your project first, then re-run, or pass --static-dir <path>.')
      return null
    }
  }

  // ─── Step 3a: Git Connection Setup ───────────────────────────────────────
  if (isGitBased) {
    const gitSetup = await setupGitConnection(gitRemote, options.yes)

    if (gitSetup) {
      connectionId = gitSetup.connectionId
      repository = gitSetup.repository
      repoName = repository?.name ?? null
      repoOwner = repository?.owner ?? null
      gitUrl = repository?.clone_url || repository?.ssh_url || null

      // Select branch
      if (repository && connectionId) {
        if (options.yes) {
          branch = gitBranch
        } else {
          branch = await selectBranch(connectionId, repository)
        }

        // Detect preset from repository (API-side detection is more accurate)
        if (!options.preset) {
          const apiPreset = await detectAndSelectPreset(repository.id, branch)
          presetSlug = apiPreset.preset
          directory = apiPreset.directory
        }
      }
    } else {
      // Failed to set up git — offer to switch to manual
      newline()
      warning('Could not set up git connection')
      const switchToManual = await promptConfirm({
        message: 'Continue with manual deployment instead?',
        default: true,
      })

      if (switchToManual) {
        isGitBased = false
      } else {
        info('Setup cancelled. Set up a git provider first:')
        info(`  ${colors.muted('temps providers add')}`)
        return null
      }
    }
  }

  // If manual mode and no API preset detection happened, use local detection or prompt.
  // Static deploys don't build an image, so they don't need a Dockerfile preset.
  if (!isGitBased && !isStatic && !options.preset) {
    if (detectedPreset) {
      // Confirm the detected preset
      if (!options.yes) {
        const useDetected = await promptConfirm({
          message: `Use detected preset: ${detectedPreset.label}?`,
          default: true,
        })

        if (!useDetected) {
          presetSlug = await selectPresetFromAll()
        } else {
          presetSlug = detectedPreset.slug
        }
      } else {
        presetSlug = detectedPreset.slug
      }
    } else {
      presetSlug = await selectPresetFromAll()
    }
  }

  // ─── Step 4: External Services ───────────────────────────────────────────
  let serviceIds: number[] = []

  if (!options.noServices) {
    if (serviceHints.length > 0 && !options.yes) {
      serviceIds = await selectServicesWithSuggestions(serviceHints)
    } else if (!options.yes) {
      serviceIds = await selectStorageServices()
    }
    // In --yes mode with no explicit flag, skip services
  }

  // ─── Step 5: Confirmation ────────────────────────────────────────────────
  newline()

  const serviceNames = serviceIds.length > 0 ? `${serviceIds.length} service(s) linked` : 'None'

  const deployLabel = isGitBased
    ? 'Automatic (on push)'
    : isStatic
      ? 'Static (folder upload)'
      : 'Manual (CLI upload)'

  box(
    [
      `Project:    ${colors.bold(projectName)}`,
      // Static bundles have no real framework preset — don't show the
      // internal `dockerfile` placeholder, it just confuses.
      isStatic ? null : `Preset:     ${colors.bold(presetSlug)}`,
      isStatic
        ? `Folder:     ${colors.bold(staticDir)}`
        : `Directory:  ${colors.bold(directory)}`,
      isGitBased && repoOwner && repoName
        ? `Repository: ${colors.bold(`${repoOwner}/${repoName}`)}`
        : null,
      isStatic ? null : `Branch:     ${colors.bold(branch)}`,
      `Services:   ${colors.bold(serviceNames)}`,
      `Deploy:     ${colors.bold(deployLabel)}`,
    ]
      .filter(Boolean)
      .join('\n'),
    `${icons.rocket} Deployment Preview`
  )

  newline()

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: 'Create project and deploy?',
      default: true,
    })

    if (!confirmed) {
      info('Setup cancelled')
      return null
    }
  }

  // ─── Step 6: Create Project ──────────────────────────────────────────────
  const project = await withSpinner('Creating project...', async () => {
    const { data, error: apiError } = await createProject({
      client,
      body: {
        name: projectName,
        main_branch: branch,
        directory,
        preset: presetSlug,
        repo_name: repoName,
        repo_owner: repoOwner,
        git_url: gitUrl,
        git_provider_connection_id: connectionId,
        automatic_deploy: isGitBased,
        source_type: isGitBased ? 'git' : isStatic ? 'static_files' : 'manual',
        // project_type mirrors the web configurator: 'static' for static_files,
        // 'docker' otherwise. This is what actually marks the project static.
        project_type: isStatic ? 'static' : 'docker',
        storage_service_ids: serviceIds,
      },
    })

    if (apiError || !data) {
      throw new Error(getErrorMessage(apiError) || 'Failed to create project')
    }

    return data
  })

  // ─── Step 7: Link Directory ──────────────────────────────────────────────
  const configPath = await writeProjectConfig({
    projectSlug: project.slug,
  })

  const apiUrl = config.get('apiUrl')
  const dashboardUrl = `${apiUrl}/projects/${project.slug}`

  newline()
  header(`${icons.check} Project Created`)
  newline()
  keyValue('ID', project.id)
  keyValue('Name', project.name)
  keyValue('Slug', project.slug)
  keyValue('Dashboard', colors.primary(dashboardUrl))
  keyValue('Config', colors.muted(configPath))
  newline()

  // ─── Step 8: First Deployment ────────────────────────────────────────────
  if (isStatic) {
    // Upload the built folder as a static bundle and deploy it. deployStatic
    // archives the directory, uploads it, triggers the deploy, and watches it.
    info(`Uploading ${colors.bold(staticDir)} and deploying...`)
    newline()

    try {
      await deployStatic({
        path: staticDir,
        project: project.slug,
        yes: true,
      })
    } catch {
      newline()
      warning('Static deploy failed. You can retry with:')
      info(`  ${colors.muted(`temps deploy:static -p ${project.slug} --path ${staticDir}`)}`)
    }
  } else if (isGitBased) {
    info('Triggering first deployment...')
    newline()

    try {
      const { data: pipelineData, error: pipelineError } = await triggerProjectPipeline({
        client,
        path: { id: project.id },
        body: {
          branch,
        },
      })

      if (pipelineError || !pipelineData) {
        warning('Could not trigger deployment automatically')
        info('Push to your repository to trigger the first deployment')
        info(`Or run: ${colors.muted(`temps deploy -p ${project.slug}`)}`)
      } else {
        success('Deployment triggered!')
        info(pipelineData.message ?? 'Pipeline started')
        newline()

        // Wait for deployment to start and get the deployment ID
        let deploymentId: number | null = null
        for (let attempt = 0; attempt < 30; attempt++) {
          const { data: deployment } = await getLastDeployment({
            client,
            path: { id: project.id },
          })

          if (deployment?.id) {
            deploymentId = deployment.id
            break
          }

          await new Promise((resolve) => setTimeout(resolve, 2000))
        }

        if (deploymentId) {
          // Use the rich deployment watcher TUI
          const result = await watchDeployment({
            projectId: project.id,
            deploymentId,
            timeoutSecs: 600,
            projectName: project.slug,
          })

          if (!result.success) {
            process.exitCode = 1
          }
        } else {
          info(`Watch progress: ${colors.muted(`temps logs -p ${project.slug}`)}`)
          info(`Check status:   ${colors.muted(`temps status`)}`)
        }
      }
    } catch {
      warning('Could not trigger deployment')
      info(`Push to your repository or run: ${colors.muted(`temps deploy -p ${project.slug}`)}`)
    }
  } else {
    // Manual deployment mode — build and deploy immediately
    info('Building and deploying...')
    newline()

    try {
      const hasDockerfile = existsSync(resolve('Dockerfile'))
      let dockerfilePath: string | undefined
      let buildArgsList: string[] | undefined

      if (!hasDockerfile && presetSlug && presetSlug !== 'dockerfile') {
        // No Dockerfile — generate one from the preset via the API
        info(`No Dockerfile found. Generating from ${colors.bold(presetSlug)} preset...`)

        const packageManager = detectPackageManager()

        const { data: dockerfileData, error: dockerfileError } = await generatePresetDockerfile({
          client,
          path: { slug: presetSlug },
          body: {
            package_manager: packageManager,
            project_name: project.slug,
            use_buildkit: true,
          },
        })

        if (dockerfileError || !dockerfileData) {
          warning('Could not generate Dockerfile from preset')
          info('Create a Dockerfile manually or run:')
          info(`  ${colors.muted(`temps deploy:local-image -p ${project.slug}`)}`)
          newline()
          info('For automatic deploys, connect a git repository:')
          info(`  ${colors.muted('temps providers add')}`)

          return {
            projectSlug: project.slug,
            projectId: project.id,
            isGitBased,
          }
        }

        // Write the generated Dockerfile to a temp directory
        const tempDir = join(tmpdir(), `temps-dockerfile-${Date.now()}`)
        mkdirSync(tempDir, { recursive: true })
        dockerfilePath = join(tempDir, 'Dockerfile')
        writeFileSync(dockerfilePath, dockerfileData.dockerfile)

        success(`Dockerfile generated from ${presetSlug} preset`)

        // Convert build args from the API response to --build-arg format
        if (dockerfileData.build_args && Object.keys(dockerfileData.build_args).length > 0) {
          buildArgsList = Object.entries(dockerfileData.build_args).map(
            ([key, value]) => `${key}=${value}`
          )
        }
      }

      await deployLocalImage({
        project: project.slug,
        dockerfile: dockerfilePath,
        buildArg: buildArgsList,
        yes: true,
      })
    } catch {
      newline()
      warning('Automatic build failed. You can deploy manually:')
      info(`  ${colors.muted(`temps deploy:local-image -p ${project.slug}`)}`)
      newline()
      info('For automatic deploys, connect a git repository:')
      info(`  ${colors.muted('temps providers add')}`)
    }
  }

  return {
    projectSlug: project.slug,
    projectId: project.id,
    isGitBased,
  }
}

// ─── Helper: Git Connection Setup ────────────────────────────────────────────

interface GitSetupResult {
  connectionId: number
  repository: RepositoryResponse
}

/**
 * Set up git connection — tries to match detected remote to existing connections,
 * falls back to interactive selection.
 */
async function setupGitConnection(
  gitRemote: DetectedGitRemote | null,
  nonInteractive?: boolean
): Promise<GitSetupResult | null> {
  // Fetch existing connections
  const connections = await withSpinner('Checking git connections...', async () => {
    return fetchGitConnections()
  })

  if (connections.length === 0) {
    warning('No git connections configured')
    info(`Set up a git provider: ${colors.muted('temps providers add')}`)
    return null
  }

  // Try to auto-match git remote to a connection
  if (gitRemote) {
    for (const conn of connections) {
      // Match by host — GitHub connections match github.com, etc.
      const connType = conn.account_type?.toLowerCase() || ''
      const remoteHost = gitRemote.host.toLowerCase()

      const isMatch =
        (connType.includes('github') && remoteHost.includes('github')) ||
        (connType.includes('gitlab') && remoteHost.includes('gitlab')) ||
        (connType.includes('bitbucket') && remoteHost.includes('bitbucket'))

      if (isMatch) {
        // Try to find the repository
        const repo = await withSpinner(
          `Looking for ${gitRemote.owner}/${gitRemote.repo}...`,
          async () => {
            return findRepositoryByName(conn.id, gitRemote.owner, gitRemote.repo)
          }
        )

        if (repo) {
          success(`Found repository: ${gitRemote.owner}/${gitRemote.repo}`)

          if (nonInteractive) {
            return { connectionId: conn.id, repository: repo }
          }

          const useRepo = await promptConfirm({
            message: `Use ${gitRemote.owner}/${gitRemote.repo} from ${conn.account_name}?`,
            default: true,
          })

          if (useRepo) {
            return { connectionId: conn.id, repository: repo }
          }
        } else {
          info(`Repository ${gitRemote.owner}/${gitRemote.repo} not found in ${conn.account_name}`)
          info('It may need to be synced. Trying...')

          // The selectRepository function handles auto-sync
        }
      }
    }
  }

  // No auto-match — interactive selection
  if (nonInteractive) {
    return null
  }

  const connection = await selectGitConnection()
  if (!connection) return null

  const repository = await selectRepository(connection.id)
  if (!repository) return null

  return { connectionId: connection.id, repository }
}

// ─── Helper: Select Preset from All ──────────────────────────────────────────

async function selectPresetFromAll(): Promise<string> {
  const presetsData = await withSpinner('Loading presets...', async () => {
    const { data, error: apiError } = await listPresets({ client })
    if (apiError) {
      throw new Error(getErrorMessage(apiError))
    }
    return data
  })

  const allPresets = presetsData?.presets || []

  if (allPresets.length === 0) {
    return 'dockerfile'
  }

  const choices = allPresets.map((p) => ({
    name: p.label,
    value: p.slug,
    description: p.description,
  }))

  choices.push({
    name: 'Custom / Dockerfile',
    value: 'dockerfile',
    description: 'Use a custom Dockerfile',
  })

  return promptSelect({
    message: 'Select framework',
    choices,
  })
}

// ─── Helper: Detect Package Manager ──────────────────────────────────────────

/**
 * Detect the package manager used by the project by checking for lockfiles.
 * Falls back to "npm" if no lockfile is found.
 */
function detectPackageManager(dir?: string): string {
  const projectDir = dir ? resolve(dir) : process.cwd()

  if (existsSync(join(projectDir, 'pnpm-lock.yaml'))) return 'pnpm'
  if (existsSync(join(projectDir, 'yarn.lock'))) return 'yarn'
  if (existsSync(join(projectDir, 'bun.lockb')) || existsSync(join(projectDir, 'bun.lock'))) return 'bun'
  if (existsSync(join(projectDir, 'package-lock.json'))) return 'npm'

  return 'npm'
}
