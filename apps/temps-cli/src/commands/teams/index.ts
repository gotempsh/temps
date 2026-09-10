// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import {
  addTeamMember,
  createTeam,
  deleteTeam,
  getProjects,
  getTeam,
  grantProjectAccess,
  listProjectAccess,
  listTeamMembers,
  listTeamProjects,
  listTeams,
  listUsers,
  removeTeamMember,
  revokeProjectAccess,
  updateTeam,
  updateTeamMemberRole
} from '../../api/sdk.gen.js'
import type {
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

/**
 * What each role actually restricts today — deliberately not "read-only" /
 * "full control". Roles narrow deployments, environments and env vars,
 * project settings, domains, and project deletion; anything outside that
 * set still follows the member's instance-wide role. Overstating a role
 * changes what an operator grants, so the labels state the enforced scope.
 */
const ROLE_HINTS: Record<TeamRole, string> = {
  owner: 'deploy, manage env vars/settings/domains, and delete the project',
  admin: 'deploy, manage env vars/settings/domains; cannot delete the project',
  deployer: 'deploy and manage env vars; cannot change settings or domains',
  viewer: 'cannot deploy, or change env vars, settings or domains'
}

/** Printed alongside interactive role pickers. */
const ROLE_ENFORCEMENT_NOTE =
  'Roles restrict deployments, env vars, project settings, domains and ' +
  "project deletion. Other actions follow the member's instance-wide role."

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
    .description('Change a member\'s role in the team')
    .option('-u, --user <user>', 'User id or email')
    .option('-r, --role <role>', `Team role (${TEAM_ROLES.join('|')})`)
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

export async function resolveTeamId(teamRef: string): Promise<number> {
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

export async function resolveUserId(userRef: string): Promise<number> {
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
export async function resolveProjectId(flagValue?: string): Promise<number> {
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

export function parseRole(value: string): TeamRole {
  const role = value.toLowerCase() as TeamRole
  if (!TEAM_ROLES.includes(role)) {
    throw new Error(`Invalid role '${value}'. Expected one of: ${TEAM_ROLES.join(', ')}`)
  }
  return role
}

async function promptRole(message: string): Promise<TeamRole> {
  info(ROLE_ENFORCEMENT_NOTE)
  return (await promptSelect({
    message,
    choices: TEAM_ROLES.map((r) => ({ name: `${r} — ${ROLE_HINTS[r]}`, value: r }))
  })) as TeamRole
}

export function slugify(name: string): string {
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
    info('Run: bunx @temps-sdk/cli teams create --name "Platform" --slug platform')
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
  info(
    `Grant it a project: bunx @temps-sdk/cli access grant ${team.slug} --project <project> --role deployer`
  )
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
        { header: 'Role', key: 'role' }
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
    info(
      `Run: bunx @temps-sdk/cli teams members add ${teamRef} --user someone@example.com --role viewer`
    )
    newline()
    return
  }

  printTable(
    members,
    [
      { header: 'User ID', key: 'user_id', width: 9 },
      { header: 'Name', accessor: (m) => m.user_name ?? '—' },
      { header: 'Email', accessor: (m) => m.user_email ?? '—' },
      { header: 'Role', key: 'role' }
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
  options: { user?: string; role?: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  const userRef = options.user || (await promptText({ message: 'User id or email', required: true }))
  const role = options.role ? parseRole(options.role) : await promptRole('New role in this team')

  const member = await withSpinner('Updating member role...', async () => {
    const [teamId, userId] = await Promise.all([resolveTeamId(teamRef), resolveUserId(userRef)])
    const { data, error } = await updateTeamMemberRole({
      client,
      path: { team_id: teamId, user_id: userId },
      body: { role }
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
    `${member.user_email ?? `User #${member.user_id}`} is now ${member.role} in '${teamRef}'`
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
  info(
    'This project is now restricted to the teams listed in: bunx @temps-sdk/cli access list'
  )
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
