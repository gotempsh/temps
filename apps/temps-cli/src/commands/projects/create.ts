import { requireAuth, config } from '../../config/store.js'
import {
  promptText,
  promptConfirm,
  promptSelect,
  promptSearch,
  type SelectOption,
  type SearchOption,
} from '../../ui/prompts.js'
import { withSpinner, startSpinner, succeedSpinner, failSpinner } from '../../ui/spinner.js'
import {
  success,
  error,
  newline,
  icons,
  colors,
  keyValue,
  header,
  info,
  warning,
} from '../../ui/output.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createProject,
  listConnections,
  listRepositoriesByConnection,
  syncRepositories,
  listPresets,
  listServices,
  getRepositoryPresetLive,
  createService,
  getRepositoryBranches,
} from '../../api/sdk.gen.js'
import type {
  ConnectionResponse,
  RepositoryResponse,
  ProjectPresetResponse,
  ServiceTypeRoute,
} from '../../api/types.gen.js'

interface CreateOptions {
  name?: string
  branch?: string
  directory?: string
  preset?: string
  connection?: string
  repo?: string
}

// Service type configuration
const SERVICE_TYPES: { id: ServiceTypeRoute; name: string; description: string }[] = [
  { id: 'postgres', name: 'PostgreSQL', description: 'Reliable Relational Database' },
  { id: 'redis', name: 'Redis', description: 'In-Memory Data Store' },
  { id: 's3', name: 'S3', description: 'Object Storage (MinIO)' },
  { id: 'mongodb', name: 'MongoDB', description: 'Document Database' },
]

export async function create(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()
  console.log(colors.bold(`${icons.sparkles} Create New Project`))
  console.log(colors.muted('─'.repeat(40)))
  newline()

  try {
    // Step 1: Select Git Connection
    const connection = await selectGitConnection()
    if (!connection) {
      error('No git connection selected. Please set up a git provider first.')
      return
    }

    // Step 2: Select Repository
    const repository = await selectRepository(connection.id)
    if (!repository) {
      error('No repository selected.')
      return
    }

    // Step 3: Select Branch
    const branch = await selectBranch(connection.id, repository)

    // Step 4: Detect and Select Preset
    const { preset, directory } = await selectPreset(repository.id, branch)

    // Step 5: Configure Project Name
    const projectName = await configureProjectName(repository, directory)

    // Step 6: Select Storage Services
    const serviceIds = await selectStorageServices()

    // Step 7: Configure Environment Variables
    const envVars = await configureEnvironmentVariables()

    // Step 8: Create the Project
    const project = await withSpinner('Creating project...', async () => {
      const { data, error: apiError } = await createProject({
        client,
        body: {
          name: projectName,
          main_branch: branch,
          directory: directory,
          preset: preset,
          repo_name: repository.name,
          repo_owner: repository.owner,
          git_url: repository.clone_url || repository.ssh_url || '',
          git_provider_connection_id: connection.id,
          automatic_deploy: true,
          storage_service_ids: serviceIds,
          environment_variables: envVars.length > 0 ? envVars : undefined,
        },
      })

      if (apiError || !data) {
        throw new Error(getErrorMessage(apiError) || 'Failed to create project')
      }

      return data
    })

    // Display success
    newline()
    header(`${icons.check} Project Created Successfully`)
    newline()

    keyValue('ID', project.id)
    keyValue('Name', project.name)
    keyValue('Slug', project.slug)
    keyValue('Repository', `${repository.owner}/${repository.name}`)
    keyValue('Branch', project.main_branch)
    keyValue('Directory', project.directory)
    keyValue('Preset', preset)
    if (serviceIds.length > 0) {
      keyValue('Services', `${serviceIds.length} linked`)
    }
    if (envVars.length > 0) {
      keyValue('Environment Variables', `${envVars.length} configured`)
    }

    newline()

    // Ask if user wants to set as default
    const setDefault = await promptConfirm({
      message: 'Set as default project?',
      default: true,
    })

    if (setDefault) {
      config.set('defaultProject', project.slug)
      success(`Default project set to "${project.slug}"`)
    }

    newline()
    info(`View your project: temps projects show ${project.slug}`)
    info(`Deploy your project: temps deploy -p ${project.slug}`)
  } catch (err) {
    error(getErrorMessage(err))
  }
}

