import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getProject,
  getProjectBySlug,
  updateProject,
  updateProjectSettings,
  updateGitSettings,
  updateProjectDeploymentConfig,
} from '../../api/sdk.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptText, promptConfirm, promptSelect } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

export async function updateProjectAction(
  options: { project: string; name?: string; json?: boolean; yes?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectIdOrSlug = options.project

  // Get project first
  const project = await withSpinner('Fetching project...', async () => {
    const id = parseInt(projectIdOrSlug, 10)

    if (!isNaN(id)) {
      const { data, error } = await getProject({
        client,
        path: { id },
      })
      if (error || !data) {
        throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
      }
      return data
    }

    // Try by slug
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectIdOrSlug },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
    }
    return data
  })

  // Collect updates - use flags if provided, otherwise prompt (unless --yes)
  let name = options.name

  if (!options.yes) {
    name = name ?? await promptText({
      message: 'Project name',
      default: project.name,
      required: true,
    })
  } else {
    // In automation mode, use provided values or keep existing
    name = name ?? project.name
  }

  const updated = await withSpinner('Updating project...', async () => {
    const { data, error } = await updateProject({
      client,
      path: { id: project.id },
      body: {
        name: name!,
        main_branch: project.main_branch,
        directory: project.directory,
        preset: project.preset ?? 'auto',
        storage_service_ids: [],
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(updated)
    return
  }

  success(`Project "${name}" updated successfully`)
}

export async function updateSettingsAction(
  options: {
    project: string
    slug?: string
    attackMode?: boolean
    previewEnvs?: boolean
    json?: boolean
    yes?: boolean
  }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectIdOrSlug = options.project

  // Get project first
  const project = await withSpinner('Fetching project...', async () => {
    const id = parseInt(projectIdOrSlug, 10)

    if (!isNaN(id)) {
      const { data, error } = await getProject({
        client,
        path: { id },
      })
      if (error || !data) {
        throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
      }
      return data
    }

    // Try by slug
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectIdOrSlug },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
    }
    return data
  })

  // Collect settings interactively if not provided via options
  let slug = options.slug
  let attackMode = options.attackMode
  let previewEnvs = options.previewEnvs

  // Only prompt if no flags provided AND not in automation mode
  if (slug === undefined && attackMode === undefined && previewEnvs === undefined && !options.yes) {
    newline()
    header('Update Project Settings')
    info(`Current settings for "${project.name}"`)
    newline()

    slug = await promptText({
      message: 'Project slug (URL-friendly identifier)',
      default: project.slug,
    })

    attackMode = await promptConfirm({
      message: 'Enable attack mode (CAPTCHA protection)?',
      default: project.attack_mode ?? false,
    })

    previewEnvs = await promptConfirm({
      message: 'Enable preview environments for branches?',
      default: project.enable_preview_environments ?? false,
    })
  }

  const updated = await withSpinner('Updating project settings...', async () => {
    const { data, error } = await updateProjectSettings({
      client,
      path: { project_id: project.id },
      body: {
        slug: slug ?? undefined,
        attack_mode: attackMode ?? undefined,
        enable_preview_environments: previewEnvs ?? undefined,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(updated)
    return
  }

  success('Project settings updated successfully')
  keyValue('Slug', slug ?? project.slug)
  keyValue('Attack Mode', attackMode ? colors.success('Enabled') : colors.muted('Disabled'))
  keyValue('Preview Environments', previewEnvs ? colors.success('Enabled') : colors.muted('Disabled'))
}

export async function updateGitAction(
  options: {
    project: string
    owner?: string
    repo?: string
    branch?: string
    directory?: string
    preset?: string
    json?: boolean
    yes?: boolean
  }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectIdOrSlug = options.project

  // Get project first
  const project = await withSpinner('Fetching project...', async () => {
    const id = parseInt(projectIdOrSlug, 10)

    if (!isNaN(id)) {
      const { data, error } = await getProject({
        client,
        path: { id },
      })
      if (error || !data) {
        throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
      }
      return data
    }

    // Try by slug
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectIdOrSlug },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
    }
    return data
  })

  // Collect git settings interactively if not all provided
  let repoOwner = options.owner
  let repoName = options.repo
  let mainBranch = options.branch
  let directory = options.directory
  let preset = options.preset

  // Only prompt if flags are missing AND not in automation mode
  if ((!repoOwner || !repoName || !mainBranch) && !options.yes) {
    newline()
    header('Update Git Settings')
    info(`Current git settings for "${project.name}"`)
    keyValue('Repository', `${project.repo_owner ?? ''}/${project.repo_name ?? ''}`)
    keyValue('Branch', project.main_branch)
    keyValue('Directory', project.directory || '/')
    keyValue('Preset', project.preset || 'auto')
    newline()

    repoOwner = options.owner ?? await promptText({
      message: 'Repository owner',
      default: project.repo_owner ?? '',
      required: true,
    })

    repoName = options.repo ?? await promptText({
      message: 'Repository name',
      default: project.repo_name ?? '',
      required: true,
    })

    mainBranch = options.branch ?? await promptText({
      message: 'Main branch',
      default: project.main_branch,
      required: true,
    })

    directory = options.directory ?? await promptText({
      message: 'Directory (relative path to app)',
      default: project.directory || '',
    })

    preset = options.preset ?? await promptSelect({
      message: 'Build preset',
      choices: [
        { name: 'Auto-detect', value: 'auto' },
        { name: 'Next.js', value: 'nextjs' },
        { name: 'Node.js', value: 'nodejs' },
        { name: 'Static', value: 'static' },
        { name: 'Docker', value: 'docker' },
        { name: 'Rust', value: 'rust' },
        { name: 'Go', value: 'go' },
        { name: 'Python', value: 'python' },
      ],
    })
  } else if (options.yes) {
    // In automation mode, use provided values or keep existing
    repoOwner = repoOwner ?? project.repo_owner ?? ''
    repoName = repoName ?? project.repo_name ?? ''
    mainBranch = mainBranch ?? project.main_branch
    directory = directory ?? project.directory ?? ''
    preset = preset ?? project.preset ?? 'auto'
  }

  const updated = await withSpinner('Updating git settings...', async () => {
    const { data, error } = await updateGitSettings({
      client,
      path: { project_id: project.id },
      body: {
        repo_owner: repoOwner!,
        repo_name: repoName!,
        main_branch: mainBranch!,
        directory: directory || '',
        preset: preset || null,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(updated)
    return
  }

  success('Git settings updated successfully')
  keyValue('Repository', `${repoOwner}/${repoName}`)
  keyValue('Branch', mainBranch!)
  keyValue('Directory', directory || '/')
  keyValue('Preset', preset || 'auto')
}

export async function updateConfigAction(
  options: {
    project: string
    replicas?: string
    cpuLimit?: string
    memoryLimit?: string
    autoDeploy?: boolean
    json?: boolean
    yes?: boolean
  }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectIdOrSlug = options.project

  // Get project first
  const project = await withSpinner('Fetching project...', async () => {
    const id = parseInt(projectIdOrSlug, 10)

    if (!isNaN(id)) {
      const { data, error } = await getProject({
        client,
        path: { id },
      })
      if (error || !data) {
        throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
      }
      return data
    }

    // Try by slug
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: projectIdOrSlug },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Project "${projectIdOrSlug}" not found`)
    }
    return data
  })

  // Collect deployment config interactively if not provided
  let replicas = options.replicas ? parseInt(options.replicas, 10) : undefined
  let cpuLimit = options.cpuLimit ? parseFloat(options.cpuLimit) : undefined
  let memoryLimit = options.memoryLimit ? parseInt(options.memoryLimit, 10) : undefined
  let autoDeploy = options.autoDeploy

  // Only prompt if no flags provided AND not in automation mode
  if (replicas === undefined && cpuLimit === undefined && memoryLimit === undefined && autoDeploy === undefined && !options.yes) {
    newline()
    header('Update Deployment Configuration')
    info(`Deployment config for "${project.name}"`)
    newline()

    const replicasStr = await promptText({
      message: 'Number of replicas',
      default: '1',
    })
    replicas = parseInt(replicasStr, 10)

    const cpuLimitStr = await promptText({
      message: 'CPU limit (cores, e.g., 0.5, 1, 2)',
      default: '1',
    })
    cpuLimit = parseFloat(cpuLimitStr)

    const memoryLimitStr = await promptText({
      message: 'Memory limit (MB)',
      default: '512',
    })
    memoryLimit = parseInt(memoryLimitStr, 10)

    autoDeploy = await promptConfirm({
      message: 'Enable automatic deployments on push?',
      default: true,
    })
  }

  const updated = await withSpinner('Updating deployment configuration...', async () => {
    const { data, error } = await updateProjectDeploymentConfig({
      client,
      path: { project_id: project.id },
      body: {
        replicas: replicas ?? undefined,
        cpuLimit: cpuLimit ?? undefined,
        memoryLimit: memoryLimit ?? undefined,
        automaticDeploy: autoDeploy ?? undefined,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json) {
    json(updated)
    return
  }

  success('Deployment configuration updated successfully')
  if (replicas !== undefined) keyValue('Replicas', replicas)
  if (cpuLimit !== undefined) keyValue('CPU Limit', `${cpuLimit} cores`)
  if (memoryLimit !== undefined) keyValue('Memory Limit', `${memoryLimit} MB`)
  if (autoDeploy !== undefined) keyValue('Auto Deploy', autoDeploy ? colors.success('Enabled') : colors.muted('Disabled'))
}
