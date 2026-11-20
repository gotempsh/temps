// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Project groups: several services (what the `projects` commands manage)
 * gathered into one unit that shares a repository, environment variables and
 * path routes. The console calls a group a "Project" and each member a
 * "Service". Not to be confused with the `services` command, which manages
 * databases, caches and storage.
 */

import type { Command } from 'commander'
import {
  addProjectToGroup,
  getInheritedEnvVars,
  clearGroupRepository,
  createGroupEnvVar,
  createGroupRoute,
  createProjectGroup,
  deleteGroupEnvVar,
  deleteGroupRoute,
  deleteProjectGroup,
  getGroupRepository,
  getProjectBySlug,
  getProjectGroup,
  getProjectGroupBySlug,
  getProjects,
  getRepositoryPresetLive,
  importProjectServices,
  detectPublicPresets,
  listGroupEnvVars,
  listGroupRoutes,
  listProjectGroups,
  removeProjectFromGroup,
  setGroupRepository,
  setRepositoryInheritance,
  updateGroupEnvVar,
  updateProjectGroup
} from '../../api/sdk.gen.js'
import type {
  ImportedApp,
  InheritedEnvVarResponse,
  InheritedEnvVarsResponse,
  GroupEnvVarResponse,
  GroupRouteResponse,
  GroupRepository,
  ProjectGroupMemberResponse,
  ProjectGroupResponse,
  PresetInfo,
  ProjectPresetResponse,
  ProjectResponse,
  SetGroupRepositoryResponse,
} from '../../api/types.gen.js'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { colors, header, icons, info, json, keyValue, newline, success, warning } from '../../ui/output.js'
import { promptConfirm, promptPassword, promptText } from '../../ui/prompts.js'
import { setQuietMode, withSpinner } from '../../ui/spinner.js'
import { parsePositiveInt, report, waitForInstantiation, waitTimeoutMs } from '../instantiations/index.js'
import { printTable, type TableColumn } from '../../ui/table.js'

const CLI = 'bunx @temps-sdk/cli'

interface JsonOption {
  json?: boolean
}

function unwrap<T>(result: { data?: T; error?: unknown }): T {
  if (result.error) {
    throw new Error(getErrorMessage(result.error))
  }
  return result.data as T
}

/** Accepts a group id or slug. */
export async function resolveGroup(ref: string): Promise<ProjectGroupResponse> {
  if (/^\d+$/.test(ref)) {
    return unwrap(await getProjectGroup({ client, path: { group_id: Number(ref) } }))
  }
  return unwrap(await getProjectGroupBySlug({ client, path: { slug: ref } }))
}

/** Accepts a service (project) id or slug. */
export async function resolveServiceId(ref: string): Promise<number> {
  if (/^\d+$/.test(ref)) {
    return Number(ref)
  }
  const project = unwrap(await getProjectBySlug({ client, path: { slug: ref } }))
  return project.id
}

/** `production` → `production`; empty/`all` → every environment. */
export function normalizeScope(environment?: string): string | null {
  const value = environment?.trim().toLowerCase()
  return !value || value === 'all' ? null : value
}

export function findVariable(
  vars: GroupEnvVarResponse[],
  key: string,
  scope: string | null
): GroupEnvVarResponse | undefined {
  return vars.find((v) => v.key === key && (v.environment_slug ?? null) === scope)
}

/** `./apps/web/` → `apps/web`; the repository root is `.`. */
export function normalizeDirectory(path: string | null | undefined): string {
  const trimmed = (path ?? '').trim().replace(/^\.\/+/, '').replace(/\/+$/, '')
  return trimmed === '' || trimmed === '.' ? '.' : trimmed
}

/** `owner/name` → parts; GitLab subgroups keep the owner's slashes. */
export function parseRepoRef(ref: string): { owner: string; name: string } {
  const value = ref.trim().replace(/\.git$/, '')
  const cut = value.lastIndexOf('/')
  if (cut <= 0 || cut === value.length - 1) {
    throw new Error(`Expected a repository as owner/name, got "${ref}"`)
  }
  return { owner: value.slice(0, cut), name: value.slice(cut + 1) }
}

const PUBLIC_HOSTS: Record<string, 'github' | 'gitlab'> = {
  'github.com': 'github',
  'gitlab.com': 'gitlab',
}

/**
 * Where `repo set` reads the repository from: a connection, or a public
 * GitHub/GitLab repository (named by `--public`, or implied by a
 * github.com/gitlab.com URL passed as `--repo`).
 */