/**
 * Step 1: Select Git Connection
 */
async function selectGitConnection(): Promise<ConnectionResponse | null> {
  const spinner = startSpinner('Loading git connections...')

  const { data, error: apiError } = await listConnections({ client })

  if (apiError) {
    failSpinner('Failed to load git connections')
    throw new Error(getErrorMessage(apiError))
  }

  succeedSpinner('Git connections loaded')

  const connections = data?.connections || []

  if (connections.length === 0) {
    newline()
    warning('No git connections found.')
    info('Set up a git provider by running: temps providers add')
    return null
  }

  if (connections.length === 1) {
    const conn = connections[0]!
    info(`Using git connection: ${conn.account_name}`)
    return conn
  }

  newline()
  const choices: SelectOption<number>[] = connections.map((conn) => ({
    name: `${conn.account_name} (${conn.account_type})`,
    value: conn.id,
    description: conn.is_active ? 'Active' : 'Inactive',
  }))

  const selectedId = await promptSelect({
    message: 'Select git connection',
    choices,
  })

  return connections.find((c) => c.id === selectedId) || null
}

/**
 * Step 2: Select Repository
 */
async function selectRepository(connectionId: number): Promise<RepositoryResponse | null> {
  const spinner = startSpinner('Loading repositories...')

  const { data, error: apiError } = await listRepositoriesByConnection({
    client,
    path: { connection_id: connectionId },
    query: { per_page: 100 },
  })

  if (apiError) {
    failSpinner('Failed to load repositories')
    throw new Error(getErrorMessage(apiError))
  }

  succeedSpinner('Repositories loaded')

  let repositories = data?.repositories || []

  // Auto-sync if no repositories found
  if (repositories.length === 0) {
    info('No repositories found. Syncing from provider...')
    await withSpinner('Syncing repositories...', async () => {
      const { error: syncError } = await syncRepositories({
        client,
        path: { connection_id: connectionId },
      })
      if (syncError) {
        throw new Error(getErrorMessage(syncError))
      }
    })

    // Reload after sync
    const { data: reloadedData, error: reloadError } = await listRepositoriesByConnection({
      client,
      path: { connection_id: connectionId },
      query: { per_page: 100 },
    })

    if (reloadError) {
      throw new Error(getErrorMessage(reloadError))
    }

    repositories = reloadedData?.repositories || []

    if (repositories.length === 0) {
      warning('No repositories found after syncing. Check your Git provider permissions.')
      return null
    }
  }

  newline()

  // Build search choices from all repositories
  const choices: SearchOption<number>[] = repositories.map((repo) => ({
    name: `${repo.owner}/${repo.name}`,
    value: repo.id,
    description: [
      repo.language,
      repo.description?.slice(0, 60),
    ].filter(Boolean).join(' • ') || undefined,
  }))

  info(`${repositories.length} repositories available. Type to search...`)
  newline()

  const selectedId = await promptSearch({
    message: 'Select repository',
    choices,
    pageSize: 15,
  })

  return repositories.find((r) => r.id === selectedId) || null
}

/**
 * Step 3: Select Branch
 */
async function selectBranch(
  connectionId: number,
  repository: RepositoryResponse
): Promise<string> {
  const spinner = startSpinner('Loading branches...')

  const { data, error: apiError } = await getRepositoryBranches({
    client,
    path: { owner: repository.owner, repo: repository.name },
    query: { connection_id: connectionId },
  })

  if (apiError || !data?.branches || data.branches.length === 0) {
    failSpinner('Could not load branches, using default')
    return repository.default_branch || 'main'
  }

  succeedSpinner('Branches loaded')

  const branches = data.branches

  // If only one branch, use it
  if (branches.length === 1) {
    info(`Using branch: ${branches[0]!.name}`)
    return branches[0]!.name
  }

  newline()

  const choices: SelectOption<string>[] = branches.map((branch) => ({
    name: branch.name,
    value: branch.name,
    description: branch.name === repository.default_branch ? 'Default branch' : undefined,
  }))

  // Put default branch first
  choices.sort((a, b) => {
    if (a.value === repository.default_branch) return -1
    if (b.value === repository.default_branch) return 1
    return 0
  })

  return await promptSelect({
    message: 'Select branch',
    choices,
    default: repository.default_branch,
  })
}

