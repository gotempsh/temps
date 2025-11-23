import { requireAuth, config } from '../../config/store.js'
import { promptText, promptConfirm } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, newline, icons, colors, keyValue, header } from '../../ui/output.js'
import { getClient } from '../../api/client.js'

interface CreateOptions {
  name?: string
  description?: string
  repo?: string
}

interface Project {
  id: number
  name: string
  description?: string
  repository_url?: string
}

export async function create(options: CreateOptions): Promise<void> {
  await requireAuth()

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

  const description =
    options.description ??
    (await promptText({
      message: 'Description (optional)',
    }))

  const repositoryUrl =
    options.repo ??
    (await promptText({
      message: 'Git repository URL (optional)',
      validate: (v) => {
        if (!v) return true
        try {
          new URL(v)
          return true
        } catch {
          return 'Please enter a valid URL'
        }
      },
    }))

  const client = getClient()

  const project = await withSpinner('Creating project...', async () => {
    const response = await client.post('/api/projects', {
      body: {
        name,
        description: description || undefined,
        repository_url: repositoryUrl || undefined,
      },
    })

    if (response.error || !response.data) {
      throw new Error('Failed to create project')
    }

    return response.data as Project
  })

  newline()
  header(`${icons.check} Project Created`)
  keyValue('ID', project.id)
  keyValue('Name', project.name)
  keyValue('Description', project.description)
  keyValue('Repository', project.repository_url)
  newline()

  // Ask if user wants to set as default
  const setDefault = await promptConfirm({
    message: 'Set as default project?',
    default: true,
  })

  if (setDefault) {
    config.set('defaultProject', project.name)
    success(`Default project set to "${project.name}"`)
  }
}
