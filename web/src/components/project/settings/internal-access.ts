// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Internal access grants (ADR 101, ADR 105): which other projects, and which
 * sandboxes, may call this project's `*.temps.local` addresses. Pure helpers for the card and the DNS
 * test tool, kept apart so they are tested without rendering.
 */
import { z } from 'zod'
import type {
  CreateInternalAccessGrantRequest,
  CreateInternalAccessSandboxGrantRequest,
  DnsInternalAccess,
  InternalAccessGrantResponse,
  InternalAccessSandboxGrantResponse,
} from '@/api/client/types.gen'

/** Mirrors `MAX_DESCRIPTION_LENGTH` in the network-policy service. */
export const MAX_GRANT_DESCRIPTION_LENGTH = 200
/**
 * Mirrors `MAX_GRANTS_PER_PROJECT`, which counts project and sandbox callers
 * together. The list endpoints accept a `page_size` of up to 100, so one
 * request returns every grant of a kind a project holds.
 */
export const MAX_GRANTS_PER_PROJECT = 100
export const GRANT_LOOKUP_PAGE_SIZE = MAX_GRANTS_PER_PROJECT
/** Rows per page of the project and sandbox lists. */
export const GRANTS_PAGE_SIZE = 20
/** `listSandboxes` returns at most this many in one request. */
export const SANDBOX_LOOKUP_LIMIT = 100

/**
 * The page of grants to show: `page`, or the last page that still has
 * grants once the list shrank below it (a removal, here or elsewhere). A
 * page past the end lists nothing and, with one page left, has no pager to
 * leave it by.
 */
export function clampGrantsPage(
  page: number,
  total: number,
  pageSize: number
): number {
  const lastPage = Math.max(1, Math.ceil(total / pageSize))
  return Math.min(Math.max(1, page), lastPage)
}

/** The section other screens link to (the DNS test tool). */
export const INTERNAL_ACCESS_SECTION_ID = 'internal-access'

/** "Checkout worker, all environments" or "Checkout worker, production". */
export function callerLabel(grant: InternalAccessGrantResponse): string {
  return grant.caller_environment_id == null
    ? `${grant.caller_project_name}, all environments`
    : `${grant.caller_project_name}, ${grant.caller_environment_name ?? `environment ${grant.caller_environment_id}`}`
}

/**
 * On an environment's card, a project-wide grant applies but is managed on
 * the project's card.
 */
export function isInherited(
  grant: { target_environment_id?: number | null },
  environmentId?: number
): boolean {
  return environmentId !== undefined && grant.target_environment_id == null
}

/** The scope fields both kinds of grant share; sandbox grants have no caller environment. */
export interface GrantScope {
  target_environment_id?: number | null
  target_environment_name?: string | null
  caller_environment_id?: number | null
  caller_environment_name?: string | null
  description?: string | null
}

/** "all environments", "staging only", or "all environments, from production". */
export function scopeSummary(grant: GrantScope): string {
  const target =
    grant.target_environment_id == null
      ? 'all environments'
      : `${grant.target_environment_name ?? `environment ${grant.target_environment_id}`} only`
  if (grant.caller_environment_id == null) return target
  return `${target}, from ${grant.caller_environment_name ?? `environment ${grant.caller_environment_id}`}`
}

/** Every distinct scope among one caller's grants, joined. */
export function scopesSummary(grants: GrantScope[]): string {
  return [...new Set(grants.map(scopeSummary))].join('; ')
}

/** What one row of a list stands for, from the grants themselves. */
export interface CallerState<G> {
  /** Every grant naming the caller that applies here. */
  grants: G[]
  /** The ones this card manages; on an environment's card, not the project-wide ones. */
  removable: G[]
  /** Allowed only by project-wide grants, managed on the project's card. */
  inherited: boolean
}

