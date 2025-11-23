import { requireAuth } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { newline, header, icons, json, colors, keyValue, formatDate } from '../../ui/output.js'
import { detailsTable, statusBadge } from '../../ui/table.js'
import { getClient } from '../../api/client.js'

interface Project {
  id: number
  name: string
  description?: string
  repository_url?: string
  status?: string
  created_at: string
  updated_at: string
}

interface ShowOptions {
  json?: boolean
}

export async function show(projectIdOrName: string, options: ShowOptions): Promise<void> {
  await requireAuth()

  const client = getClient()

  const project = await withSpinner('Fetching project...', async () => {
    // Try to parse as ID first
    const id = parseInt(projectIdOrName, 10)
    const endpoint = isNaN(id) ? `/api/projects/by-name/${projectIdOrName}` : `/api/projects/${id}`

    const response = await client.get(endpoint as '/api/projects/{id}')

    if (response.error || !response.data) {
      throw new Error(`Project "${projectIdOrName}" not found`)
    }

    return response.data as Project
  })

  if (options.json) {
    json(project)
    return
  }

  newline()
  header(`${icons.folder} ${project.name}`)

  detailsTable({
    ID: project.id,
    Name: project.name,
    Description: project.description ?? 'No description',
    Repository: project.repository_url ?? 'Not connected',
    Status: statusBadge(project.status ?? 'active'),
    Created: formatDate(project.created_at),
    Updated: formatDate(project.updated_at),
  })

  newline()
}