export function resolveRepositorySource(options: {
  connection?: string
  public?: string
  repo: string
}): { connectionId?: number; publicProvider?: 'github' | 'gitlab'; owner: string; name: string } {
  let repo = options.repo.trim()
  let urlProvider: 'github' | 'gitlab' | undefined
  // A URL has a scheme, or starts with one of the known hosts; anything
  // else is owner/name (whose owner may contain dots on GitLab).
  const url =
    repo.match(/^https?:\/\/([^/]+)\/(.+)$/i) ?? repo.match(/^(github\.com|gitlab\.com)\/(.+)$/i)
  if (url) {
    const [, host = '', path = ''] = url
    urlProvider = PUBLIC_HOSTS[host.toLowerCase()]
    if (!urlProvider) {
      throw new Error(
        `Only github.com and gitlab.com repositories can be used without a connection; for ${host}, pass --connection <id> and --repo owner/name`
      )
    }
    repo = path.replace(/\/$/, '')
  }
  const { owner, name } = parseRepoRef(repo)
  if (options.connection !== undefined) {
    if (options.public !== undefined) {
      throw new Error('Pass either --connection or --public, not both')
    }
    const connectionId = Number(options.connection)
    if (!Number.isInteger(connectionId) || connectionId <= 0) {
      throw new Error(`--connection must be a git connection id, got "${options.connection}"`)
    }
    return { connectionId, owner, name }
  }
  const publicProvider = (options.public?.toLowerCase() ?? urlProvider) as
    | 'github'
    | 'gitlab'
    | undefined
  if (!publicProvider) {
    throw new Error(
      'Pass --connection <id> for a connected repository, or --public github|gitlab (or a github.com/gitlab.com URL as --repo) for a public one'
    )
  }
  if (publicProvider !== 'github' && publicProvider !== 'gitlab') {
    throw new Error(`--public must be github or gitlab, got "${options.public}"`)
  }
  return { publicProvider, owner, name }
}

/** A service name for an app at `path`: its folder name (repo name at the root), suffixed when taken. */
export function suggestServiceName(path: string, repoName: string, taken: ReadonlySet<string>): string {
  const dir = normalizeDirectory(path)
  const base =
    (dir === '.' ? repoName : dir.split('/').pop() || repoName)
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, '-')
      .replace(/^-+|-+$/g, '') || 'service'
  if (!taken.has(base)) return base
  for (let i = 2; ; i++) {
    if (!taken.has(`${base}-${i}`)) return `${base}-${i}`
  }
}

/** An app from the public-repository scan, in the connected scan's shape. */
export function fromPublicPreset(p: PresetInfo): ProjectPresetResponse {
  return {
    path: p.path,
    preset: p.preset,
    presetLabel: p.preset_label,
    projectType: p.project_type,
    exposedPort: p.exposed_port,
    dockerfilePath: p.dockerfile_path,
    composeFiles: p.compose_files,
    iconUrl: p.icon_url,
  }
}

/**
 * An app of the project's repository, as the import endpoint takes it: the
 * service builds from the app's directory on the project's branch.
 */
export function importedApp(app: ProjectPresetResponse, name: string): ImportedApp {
  return {
    name,
    directory: normalizeDirectory(app.path),
    preset: app.preset,
    exposed_port: app.exposedPort ?? undefined,
    preset_config:
      app.preset === 'dockerfile' && app.dockerfilePath
        ? { dockerfilePath: app.dockerfilePath }
        : app.preset === 'docker-compose'
          ? { composePath: app.composeFiles?.[0] ?? 'docker-compose.yml' }
          : undefined,
  }
}

/** A service of `group` by id, slug or name. */
export function findMember(
  group: Pick<ProjectGroupResponse, 'name' | 'projects'>,
  ref: string
): ProjectGroupMemberResponse {
  const needle = ref.trim().toLowerCase()
  const member = group.projects.find(
    (p) => String(p.id) === needle || p.slug.toLowerCase() === needle || p.name.toLowerCase() === needle
  )
  if (!member) {
    const names = group.projects.map((p) => p.slug).join(', ') || 'none'
    throw new Error(`${ref} is not a service of ${group.name} (services: ${names})`)
  }
  return member
}

/** `production → production`, `preview-x → (not deployed)`, one per environment. */
export function describeRouteEnvironments(route: GroupRouteResponse): string {
  if (route.environments.length === 0) return 'no environments yet'
  return route.environments
    .map((e) => {
      if (!e.target_environment_slug) return `${e.source_environment_slug} → (no matching environment)`
      const state = e.target_deployed ? '' : ' (not deployed)'
      return `${e.source_environment_slug} → ${e.target_environment_slug}${state}`
    })
    .join(', ')
}

function describeScope(v: { environment_slug?: string | null }): string {
  return v.environment_slug ?? 'all environments'
}

/** The services of a `repo set` that could not be pointed at the new repository. */
export function failedRepoSync(result: Pick<SetGroupRepositoryResponse, 'services'>): SetGroupRepositoryResponse['services'] {
  return result.services.filter((s) => s.error)
}

/**
 * Every item of a paginated list, page by page. Stops at `total`, on an empty
 * page, or after `maxPages` (a guard against a server that never ends).
 */
export async function fetchAllPages<T>(
  loadPage: (page: number, perPage: number) => Promise<{ items: T[]; total: number }>,
  perPage = 100,
  maxPages = 1_000
): Promise<T[]> {
  const all: T[] = []
  for (let page = 1; page <= maxPages; page++) {
    const { items, total } = await loadPage(page, perPage)
    all.push(...items)
    if (items.length === 0 || all.length >= total) return all
  }
  throw new Error(`Stopped after ${maxPages} pages of ${perPage}; the list did not end`)
}

/** Every service on the server (the import needs them all to avoid duplicates). */
async function fetchAllServices(): Promise<ProjectResponse[]> {
  return fetchAllPages(async (page, perPage) => {
    const list = unwrap(await getProjects({ client, query: { page, per_page: perPage } }))
    return { items: list.projects, total: list.total }
  })
}

