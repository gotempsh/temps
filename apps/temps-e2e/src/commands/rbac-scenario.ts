/**
 * RBAC / project-team-access lifecycle against a live Temps instance --
 * proving the actual permission BOUNDARY, not just that team/access CRUD
 * endpoints return 2xx.
 *
 * `@temps-sdk/api` (what every other scenario in this suite drives) has
 * ZERO /teams or /projects/{id}/access endpoints -- confirmed via
 * `packages/api/openapi.json` -- so team/access/role management here goes
 * through the real CLI subprocess (`cli-exec.ts`, same pattern as
 * `cli-scenario.ts`), while the actual permission-boundary assertions use a
 * SECOND, independently-authenticated SDK client for the low-privilege user
 * being tested.
 *
 *   1. create a low-privilege second user (role "user") and mint their OWN
 *      bearer key directly from the DB (`db-apikey.ts`) -- minting a NEW key
 *      while already authenticated via API key is deliberately blocked
 *      (anti-privilege-escalation), and login only sets a cookie, so
 *      DB-direct is the only way to get an independent credential here
 *   2. before any grant exists: the second user's own GET /projects
 *      includes a throwaway project -- proves "unrestricted by default"
 *   3. create a team (no members yet) and grant it ADMIN access to that
 *      project -- assert the second user's visibility is cut immediately,
 *      even though they are not (yet) a member of the granted team, proving
 *      restriction is a property of the PROJECT the moment any grant
 *      exists, not of the individual user
 *   4. add the second user to the team as viewer -- assert read works,
 *      write is 403 with the exact `projects:write` permission the guard
 *      names
 *   5. promote to deployer -- assert write is still 403, but deploy now
 *      succeeds (proves the grant reached the deployment pipeline, not just
 *      that the guard let the request through)
 *   6. promote to admin -- assert write now succeeds, and the response body
 *      reflects the actual new name (round-trip proof, not just "not 403")
 *   7. revoke the grant -- assert access is denied again on the very next
 *      request (proves the checker's cache invalidation is synchronous, not
 *      relying on a TTL to lapse)
 *   8. delete the team -- assert the cascade produces its own audit trail
 *   9. cross-check every mutation against GET /audit/logs for the exact
 *      operation_type + matching project_id/team_id/role in `data`
 *  10. teardown: delete the second user, the project, and (best-effort) the
 *      team if step 8 somehow didn't already remove it
 *
 * Real design point discovered live (not a bug, but easy to get wrong when
 * writing this kind of test): a member's EFFECTIVE permission on a project
 * is the INTERSECTION of their own role WITHIN the team (`teams members
 * set-role`) and the team's GRANT role ON THE PROJECT (`access grant`) --
 * `checker.rs`'s `effective_project_permissions` computes
 * `member_side ∩ grant_side`, with the grant acting as a hard ceiling no
 * member role can exceed. The grant here is therefore set to `admin` (a
 * high ceiling) ONCE, and only the MEMBER's own role is escalated
 * viewer -> deployer -> admin across steps 4-6 -- escalating the grant
 * itself while leaving the member fixed would have tested the same
 * intersection from the other side, but not both together.
 *
 * Real, pre-existing CLI bug found + fixed while building this:
 * apps/temps-cli/src/commands/users/index.ts's AVAILABLE_ROLES was
 * ['admin','developer','viewer'] (and defaulted new users to 'viewer') --
 * none of which except 'admin' are valid instance-wide roles.
 * crates/temps-entities/src/types.rs's RoleType only ever accepts
 * "admin"/"user"; 'developer'/'viewer' are TEAM roles, a wholly separate
 * concept. Every `users create` without an explicit --roles flag, and every
 * default-role interactive flow, was silently sending a role the backend
 * would 400 on ("Unsupported role"). Fixed to ['admin','user'], default
 * 'user'; the CLI's own unit tests encoded the same wrong role set and are
 * fixed alongside it.
 */