export function callerState<G extends GrantScope>(
  grants: G[],
  matches: (grant: G) => boolean,
  environmentId?: number
): CallerState<G> {
  const mine = grants.filter(matches)
  const removable = mine.filter((grant) => !isInherited(grant, environmentId))
  return {
    grants: mine,
    removable,
    inherited: mine.length > 0 && removable.length === 0,
  }
}

/**
 * Whether a grants lookup can decide "allowed": it loaded, and it holds every
 * grant. A partial answer would show an allowed caller as "Allow".
 */
export function grantsLookupComplete(
  list: { items: unknown[]; total: number } | undefined
): boolean {
  return list !== undefined && list.items.length >= list.total
}

/**
 * Roles without `projects:write` (`Role::permissions` in temps-authz):
 * they may read internal access but not change it. Admin, user and custom
 * roles try, and a refusal says why in the row.
 */
const ROLES_WITHOUT_PROJECTS_WRITE: Record<string, string> = {
  platform_admin: 'Platform admin',
  reader: 'Reader',
  api_reader: 'API reader',
}

/** Why the user cannot change internal access, or `null` when they may try. */
export function internalAccessWriteBlock(
  role: string | null | undefined
): string | null {
  const name = role ? ROLES_WITHOUT_PROJECTS_WRITE[role] : undefined
  return name === undefined
    ? null
    : `Your role (${name}) cannot change internal access; it needs projects:write.`
}

/** One click: the caller's every environment may call the whole project, or the environment this card is for. */
export function oneClickProjectGrant(
  callerProjectId: number,
  environmentId?: number
): CreateInternalAccessGrantRequest {
  return environmentId === undefined
    ? { caller_project_id: callerProjectId }
    : {
        caller_project_id: callerProjectId,
        target_environment_id: environmentId,
      }
}

export function oneClickSandboxGrant(
  sandboxId: string,
  environmentId?: number
): CreateInternalAccessSandboxGrantRequest {
  return environmentId === undefined
    ? { sandbox_id: sandboxId }
    : { sandbox_id: sandboxId, target_environment_id: environmentId }
}

/** The "Limit…" form: the narrower scope and a reason. */
export const grantLimitsSchema = z.object({
  /** `null`: every environment of the calling project. */
  callerEnvironmentId: z.number().int().positive().nullable(),
  /** `null`: every environment of the project being called. */
  targetEnvironmentId: z.number().int().positive().nullable(),
  description: z.string().max(MAX_GRANT_DESCRIPTION_LENGTH, {
    error: `At most ${MAX_GRANT_DESCRIPTION_LENGTH} characters`,
  }),
})

export type GrantLimits = z.infer<typeof grantLimitsSchema>

export function defaultGrantLimits(environmentId?: number): GrantLimits {
  return {
    callerEnvironmentId: null,
    targetEnvironmentId: environmentId ?? null,
    description: '',
  }
}

function trimmedReason(description: string): string | null {
  const reason = description.trim()
  return reason === '' ? null : reason
}

export function limitedProjectGrant(
  callerProjectId: number,
  limits: GrantLimits
): CreateInternalAccessGrantRequest {
  return {
    caller_project_id: callerProjectId,
    caller_environment_id: limits.callerEnvironmentId,
    target_environment_id: limits.targetEnvironmentId,
    description: trimmedReason(limits.description),
  }
}

export function limitedSandboxGrant(
  sandboxId: string,
  limits: GrantLimits
): CreateInternalAccessSandboxGrantRequest {
  return {
    sandbox_id: sandboxId,
    target_environment_id: limits.targetEnvironmentId,
    description: trimmedReason(limits.description),
  }
}

/**
 * The user's sandboxes as the list shows them: live ones whose name or id
 * contains `search`, running ones first, otherwise newest first as listed.
 */
export function sandboxesForAllowList<
  S extends { id: string; name: string; status: string },
