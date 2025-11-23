import { credentials } from '../../config/store.js'
import { success, info, newline } from '../../ui/output.js'

export async function logout(): Promise<void> {
  newline()

  if (!(await credentials.isAuthenticated())) {
    info('Not currently logged in')
    return
  }

  const email = await credentials.get('email')
  await credentials.clear()

  success(`Logged out${email ? ` from ${email}` : ''}`)
}
