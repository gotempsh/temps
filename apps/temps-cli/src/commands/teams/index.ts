import type { Command } from 'commander'
import {
  addTeamMember,
  createCustomRole,
  createTeam,
  deleteCustomRole,
  deleteTeam,
  getProjects,
  getTeam,
  grantProjectAccess,
  listCustomRoles,
  listProjectAccess,
  listTeamMembers,
  listTeamProjects,
  listTeams,
  listUsers,
  removeTeamMember,
  revokeProjectAccess,
  updateCustomRole,
  updateTeam,
  updateTeamMemberRole
} from '../../api/sdk.gen.js'
import type {
  CustomRoleResponse,
  ProjectAccessResponse,
  TeamMemberResponse,
  TeamResponse,
  TeamRole
} from '../../api/types.gen.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { colors, header, icons, info, json, keyValue, newline, success, warning } from '../../ui/output.js'
import { promptConfirm, promptSelect, promptText } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'

const TEAM_ROLES: TeamRole[] = ['owner', 'admin', 'deployer', 'viewer']

/** What each fixed role can do, shown in interactive role pickers. */
const ROLE_HINTS: Record<TeamRole, string> = {
  owner: 'everything an admin can do, plus deleting the project',
  admin: 'full control of the project, but cannot delete it',
  deployer: 'deploy and manage env vars/pipelines; no deletes',
  viewer: 'read-only'
}

interface JsonOption {
  json?: boolean
}

interface ProjectOption extends JsonOption {
  project?: string
}