>(sandboxes: S[], search: string): S[] {
  const needle = search.trim().toLowerCase()
  const live = sandboxes.filter(
    (sandbox) =>
      sandbox.status !== 'destroyed' &&
      (needle === '' ||
        sandbox.name.toLowerCase().includes(needle) ||
        sandbox.id.toLowerCase().includes(needle))
  )
  return [
    ...live.filter((sandbox) => sandbox.status === 'running'),
    ...live.filter((sandbox) => sandbox.status !== 'running'),
  ]
}

/** Rows `page` (1-indexed) shows. */
export function pageOf<T>(
  items: T[],
  page: number,
  pageSize = GRANTS_PAGE_SIZE
): T[] {
  return items.slice((page - 1) * pageSize, page * pageSize)
}

/** The address an app in the calling project would use. */
export function exampleAddress(
  projectSlug: string,
  environmentSlug = 'production'
): string {
  return `http://${environmentSlug}.${projectSlug}.temps.local`
}

/** The `Callout` tones the DNS test tool uses. */
export type DiagnosticTone = 'success' | 'warning' | 'error'

export interface InternalAccessDiagnostic {
  tone: DiagnosticTone
  title: string
  detail: string
  /** Where to fix it; set only when a grant would change the outcome. */
  setupPath: string | null
}

/**
 * What the DNS test tool says about calling an internal name from the
 * project it asked as. `null` when the name's own project asked.
 */
export function internalAccessDiagnostic(
  access: DnsInternalAccess,
  askedAsName: string
): InternalAccessDiagnostic | null {
  const owner = `${access.target_project_name} (${access.target_environment_name})`
  switch (access.reach) {
    case 'same_project':
      return null
    case 'granted':
      return {
        tone: 'success',
        title: 'Calls are allowed',
        detail: `${owner} lets every environment of ${askedAsName} call this address.`,
        setupPath: null,
      }
    case 'some_environments':
      return {
        tone: 'warning',
        title: 'Only some environments may call it',
        detail: `${owner} lets only some environments of ${askedAsName} call this address. The others get 403.`,
        setupPath: access.setup_path,
      }
    default:
      return {
        tone: 'error',
        title: 'Calls get 403',
        detail: `The name resolves, but ${owner} belongs to another project. The internal proxy refuses ${askedAsName} until ${access.target_project_name} allows it under Internal access.`,
        setupPath: access.setup_path,
      }
  }
}

// ── Sandbox callers (ADR 105) ───────────────────────────────────────────────

/**
 * "Sandbox agent scratchpad · sbx_… (stopped)": the name its owner gave it,
 * and the id a refused call names.
 */
export function sandboxCallerLabel(
  grant: InternalAccessSandboxGrantResponse
): string {
  const state =
    grant.sandbox_status === 'running' ? '' : ` (${grant.sandbox_status})`
  return `Sandbox ${namedSandbox(grant.sandbox_name, grant.sandbox_id)}${state}`
}

/** "agent scratchpad · sbx_…", or just the id when it has no other name. */
export function namedSandbox(name: string, id: string): string {
  const label = name.trim()
  return label === '' || label === id ? id : `${label} · ${id}`
}

/** The command a sandbox runs to call an internal address. */
export function sandboxCurl(
  projectSlug: string,
  environmentSlug = 'production'
) {
  return `curl -sS ${exampleAddress(projectSlug, environmentSlug)}/`
}

/**
 * What the DNS test tool says about a sandbox calling an internal name
 * (ADR 105). A sandbox is never "the same project".
 */
export function sandboxAccessDiagnostic(
  access: DnsInternalAccess,
  sandboxId: string
): InternalAccessDiagnostic {
  const owner = `${access.target_project_name} (${access.target_environment_name})`
  if (access.reach === 'granted') {
    return {
      tone: 'success',
      title: 'Calls are allowed',
      detail: `${owner} lets sandbox ${sandboxId} call this address over plain http.`,
      setupPath: null,
    }
  }
  return {
    tone: 'error',
    title: 'Calls get 403',
    detail: `The internal proxy refuses sandbox ${sandboxId} until ${access.target_project_name} adds it as a caller under Internal access.`,
    setupPath: access.setup_path,
  }
}
