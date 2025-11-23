import { credentials, config } from '../../config/store.js'
import { promptPassword } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { success, info, icons, colors, newline, box } from '../../ui/output.js'
import { getClient } from '../../api/client.js'
import { AuthenticationError } from '../../utils/errors.js'

interface LoginOptions {
  apiKey?: string
}

export async function login(options: LoginOptions): Promise<void> {
  newline()

  if (await credentials.isAuthenticated()) {
    const existingEmail = await credentials.get('email')
    info(`Already logged in as ${colors.bold(existingEmail ?? 'unknown')}`)
    info('Run "temps logout" first to switch accounts')
    return
  }

  const apiKey = options.apiKey ?? (await promptPassword({
    message: 'API Key',
    validate: (value) => {
      if (!value || value.trim().length === 0) {
        return 'API key is required'
      }
      return true
    },
  }))

  await loginWithApiKey(apiKey)
}

async function loginWithApiKey(apiKey: string): Promise<void> {
  const client = getClient()

  // Temporarily set the API key to validate it
  await credentials.set('apiKey', apiKey)

  try {
    const result = await withSpinner(
      'Validating API key...',
      async () => {
        const response = await client.get<{ id: number; email: string }>('/auth/me')
        return response.data
      },
      { successText: 'API key validated' }
    )

    await credentials.setAll({
      apiKey,
      userId: result.id,
      email: result.email,
    })

    newline()
    box(
      `Logged in as ${colors.bold(result.email)}
API: ${colors.muted(config.get('apiUrl'))}
Credentials stored in: ${colors.muted(credentials.path)}`,
      `${icons.sparkles} Welcome to Temps!`
    )
  } catch (err) {
    await credentials.clear()
    throw new AuthenticationError('Invalid API key')
  }
}
