// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as z from 'zod'
import type {
  EnvironmentResponse,
  MonitorResponse,
  StatusPageAppearance,
  StatusPageDomainResponse,
  StatusPageMonitorInput,
  StatusPageResponse,
} from '@/api/client'

/** Server-side limits, mirrored for inline validation. */
export const MAX_TITLE_CHARS = 120
export const MAX_DESCRIPTION_CHARS = 1000
export const MAX_DISPLAY_NAME_CHARS = 80
export const MAX_GROUP_NAME_CHARS = 80
export const MAX_MONITORS_PER_PAGE = 50
export const MAX_INCIDENT_UPDATE_CHARS = 5000
export const MAX_INCIDENT_PUBLIC_TITLE_CHARS = 200
export const MAX_PAGE_URL_CHARS = 2048
export const DEFAULT_ACCENT_COLOR = '#4f46e5'

export const INCIDENT_SEVERITIES = ['minor', 'major', 'critical'] as const

export const INCIDENT_STATUSES = [
  'investigating',
  'identified',
  'monitoring',
  'resolved',
] as const

const componentRowSchema = z.object({
  monitor_id: z.number(),
  /** Internal monitor name; shown to the team only, never published. */
  monitor_name: z.string(),
  project_id: z.number(),
  project_name: z.string(),
  environment_name: z.string().nullable(),
  display_name: z
    .string()
    .trim()
    .min(1, 'Give this component a public name')
    .max(
      MAX_DISPLAY_NAME_CHARS,
      `At most ${MAX_DISPLAY_NAME_CHARS} characters`
    ),
  group_name: z
    .string()
    .trim()
    .max(MAX_GROUP_NAME_CHARS, `At most ${MAX_GROUP_NAME_CHARS} characters`),
})

/** The components of a page, in display order. */
export const componentsSchema = z
  .array(componentRowSchema)
  .min(1, 'Add at least one component')
  .max(
    MAX_MONITORS_PER_PAGE,
    `A status page can show at most ${MAX_MONITORS_PER_PAGE} components`
  )

export const statusPageFormSchema = z.object({ components: componentsSchema })

export type StatusPageFormValues = z.infer<typeof statusPageFormSchema>
export type ComponentRow = z.infer<typeof componentRowSchema>

/** The page's components as editable rows, in page order. */
export function componentRowsFromPage(
  page: Pick<StatusPageResponse, 'monitors'>
): ComponentRow[] {
  return [...page.monitors]
    .sort((a, b) => a.position - b.position)
    .map((m) => ({
      monitor_id: m.monitor_id,
      monitor_name: m.monitor_name,
      project_id: m.project_id,
      project_name: m.project_name,
      environment_name: m.environment_name ?? null,
      display_name: m.display_name,
      group_name: m.group_name ?? '',
    }))
}

/** The selection to send to the API, in display order. */
export function toSelection(rows: ComponentRow[]): StatusPageMonitorInput[] {
  return rows.map((row) => ({
    monitor_id: row.monitor_id,
    display_name: row.display_name.trim(),
    group_name: row.group_name.trim() === '' ? null : row.group_name.trim(),
  }))
}

export function formDefaults(page: StatusPageResponse): StatusPageFormValues {
  return { components: componentRowsFromPage(page) }
}

/** Group names already used on a page, for one-click reuse. */
export function groupNames(rows: Pick<ComponentRow, 'group_name'>[]): string[] {
  return [...new Set(rows.map((r) => r.group_name.trim()).filter(Boolean))]
}

/**
 * Filter matching every cached query of one generated SDK operation. The
 * generated keys are `[{ _id, baseUrl, path, query, ... }]`, and TanStack
 * matches object keys partially, so `[{ _id }]` covers all variants.
 */
export function operationQueries(operationId: string) {
  return { queryKey: [{ _id: operationId }] }
}

export const incidentUpdateSchema = z.object({
  status: z.enum(INCIDENT_STATUSES),
  message: z
    .string()
    .trim()
    .min(1, 'Write a message for the update')
    .max(
      MAX_INCIDENT_UPDATE_CHARS,
      `At most ${MAX_INCIDENT_UPDATE_CHARS.toLocaleString('en-US')} characters`
    ),
  is_public: z.boolean(),
})