/**
 * Variable names are case-sensitive on the server, so `log_level` and
 * `LOG_LEVEL` are two variables. Names are sent as typed; this only says so
 * when a name is not all upper case, which is usually a typo.
 */
export function caseHint(key: string): string | undefined {
  return key !== key.toUpperCase()
    ? `Variable names are case-sensitive: ${key} and ${key.toUpperCase()} are different variables`
    : undefined
}

/** A value piped on stdin, minus the one line break `echo` or a file adds. */
export function stripFinalNewline(value: string): string {
  return value.replace(/\r?\n$/, '')
}

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = []
  for await (const chunk of process.stdin) chunks.push(Buffer.from(chunk))
  return stripFinalNewline(Buffer.concat(chunks).toString('utf8'))
}

/**
 * The value for `vars set`: the argument, stdin with --value-stdin, or a
 * hidden prompt, so a secret never has to be typed on the command line where
 * shell history and `ps` can see it. Without a terminal to prompt on, a
 * missing value is an error rather than an empty variable.
 */
async function resolveValue(key: string, value: string | undefined, options: { valueStdin?: boolean }): Promise<string> {
  if (value !== undefined) {
    if (options.valueStdin) throw new Error('Pass the value as an argument or with --value-stdin, not both')
    return value
  }
  if (options.valueStdin) return readStdin()
  if (!process.stdin.isTTY) {
    throw new Error(`No value for ${key}: pass it as an argument, or pipe it with --value-stdin`)
  }
  return promptPassword({ message: `Value for ${key} (hidden)` })
}

/** What `projects show` and `projects group` say about a service's project (group). */
export function describeMembership(group: ProjectGroupResponse | null | undefined): string {
  if (!group) return 'none (standalone service)'
  return `${group.name} (${group.slug})`
}

/** One row per inherited variable: where it applies and whether the service overrides it. */
export function describeInherited(v: InheritedEnvVarResponse): { scope: string; value: string; previews: string; effect: string } {
  return {
    scope: describeScope(v),
    value: v.is_secret ? '(secret)' : '(shared)',
    previews: v.environment_slug ? '—' : v.include_in_preview ? 'yes' : 'no',
    effect: v.defined_on_project ? "overridden by the service's own variable" : 'applies',
  }
}

