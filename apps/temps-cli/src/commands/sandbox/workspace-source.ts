/**
 * Works out *what code* a workspace sandbox should start from.
 *
 * `sandbox create` originally took only raw `--git-url` + a numeric
 * `--git-connection`, which meant looking up an ID in another command
 * before you could create anything. Every other part of the CLI already
 * knows how to resolve this — `resolveProjectSlug` reads `.temps/`, the
 * git-connection helpers offer pickers, and `detect-project` reads the
 * local checkout. This module wires those together for sandboxes.
 *
 * Resolution order, first match wins:
 *
 *   1. `--git-url` / `--tarball-url` — explicit, no lookups. The escape
 *      hatch for anything the server can reach.
 *   2. `--repo owner/name` — a repo on one of your git connections that
 *      has no temps project. Resolves the clone URL and connection so
 *      private repos work without handling a token.
 *   3. `--project <slug>`, or a project resolved from `.temps/config.json`
 *      / `$TEMPS_PROJECT` / the active context.
 *   4. The git remote of the current directory.
 *   5. Interactive pickers (connection → repo → branch).
 *
 * Step 5 is skipped when stdin isn't a TTY, and the error then names the
 * exact flag to pass instead of hanging on a prompt nothing can answer.
 */
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { getProjectBySlug } from '../../api/sdk.gen.js'
import type { RepositoryResponse } from '../../api/types.gen.js'
import {
  selectGitConnection,
  selectRepository,
  selectBranch,
  findRepositoryByName,
} from '../../lib/git-connection.js'
import { resolveProjectSlug } from '../../config/resolve-project.js'
import { detectGitRemote, detectGitBranch } from '../../lib/detect-project.js'
import { info, colors } from '../../ui/output.js'

export interface WorkspaceSourceOptions {
  project?: string
  repo?: string
  branch?: string
  gitUrl?: string
  gitRev?: string
  gitDepth?: string
  gitConnection?: string
  gitUsername?: string
  gitPassword?: string
  tarballUrl?: string
}

export interface ResolvedWorkspaceSource {
  /** Sent as `project_id`; the server derives repo + credential from it. */
  projectId?: number
  /** Sent as `source`; wins over `projectId` server-side. */
  source?: Record<string, unknown>
  /** One line describing what was resolved and how, for the user. */
  origin: string
}

/**
 * Slug → numeric project id, for the places the API keys on the id.
 * Shared with `sandbox list --project` so both accept the same thing.
 */
export async function resolveProjectIdBySlug(slug: string): Promise<number> {
  await setupClient()
  const { data, error } = await getProjectBySlug({ client, path: { slug } })
  if (error || !data) {
    throw new Error(
      `Project "${slug}" not found${error ? `: ${getErrorMessage(error)}` : ''}`,
    )
  }
  return data.id
}

/** `owner/name`, rejecting the near-misses people actually type. */
function parseRepoFlag(value: string): { owner: string; name: string } {
  const parts = value.split('/').filter((p) => p.length > 0)
  if (parts.length !== 2) {
    throw new Error(
      `--repo must be "owner/name" (got "${value}"). ` +
        'For a URL that isn\'t on a connected git provider, use --git-url instead.',
    )
  }
  return { owner: parts[0]!, name: parts[1]! }
}

function parseConnectionFlag(value: string): number {
  const id = Number(value)
  if (!Number.isInteger(id) || id <= 0) {
    throw new Error('--git-connection must be a positive integer')
  }
  return id
}

/**
 * Prompts are only reachable on a TTY. In CI or a piped shell, inquirer
 * would either hang or throw something opaque, so fail early with the
 * flag that would have avoided the prompt.
 */
function requireInteractive(what: string, flagHint: string): void {
  if (!process.stdin.isTTY) {
    throw new Error(
      `Cannot prompt for ${what} — stdin is not a TTY. Pass ${flagHint} explicitly.`,
    )
  }
}