export type IncidentUpdateValues = z.infer<typeof incidentUpdateSchema>

/** The incident's current status if it is a known one, else "investigating". */
export function initialIncidentStatus(
  status: string
): IncidentUpdateValues['status'] {
  return (INCIDENT_STATUSES as readonly string[]).includes(status)
    ? (status as IncidentUpdateValues['status'])
    : 'investigating'
}

/**
 * Client-side shape check for a custom hostname. The server normalizes
 * (case, trailing dot, IDNA) and decides; this only catches obvious mistakes
 * such as pasting a URL.
 */
export const addDomainSchema = z.object({
  hostname: z
    .string()
    .trim()
    .min(1, 'Enter a hostname such as status.example.com')
    .max(253, 'At most 253 characters')
    .refine((value) => !value.includes('*'), 'Wildcards are not supported')
    .refine(
      (value) => !/[/:@\s?#]/.test(value),
      'Enter just the hostname, without https:// or a path'
    )
    .refine(
      (value) => value.replace(/\.$/, '').includes('.'),
      'Use a fully qualified hostname such as status.example.com'
    ),
  verification_method: z.enum(['http-01', 'dns-01']),
})

export type AddDomainValues = z.infer<typeof addDomainSchema>

/** Human label for a custom domain's state. */
export function domainStatusLabel(
  domain: Pick<StatusPageDomainResponse, 'status' | 'serving'>
): string {
  switch (domain.status) {
    case 'active':
      return domain.serving ? 'Serving' : 'Verified · page unpublished'
    case 'failed':
      return 'Verification failed'
    case 'unverified':
      return 'Needs verification'
    case 'renewal_pending':
      return 'Serving · renewal in progress'
    case 'renewal_failed':
      return 'Serving · renewal failed'
    default:
      return 'Waiting for DNS'
  }
}

/** Days to renew a certificate before it expires, as the renewal job does. */
export const CERTIFICATE_RENEWAL_WINDOW_DAYS = 30

/** Whole days until `expiresAt`, or null when unknown. */
export function daysUntil(
  expiresAt: string | null | undefined,
  now: Date = new Date()
): number | null {
  if (!expiresAt) return null
  const expires = new Date(expiresAt).getTime()
  if (Number.isNaN(expires)) return null
  return Math.floor((expires - now.getTime()) / 86_400_000)
}

/** What the operator can do with a custom domain right now. */
export function domainActions(
  domain: Pick<StatusPageDomainResponse, 'status'>,
  pagePublished: boolean
): { verify: boolean; renew: boolean } {
  const hasCertificate =
    domain.status === 'active' ||
    domain.status === 'renewal_pending' ||
    domain.status === 'renewal_failed'
  return {
    // Completes an open order, or starts one; certificates are only issued
    // for published pages.
    verify:
      pagePublished &&
      domain.status !== 'active' &&
      domain.status !== 'renewal_failed',
    // A new order while the current certificate keeps serving.
    renew:
      pagePublished && hasCertificate && domain.status !== 'renewal_pending',
  }
}

/** Title-case an internal name for use as a suggested public one. */
export function suggestPublicName(name: string): string {
  const words = name.replace(/[-_]+/g, ' ').trim().split(/\s+/).filter(Boolean)
  if (words.length === 0) return ''
  return words
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
    .slice(0, MAX_DISPLAY_NAME_CHARS)
}

/** One monitor offered in the component picker. */
export interface MonitorChoice {
  monitor: MonitorResponse
  /** Environment it checks, `null` for a project-level monitor. */
  environment_name: string | null
  is_preview: boolean
}

/**
 * A project's monitors for the picker: production (non-preview) environments
 * first, then previews, then monitors that check no environment.
 */
export function buildMonitorChoices(
  environments: EnvironmentResponse[],
  monitors: MonitorResponse[]
): MonitorChoice[] {
  const byId = new Map(environments.map((e) => [e.id, e]))
  const rank = (choice: MonitorChoice) =>
    choice.environment_name === null ? 2 : choice.is_preview ? 1 : 0
  return monitors
    .map((monitor) => {
      const environment =
        monitor.environment_id == null
          ? undefined
          : byId.get(monitor.environment_id)
      return {
        monitor,
        environment_name: environment?.name ?? null,
        is_preview: environment?.is_preview ?? false,
      }
    })
    .sort(
      (a, b) =>
        rank(a) - rank(b) ||
        Number(b.monitor.is_managed) - Number(a.monitor.is_managed)
    )
}

/**
 * A new component row for a picked monitor. A production environment is
 * named after its project ("Billing"), anything else after both ("Billing
 * staging"); the name is always editable before saving.
 */
export function componentRow(
  project: { id: number; name: string },
  choice: MonitorChoice,
  group_name = ''
): ComponentRow {
  const production = choice.environment_name !== null && !choice.is_preview
  return {
    monitor_id: choice.monitor.id,
    monitor_name: choice.monitor.name,
    project_id: project.id,
    project_name: project.name,
    environment_name: choice.environment_name,
    display_name: suggestPublicName(
      production ? project.name : `${project.name} ${choice.monitor.name}`
    ),
    group_name,
  }
}

/** How a monitor is named in the environment dropdown. */
export function choiceLabel(choice: MonitorChoice): string {
  if (choice.environment_name === null) {
    return `${choice.monitor.name} (project monitor)`
  }
  return choice.is_preview
    ? `${choice.environment_name} (preview)`
    : choice.environment_name
}

/** The best monitor to offer first: the first one not already on the page. */
export function firstAvailableChoice(
  choices: MonitorChoice[],
  taken: ReadonlySet<number>
): MonitorChoice | undefined {
  return choices.find((c) => !taken.has(c.monitor.id))
}

/** Whether a row still carries the public name the console suggested. */
function hasSuggestedName(row: ComponentRow): boolean {
  const name = row.display_name.trim()
  return (
    name === '' ||
    name === suggestPublicName(row.project_name) ||
    name === suggestPublicName(`${row.project_name} ${row.monitor_name}`)
  )
}

/**
 * Point an existing component at another project or environment. Its group
 * is kept, and so is a public name the operator typed; a suggested name
 * follows the new monitor.
 */
export function switchComponent(
  row: ComponentRow,
  project: { id: number; name: string },
  choice: MonitorChoice
): ComponentRow {
  const next = componentRow(project, choice, row.group_name)
  return hasSuggestedName(row)
    ? next
    : { ...next, display_name: row.display_name }
}

export const createStatusPageSchema = z.object({
  title: z
    .string()
    .trim()
    .min(1, 'Title is required')
    .max(MAX_TITLE_CHARS, `At most ${MAX_TITLE_CHARS} characters`),
  components: componentsSchema,
  publish: z.boolean(),
})

export type CreateStatusPageValues = z.infer<typeof createStatusPageSchema>

export function createDefaults(
  title: string,
  components: ComponentRow[]
): CreateStatusPageValues {
  return {
    title: title.slice(0, MAX_TITLE_CHARS),
    publish: true,
    components,
  }
}

const httpsUrl = (label: string) =>
  z
    .string()
    .trim()
    .max(MAX_PAGE_URL_CHARS, `At most ${MAX_PAGE_URL_CHARS} characters`)
    .refine(
      (value) => value === '' || /^https:\/\/[^\s/@]+(\/\S*)?$/i.test(value),
      `${label} must be an https:// address`
    )

export const appearanceFormSchema = z.object({
  title: z
    .string()
    .trim()
    .min(1, 'Title is required')
    .max(MAX_TITLE_CHARS, `At most ${MAX_TITLE_CHARS} characters`),
  description: z
    .string()
    .max(MAX_DESCRIPTION_CHARS, `At most ${MAX_DESCRIPTION_CHARS} characters`),
  theme: z.enum(['auto', 'light', 'dark']),
  accent_color: z
    .string()
    .trim()
    .refine(
      (value) => value === '' || /^#?[0-9a-f]{6}$/i.test(value),
      'Use a hex color such as #4f46e5'
    ),
  logo_url: httpsUrl('Logo URL'),
  website_url: httpsUrl('Website URL'),
  show_uptime_percentage: z.boolean(),
  show_response_times: z.boolean(),
})

export type AppearanceFormValues = z.infer<typeof appearanceFormSchema>

export function appearanceDefaults(
  page: StatusPageResponse
): AppearanceFormValues {
  const appearance: StatusPageAppearance = page.appearance
  return {
    title: page.title,
    description: page.description ?? '',
    theme: appearance.theme,
    accent_color: appearance.accent_color ?? '',
    logo_url: appearance.logo_url ?? '',
    website_url: appearance.website_url ?? '',
    show_uptime_percentage: appearance.show_uptime_percentage,
    show_response_times: appearance.show_response_times,
  }
}

/** `#rrggbb` with the hash, lowercase; empty stays empty (default color). */
export function normalizeAccent(value: string): string {
  const trimmed = value.trim()
  if (trimmed === '') return ''
  return `#${trimmed.replace(/^#/, '').toLowerCase()}`
}

export const customIncidentSchema = z.object({
  public_title: z
    .string()
    .trim()
    .min(1, 'Write a short headline visitors will see')
    .max(
      MAX_INCIDENT_PUBLIC_TITLE_CHARS,
      `At most ${MAX_INCIDENT_PUBLIC_TITLE_CHARS} characters`
    ),
  severity: z.enum(INCIDENT_SEVERITIES),
  status: z.enum(INCIDENT_STATUSES),
  /**
   * `project:<id>` for every component of one project on this page, or
   * `monitor:<id>` for one component.
   */
  target: z.string().regex(/^(project|monitor):\d+$/, 'Pick what is affected'),
  message: z
    .string()
    .trim()
    .min(1, 'Write what is happening')
    .max(
      MAX_INCIDENT_UPDATE_CHARS,
      `At most ${MAX_INCIDENT_UPDATE_CHARS.toLocaleString('en-US')} characters`
    ),
  is_public: z.boolean(),
})

export type CustomIncidentValues = z.infer<typeof customIncidentSchema>

/** What an incident on this page can be about. */
export interface IncidentTarget {
  value: string
  label: string
  project_id: number
}

/**
 * Incidents belong to a project. A page drawing from several projects offers
 * each project as a whole and each of its components.
 */
export function incidentTargets(
  page: Pick<StatusPageResponse, 'monitors' | 'projects'>
): IncidentTarget[] {
  const several = page.projects.length > 1
  const targets: IncidentTarget[] = []
  for (const project of page.projects) {
    targets.push({
      value: `project:${project.id}`,
      label: several ? `Everything from ${project.name}` : 'All components',
      project_id: project.id,
    })
    for (const m of page.monitors) {
      if (m.project_id !== project.id) continue
      targets.push({
        value: `monitor:${m.monitor_id}`,
        label: m.group_name
          ? `${m.group_name} · ${m.display_name}`
          : m.display_name,
        project_id: project.id,
      })
    }
  }
  return targets
}

/**
 * Where to post the incident (`POST /projects/{project_id}/incidents`) and
 * its body, or `null` when the target is not on the page.
 */
export function customIncidentBody(
  values: CustomIncidentValues,
  page: Pick<StatusPageResponse, 'monitors' | 'projects'>
) {
  const [kind, raw] = values.target.split(':')
  const id = Number.parseInt(raw ?? '', 10)
  const projectId =
    kind === 'project'
      ? page.projects.find((p) => p.id === id)?.id
      : page.monitors.find((m) => m.monitor_id === id)?.project_id
  if (projectId === undefined) return null
  return {
    project_id: projectId,
    body: {
      // The internal title mirrors the public one; it is what the team sees
      // in incident lists and notifications.
      title: values.public_title.trim(),
      public_title: values.public_title.trim(),
      severity: values.severity,
      status: values.status,
      monitor_id: kind === 'monitor' ? id : null,
      environment_id: null,
      description: null,
      message: values.message.trim(),
      is_public: values.is_public,
    },
  }
}
