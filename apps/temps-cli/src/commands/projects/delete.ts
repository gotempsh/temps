import { requireAuth, config } from '../../config/store.js'
import { promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, warning, newline, colors } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface DeleteOptions {
  force?: boolean
}

export async function remove(projectIdOrName: string, options: DeleteOptions): Promise<void> {
  await requireAuth()

  newline()

  if (!options.force) {
    warning(`You are about to delete project "${colors.bold(projectIdOrName)}"`)
    warning('This action cannot be undone!')
    newline()

    const confirmed = await promptConfirm({
      message: `Delete project "${projectIdOrName}"?`,
      default: false,
    })

    if (!confirmed) {
      console.log('Cancelled')
      return
    }
  }

  const client = getClient()

  await withSpinner('Deleting project...', async () => {
    // Try to parse as ID first
    const id = parseInt(projectIdOrName, 10)
    const endpoint = isNaN(id)
      ? `/api/projects/by-name/${projectIdOrName}`
      : `/api/projects/${id}`

    const response = await client.delete(endpoint as '/api/projects/{id}')

    if (response.error) {
      throw new Error(`Failed to delete project "${projectIdOrName}"`)
    }
  })

  success(`Project "${projectIdOrName}" deleted`)

  // Clear default if this was the default project
  if (config.get('defaultProject') === projectIdOrName) {
    config.set('defaultProject', undefined)
  }
}
