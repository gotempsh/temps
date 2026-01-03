import { requireAuth, config } from '../../config/store.js'
import { promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, warning, newline, colors } from '../../ui/output.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { deleteProject, getProjectBySlug } from '../../api/sdk.gen.js'

interface DeleteOptions {
  project: string
  force?: boolean
  yes?: boolean
}

export async function remove(options: DeleteOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectIdOrName = options.project

  newline()

  // Support both --force and --yes for skipping confirmation
  if (!options.force && !options.yes) {
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

  await withSpinner('Deleting project...', async () => {
    // Try to parse as ID first
    let id = parseInt(projectIdOrName, 10)

    if (isNaN(id)) {
      // Get the project by slug to find its ID
      const { data, error } = await getProjectBySlug({ client, path: { slug: projectIdOrName } })
      if (error || !data) {
        throw new Error(`Project "${projectIdOrName}" not found`)
      }
      id = data.id
    }

    const { error } = await deleteProject({ client, path: { id } })

    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Project "${projectIdOrName}" deleted`)

  // Clear default if this was the default project
  if (config.get('defaultProject') === projectIdOrName) {
    config.set('defaultProject', undefined)
  }
}
