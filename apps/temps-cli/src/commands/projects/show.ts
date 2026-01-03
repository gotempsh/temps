import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, keyValue, formatDate } from '../../ui/output.js'
import { detailsTable, statusBadge } from '../../ui/table.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getProject, getProjectBySlug } from '../../api/sdk.gen.js'

interface ShowOptions {
  project: string
  json?: boolean
}

export async function show(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectIdOrName = options.project

  const project = await withSpinner('Fetching project...', async () => {
    // Try to parse as ID first
    const id = parseInt(projectIdOrName, 10)

    if (!isNaN(id)) {
      const { data, error } = await getProject({ client, path: { id } })
      if (error) {
        throw new Error(`Project "${projectIdOrName}" not found`)
      }
      return data
    }

    // Try by slug
    const { data, error } = await getProjectBySlug({ client, path: { slug: projectIdOrName } })
    if (error) {
      throw new Error(`Project "${projectIdOrName}" not found`)
    }
    return data
  })

  if (!project) {
    throw new Error(`Project "${projectIdOrName}" not found`)
  }

  if (options.json) {
    json(project)
    return
  }

  newline()
  header(`${icons.folder} ${project.name}`)

  detailsTable({
    ID: project.id,
    Name: project.name,
    Slug: project.slug,
    Directory: project.directory,
    'Main Branch': project.main_branch,
    Repository: project.repo_name ? `${project.repo_owner}/${project.repo_name}` : 'Not connected',
    'Attack Mode': project.attack_mode ? 'Enabled' : 'Disabled',
    'Preview Envs': project.enable_preview_environments ? 'Enabled' : 'Disabled',
    Created: formatDate(new Date(project.created_at * 1000).toISOString()),
    Updated: formatDate(new Date(project.updated_at * 1000).toISOString()),
  })

  newline()
}