export function registerProjectGroupsCommands(program: Command): void {
  const groups = program
    .command('project-groups')
    .alias('pg')
    .description(
      'Manage Projects (what the console calls a Project): groups of services that ship together and share a repository, variables and path routes. ' +
        'A service is what the `projects` command manages; the `services` command manages databases and caches, not these.'
    )

  groups
    .command('list')
    .alias('ls')
    .description('List projects (groups) and their services')
    .option('--page <n>', 'Page number (default 1)')
    .option('--per-page <n>', 'How many per page (default 100, at most 100)')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  groups
    .command('show <group>')
    .alias('get')
    .description('Show a project (group), by slug or id')
    .option('--json', 'Output in JSON format')
    .action(showAction)

  groups
    .command('create')
    .alias('new')
    .description('Create a project (group), optionally with services')
    .option('-n, --name <name>', 'Project name')
    .option('-s, --slug <slug>', 'URL slug (derived from the name when omitted)')
    .option('-d, --description <text>', 'What the services do together')
    .option(
      '--service <service...>',
      'Services to put in the project, by slug or id as listed by `projects list` (apps, not the databases of the `services` command)'
    )
    .option('--json', 'Output in JSON format')
    .action(createAction)

  groups
    .command('update <group>')
    .alias('edit')
    .description('Rename a project (group) or change its description')
    .option('-n, --name <name>', 'New name')
    .option('-s, --slug <slug>', 'New slug')
    .option('-d, --description <text>', 'New description ("" clears it)')
    .option('--json', 'Output in JSON format')
    .action(updateAction)

  groups
    .command('delete <group>')
    .alias('rm')
    .description('Delete a project (group); its services keep running as standalone services')
    .option('-y, --yes', 'Skip confirmation')
    .action(deleteAction)

  groups
    .command('add <group> <services...>')
    .description(
      'Add services to a project (group), by slug or id as listed by `projects list`; moves them out of any other project'
    )
    .action(addAction)

  groups
    .command('remove <group> <service>')
    .description('Remove a service (slug or id) from a project (group); it becomes a standalone service')
    .action(removeAction)

  const repo = groups
    .command('repo')
    .description('The repository a project deploys its services from (monorepos)')

  repo
    .command('show <group>')
    .description("Show the project's repository and which services follow it")
    .option('--json', 'Output in JSON format')
    .action(repoShowAction)

  repo
    .command('set <group>')
    .description(
      'Point the project at a repository and branch; services that follow it are updated to match'
    )
    .option('-c, --connection <id>', 'Git connection id (see: git-providers / connections)')
    .option('--public <provider>', 'Use a public repository without a connection: github or gitlab')
    .requiredOption(
      '-r, --repo <owner/name|url>',
      'Repository, e.g. acme/shop; a github.com/gitlab.com URL implies --public'
    )
    .requiredOption('-b, --branch <branch>', 'Branch to deploy from, e.g. main')
    .option('--json', 'Output in JSON format')
    .action(repoSetAction)

  repo
    .command('unset <group>')
    .description('Remove the repository; services keep their current settings and stop following')
    .option('-y, --yes', 'Skip confirmation')
    .action(repoUnsetAction)

  groups
    .command('import <group>')
    .description("Scan the project's repository and add its apps as services that follow it")
    .option('-p, --path <dir...>', 'Only these app directories (default: every app without a service)')
    .option('--dry-run', 'List what would be created without creating anything')
    .option('--timeout <minutes>', 'Stop waiting after this long (default 30); the import keeps running on the server')
    .option('--json', 'Output in JSON format')
    .action(importAction)

  groups
    .command('follow <group> <service>')
    .description("Make a service follow the project's repository and branch (keeps its directory)")
    .action((group: string, service: string) => followAction(group, service, true))

  groups
    .command('unfollow <group> <service>')
    .description("Stop a service following the project's repository; it keeps its current settings")
    .action((group: string, service: string) => followAction(group, service, false))

  const routes = groups
    .command('routes')
    .description("Serve a path of one service's domains from another service (e.g. /api → backend)")

  routes
    .command('list <group>')
    .alias('ls')
    .description('List path routes and how each applies per environment')
    .option('--json', 'Output in JSON format')
    .action(routesListAction)

  routes
    .command('add <group> <source> <prefix> <target>')
    .description(
      'Send requests under <prefix> on <source>\'s domains to <target>, e.g. `routes add shop web /api api`'
    )
    .option('--strip-prefix', 'Forward /api/users as /users (default: keep the full path)')
    .option('--json', 'Output in JSON format')
    .action(routesAddAction)

  routes
    .command('remove <group> <source> <prefix>')
    .alias('rm')
    .description('Stop routing <prefix>; <source> serves it itself again')
    .option('-y, --yes', 'Skip confirmation')
    .action(routesRemoveAction)

  const vars = groups
    .command('vars')
    .description('Variables shared by every service in a project (group)')

  vars
    .command('list <group>')
    .alias('ls')
    .description('List shared variables (secret values are never shown)')
    .option('--json', 'Output in JSON format')
    .action(varsListAction)

  vars
    .command('set <group> <key> [value]')
    .description(
      "Create or update a shared variable; a service's own variable still wins. " +
        'Omit the value to type it at a hidden prompt, or pass --value-stdin to pipe it (keeps secrets out of shell history)'
    )
    .option('-e, --environment <slug>', 'Only this environment (e.g. production); default: all')
    .option('--secret', 'Write-only: the value can never be read back')
    .option('--value-stdin', 'Read the value from stdin, e.g. `cat key.pem | ... vars set shop TLS_KEY --secret --value-stdin`')
    .option('--preview', 'Also send an all-environments variable to preview deployments (off by default)')
    .option('--no-preview', 'Do not send it to preview deployments')
    .option('--json', 'Output in JSON format')
    .action(varsSetAction)

  vars
    .command('inherited <service>')
    .description(
      "Show the shared variables a service (slug or id) receives from its project, and which of them the service's own variables override"
    )
    .option('--json', 'Output in JSON format')
    .action(varsInheritedAction)

  vars
    .command('unset <group> <key>')
    .description('Remove a shared variable')
    .option('-e, --environment <slug>', 'The environment it was scoped to; default: all')
    .option('-y, --yes', 'Skip confirmation')
    .action(varsUnsetAction)
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async function listAction(options: { page?: string; perPage?: string } & JsonOption): Promise<void> {
  const page = options.page === undefined ? 1 : parsePositiveInt(options.page, '--page')
  const perPage = options.perPage === undefined ? 100 : parsePositiveInt(options.perPage, '--per-page')
  if (perPage > 100) throw new Error(`--per-page can be at most 100, got ${perPage}`)
  await requireAuth()
  await setupClient()
  const data = await withSpinner('Fetching projects...', async () =>
    unwrap(await listProjectGroups({ client, query: { page, page_size: perPage } }))
  )
  const list = data.groups

  if (options.json) {
    json(list)
    return
  }

  newline()
  header(`${icons.folder} Projects (${data.total})`)
  if (list.length === 0) {
    if (page > 1) {
      info(`No projects on page ${page}`)
      newline()
      return
    }
    info('No projects yet')
    info(`Run: ${CLI} project-groups create --name "Acme Shop" --service web --service api`)
    newline()
    return
  }

  const columns: TableColumn<ProjectGroupResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Slug', key: 'slug' },
    {
      header: 'Services',
      accessor: (g) => (g.projects.length ? g.projects.map((p) => p.slug).join(', ') : '—')
    }
  ]
  printTable(list, columns, { style: 'minimal' })
  const first = (page - 1) * perPage + 1
  const last = first + list.length - 1
  if (last < data.total) {
    info(`Showing ${first}-${last} of ${data.total}; next page: --page ${page + 1}`)
  }
  newline()
}

function printGroup(group: ProjectGroupResponse): void {
  newline()
  header(`${icons.folder} ${group.name}`)
  keyValue('ID', group.id)
  keyValue('Slug', group.slug)
  if (group.description) {
    keyValue('Description', group.description)
  }
  if (group.repository) {
    const r = group.repository
    keyValue(
      'Repository',
      `${r.repo_owner}/${r.repo_name}@${r.main_branch}${r.public_provider ? ` (public, ${r.public_provider})` : ''}`
    )
  }
  if (group.projects.length === 0) {
    keyValue('Services', '(none)')
  } else {
    newline()
    printTable(group.projects, [
      { header: 'Service', key: 'slug', color: (v) => colors.bold(v) },
      { header: 'Directory', accessor: (p) => normalizeDirectory(p.directory) },
      { header: 'Branch', key: 'main_branch' },
      { header: 'Repository', accessor: (p) => followLabel(p) },
    ], { style: 'minimal' })
  }
  newline()
}

