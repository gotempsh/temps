import { requireAuth, config } from '../../config/store.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, newline, icons, colors, keyValue, header } from '../../ui/output.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { createProject } from '../../api/sdk.gen.js'

interface CreateOptions {
  name?: string
  branch?: string
  directory?: string
  preset?: string
}

export async function create(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  newline()
  console.log(colors.bold(`${icons.sparkles} Create New Project`))
  newline()

  // Gather project info
  const name =
    options.name ??
    (await promptText({
      message: 'Project name',
      required: true,
      validate: (v) => (v.length >= 2 ? true : 'Name must be at least 2 characters'),
    }))

  const mainBranch =
    options.branch ??
    ((await promptText({
      message: 'Main branch',
      default: 'main',
    })) || 'main')

  const directory =
    options.directory ??
    ((await promptText({
      message: 'Directory (relative path in repo)',
      default: '.',
    })) || '.')

  const preset =
    options.preset ??
    ((await promptText({
      message: 'Preset (e.g., nodejs, python, static)',
      default: 'nodejs',
    })) || 'nodejs')

  const project = await withSpinner('Creating project...', async () => {
    const { data, error } = await createProject({
      client,
      body: {
        name,
        main_branch: mainBranch,
        directory,
        preset,
        storage_service_ids: [],
      },
    })

    if (error) {
      throw new Error(getErrorMessage(error))
    }

    return data
  })

  newline()
  header(`${icons.check} Project Created`)
  keyValue('ID', project.id)
  keyValue('Name', project.name)
  keyValue('Slug', project.slug)
  keyValue('Main Branch', project.main_branch)
  keyValue('Directory', project.directory)
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
}