/** Resolve the connection to use, prompting only if we must. */
async function resolveConnectionId(
  options: WorkspaceSourceOptions,
  why: string,
): Promise<number | undefined> {
  if (options.gitConnection !== undefined) {
    return parseConnectionFlag(options.gitConnection)
  }
  // Inline credentials are an alternative to a connection, not a companion.
  if (options.gitUsername || options.gitPassword) return undefined

  requireInteractive(why, '--git-connection <id>')
  const connection = await selectGitConnection()
  return connection?.id
}

function gitSource(
  url: string,
  revision: string | undefined,
  connectionId: number | undefined,
  options: WorkspaceSourceOptions,
): Record<string, unknown> {
  const source: Record<string, unknown> = { type: 'git', url }
  if (revision) source.revision = revision
  if (connectionId !== undefined) source.git_connection_id = connectionId
  if (options.gitUsername) source.username = options.gitUsername
  if (options.gitPassword) source.password = options.gitPassword
  if (options.gitDepth !== undefined) {
    const depth = Number(options.gitDepth)
    if (!Number.isInteger(depth) || depth <= 0) {
      throw new Error('--git-depth must be a positive integer')
    }
    source.depth = depth
  }
  return source
}

/** The ref to check out: `--branch` preferred, `--git-rev` kept as an alias. */
function requestedRef(options: WorkspaceSourceOptions): string | undefined {
  return options.branch ?? options.gitRev
}

function cloneUrlFor(repository: RepositoryResponse): string {
  // SSH URLs can't be cloned inside the sandbox — there's no key in there.
  // The connection's token drives HTTPS Basic auth instead.
  const url = repository.clone_url
  if (!url) {
    throw new Error(
      `Repository ${repository.full_name} has no HTTPS clone URL on this connection. ` +
        'Pass --git-url with an https:// URL instead.',
    )
  }
  return url
}