/**
 * Step 4: Detect and Select Preset
 */
async function selectPreset(
  repositoryId: number,
  branch: string
): Promise<{ preset: string; directory: string }> {
  const spinner = startSpinner('Detecting framework...')

  // Try to detect preset from repository
  const { data: presetData, error: presetError } = await getRepositoryPresetLive({
    client,
    path: { repository_id: repositoryId },
    query: { branch },
  })

  let detectedPresets: ProjectPresetResponse[] = []
  if (!presetError && presetData?.presets) {
    detectedPresets = presetData.presets
  }

  succeedSpinner(
    detectedPresets.length > 0
      ? `Detected ${detectedPresets.length} framework(s)`
      : 'No frameworks detected'
  )

  // Load all available presets
  const { data: allPresetsData } = await listPresets({ client })
  const allPresets = allPresetsData?.presets || []

  newline()

  // Show detected presets first, then allow browsing all
  if (detectedPresets.length > 0) {
    const detectedChoices: SelectOption<string>[] = detectedPresets.map((p) => ({
      name: `${p.preset || 'unknown'} ${colors.muted(`(${p.path || '.'})`)}`,
      value: `${p.preset}::${p.path || '.'}`,
      description: 'Detected in repository',
    }))

    detectedChoices.push({
      name: colors.muted('Browse all frameworks...'),
      value: 'browse_all',
      description: 'Select from all available presets',
    })

    const selected = await promptSelect({
      message: 'Select framework',
      choices: detectedChoices,
    })

    if (selected !== 'browse_all') {
      const [preset, path] = selected.split('::')
      return { preset: preset!, directory: path || './' }
    }
  }

  // Show all presets
  if (allPresets.length === 0) {
    warning('No presets available')
    return { preset: 'custom', directory: './' }
  }

  const allChoices: SelectOption<string>[] = allPresets.map((p) => ({
    name: p.label,
    value: p.slug,
    description: p.description,
  }))

  allChoices.push({
    name: 'Custom / Dockerfile',
    value: 'dockerfile',
    description: 'Use a custom Dockerfile',
  })

  const preset = await promptSelect({
    message: 'Select framework',
    choices: allChoices,
  })

  // Ask for directory
  const directory = await promptText({
    message: 'Root directory (relative to repo)',
    default: './',
  })

  return { preset, directory: directory || './' }
}

/**
 * Step 5: Configure Project Name
 */
