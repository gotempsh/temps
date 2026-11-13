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
/** Mirrors `MAX_GRANTS_PER_PROJECT`. */
export const MAX_GRANTS_PER_PROJECT = 100
export const GRANTS_PAGE_SIZE = 20

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

const CHOOSE_CALLER = 'Choose the project that will make the calls'

export const internalAccessFormSchema = z.object({
  /**
   * `null` until a project is picked; saving requires one. The refinement
   * returns a plain boolean, not a type guard, so the form's input and output
   * types stay the same for `zodResolver`.
   */
  callerProjectId: z
    .number()
    .int()
    .positive()
    .nullable()
    .refine((value): boolean => value !== null, { error: CHOOSE_CALLER }),
  /** `null`: every environment of the calling project. */
  callerEnvironmentId: z.number().int().positive().nullable(),
  /** `null`: every environment of this project. */
  targetEnvironmentId: z.number().int().positive().nullable(),
  description: z.string().max(MAX_GRANT_DESCRIPTION_LENGTH, {
    error: `At most ${MAX_GRANT_DESCRIPTION_LENGTH} characters`,
  }),
})

export type InternalAccessFormValues = z.infer<typeof internalAccessFormSchema>

/**
 * The form's starting values. On an environment's card the target is that
 * environment; on the project card it starts as the whole project.
 */
export function emptyGrantForm(
  environmentId?: number
): InternalAccessFormValues {
  return {
    callerProjectId: null,
    callerEnvironmentId: null,
    targetEnvironmentId: environmentId ?? null,
    description: '',
  }
}

/** `null` while the form cannot be sent (no calling project yet). */
export function toCreateRequest(
  values: InternalAccessFormValues
): CreateInternalAccessGrantRequest | null {
  if (values.callerProjectId === null) return null
  const description = values.description.trim()
  return {
    caller_project_id: values.callerProjectId,
    caller_environment_id: values.callerEnvironmentId,
    target_environment_id: values.targetEnvironmentId,
    description: description === '' ? null : description,
  }
}

/** "Checkout worker, all environments" or "Checkout worker, production". */
export function callerLabel(grant: InternalAccessGrantResponse): string {
  return grant.caller_environment_id == null
    ? `${grant.caller_project_name}, all environments`
    : `${grant.caller_project_name}, ${grant.caller_environment_name ?? `environment ${grant.caller_environment_id}`}`
}

/** What the grant opens on this project. */
export function targetLabel(grant: InternalAccessGrantResponse): string {
  return grant.target_environment_id == null
    ? 'Every environment'
    : (grant.target_environment_name ??
        `Environment ${grant.target_environment_id}`)
}

/**
 * On an environment's card, a project-wide grant applies but is managed on
 * the project's card.
 */
export function isInherited(
  grant: InternalAccessGrantResponse,
  environmentId?: number
): boolean {
  return environmentId !== undefined && grant.target_environment_id == null
}

/** The address an app in the calling project would use. */
export function exampleAddress(
  projectSlug: string,
  environmentSlug = 'production'
): string {
  return `http://${environmentSlug}.${projectSlug}.temps.local`
}

/** The add button: what saving does, naming what becomes callable. */
export function allowLabel(targetName: string): string {
  return `Allow it to call ${targetName}`
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

const CHOOSE_SANDBOX = 'Choose the sandbox that will make the calls'

export const sandboxGrantFormSchema = z.object({
  /** Public id of one of the user's sandboxes; `''` until one is picked. */
  sandboxId: z.string().min(1, { error: CHOOSE_SANDBOX }),
  /** `null`: every environment of this project. */
  targetEnvironmentId: z.number().int().positive().nullable(),
  description: z.string().max(MAX_GRANT_DESCRIPTION_LENGTH, {
    error: `At most ${MAX_GRANT_DESCRIPTION_LENGTH} characters`,
  }),
})

export type SandboxGrantFormValues = z.infer<typeof sandboxGrantFormSchema>

export function emptySandboxGrantForm(
  environmentId?: number
): SandboxGrantFormValues {
  return {
    sandboxId: '',
    targetEnvironmentId: environmentId ?? null,
    description: '',
  }
}

/** `null` while no sandbox is picked. */
export function toCreateSandboxRequest(
  values: SandboxGrantFormValues
): CreateInternalAccessSandboxGrantRequest | null {
  if (values.sandboxId === '') return null
  const description = values.description.trim()
  return {
    sandbox_id: values.sandboxId,
    target_environment_id: values.targetEnvironmentId,
    description: description === '' ? null : description,
  }
}

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

/** What a sandbox grant opens on this project. */
export function sandboxTargetLabel(
  grant: InternalAccessSandboxGrantResponse
): string {
  return grant.target_environment_id == null
    ? 'Every environment'
    : (grant.target_environment_name ??
        `Environment ${grant.target_environment_id}`)
}

/** On an environment's card, a project-wide sandbox grant is inherited. */
export function isSandboxGrantInherited(
  grant: InternalAccessSandboxGrantResponse,
  environmentId?: number
): boolean {
  return environmentId !== undefined && grant.target_environment_id == null
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
