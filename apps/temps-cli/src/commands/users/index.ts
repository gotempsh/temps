import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listUsers,
  createUser,
  deleteUser,
  updateUser,
  restoreUser,
  assignRole,
  removeRole,
  getCurrentUser,
} from '../../api/sdk.gen.js'
import type { RouteUserWithRoles } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptConfirm, promptSelect } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

const AVAILABLE_ROLES = ['admin', 'developer', 'viewer']

interface CreateOptions {
  username?: string
  email?: string
  password?: string
  roles?: string
  yes?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface RestoreOptions {
  id: string
}

interface RoleOptions {
  id: string
  add?: string
  remove?: string
}

export function registerUsersCommands(program: Command): void {
  const users = program
    .command('users')
    .description('Manage platform users')

  users
    .command('list')
    .alias('ls')
    .description('List all users')
    .option('--json', 'Output in JSON format')
    .action(listUsersAction)

  users
    .command('create')
    .alias('add')
    .description('Create a new user')
    .option('-u, --username <username>', 'Username')
    .option('-e, --email <email>', 'Email address')
    .option('-p, --password <password>', 'Password (if not provided, invite email will be sent)')
    .option('-r, --roles <roles>', 'Comma-separated roles (admin, developer, viewer)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createUserAction)

  users
    .command('me')
    .description('Show current user info')
    .option('--json', 'Output in JSON format')
    .action(showCurrentUser)

  users
    .command('remove')
    .alias('rm')
    .description('Remove a user')
    .requiredOption('--id <id>', 'User ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeUser)

  users
    .command('restore')
    .description('Restore a deleted user')
    .requiredOption('--id <id>', 'User ID')
    .action(restoreUserAction)

  users
    .command('role')
    .description('Manage user roles')
    .requiredOption('--id <id>', 'User ID')
    .option('--add <role>', 'Add a role to user')
    .option('--remove <role>', 'Remove a role from user')
    .action(manageRoles)
}

async function listUsersAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const usersData = await withSpinner('Fetching users...', async () => {
    const { data, error } = await listUsers({
      client,
      query: {
        include_deleted: false,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(usersData)
    return
  }

  newline()
  header(`${icons.info} Users (${usersData.length})`)

  if (usersData.length === 0) {
    info('No users found')
    newline()
    return
  }

  const columns: TableColumn<RouteUserWithRoles>[] = [
    { header: 'ID', accessor: (u) => u.user.id.toString(), width: 6 },
    { header: 'Username', accessor: (u) => u.user.username, color: (v) => colors.bold(v) },
    { header: 'Email', accessor: (u) => u.user.email || '-' },
    { header: 'Roles', accessor: (u) => u.roles.map(r => r.name).join(', ') || 'None' },
    { header: 'MFA', accessor: (u) => u.user.mfa_enabled ? 'Yes' : 'No', color: (v) => v === 'Yes' ? colors.success(v) : colors.muted(v) },
    { header: 'Status', accessor: (u) => u.user.deleted_at ? 'deleted' : 'active', color: (v) => statusBadge(v === 'active' ? 'active' : 'inactive') },
  ]

  printTable(usersData, columns, { style: 'minimal' })
  newline()
}

async function createUserAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let username: string
  let email: string | null = null
  let password: string | null = null
  let selectedRoles: string[] = []

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.username

  if (isAutomation) {
    username = options.username!
    email = options.email || null
    password = options.password || null

    if (options.roles) {
      selectedRoles = options.roles.split(',').map(r => r.trim().toLowerCase())
      // Validate roles
      for (const role of selectedRoles) {
        if (!AVAILABLE_ROLES.includes(role)) {
          warning(`Invalid role: ${role}. Available roles: ${AVAILABLE_ROLES.join(', ')}`)
          return
        }
      }
    }

    if (selectedRoles.length === 0) {
      selectedRoles = ['viewer'] // Default role
    }
  } else {
    // Interactive mode
    username = options.username || await promptText({
      message: 'Username',
      required: true,
    })

    email = options.email || await promptText({
      message: 'Email (optional)',
      default: '',
    }) || null

    password = options.password || await promptPassword({
      message: 'Password (leave blank to send invite email)',
    }) || null

    let addMore = true

    while (addMore) {
      const role = await promptSelect({
        message: 'Add role',
        choices: AVAILABLE_ROLES.filter(r => !selectedRoles.includes(r)).map(r => ({
          name: r.charAt(0).toUpperCase() + r.slice(1),
          value: r,
        })),
      })
      selectedRoles.push(role)

      if (selectedRoles.length < AVAILABLE_ROLES.length) {
        addMore = await promptConfirm({
          message: 'Add another role?',
          default: false,
        })
      } else {
        addMore = false
      }
    }
  }

  await withSpinner('Creating user...', async () => {
    const { error } = await createUser({
      client,
      body: {
        username,
        email,
        password,
        roles: selectedRoles,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`User "${username}" created successfully`)
  if (!password) {
    info('An invitation email will be sent to the user')
  }
}

async function showCurrentUser(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const user = await withSpinner('Fetching user info...', async () => {
    const { data, error } = await getCurrentUser({ client })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? 'Failed to get current user')
    }
    return data
  })

  if (options.json) {
    json(user)
    return
  }

  newline()
  header(`${icons.info} Current User`)
  keyValue('ID', user.id)
  keyValue('Username', user.username)
  keyValue('Name', user.name || colors.muted('Not set'))
  keyValue('Email', user.email || colors.muted('Not set'))
  keyValue('MFA Enabled', user.mfa_enabled ? colors.success('Yes') : colors.muted('No'))
  newline()
}

async function removeUser(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid user ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('This will delete the user and all associated data!')
    const confirmed = await promptConfirm({
      message: `Delete user with ID ${id}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting user...', async () => {
    const { error } = await deleteUser({
      client,
      path: { user_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('User deleted')
  info(`The user can be restored using: temps users restore --id ${id}`)
}

async function restoreUserAction(options: RestoreOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid user ID')
    return
  }

  await withSpinner('Restoring user...', async () => {
    const { error } = await restoreUser({
      client,
      path: { user_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('User restored')
}

async function manageRoles(options: RoleOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid user ID')
    return
  }

  if (options.add) {
    if (!AVAILABLE_ROLES.includes(options.add)) {
      warning(`Invalid role: ${options.add}. Available roles: ${AVAILABLE_ROLES.join(', ')}`)
      return
    }

    await withSpinner(`Adding role "${options.add}"...`, async () => {
      const { error } = await assignRole({
        client,
        path: { user_id: id },
        body: {
          role_type: options.add!,
          user_id: id,
        },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
    })

    success(`Role "${options.add}" added to user ${id}`)
  }

  if (options.remove) {
    if (!AVAILABLE_ROLES.includes(options.remove)) {
      warning(`Invalid role: ${options.remove}. Available roles: ${AVAILABLE_ROLES.join(', ')}`)
      return
    }

    await withSpinner(`Removing role "${options.remove}"...`, async () => {
      const { error } = await removeRole({
        client,
        path: {
          user_id: id,
          role_type: options.remove!,
        },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
    })

    success(`Role "${options.remove}" removed from user ${id}`)
  }

  if (!options.add && !options.remove) {
    info('Usage: temps users role --id <user_id> --add <role> or --remove <role>')
    info(`Available roles: ${AVAILABLE_ROLES.join(', ')}`)
  }
}