export async function resolveWorkspaceSource(
  options: WorkspaceSourceOptions,
): Promise<ResolvedWorkspaceSource> {
  // The sandbox commands talk to /v1/sandbox with their own fetch wrapper,
  // so the generated SDK client used by the project and git-connection
  // lookups below hasn't been configured yet. Idempotent.
  await setupClient()

  const ref = requestedRef(options)

  // 1. Explicit URLs — no lookups, no prompts.
  if (options.tarballUrl) {
    if (options.gitUrl) {
      throw new Error('--git-url and --tarball-url are mutually exclusive')
    }
    return {
      source: { type: 'tarball', url: options.tarballUrl },
      origin: `tarball ${options.tarballUrl}`,
    }
  }
  if (options.gitUrl) {
    const connectionId =
      options.gitConnection !== undefined
        ? parseConnectionFlag(options.gitConnection)
        : undefined
    if ((options.gitUsername || options.gitPassword) && connectionId !== undefined) {
      throw new Error(
        '--git-connection is mutually exclusive with --git-username/--git-password',
      )
    }
    if (
      (options.gitUsername && !options.gitPassword) ||
      (!options.gitUsername && options.gitPassword)
    ) {
      throw new Error('--git-username and --git-password must be provided together')
    }
    return {
      source: gitSource(options.gitUrl, ref, connectionId, options),
      origin: `${options.gitUrl}${ref ? ` @ ${ref}` : ''}`,
    }
  }

  // 2. `--repo owner/name` — a connected repo with no temps project.
  if (options.repo) {
    const { owner, name } = parseRepoFlag(options.repo)
    const connectionId = await resolveConnectionId(
      options,
      `the git connection hosting ${options.repo}`,
    )
    if (connectionId === undefined) {
      throw new Error(
        `No git connection available to resolve ${options.repo}. ` +
          'Connect a provider with `temps providers add`, or pass --git-url.',
      )
    }
    const repository = await findRepositoryByName(connectionId, owner, name)
    if (!repository) {
      throw new Error(
        `Repository ${owner}/${name} not found on git connection ${connectionId}. ` +
          'Check the spelling, or run `temps providers repos sync` if it was added recently.',
      )
    }
    const revision = ref ?? repository.default_branch
    return {
      source: gitSource(cloneUrlFor(repository), revision, connectionId, options),
      origin: `${repository.full_name} @ ${revision}`,
    }
  }

  // 3. A temps project, named or inferred from the working directory.
  const resolvedProject = await resolveProjectSlug(options.project)
  if (resolvedProject) {
    const { data, error } = await getProjectBySlug({
      client,
      path: { slug: resolvedProject.slug },
    })
    if (error || !data) {
      // An explicit --project that doesn't resolve is a hard error; an
      // inherited one (from .temps or a context default) may just be
      // stale, so fall through to the remaining strategies.
      if (options.project) {
        throw new Error(
          `Project "${resolvedProject.slug}" not found: ${error ? getErrorMessage(error) : 'no such project'}`,
        )
      }
    } else if (data.git_url) {
      const via =
        resolvedProject.source === 'flag'
          ? ''
          : ` (from ${resolvedProject.source.replace('-', ' ')})`
      // Without a ref override, hand the server `project_id` and let it
      // derive repo + branch + credential — one less thing to get wrong
      // here, and the sandbox stays attributed to the project.
      if (!ref) {
        return {
          projectId: data.id,
          origin: `project ${colors.bold(resolvedProject.slug)} @ ${data.main_branch}${via}`,
        }
      }
      // With a ref override we must build the source ourselves, but keep
      // the attribution so the workspace still lists under the project.
      return {
        projectId: data.id,
        source: gitSource(
          data.git_url,
          ref,
          data.git_provider_connection_id ?? undefined,
          options,
        ),
        origin: `project ${colors.bold(resolvedProject.slug)} @ ${ref}${via}`,
      }
    } else if (options.project) {
      throw new Error(
        `Project "${resolvedProject.slug}" has no git repository connected. ` +
          'Pass --repo owner/name or --git-url to choose the code yourself.',
      )
    }
  }

  // 4. The git remote of the current directory.
  const remote = detectGitRemote()
  if (remote) {
    const revision = ref ?? detectGitBranch() ?? undefined
    const connectionId = options.gitConnection
      ? parseConnectionFlag(options.gitConnection)
      : undefined
    info(
      `Using the git remote of this directory: ${colors.bold(`${remote.owner}/${remote.repo}`)}`,
    )
    // Prefer the connection's own clone URL when the repo is connected —
    // it's the one the server can authenticate. Falls back to the raw
    // remote, which is fine for public repos.
    if (connectionId !== undefined) {
      const repository = await findRepositoryByName(connectionId, remote.owner, remote.repo)
      if (repository) {
        return {
          source: gitSource(cloneUrlFor(repository), revision, connectionId, options),
          origin: `${repository.full_name} @ ${revision ?? repository.default_branch}`,
        }
      }
    }
    const httpsUrl = `https://${remote.host}/${remote.owner}/${remote.repo}.git`
    return {
      source: gitSource(httpsUrl, revision, connectionId, options),
      origin: `${remote.owner}/${remote.repo}${revision ? ` @ ${revision}` : ''}`,
    }
  }

  // 5. Nothing to infer — pick it interactively.
  requireInteractive('a repository', '--repo owner/name, --project <slug>, or --git-url <url>')
  const connection = await selectGitConnection()
  if (!connection) {
    throw new Error(
      'No git connections available. Run `temps providers add` to connect a provider, ' +
        'or pass --git-url to clone a public repo.',
    )
  }
  const repository = await selectRepository(connection.id)
  if (!repository) {
    throw new Error('No repository selected.')
  }
  const revision = ref ?? (await selectBranch(connection.id, repository))
  return {
    source: gitSource(cloneUrlFor(repository), revision, connection.id, options),
    origin: `${repository.full_name} @ ${revision}`,
  }
}
