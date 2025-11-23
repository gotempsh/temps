import { credentials, config } from '../../config/store.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, colors, newline, keyValue, header, icons, json } from '../../ui/output.js'
import { getClient } from '../../api/client.js'
import { AuthenticationError } from '../../utils/errors.js'

interface WhoamiOptions {
  json?: boolean
}

export async function whoami(options?: WhoamiOptions): Promise<void> {
  if (!(await credentials.isAuthenticated())) {
    info('Not logged in. Run "temps login" to authenticate.')
    return
  }

  const client = getClient()

  const user = await withSpinner('Fetching user info...', async () => {
    const response = await client.get<{
      id: number
      email: string
      name?: string
      created_at?: string
    }>('/auth/me')

    return response.data
  })

  if (options?.json) {
    json(user)
    return
  }

  newline()
  header(`${icons.key} Current User`)
  keyValue('Email', user.email)
  keyValue('Name', user.name)
  keyValue('User ID', user.id)
  keyValue('API URL', config.get('apiUrl'))
  keyValue('Credentials', credentials.path)
  newline()
}