export function registerTeamsCommands(program: Command): void {
  const teams = program
    .command('teams')
    .description('Manage teams and project access')

  teams
    .command('list')
    .alias('ls')
    .description('List all teams')
    .option('--json', 'Output in JSON format')
    .action(listTeamsAction)

  teams
    .command('create')
    .alias('add')
    .description('Create a new team')
    .option('-n, --name <name>', 'Team name')
    .option('-s, --slug <slug>', 'URL-safe slug ([a-z0-9-]+)')
    .option('-d, --description <description>', 'Team description')
    .option('--json', 'Output in JSON format')
    .action(createTeamAction)

  teams
    .command('show <team>')
    .description('Show a team with its members and projects')
    .option('--json', 'Output in JSON format')
    .action(showTeamAction)

  teams
    .command('update <team>')
    .description('Update a team name or description')
    .option('-n, --name <name>', 'New team name')
    .option('-d, --description <description>', 'New description')
    .option('--json', 'Output in JSON format')
    .action(updateTeamAction)

  teams
    .command('delete <team>')
    .alias('rm')
    .description('Delete a team (removes its members and project grants)')
    .option('-y, --yes', 'Skip confirmation')
    .action(deleteTeamAction)

  // --- members ---

  const members = teams
    .command('members')
    .description('Manage team membership')

  members
    .command('list <team>')
    .alias('ls')
    .description('List a team\'s members')
    .option('--json', 'Output in JSON format')
    .action(listMembersAction)

  members
    .command('add <team>')
    .description('Add a user to a team')
    .option('-u, --user <user>', 'User id or email')
    .option('-r, --role <role>', `Team role (${TEAM_ROLES.join('|')})`)
    .option('--json', 'Output in JSON format')
    .action(addMemberAction)

  members
    .command('set-role <team>')
    .description('Change a member\'s role, or assign them a custom role')
    .option('-u, --user <user>', 'User id or email')
    .option('-r, --role <role>', `Fixed team role (${TEAM_ROLES.join('|')})`)
    .option('-c, --custom-role <role>', 'Custom role id or slug (mutually exclusive with --role)')
    .option('--json', 'Output in JSON format')
    .action(setMemberRoleAction)

  members
    .command('remove <team>')
    .alias('rm')
    .description('Remove a user from a team')
    .option('-u, --user <user>', 'User id or email')
    .option('-y, --yes', 'Skip confirmation')
    .action(removeMemberAction)

  // --- project access ---

  const access = program
    .command('access')
    .description('Manage which teams can reach a project')

  access
    .command('list')
    .alias('ls')
    .description('List the teams granted access to a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(listAccessAction)

  access
    .command('grant <team>')
    .description('Grant a team access to a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-r, --role <role>', `Role the team holds on the project (${TEAM_ROLES.join('|')})`)
    .option('--json', 'Output in JSON format')
    .action(grantAccessAction)

  access
    .command('revoke <team>')
    .description('Revoke a team\'s access to a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-y, --yes', 'Skip confirmation')
    .action(revokeAccessAction)

  // --- custom roles ---

  const roles = program
    .command('custom-roles')
    .description('Manage admin-defined permission sets')

  roles
    .command('list')
    .alias('ls')
    .description('List custom roles')
    .option('--json', 'Output in JSON format')
    .action(listCustomRolesAction)

  roles
    .command('create')
    .alias('add')
    .description('Create a custom role')
    .option('-n, --name <name>', 'Role name')
    .option('-s, --slug <slug>', 'URL-safe slug ([a-z0-9-]+)')
    .option('-d, --description <description>', 'Role description')
    .option('--permissions <permissions>', 'Comma-separated permissions, e.g. deployments:read,logs:read')
    .option('--json', 'Output in JSON format')
    .action(createCustomRoleAction)

  roles
    .command('update <role>')
    .description('Update a custom role (--permissions replaces the whole set)')
    .option('-n, --name <name>', 'New name')
    .option('-d, --description <description>', 'New description')
    .option('--permissions <permissions>', 'Comma-separated permissions replacing the current set')
    .option('--json', 'Output in JSON format')
    .action(updateCustomRoleAction)

  roles
    .command('delete <role>')
    .alias('rm')
    .description('Delete a custom role (members fall back to their fixed role)')
    .option('-y, --yes', 'Skip confirmation')
    .action(deleteCustomRoleAction)
}

// ---------------------------------------------------------------------------
// Resolution helpers
//
// Every id argument accepts a human-friendly alias (team slug, user email,
// project slug) so nobody has to look up a numeric id first.
// ---------------------------------------------------------------------------

async function fetchTeams(): Promise<TeamResponse[]> {
  const { data, error } = await listTeams({ client, query: { page_size: 100 } })
  if (error) {
    throw new Error(getErrorMessage(error))
  }
  return data?.teams ?? []
}

async function resolveTeamId(teamRef: string): Promise<number> {
  if (/^\d+$/.test(teamRef)) {
    return Number(teamRef)
  }
  const teams = await fetchTeams()
  const match = teams.find((t) => t.slug === teamRef || t.name === teamRef)
  if (!match) {
    throw new Error(
      `No team matches '${teamRef}'. Known slugs: ${teams.map((t) => t.slug).join(', ') || '(none)'}`
    )
  }
  return match.id
}

async function resolveUserId(userRef: string): Promise<number> {
  if (/^\d+$/.test(userRef)) {
    return Number(userRef)
  }
  const { data, error } = await listUsers({ client, query: { include_deleted: false } })
  if (error) {
    throw new Error(getErrorMessage(error))
  }
  const users = data ?? []
  const match = users.find((u) => u.user.email === userRef || u.user.name === userRef)
  if (!match) {
    throw new Error(`No user matches '${userRef}' (expected a user id or email)`)
  }
  return match.user.id
}

/**
 * The access endpoints take a numeric project id, so a slug has to be
 * resolved before the call rather than passed through.
 */
async function resolveProjectId(flagValue?: string): Promise<number> {
  const { slug } = await requireProjectSlug(flagValue)
  if (/^\d+$/.test(slug)) {
    return Number(slug)
  }
  const { data, error } = await getProjects({ client, query: { per_page: 100 } })
  if (error) {
    throw new Error(getErrorMessage(error))
  }
  const projects = data?.projects ?? []
  const match = projects.find((p) => p.slug === slug || p.name === slug)
  if (!match) {
    throw new Error(`No project matches '${slug}'`)
  }
  return match.id
}

async function fetchCustomRoles(): Promise<CustomRoleResponse[]> {
  const { data, error } = await listCustomRoles({ client, query: { page_size: 100 } })
  if (error) {
    throw new Error(getErrorMessage(error))
  }
  return data?.roles ?? []
}

async function resolveCustomRoleId(roleRef: string): Promise<number> {
  if (/^\d+$/.test(roleRef)) {
    return Number(roleRef)
  }
  const roles = await fetchCustomRoles()
  const match = roles.find((r) => r.slug === roleRef || r.name === roleRef)
  if (!match) {
    throw new Error(
      `No custom role matches '${roleRef}'. Known slugs: ${roles.map((r) => r.slug).join(', ') || '(none)'}`
    )
  }
  return match.id
}

function parseRole(value: string): TeamRole {
  const role = value.toLowerCase() as TeamRole
  if (!TEAM_ROLES.includes(role)) {
    throw new Error(`Invalid role '${value}'. Expected one of: ${TEAM_ROLES.join(', ')}`)
  }
  return role
}

async function promptRole(message: string): Promise<TeamRole> {
  return (await promptSelect({
    message,
    choices: TEAM_ROLES.map((r) => ({ name: `${r} — ${ROLE_HINTS[r]}`, value: r }))
  })) as TeamRole
}

function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
}