function followLabel(p: ProjectGroupMemberResponse): string {
  if (!p.inherits_repository) return p.repo_owner ? `${p.repo_owner}/${p.repo_name} (own)` : '—'
  return p.follows_repository === false ? 'follows project (NOT in sync)' : 'follows project'
}

async function showAction(ref: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await withSpinner('Fetching project...', () => resolveGroup(ref))
  if (options.json) {
    json(group)
    return
  }
  printGroup(group)
}

async function createAction(
  options: {
    name?: string
    slug?: string
    description?: string
    service?: string[]
  } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()

  const name = options.name || (await promptText({ message: 'Project name', required: true }))
  const group = await withSpinner('Creating project...', async () => {
    const projectIds: number[] = []
    for (const ref of options.service ?? []) {
      projectIds.push(await resolveServiceId(ref))
    }
    return unwrap(
      await createProjectGroup({
        client,
        body: {
          name,
          slug: options.slug ?? null,
          description: options.description ?? null,
          project_ids: projectIds
        }
      })
    )
  })

  if (options.json) {
    json(group)
    return
  }
  success(`Created project ${colors.bold(group.name)} (${group.slug})`)
  printGroup(group)
  info(`Share a variable: ${CLI} project-groups vars set ${group.slug} LOG_LEVEL info`)
}

async function updateAction(
  ref: string,
  options: { name?: string; slug?: string; description?: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()
  if (options.name === undefined && options.slug === undefined && options.description === undefined) {
    throw new Error('Nothing to change: pass --name, --slug or --description')
  }
  const group = await withSpinner('Updating project...', async () => {
    const current = await resolveGroup(ref)
    return unwrap(
      await updateProjectGroup({
        client,
        path: { group_id: current.id },
        body: {
          name: options.name ?? null,
          slug: options.slug ?? null,
          description: options.description ?? null
        }
      })
    )
  })
  if (options.json) {
    json(group)
    return
  }
  success(`Updated project ${colors.bold(group.name)}`)
  printGroup(group)
}

async function deleteAction(ref: string, options: { yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  if (!options.yes) {
    const ok = await promptConfirm({
      message: `Delete project "${group.name}"? Its ${group.projects.length} service(s) keep running as standalone services, and its shared variables are deleted.`,
      default: false
    })
    if (!ok) {
      info('Cancelled')
      return
    }
  }
  await withSpinner('Deleting project...', async () =>
    unwrap(await deleteProjectGroup({ client, path: { group_id: group.id } }))
  )
  success(`Deleted project ${colors.bold(group.name)}; its services are standalone now`)
}

async function addAction(ref: string, services: string[]): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  for (const serviceRef of services) {
    await withSpinner(`Adding ${serviceRef} to ${group.slug}...`, async () => {
      const projectId = await resolveServiceId(serviceRef)
      unwrap(
        await addProjectToGroup({
          client,
          path: { group_id: group.id, project_id: projectId }
        })
      )
    })
    success(`${serviceRef} is now in ${group.name}`)
  }
  info('Shared variables apply on each service\'s next deployment')
}

async function removeAction(ref: string, serviceRef: string): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  const projectId = await resolveServiceId(serviceRef)
  await withSpinner(`Removing ${serviceRef} from ${group.slug}...`, async () =>
    unwrap(
      await removeProjectFromGroup({
        client,
        path: { group_id: group.id, project_id: projectId }
      })
    )
  )
  success(`${serviceRef} is now standalone; it stops receiving ${group.name}'s shared variables on its next deployment`)
}

function printRoutes(routes: GroupRouteResponse[]): void {
  const columns: TableColumn<GroupRouteResponse>[] = [
    { header: 'Service', key: 'source_project_name', color: (v) => colors.bold(v) },
    { header: 'Path', key: 'path_prefix' },
    { header: 'Served by', key: 'target_project_name' },
    { header: 'Strip', accessor: (r) => (r.strip_prefix ? 'yes' : 'no') },
    { header: 'Environments', accessor: describeRouteEnvironments },
  ]
  printTable(routes, columns, { style: 'minimal' })
}

async function routesListAction(ref: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()
  const { group, routes } = await withSpinner('Fetching routes...', async () => {
    const group = await resolveGroup(ref)
    const routes = unwrap(await listGroupRoutes({ client, path: { group_id: group.id } }))
    return { group, routes }
  })
  if (options.json) {
    json(routes)
    return
  }
  newline()
  header(`${icons.info} Path routes in ${group.name} (${routes.length})`)
  if (routes.length === 0) {
    info(
      `Every service answers on its own domains. To serve /api on a frontend's domain from an API service, run: ${CLI} project-groups routes add ${group.slug} <frontend> /api <api>`
    )
    newline()
    return
  }
  printRoutes(routes)
  newline()
}

