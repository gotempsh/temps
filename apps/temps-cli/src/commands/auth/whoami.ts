import { credentials, config } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, newline, keyValue, header, icons, json } from '../../ui/output.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { getCurrentUser } from '../../api/sdk.gen.js'

interface WhoamiOptions {
  json?: boolean
}

export async function whoami(options?: WhoamiOptions): Promise<void> {
  if (!(await credentials.isAuthenticated())) {
    info('Not logged in. Run "temps login" to authenticate.')
    return
  }

  await setupClient()

  const user = await withSpinner('Fetching user info...', async () => {
    const { data, error } = await getCurrentUser({ client })

    if (error) {
      throw new Error(getErrorMessage(error))
    }

    return data
  })

  if (options?.json) {
    json(user)
    return
  }

  if (!user) {
    info('No user found')
    return
  }

  newline()
  header(`${icons.key} Current User`)
  keyValue('Email', user.email ?? 'N/A')
  keyValue('Name', user.name)
  keyValue('Username', user.username)
  keyValue('User ID', user.id)
  keyValue('MFA Enabled', user.mfa_enabled ? 'Yes' : 'No')
  keyValue('API URL', config.get('apiUrl'))
  keyValue('Credentials', credentials.path)
  newline()
}