// ---------------------------------------------------------------------------
// Teams
// ---------------------------------------------------------------------------

async function listTeamsAction(options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const teams = await withSpinner('Fetching teams...', fetchTeams)

  if (options.json) {
    json(teams)
    return
  }

  newline()
  header(`${icons.info} Teams (${teams.length})`)

  if (teams.length === 0) {
    info('No teams yet')
    info('Run: temps teams create --name "Platform" --slug platform')
    newline()
    return
  }

  const columns: TableColumn<TeamResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Slug', key: 'slug' },
    { header: 'Description', accessor: (t) => t.description ?? '—' }
  ]

  printTable(teams, columns, { style: 'minimal' })
  newline()
}

async function createTeamAction(options: {
  name?: string
  slug?: string
  description?: string
} & JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const name = options.name || (await promptText({ message: 'Team name', required: true }))
  const slug = options.slug || (await promptText({ message: 'Slug', default: slugify(name) }))
  const description = options.description

  const team = await withSpinner('Creating team...', async () => {
    const { data, error } = await createTeam({
      client,
      body: { name, slug, description: description ?? null }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(team)
    return
  }

  newline()
  success(`Created team '${team.name}' (id ${team.id})`)
  info(`Grant it a project: temps access grant ${team.slug} --project <project> --role deployer`)
  newline()
}

async function showTeamAction(teamRef: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching team...', async () => {
    const teamId = await resolveTeamId(teamRef)
    const [teamRes, membersRes, projectsRes] = await Promise.all([
      getTeam({ client, path: { team_id: teamId } }),
      listTeamMembers({ client, path: { team_id: teamId } }),
      listTeamProjects({ client, path: { team_id: teamId } })
    ])
    if (teamRes.error) throw new Error(getErrorMessage(teamRes.error))
    if (membersRes.error) throw new Error(getErrorMessage(membersRes.error))
    if (projectsRes.error) throw new Error(getErrorMessage(projectsRes.error))
    return {
      team: teamRes.data!,
      members: membersRes.data ?? [],
      projects: projectsRes.data ?? []
    }
  })

  if (options.json) {
    json(result)
    return
  }

  const { team, members, projects } = result

  newline()
  header(`${icons.info} ${team.name}`)
  keyValue('ID', team.id)
  keyValue('Slug', team.slug)
  keyValue('Description', team.description ?? '—')

  newline()
  header(`Members (${members.length})`)
  if (members.length === 0) {
    info('No members')
  } else {
    printTable(
      members,
      [
        { header: 'User', accessor: (m) => m.user_name ?? `#${m.user_id}` },
        { header: 'Email', accessor: (m) => m.user_email ?? '—' },
        {
          header: 'Role',
          accessor: (m) => (m.custom_role_id ? `custom #${m.custom_role_id}` : m.role)
        }
      ] as TableColumn<TeamMemberResponse>[],
      { style: 'minimal' }
    )
  }

  newline()
  header(`Projects (${projects.length})`)
  if (projects.length === 0) {
    info('This team has not been granted access to any project')
  } else {
    printTable(
      projects,
      [
        { header: 'Project ID', key: 'project_id', width: 12 },
        { header: 'Role', key: 'role' }
      ] as TableColumn<ProjectAccessResponse>[],
      { style: 'minimal' }
    )
  }
  newline()
}

async function updateTeamAction(
  teamRef: string,
  options: { name?: string; description?: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.name && options.description === undefined) {
    warning('Nothing to update — pass --name and/or --description')
    return
  }

  const team = await withSpinner('Updating team...', async () => {
    const teamId = await resolveTeamId(teamRef)
    const { data, error } = await updateTeam({
      client,
      path: { team_id: teamId },
      body: { name: options.name ?? null, description: options.description ?? null }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(team)
    return
  }

  newline()
  success(`Updated team '${team.name}'`)
  newline()
}

async function deleteTeamAction(teamRef: string, options: { yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const teamId = await resolveTeamId(teamRef)

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Delete team '${teamRef}'? Its members lose access to every project granted through it.`,
      default: false
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting team...', async () => {
    const { error } = await deleteTeam({ client, path: { team_id: teamId } })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  newline()
  success(`Deleted team '${teamRef}'`)
  newline()
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

async function listMembersAction(teamRef: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const members = await withSpinner('Fetching members...', async () => {
    const teamId = await resolveTeamId(teamRef)
    const { data, error } = await listTeamMembers({ client, path: { team_id: teamId } })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(members)
    return
  }

  newline()
  header(`${icons.info} Members of '${teamRef}' (${members.length})`)
  if (members.length === 0) {
    info('No members')
    info(`Run: temps teams members add ${teamRef} --user someone@example.com --role viewer`)
    newline()
    return
  }

  printTable(
    members,
    [
      { header: 'User ID', key: 'user_id', width: 9 },
      { header: 'Name', accessor: (m) => m.user_name ?? '—' },
      { header: 'Email', accessor: (m) => m.user_email ?? '—' },
      {
        header: 'Role',
        accessor: (m) => (m.custom_role_id ? `custom #${m.custom_role_id}` : m.role)
      }
    ] as TableColumn<TeamMemberResponse>[],
    { style: 'minimal' }
  )
  newline()
}

async function addMemberAction(
  teamRef: string,
  options: { user?: string; role?: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  const userRef = options.user || (await promptText({ message: 'User id or email', required: true }))
  const role = options.role ? parseRole(options.role) : await promptRole('Role in this team')

  const member = await withSpinner('Adding member...', async () => {
    const [teamId, userId] = await Promise.all([resolveTeamId(teamRef), resolveUserId(userRef)])
    const { data, error } = await addTeamMember({
      client,
      path: { team_id: teamId },
      body: { user_id: userId, role }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(member)
    return
  }

  newline()
  success(`Added ${member.user_email ?? `user #${member.user_id}`} to '${teamRef}' as ${role}`)
  newline()
}

async function setMemberRoleAction(
  teamRef: string,
  options: { user?: string; role?: string; customRole?: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  if (options.role && options.customRole) {
    warning('--role and --custom-role are mutually exclusive')
    return
  }

  const userRef = options.user || (await promptText({ message: 'User id or email', required: true }))

  let role: TeamRole | null = null
  let customRoleId: number | null = null

  if (options.customRole) {
    customRoleId = await resolveCustomRoleId(options.customRole)
  } else if (options.role) {
    role = parseRole(options.role)
  } else {
    role = await promptRole('New role in this team')
  }

  const member = await withSpinner('Updating member role...', async () => {
    const [teamId, userId] = await Promise.all([resolveTeamId(teamRef), resolveUserId(userRef)])
    const { data, error } = await updateTeamMemberRole({
      client,
      path: { team_id: teamId, user_id: userId },
      body: { role, custom_role_id: customRoleId }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(member)
    return
  }

  newline()
  success(
    `${member.user_email ?? `User #${member.user_id}`} is now ${
      member.custom_role_id ? `custom role #${member.custom_role_id}` : member.role
    } in '${teamRef}'`
  )
  newline()
}

async function removeMemberAction(
  teamRef: string,
  options: { user?: string; yes?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const userRef = options.user || (await promptText({ message: 'User id or email', required: true }))
  const [teamId, userId] = await Promise.all([resolveTeamId(teamRef), resolveUserId(userRef)])

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Remove ${userRef} from '${teamRef}'?`,
      default: false
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing member...', async () => {
    const { error } = await removeTeamMember({
      client,
      path: { team_id: teamId, user_id: userId }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  newline()
  success(`Removed ${userRef} from '${teamRef}'`)
  newline()
}

// ---------------------------------------------------------------------------
// Project access
// ---------------------------------------------------------------------------

async function listAccessAction(options: ProjectOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching project access...', async () => {
    const projectId = await resolveProjectId(options.project)
    const [accessRes, teams] = await Promise.all([
      listProjectAccess({ client, path: { project_id: projectId } }),
      fetchTeams()
    ])
    if (accessRes.error) {
      throw new Error(getErrorMessage(accessRes.error))
    }
    return { grants: accessRes.data ?? [], teams, projectId }
  })

  if (options.json) {
    json(result.grants)
    return
  }

  const teamName = (id: number) => result.teams.find((t) => t.id === id)?.name ?? `#${id}`

  newline()
  header(`${icons.info} Team access for project ${result.projectId} (${result.grants.length})`)
  if (result.grants.length === 0) {
    info('No team grants — this project is visible to every user with project permissions')
    info('Adding the first grant is what restricts it to specific teams')
    newline()
    return
  }

  printTable(
    result.grants,
    [
      { header: 'Team', accessor: (g) => teamName(g.team_id) },
      { header: 'Team ID', key: 'team_id', width: 9 },
      { header: 'Role', key: 'role' }
    ] as TableColumn<ProjectAccessResponse>[],
    { style: 'minimal' }
  )
  newline()
}

async function grantAccessAction(
  teamRef: string,
  options: { role?: string } & ProjectOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  const role = options.role ? parseRole(options.role) : await promptRole('Role this team holds on the project')

  const grant = await withSpinner('Granting access...', async () => {
    const [projectId, teamId] = await Promise.all([
      resolveProjectId(options.project),
      resolveTeamId(teamRef)
    ])
    const { data, error } = await grantProjectAccess({
      client,
      path: { project_id: projectId },
      body: { team_id: teamId, role }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(grant)
    return
  }

  newline()
  success(`Granted '${teamRef}' ${role} access to project ${grant.project_id}`)
  info('This project is now restricted to the teams listed in: temps access list')
  newline()
}

async function revokeAccessAction(
  teamRef: string,
  options: { yes?: boolean } & ProjectOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  const [projectId, teamId] = await Promise.all([
    resolveProjectId(options.project),
    resolveTeamId(teamRef)
  ])

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Revoke '${teamRef}' access to project ${projectId}?`,
      default: false
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Revoking access...', async () => {
    const { error } = await revokeProjectAccess({
      client,
      path: { project_id: projectId, team_id: teamId }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  newline()
  success(`Revoked '${teamRef}' access to project ${projectId}`)
  warning('Removing the last grant makes this project unrestricted again')
  newline()
}

// ---------------------------------------------------------------------------
// Custom roles
// ---------------------------------------------------------------------------

async function listCustomRolesAction(options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const roles = await withSpinner('Fetching custom roles...', fetchCustomRoles)

  if (options.json) {
    json(roles)
    return
  }

  newline()
  header(`${icons.info} Custom roles (${roles.length})`)
  if (roles.length === 0) {
    info('No custom roles')
    info('Run: temps custom-roles create --name "Release Manager" --permissions deployments:create,deployments:read')
    newline()
    return
  }

  printTable(
    roles,
    [
      { header: 'ID', key: 'id', width: 6 },
      { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
      { header: 'Slug', key: 'slug' },
      { header: 'Permissions', accessor: (r) => String(r.permissions.length) }
    ] as TableColumn<CustomRoleResponse>[],
    { style: 'minimal' }
  )
  newline()
}

async function createCustomRoleAction(options: {
  name?: string
  slug?: string
  description?: string
  permissions?: string
} & JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()

  const name = options.name || (await promptText({ message: 'Role name', required: true }))
  const slug = options.slug || (await promptText({ message: 'Slug', default: slugify(name) }))
  const permissionsInput =
    options.permissions ||
    (await promptText({
      message: 'Permissions (comma-separated, e.g. deployments:read,logs:read)',
      required: true
    }))

  const permissions = permissionsInput
    .split(',')
    .map((p) => p.trim())
    .filter(Boolean)

  const role = await withSpinner('Creating custom role...', async () => {
    const { data, error } = await createCustomRole({
      client,
      body: { name, slug, description: options.description ?? null, permissions }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(role)
    return
  }

  newline()
  success(`Created custom role '${role.name}' (id ${role.id}) with ${role.permissions.length} permission(s)`)
  info(`Assign it: temps teams members set-role <team> --user <email> --custom-role ${role.slug}`)
  newline()
}

async function updateCustomRoleAction(
  roleRef: string,
  options: { name?: string; description?: string; permissions?: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.name && options.description === undefined && !options.permissions) {
    warning('Nothing to update — pass --name, --description and/or --permissions')
    return
  }

  const permissions = options.permissions
    ? options.permissions.split(',').map((p) => p.trim()).filter(Boolean)
    : null

  const role = await withSpinner('Updating custom role...', async () => {
    const roleId = await resolveCustomRoleId(roleRef)
    const { data, error } = await updateCustomRole({
      client,
      path: { role_id: roleId },
      body: {
        name: options.name ?? null,
        description: options.description ?? null,
        permissions
      }
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data!
  })

  if (options.json) {
    json(role)
    return
  }

  newline()
  success(`Updated custom role '${role.name}' (${role.permissions.length} permission(s))`)
  newline()
}

async function deleteCustomRoleAction(
  roleRef: string,
  options: { yes?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  const roleId = await resolveCustomRoleId(roleRef)

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Delete custom role '${roleRef}'? Members holding it fall back to their fixed team role.`,
      default: false
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting custom role...', async () => {
    const { error } = await deleteCustomRole({ client, path: { role_id: roleId } })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  newline()
  success(`Deleted custom role '${roleRef}'`)
  newline()
}