import {
  getProjects,
  getProject,
  updateProject,
  deployFromImage,
  getProjectDeployments,
  deleteUser,
  listAuditLogs,
} from '@temps-sdk/api'
import { makeClient, resolveConfig, unwrap } from '../lib/client.ts'
import { createE2eProject, getProductionEnvironment, teardown, makeRunId } from '../lib/flows.ts'
import { runCliOk, runCliJson } from '../lib/cli-exec.ts'
import { mintDbApiKey } from '../lib/db-apikey.ts'
import type { Client } from '@temps-sdk/api/client'

export interface RbacScenarioOptions {
  tempsRoot?: string
  databaseUrl?: string
  image?: string
  keep?: boolean
  json?: boolean
  connection: { url?: string; apiKey?: string }
}

interface StepLog {
  step: string
  ok: boolean
  detail?: string
  ms?: number
}

interface RbacScenarioResult {
  runId: string
  ok: boolean
  steps: StepLog[]
}

/** Deep-ish helper: does this audit row's `data` reference this exact project/team/role? */
function auditRowMatches(
  row: { data?: unknown },
  match: { project_id?: number; team_id?: number; role?: string },
): boolean {
  const d = row.data as { project_id?: number; team_id?: number; role?: string } | undefined
  if (!d) return false
  if (match.project_id !== undefined && d.project_id !== match.project_id) return false
  if (match.team_id !== undefined && d.team_id !== match.team_id) return false
  if (match.role !== undefined && d.role !== match.role) return false
  return true
}

