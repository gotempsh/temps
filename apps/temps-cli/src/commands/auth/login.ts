import { credentials, config } from '../../config/store.js'
import { promptPassword } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, icons, colors, newline, box } from '../../ui/output.js'
import { setupClient, client } from '../../lib/api-client.js'
import { getCurrentUser } from '../../api/sdk.gen.js'
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
  // Temporarily set the API key to validate it
  await credentials.set('apiKey', apiKey)

  // Setup client with the new credentials
  await setupClient()

  try {
    const result = await withSpinner(
      'Validating API key...',
      async () => {
        const { data, error } = await getCurrentUser({ client })
        if (error) {
          throw new AuthenticationError('Invalid API key')
        }
        return data
      },
      { successText: 'API key validated' }
    )
    if (!result) {
      throw new AuthenticationError('Invalid API key')
    }

    await credentials.setAll({
      apiKey,
      userId: result.id,
      email: result.email ?? undefined,
    })

    newline()
    const lines = [
      result.email ? `Logged in as ${colors.bold(result.email)}` : null,
      `API: ${colors.muted(config.get('apiUrl'))}`,
      `Credentials stored in: ${colors.muted(credentials.path)}`,
    ].filter(Boolean).join('\n')
    box(lines, `${icons.sparkles} Welcome to Temps!`)
  } catch (err) {
    await credentials.clear()
    throw new AuthenticationError('Invalid API key')
  }
}
