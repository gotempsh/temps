// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as z from 'zod'
import type {
  MonitorResponse,
  StatusPageDomainResponse,
  StatusPageMonitorInput,
  StatusPageResponse,
} from '@/api/client'

/** Server-side limits, mirrored for inline validation. */
export const MAX_TITLE_CHARS = 120
export const MAX_DESCRIPTION_CHARS = 1000
export const MAX_DISPLAY_NAME_CHARS = 80
export const MAX_MONITORS_PER_PAGE = 50
export const MAX_INCIDENT_UPDATE_CHARS = 5000

export const INCIDENT_STATUSES = [
  'investigating',
  'identified',
  'monitoring',
  'resolved',
] as const

const monitorRowSchema = z
  .object({
    monitor_id: z.number(),
    monitor_name: z.string(),
    selected: z.boolean(),
    display_name: z.string(),
  })
  .superRefine((row, ctx) => {
    if (!row.selected) return
    const name = row.display_name.trim()
    if (name.length === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['display_name'],
        message: 'Give this monitor a public name',
      })
    } else if (name.length > MAX_DISPLAY_NAME_CHARS) {
      ctx.addIssue({
        code: 'custom',
        path: ['display_name'],
        message: `At most ${MAX_DISPLAY_NAME_CHARS} characters`,
      })
    }
  })

export const statusPageFormSchema = z.object({
  title: z
    .string()
    .trim()
    .min(1, 'Title is required')
    .max(MAX_TITLE_CHARS, `At most ${MAX_TITLE_CHARS} characters`),
  description: z
    .string()
    .max(MAX_DESCRIPTION_CHARS, `At most ${MAX_DESCRIPTION_CHARS} characters`),
  monitors: z
    .array(monitorRowSchema)
    .refine(
      (rows) => rows.filter((r) => r.selected).length <= MAX_MONITORS_PER_PAGE,
      `A status page can show at most ${MAX_MONITORS_PER_PAGE} monitors`
    ),
})

export type StatusPageFormValues = z.infer<typeof statusPageFormSchema>
export type MonitorRow = StatusPageFormValues['monitors'][number]

/**
 * One row per project monitor: selected monitors first, in their page order,
 * then the rest. Unselected rows start with an empty public name on purpose —
 * the internal monitor name is never pre-filled as the public one.
 */
export function buildMonitorRows(
  monitors: MonitorResponse[],
  page?: Pick<StatusPageResponse, 'monitors'>
): MonitorRow[] {
  const selected = [...(page?.monitors ?? [])].sort(
    (a, b) => a.position - b.position
  )
  const byId = new Map(monitors.map((m) => [m.id, m]))
  const rows: MonitorRow[] = selected.map((s) => ({
    monitor_id: s.monitor_id,
    monitor_name: byId.get(s.monitor_id)?.name ?? s.monitor_name,
    selected: true,
    display_name: s.display_name,
  }))
  const selectedIds = new Set(selected.map((s) => s.monitor_id))
  for (const monitor of monitors) {
    if (selectedIds.has(monitor.id)) continue
    rows.push({
      monitor_id: monitor.id,
      monitor_name: monitor.name,
      selected: false,
      display_name: '',
    })
  }
  return rows
}

/** The selection to send to the API, in display order. */
export function toSelection(rows: MonitorRow[]): StatusPageMonitorInput[] {
  return rows
    .filter((row) => row.selected)
    .map((row) => ({
      monitor_id: row.monitor_id,
      display_name: row.display_name.trim(),
    }))
}

export function formDefaults(
  monitors: MonitorResponse[],
  page?: StatusPageResponse
): StatusPageFormValues {
  return {
    title: page?.title ?? '',
    description: page?.description ?? '',
    monitors: buildMonitorRows(monitors, page),
  }
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