async function configureProjectName(
  repository: RepositoryResponse,
  directory: string
): Promise<string> {
  // Generate default name from repo and directory
  let defaultName = repository.name

  // If directory is not root, append it
  if (directory && directory !== './' && directory !== '.' && directory !== 'root') {
    const cleanDir = directory.replace(/^\.\//, '').replace(/\//g, '-').replace(/[^a-zA-Z0-9-]/g, '')
    if (cleanDir) {
      defaultName = `${repository.name}-${cleanDir}`
    }
  }

  newline()
  return await promptText({
    message: 'Project name',
    default: defaultName,
    required: true,
    validate: (v) => (v.length >= 2 ? true : 'Name must be at least 2 characters'),
  })
}

/**
 * Step 6: Select Storage Services
 */
async function selectStorageServices(): Promise<number[]> {
  newline()

  const addServices = await promptConfirm({
    message: 'Add storage services (PostgreSQL, Redis, etc.)?',
    default: false,
  })

  if (!addServices) {
    return []
  }

  // Load existing services
  const spinner = startSpinner('Loading services...')
  const { data: servicesData } = await listServices({ client })
  succeedSpinner('Services loaded')

  const existingServices = servicesData || []
  const selectedServiceIds: number[] = []

  newline()

  // Show existing services if any
  if (existingServices.length > 0) {
    const serviceChoices: SelectOption<number | string>[] = existingServices.map((s) => ({
      name: `${s.name} (${s.service_type})`,
      value: s.id,
      description: `Created ${new Date(s.created_at).toLocaleDateString()}`,
    }))

    serviceChoices.push({
      name: colors.success('+ Create new service'),
      value: 'create_new',
      description: 'Create a new storage service',
    })

    const selected = await promptSelect({
      message: 'Select existing service or create new',
      choices: serviceChoices,
    })

    if (selected !== 'create_new') {
      selectedServiceIds.push(selected as number)

      // Ask if they want to add more
      let addMore = true
      while (addMore) {
        addMore = await promptConfirm({
          message: 'Add another service?',
          default: false,
        })

        if (addMore) {
          const remainingServices = existingServices.filter(
            (s) => !selectedServiceIds.includes(s.id)
          )

          if (remainingServices.length === 0) {
            info('No more services available')
            break
          }

          const moreChoices: SelectOption<number | string>[] = remainingServices.map((s) => ({
            name: `${s.name} (${s.service_type})`,
            value: s.id,
          }))

          moreChoices.push({
            name: colors.success('+ Create new service'),
            value: 'create_new',
            description: 'Create a new storage service',
          })

          const moreSelected = await promptSelect({
            message: 'Select service',
            choices: moreChoices,
          })

          if (moreSelected === 'create_new') {
            const newServiceId = await createNewService()
            if (newServiceId) {
              selectedServiceIds.push(newServiceId)
            }
          } else {
            selectedServiceIds.push(moreSelected as number)
          }
        }
      }

      return selectedServiceIds
    }
  }

  // Create new service
  const newServiceId = await createNewService()
  if (newServiceId) {
    selectedServiceIds.push(newServiceId)
  }

  return selectedServiceIds
}

/**
 * Helper: Create a new storage service
 */
async function createNewService(): Promise<number | null> {
  newline()

  const typeChoices: SelectOption<ServiceTypeRoute>[] = SERVICE_TYPES.map((t) => ({
    name: t.name,
    value: t.id,
    description: t.description,
  }))

  const serviceType = await promptSelect({
    message: 'Select service type',
    choices: typeChoices,
  })

  const serviceName = await promptText({
    message: 'Service name',
    default: `${serviceType}-${Date.now().toString(36)}`,
    required: true,
  })

  const { data, error: apiError } = await withSpinner(
    `Creating ${serviceType} service...`,
    async () => {
      return await createService({
        client,
        body: {
          name: serviceName,
          service_type: serviceType,
          parameters: {}, // Empty parameters uses defaults
        },
      })
    }
  )

  if (apiError || !data) {
    error(`Failed to create service: ${getErrorMessage(apiError)}`)
    return null
  }

  success(`Service "${serviceName}" created`)
  return data.id
}

/**
 * Step 7: Configure Environment Variables
 */
async function configureEnvironmentVariables(): Promise<[string, string][]> {
  newline()

  const addEnvVars = await promptConfirm({
    message: 'Add environment variables?',
    default: false,
  })

  if (!addEnvVars) {
    return []
  }

  const envVars: [string, string][] = []

  let addMore = true
  while (addMore) {
    newline()
    const key = await promptText({
      message: 'Variable name (e.g., DATABASE_URL)',
      required: true,
      validate: (v) => {
        if (!v) return 'Variable name is required'
        if (!/^[A-Z_][A-Z0-9_]*$/i.test(v)) {
          return 'Variable name must start with a letter or underscore and contain only letters, numbers, and underscores'
        }
        return true
      },
    })

    const value = await promptText({
      message: `Value for ${key}`,
      required: true,
    })

    envVars.push([key, value])

    addMore = await promptConfirm({
      message: 'Add another environment variable?',
      default: false,
    })
  }

  return envVars
}