export async function rbacScenarioCommand(opts: RbacScenarioOptions): Promise<void> {
  const cfg = resolveConfig(opts.connection)
  const client = makeClient(cfg)
  const json = !!opts.json
  const log = (msg: string) => {
    if (!json) process.stderr.write(msg + '\n')
  }
  if (!json) log(`Temps RBAC/teams scenario  ->  ${cfg.url}`)

  const tempsRoot = opts.tempsRoot ?? process.env.TEMPS_ROOT
  const databaseUrl = opts.databaseUrl ?? process.env.TEMPS_DATABASE_URL
  if (!tempsRoot || !databaseUrl) {
    throw new Error(
      'RBAC testing needs DB-direct access to mint a second user\'s own bearer key: pass ' +
        '--temps-root <checkout> --database-url <postgres url>, or set TEMPS_ROOT / ' +
        'TEMPS_DATABASE_URL. See apps/temps-e2e/README.md.',
    )
  }
  const image = opts.image ?? 'ghcr.io/temps-sh/e2e-hello:latest'

  const runId = makeRunId(Date.now())
  const steps: StepLog[] = []
  const step = async <T>(name: string, fn: () => Promise<T>): Promise<T> => {
    const t0 = performance.now()
    log(`\n▶ ${name}`)
    try {
      const r = await fn()
      const ms = performance.now() - t0
      steps.push({ step: name, ok: true, ms })
      log(`  ✓ ${name} (${(ms / 1000).toFixed(1)}s)`)
      return r
    } catch (e) {
      const ms = performance.now() - t0
      steps.push({ step: name, ok: false, detail: (e as Error).message, ms })
      log(`  ✗ ${name}: ${(e as Error).message}`)
      throw e
    }
  }

  const secondUsername = `${runId}-viewer`.replace(/[^a-z0-9-]/gi, '-').slice(0, 32)
  const secondEmail = `${runId}-second@e2e.test`
  const secondPassword = `E2e!${runId}Aa1`
  const teamName = `${runId}-team`
  const teamSlug = teamName.slice(0, 40)
  // A second team, granted access and never revoked, so that revoking the
  // PRIMARY team's grant later doesn't drop the project's grant count to
  // zero -- which would (correctly) make the project unrestricted again
  // ("removing the last grant makes this project unrestricted", per the
  // CLI's own revokeAccessAction warning) and defeat the revoke assertion
  // by making it pass for the wrong reason.
  const guardTeamName = `${runId}-guard-team`
  const guardTeamSlug = guardTeamName.slice(0, 40)

  const projectIds: number[] = []
  const deployments: { projectId: number; deploymentId: number }[] = []
  let secondUserId: number | undefined
  let teamId: number | undefined
  let guardTeamId: number | undefined
  let guardTeamSlugValue: string | undefined
  let teamDeleted = false
  let guardTeamDeleted = false
  let secondUserClient: Client | undefined

  try {
    await step('create a low-privilege second user (role: user, via CLI)', () =>
      runCliOk(['users', 'create', '-u', secondUsername, '-e', secondEmail, '-p', secondPassword, '-r', 'user', '-y'], cfg),
    )

    secondUserId = await step('resolve the second user\'s id (users create has no --json output)', async () => {
      const users = await runCliJson<Array<{ user: { id: number; email: string } }>>(['users', 'list'], cfg)
      const match = users.find((u) => u.user.email === secondEmail)
      if (!match) throw new Error(`created user ${secondEmail} not found in users list`)
      return match.user.id
    })
    log(`  second user #${secondUserId} (${secondEmail})`)

    const minted = await step("mint the second user's OWN bearer key (DB-direct, not login/API-key)", () =>
      mintDbApiKey(
        { tempsRoot, databaseUrl },
        { name: `${runId}-second-key`, role: 'user', userEmail: secondEmail },
      ),
    )
    secondUserClient = makeClient({ url: cfg.url, apiKey: minted.apiKey })

    const project = await step('create a throwaway project (admin identity)', () =>
      createE2eProject(client, { name: `${runId}-rbac-app` }),
    )
    projectIds.push(project.id)
    log(`  project #${project.id} (${project.slug})`)

    await step('before any grant: second user\'s GET /projects includes it (unrestricted by default)', async () => {
      const list = unwrap(
        await getProjects({ client: secondUserClient!, query: { page: 1, per_page: 100 } }),
        'getProjects',
      )
      const ids = (list.projects ?? []).map((p) => p.id)
      if (!ids.includes(project.id)) {
        throw new Error(`second user's project list ${JSON.stringify(ids)} does not include ${project.id}`)
      }
    })

    const team = await step('create a team (no members yet, via CLI)', () =>
      runCliJson<{ id: number; slug: string }>(['teams', 'create', '-n', teamName, '-s', teamSlug], cfg),
    )
    teamId = team.id
    log(`  team #${team.id} (${team.slug})`)

    await step(
      'grant the team ADMIN access to the project (via CLI) -- a high ceiling, so the member-role escalation below is what actually gates permissions',
      () => runCliOk(['access', 'grant', team.slug, '-p', project.slug, '-r', 'admin'], cfg),
    )

    const guardTeam = await step(
      'create + grant a second "guard" team (via CLI) -- keeps the project restricted after the primary grant is revoked later',
      async () => {
        const t = await runCliJson<{ id: number; slug: string }>(['teams', 'create', '-n', guardTeamName, '-s', guardTeamSlug], cfg)
        await runCliOk(['access', 'grant', t.slug, '-p', project.slug, '-r', 'viewer'], cfg)
        return t
      },
    )
    guardTeamId = guardTeam.id
    guardTeamSlugValue = guardTeam.slug
    log(`  guard team #${guardTeam.id} (${guardTeam.slug})`)

    await step(
      'second user (NOT yet a team member) immediately loses visibility -- restriction is a project property, not a per-user one',
      async () => {
        const list = unwrap(
          await getProjects({ client: secondUserClient!, query: { page: 1, per_page: 100 } }),
          'getProjects',
        )
        const ids = (list.projects ?? []).map((p) => p.id)
        if (ids.includes(project.id)) {
          throw new Error(`second user can still see project ${project.id} in the list after the first grant`)
        }
        const res = await getProject({ client: secondUserClient!, path: { id: project.id } })
        if (!res.error || res.response?.status !== 403) {
          throw new Error(`GET /projects/${project.id} (non-member) returned HTTP ${res.response?.status}, expected 403`)
        }
      },
    )

    await step('add the second user to the team as viewer (via CLI)', () =>
      runCliOk(['teams', 'members', 'add', team.slug, '-u', secondEmail, '-r', 'viewer'], cfg),
    )

    await step('viewer: read succeeds (200 with real fields), write is 403 naming projects:write', async () => {
      const read = unwrap(await getProject({ client: secondUserClient!, path: { id: project.id } }), 'getProject')
      if (read.id !== project.id) throw new Error(`GET /projects/${project.id} as viewer returned id=${read.id}`)

      const write = await updateProject({
        client: secondUserClient!,
        path: { id: project.id },
        body: {
          name: `${project.name}-renamed`,
          directory: '.',
          main_branch: 'main',
          preset: 'dockerfile',
          storage_service_ids: [],
          source_type: 'docker_image',
          is_web_app: true,
        },
      })
      if (!write.error || write.response?.status !== 403) {
        throw new Error(`PATCH as viewer returned HTTP ${write.response?.status}, expected 403`)
      }
      const body = write.error as { required_permission?: string } | undefined
      if (body?.required_permission !== 'projects:write') {
        throw new Error(`PATCH-as-viewer 403 body.required_permission="${body?.required_permission}", expected "projects:write"`)
      }
    })

    const env = await step('resolve production environment (admin identity)', () =>
      getProductionEnvironment(client, project.id),
    )

    await step('promote to deployer (via CLI): write still 403, but deploy now succeeds', async () => {
      await runCliOk(['teams', 'members', 'set-role', team.slug, '-u', secondEmail, '-r', 'deployer'], cfg)

      const write = await updateProject({
        client: secondUserClient!,
        path: { id: project.id },
        body: {
          name: `${project.name}-renamed`,
          directory: '.',
          main_branch: 'main',
          preset: 'dockerfile',
          storage_service_ids: [],
          source_type: 'docker_image',
          is_web_app: true,
        },
      })
      if (!write.error || write.response?.status !== 403) {
        throw new Error(`PATCH as deployer returned HTTP ${write.response?.status}, expected still-403`)
      }

      const deploy = unwrap(
        await deployFromImage({
          client: secondUserClient!,
          path: { project_id: project.id, environment_id: env.id },
          body: { image_ref: image },
        }),
        'deployFromImage (as deployer)',
      )
      deployments.push({ projectId: project.id, deploymentId: deploy.id })

      const list = unwrap(
        await getProjectDeployments({ client, path: { id: project.id } }),
        'getProjectDeployments (admin readback)',
      )
      const ids = (Array.isArray(list) ? list : (list as { deployments?: Array<{ id: number }> }).deployments ?? []).map(
        (d: { id: number }) => d.id,
      )
      if (!ids.includes(deploy.id)) {
        throw new Error(`deployer's deployment ${deploy.id} not visible in admin's GET .../deployments (${JSON.stringify(ids)})`)
      }
    })

    const newName = `${project.name}-renamed-for-real`
    await step('promote to admin (via CLI): write now succeeds and the response reflects the real new name', async () => {
      await runCliOk(['teams', 'members', 'set-role', team.slug, '-u', secondEmail, '-r', 'admin'], cfg)

      const write = unwrap(
        await updateProject({
          client: secondUserClient!,
          path: { id: project.id },
          body: {
            name: newName,
            directory: '.',
            main_branch: 'main',
            preset: 'dockerfile',
            storage_service_ids: [],
            source_type: 'docker_image',
            is_web_app: true,
          },
        }),
        'updateProject (as team-admin)',
      )
      if (write.name !== newName) {
        throw new Error(`updateProject as team-admin returned name="${write.name}", expected "${newName}"`)
      }
    })

    await step('revoke the grant (via CLI): access denied again on the VERY NEXT request', async () => {
      await runCliOk(['access', 'revoke', team.slug, '-p', project.slug, '-y'], cfg)
      const res = await getProject({ client: secondUserClient!, path: { id: project.id } })
      if (!res.error || res.response?.status !== 403) {
        throw new Error(`GET /projects/${project.id} immediately after revoke returned HTTP ${res.response?.status}, expected 403`)
      }
    })

    await step('delete the team (via CLI): cascades the grant/membership removal', async () => {
      await runCliOk(['teams', 'delete', team.slug, '-y'], cfg)
      teamDeleted = true
    })

    await step('delete the guard team (via CLI): only needed to keep the project restricted through the revoke assertion above', async () => {
      await runCliOk(['teams', 'delete', guardTeam.slug, '-y'], cfg)
      guardTeamDeleted = true
    })

    await step('audit trail: PROJECT_ACCESS_GRANTED / TEAM_MEMBER_ROLE_UPDATED / PROJECT_ACCESS_REVOKED / TEAM_DELETED all recorded with matching data', async () => {
      const grants = unwrap(
        await listAuditLogs({
          client,
          query: { operation_type: 'PROJECT_ACCESS_GRANTED', user_id: undefined, from: undefined, to: undefined, limit: 50, offset: 0 },
        }),
        'listAuditLogs(PROJECT_ACCESS_GRANTED)',
      ) as Array<{ data?: unknown }>
      if (!grants.some((r) => auditRowMatches(r, { project_id: project.id, team_id: team.id, role: 'admin' }))) {
        throw new Error(`no PROJECT_ACCESS_GRANTED audit row matches project=${project.id} team=${team.id} role=admin`)
      }

      const roleUpdates = unwrap(
        await listAuditLogs({
          client,
          query: { operation_type: 'TEAM_MEMBER_ROLE_UPDATED', user_id: undefined, from: undefined, to: undefined, limit: 50, offset: 0 },
        }),
        'listAuditLogs(TEAM_MEMBER_ROLE_UPDATED)',
      ) as Array<{ data?: unknown }>
      if (!roleUpdates.some((r) => auditRowMatches(r, { team_id: team.id, role: 'admin' }))) {
        throw new Error(`no TEAM_MEMBER_ROLE_UPDATED audit row matches team=${team.id} role=admin (the final promotion)`)
      }

      const revokes = unwrap(
        await listAuditLogs({
          client,
          query: { operation_type: 'PROJECT_ACCESS_REVOKED', user_id: undefined, from: undefined, to: undefined, limit: 50, offset: 0 },
        }),
        'listAuditLogs(PROJECT_ACCESS_REVOKED)',
      ) as Array<{ data?: unknown }>
      if (!revokes.some((r) => auditRowMatches(r, { project_id: project.id, team_id: team.id }))) {
        throw new Error(`no PROJECT_ACCESS_REVOKED audit row matches project=${project.id} team=${team.id}`)
      }

      const teamDeletes = unwrap(
        await listAuditLogs({
          client,
          query: { operation_type: 'TEAM_DELETED', user_id: undefined, from: undefined, to: undefined, limit: 50, offset: 0 },
        }),
        'listAuditLogs(TEAM_DELETED)',
      ) as Array<{ data?: unknown }>
      if (!teamDeletes.some((r) => auditRowMatches(r, { team_id: team.id }))) {
        throw new Error(`no TEAM_DELETED audit row matches team=${team.id}`)
      }
    })
  } catch {
    // Failure already recorded in `steps`; fall through to teardown.
  } finally {
    if (opts.keep) {
      log(
        `\n(kept resources: projects=${projectIds.join(',')} secondUser=${secondUserId} ` +
          `team=${teamId} guardTeam=${guardTeamId})`,
      )
    } else {
      if (teamId !== undefined && !teamDeleted) {
        await runCliOk(['teams', 'delete', String(teamId), '-y'], cfg).catch(() => undefined)
      }
      if (guardTeamSlugValue !== undefined && !guardTeamDeleted) {
        await runCliOk(['teams', 'delete', guardTeamSlugValue, '-y'], cfg).catch(() => undefined)
      }
      if (secondUserId !== undefined) {
        await deleteUser({ client, path: { user_id: secondUserId } }).catch(() => undefined)
      }
      const td = await teardown(client, { deployments, projectIds })
      log(
        `\n▶ teardown: tore down ${td.teardownDeployments} deployment(s), ` +
          `deleted ${td.deletedProjects} project(s), removed second user + team(s)` +
          (td.errors.length ? ` (${td.errors.length} errors)` : ''),
      )
      for (const e of td.errors) log(`    ! ${e}`)
    }
  }

  const ok = steps.length > 0 && steps.every((s) => s.ok)
  const result: RbacScenarioResult = { runId, ok, steps }

  if (json) {
    process.stdout.write(JSON.stringify(result, null, 2) + '\n')
  } else {
    log(`\n${ok ? '✅ rbac-scenario PASSED' : '❌ rbac-scenario FAILED'}`)
  }
  if (!ok) process.exitCode = 1
}
