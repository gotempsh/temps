// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Internal access grants (ADR 100): which other projects may call this
 * project's `*.temps.local` addresses. Pure helpers for the card and the DNS
 * test tool, kept apart so they are tested without rendering.
 */
import { z } from 'zod'
import type {
  CreateInternalAccessGrantRequest,
  DnsInternalAccess,
  InternalAccessGrantResponse,
} from '@/api/client/types.gen'

/** Mirrors `MAX_DESCRIPTION_LENGTH` in the network-policy service. */
export const MAX_GRANT_DESCRIPTION_LENGTH = 200
/** Mirrors `MAX_GRANTS_PER_PROJECT`. */
export const MAX_GRANTS_PER_PROJECT = 100
export const GRANTS_PAGE_SIZE = 20

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