async function routesAddAction(
  ref: string,
  sourceRef: string,
  prefix: string,
  targetRef: string,
  options: { stripPrefix?: boolean } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  const source = findMember(group, sourceRef)
  const target = findMember(group, targetRef)
  const route = await withSpinner('Adding route...', async () =>
    unwrap(
      await createGroupRoute({
        client,
        path: { group_id: group.id },
        body: {
          source_project_id: source.id,
          path_prefix: prefix,
          target_project_id: target.id,
          strip_prefix: options.stripPrefix ?? false,
        },
      })
    )
  )
  if (options.json) {
    json(route)
    return
  }
  success(`${route.source_project_name} now sends ${route.path_prefix} to ${route.target_project_name}`)
  keyValue('Environments', describeRouteEnvironments(route))
  info(
    `Requests under ${route.path_prefix} get ${route.target_project_name}'s protections (password, attack mode, limits), not ${route.source_project_name}'s.`
  )
  if (route.environments.some((e) => !e.target_deployed)) {
    info(
      `Requests under ${route.path_prefix} get a 502 in environments where ${route.target_project_name} is not deployed; deploy it there to serve them.`
    )
  }
}

async function routesRemoveAction(
  ref: string,
  sourceRef: string,
  prefix: string,
  options: { yes?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  const source = findMember(group, sourceRef)
  const routes = unwrap(await listGroupRoutes({ client, path: { group_id: group.id } }))
  const wanted = `/${prefix.trim().replace(/^\/+|\/+$/g, '')}`
  const route = routes.find((r) => r.source_project_id === source.id && r.path_prefix === wanted)
  if (!route) {
    throw new Error(`${source.name} has no route for ${wanted}. Run: ${CLI} project-groups routes list ${group.slug}`)
  }
  if (!options.yes) {
    const ok = await promptConfirm({
      message: `Stop sending ${route.path_prefix} on ${route.source_project_name} to ${route.target_project_name}?`,
      default: false,
    })
    if (!ok) {
      info('Cancelled')
      return
    }
  }
  await withSpinner('Removing route...', async () =>
    unwrap(await deleteGroupRoute({ client, path: { group_id: group.id, route_id: route.id } }))
  )
  success(`${route.source_project_name} serves ${route.path_prefix} itself again`)
}

async function varsListAction(ref: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()
  const { group, vars } = await withSpinner('Fetching shared variables...', async () => {
    const group = await resolveGroup(ref)
    const vars = unwrap(await listGroupEnvVars({ client, path: { group_id: group.id } }))
    return { group, vars }
  })
  if (options.json) {
    json(vars)
    return
  }
  newline()
  header(`${icons.info} Shared variables in ${group.name} (${vars.length})`)
  if (vars.length === 0) {
    info(`Run: ${CLI} project-groups vars set ${group.slug} API_BASE_URL https://api.example.com`)
    newline()
    return
  }
  const columns: TableColumn<GroupEnvVarResponse>[] = [
    { header: 'Key', key: 'key', color: (v) => colors.bold(v) },
    { header: 'Environments', accessor: describeScope },
    { header: 'Value', accessor: (v) => (v.is_secret ? '(secret)' : (v.value ?? '')) },
    {
      header: 'Previews',
      accessor: (v) => (v.environment_slug ? '—' : v.include_in_preview ? 'yes' : 'no')
    }
  ]
  printTable(vars, columns, { style: 'minimal' })
  newline()
}

async function varsSetAction(
  ref: string,
  key: string,
  valueArg: string | undefined,
  options: { environment?: string; secret?: boolean; preview?: boolean; valueStdin?: boolean } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = normalizeScope(options.environment)
  const hint = caseHint(key)
  if (hint) warning(hint)
  const value = await resolveValue(key, valueArg, options)
  const result = await withSpinner(`Saving ${key}...`, async () => {
    const group = await resolveGroup(ref)
    const existing = findVariable(
      unwrap(await listGroupEnvVars({ client, path: { group_id: group.id } })),
      key,
      scope
    )
    const saved = existing
      ? unwrap(
          await updateGroupEnvVar({
            client,
            path: { group_id: group.id, var_id: existing.id },
            body: {
              value,
              is_secret: options.secret ? true : null,
              // Unchanged unless --preview / --no-preview was passed.
              include_in_preview: options.preview ?? null
            }
          })
        )
      : unwrap(
          await createGroupEnvVar({
            client,
            path: { group_id: group.id },
            body: {
              key,
              value,
              is_secret: Boolean(options.secret),
              environment_slug: scope,
              // Previews only get shared variables they are opted into.
              include_in_preview: options.preview === true
            }
          })
        )
    return { group, saved, created: !existing }
  })
  if (options.json) {
    json(result.saved)
    return
  }
  success(
    `${result.created ? 'Added' : 'Updated'} ${colors.bold(result.saved.key)} for ${describeScope(result.saved)} in ${result.group.name}`
  )
  info('Services pick it up on their next deployment; a service\'s own variable with the same name wins')
}

async function varsUnsetAction(
  ref: string,
  key: string,
  options: { environment?: string; yes?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()
  const scope = normalizeScope(options.environment)
  const group = await resolveGroup(ref)
  const vars = unwrap(await listGroupEnvVars({ client, path: { group_id: group.id } }))
  const existing = findVariable(vars, key, scope)
  if (!existing) {
    const elsewhere = vars.filter((v) => v.key === key).map((v) => v.environment_slug ?? 'all')
    const otherCase = vars.find((v) => v.key !== key && v.key.toUpperCase() === key.toUpperCase())
    throw new Error(
      `${group.name} has no shared variable ${key} for ${scope ?? 'all environments'}` +
        (elsewhere.length ? `; it is set for: ${elsewhere.join(', ')} (pass --environment)` : '') +
        (otherCase ? `; names are case-sensitive, did you mean ${otherCase.key}?` : '')
    )
  }
  if (!options.yes) {
    const ok = await promptConfirm({
      message: `Remove ${existing.key} (${describeScope(existing)}) from ${group.name}?`,
      default: false
    })
    if (!ok) {
      info('Cancelled')
      return
    }
  }
  await withSpinner(`Removing ${existing.key}...`, async () =>
    unwrap(await deleteGroupEnvVar({ client, path: { group_id: group.id, var_id: existing.id } }))
  )
  success(`Removed ${existing.key}; services stop receiving it on their next deployment`)
}

async function varsInheritedAction(serviceRef: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()
  const inherited: InheritedEnvVarsResponse = await withSpinner('Fetching inherited variables...', async () => {
    const projectId = await resolveServiceId(serviceRef)
    return unwrap(await getInheritedEnvVars({ client, path: { project_id: projectId } }))
  })
  if (options.json) {
    json(inherited)
    return
  }
  newline()
  if (!inherited.group) {
    info(`${serviceRef} is not in a project, so it inherits no shared variables.`)
    info(`Add it to one: ${CLI} project-groups add <project> ${serviceRef}`)
    newline()
    return
  }
  header(`${icons.info} ${serviceRef} inherits from ${inherited.group.name} (${inherited.variables.length})`)
  if (inherited.variables.length === 0) {
    info(`${inherited.group.name} shares no variables yet. Run: ${CLI} project-groups vars set ${inherited.group.slug} LOG_LEVEL info`)
    newline()
    return
  }
  printTable(
    inherited.variables,
    [
      { header: 'Key', key: 'key', color: (v) => colors.bold(v) },
      { header: 'Environments', accessor: (v) => describeInherited(v).scope },
      { header: 'Secret', accessor: (v) => (v.is_secret ? 'yes' : 'no') },
      { header: 'Previews', accessor: (v) => describeInherited(v).previews },
      { header: 'Effect', accessor: (v) => describeInherited(v).effect },
    ],
    { style: 'minimal' }
  )
  info("Values are not shown here; see them with: " + `${CLI} project-groups vars list ${inherited.group.slug}`)
  newline()
}

// ---------------------------------------------------------------------------
// Repository (monorepos)
// ---------------------------------------------------------------------------

async function loadRepository(groupId: number): Promise<GroupRepository | null> {
  const data = unwrap(await getGroupRepository({ client, path: { group_id: groupId } }))
  return data.repository ?? null
}

async function repoShowAction(ref: string, options: JsonOption): Promise<void> {
  await requireAuth()
  await setupClient()
  const { group, repository } = await withSpinner('Fetching repository...', async () => {
    const group = await resolveGroup(ref)
    return { group, repository: await loadRepository(group.id) }
  })
  if (options.json) {
    json({ repository, services: group.projects })
    return
  }
  if (!repository) {
    info(`${group.name} has no repository.`)
    info(`Run: ${CLI} project-groups repo set ${group.slug} --connection <id> --repo owner/name --branch main`)
    info(`  or: ${CLI} project-groups repo set ${group.slug} --repo https://github.com/owner/name --branch main`)
    return
  }
  printGroup(group)
  if (repository.public_provider) {
    info('Public repository, read without a connection: pushes do not trigger deploys; deploy each service when you want it updated.')
  } else if (!repository.git_provider_connection_id) {
    info('Its git connection was removed; set the repository again to keep services in sync.')
  } else if (!repository.repository_id) {
    info("It is not in the connection's synced repository list, so it cannot be scanned yet.")
  }
}

async function repoSetAction(
  ref: string,
  options: { connection?: string; public?: string; repo: string; branch: string } & JsonOption
): Promise<void> {
  await requireAuth()
  await setupClient()
  const { connectionId, publicProvider, owner, name } = resolveRepositorySource(options)
  const result = await withSpinner('Saving repository...', async () => {
    const group = await resolveGroup(ref)
    return unwrap(
      await setGroupRepository({
        client,
        path: { group_id: group.id },
        body: {
          git_provider_connection_id: connectionId,
          public_provider: publicProvider,
          repo_owner: owner,
          repo_name: name,
          main_branch: options.branch.trim(),
        },
      })
    )
  })
  const failed = failedRepoSync(result)
  // A partial failure is a failure, with or without --json.
  if (failed.length > 0) process.exitCode = 1
  if (options.json) {
    json(result)
    return
  }
  success(`${result.group.name} deploys from ${owner}/${name}@${options.branch.trim()}`)
  for (const s of result.services) {
    if (s.error) console.log(`  ${colors.error('✗')} ${s.name}: ${s.error}`)
    else console.log(`  ${colors.success('✓')} ${s.name} updated`)
  }
  if (failed.length > 0) {
    info('Those services still use their previous repository or branch; fix the cause and run this again.')
  } else if (result.services.length === 0) {
    info(`Add its apps as services: ${CLI} project-groups import ${result.group.slug}`)
  }
}

async function repoUnsetAction(ref: string, options: { yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  if (!group.repository) {
    info(`${group.name} has no repository.`)
    return
  }
  if (!options.yes) {
    const ok = await promptConfirm({
      message: `Disconnect ${group.repository.repo_owner}/${group.repository.repo_name} from ${group.name}? Services keep their current repository and branch.`,
      default: false,
    })
    if (!ok) {
      info('Cancelled')
      return
    }
  }
  await withSpinner('Disconnecting repository...', async () =>
    unwrap(await clearGroupRepository({ client, path: { group_id: group.id } }))
  )
  success(`${group.name} no longer has a repository; its services keep theirs`)
}

async function followAction(ref: string, serviceRef: string, inherit: boolean): Promise<void> {
  await requireAuth()
  await setupClient()
  const group = await resolveGroup(ref)
  const projectId = await resolveServiceId(serviceRef)
  await withSpinner(inherit ? `Pointing ${serviceRef} at the project repository...` : `Detaching ${serviceRef}...`, async () =>
    unwrap(
      await setRepositoryInheritance({
        client,
        path: { group_id: group.id, project_id: projectId },
        body: { inherit },
      })
    )
  )
  success(
    inherit
      ? `${serviceRef} follows ${group.name}'s repository and branch`
      : `${serviceRef} keeps its current settings and no longer follows ${group.name}`
  )
}

async function importAction(
  ref: string,
  options: { path?: string[]; dryRun?: boolean; timeout?: string } & JsonOption
): Promise<void> {
  if (options.json) setQuietMode(true)
  const timeoutMs = waitTimeoutMs(options.timeout)
  await requireAuth()
  await setupClient()
  const { group, repository, apps, services } = await withSpinner('Scanning repository...', async () => {
    const group = await resolveGroup(ref)
    const repository = await loadRepository(group.id)
    if (!repository) {
      throw new Error(
        `${group.name} has no repository. Run: ${CLI} project-groups repo set ${group.slug} --connection <id> --repo owner/name --branch main`
      )
    }
    let apps: ProjectPresetResponse[]
    if (repository.public_provider) {
      const scan = unwrap(
        await detectPublicPresets({
          client,
          path: {
            provider: repository.public_provider,
            owner: repository.repo_owner,
            repo: repository.repo_name,
          },
          query: { branch: repository.main_branch },
        })
      )
      apps = scan.presets.map(fromPublicPreset)
    } else {
      if (!repository.repository_id) {
        throw new Error(
          `${repository.repo_owner}/${repository.repo_name} is not in its git connection's synced repositories yet; sync the connection and retry`
        )
      }
      const scan = unwrap(
        await getRepositoryPresetLive({
          client,
          path: { repository_id: repository.repository_id },
          query: { branch: repository.main_branch },
        })
      )
      apps = scan.presets
    }
    return { group, repository, apps, services: await fetchAllServices() }
  })

  const used = new Map<string, string>()
  for (const s of services) {
    if (s.repo_owner === repository.repo_owner && s.repo_name === repository.repo_name) {
      used.set(normalizeDirectory(s.directory), s.slug)
    }
  }
  const wanted = options.path?.map(normalizeDirectory)
  const unknown = (wanted ?? []).filter((w) => !apps.some((a) => normalizeDirectory(a.path) === w))
  if (unknown.length > 0) {
    throw new Error(
      `No app detected at ${unknown.join(', ')}. Detected: ${apps.map((a) => normalizeDirectory(a.path)).join(', ') || '(none)'}`
    )
  }
  const taken = new Set(services.map((s) => s.name.toLowerCase()))
  const plan = apps
    .filter((a) => {
      const dir = normalizeDirectory(a.path)
      return wanted ? wanted.includes(dir) : !used.has(dir)
    })
    .map((app) => {
      const name = suggestServiceName(app.path, repository.repo_name, taken)
      taken.add(name)
      return { app, name, existing: used.get(normalizeDirectory(app.path)) }
    })

  if (options.dryRun || plan.length === 0) {
    if (options.json) {
      json(plan.map((p) => ({ directory: normalizeDirectory(p.app.path), preset: p.app.preset, name: p.name, existing: p.existing ?? null })))
      return
    }
    if (plan.length === 0) {
      info(`Every app in ${repository.repo_owner}/${repository.repo_name}@${repository.main_branch} already has a service.`)
      return
    }
    info(`Would add ${plan.length} service(s) to ${group.name}:`)
    for (const p of plan) {
      console.log(`  ${p.name}  ${colors.muted(`${normalizeDirectory(p.app.path)} · ${p.app.presetLabel}`)}${p.existing ? colors.muted(` (already deployed as ${p.existing})`) : ''}`)
    }
    return
  }

  // The server creates every service, adds it to the project and makes it
  // follow the repository, step by step. If a step fails, retrying continues
  // from there instead of creating the services again.
  const started = unwrap(
    await importProjectServices({
      client,
      path: { group_id: group.id },
      body: {
        apps: plan.map(({ app, name }) => importedApp(app, name)),
        idempotency_key: crypto.randomUUID(),
      },
    })
  )
  const finished = await waitForInstantiation(started.id, { timeoutMs, announce: true })
  report(finished, options)
}
