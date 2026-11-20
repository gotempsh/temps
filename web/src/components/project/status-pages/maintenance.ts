// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  CreateMaintenanceRequest,
  MaintenanceResponse,
  UpdateMaintenanceRequest,
} from '@/api/client'

export const MAX_MAINTENANCE_TITLE_CHARS = 120
export const MAX_MAINTENANCE_DESCRIPTION_CHARS = 2000
export const MAX_MAINTENANCE_WINDOW_MS = 7 * 24 * 60 * 60 * 1000
/** How far back a start may lie; matches the server's slack. */
export const MAINTENANCE_BACKDATE_SLACK_MS = 5 * 60 * 1000
export const MAINTENANCE_PAGE_SIZE = 20

const pad = (n: number) => String(n).padStart(2, '0')

/** A `datetime-local` input value for `date` in the browser's zone. */
export function toLocalInput(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

/** A `datetime-local` value as a Date in the browser's zone, or null. */
export function fromLocalInput(value: string): Date | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/.test(value)) return null
  const date = new Date(value)
  return isNaN(date.getTime()) ? null : date
}

/**
 * The schedule form. `now` is passed in so the checks are testable; a
 * window that has started keeps its start, so `started` skips the start
 * checks.
 */
export function maintenanceSchema(now: () => Date, started = false) {
  return z
    .object({
      title: z
        .string()
        .trim()
        .min(1, 'Give the window a title; visitors see it')
        .max(
          MAX_MAINTENANCE_TITLE_CHARS,
          `At most ${MAX_MAINTENANCE_TITLE_CHARS} characters`
        ),
      description: z
        .string()
        .trim()
        .max(
          MAX_MAINTENANCE_DESCRIPTION_CHARS,
          `At most ${MAX_MAINTENANCE_DESCRIPTION_CHARS.toLocaleString('en-US')} characters`
        ),
      startsAt: z.string(),
      endsAt: z.string(),
      monitorIds: z
        .array(z.number())
        .min(1, 'Pick at least one component the work affects'),
    })
    .superRefine((values, ctx) => {
      const starts = fromLocalInput(values.startsAt)
      const ends = fromLocalInput(values.endsAt)
      if (!starts) {
        ctx.addIssue({
          code: 'custom',
          path: ['startsAt'],
          message: 'Pick a start',
        })
      }
      if (!ends) {
        ctx.addIssue({
          code: 'custom',
          path: ['endsAt'],
          message: 'Pick an end',
        })
      }
      if (!starts || !ends) return
      const current = now().getTime()
      if (
        !started &&
        starts.getTime() < current - MAINTENANCE_BACKDATE_SLACK_MS
      ) {
        ctx.addIssue({
          code: 'custom',
          path: ['startsAt'],
          message: 'A window cannot start in the past',
        })
      }
      if (ends.getTime() <= starts.getTime()) {
        ctx.addIssue({
          code: 'custom',
          path: ['endsAt'],
          message: 'End after the start',
        })
      } else if (
        ends.getTime() - starts.getTime() >
        MAX_MAINTENANCE_WINDOW_MS
      ) {
        ctx.addIssue({
          code: 'custom',
          path: ['endsAt'],
          message: 'A window lasts at most 7 days',
        })
      } else if (ends.getTime() <= current) {
        ctx.addIssue({
          code: 'custom',
          path: ['endsAt'],
          message: 'End in the future',
        })
      }
    })
}

export type MaintenanceValues = z.infer<ReturnType<typeof maintenanceSchema>>

/** Defaults for a new window: starting at the next hour, for one hour. */
export function newMaintenanceValues(
  now: Date,
  monitorIds: number[]
): MaintenanceValues {
  const starts = new Date(now)
  starts.setMinutes(0, 0, 0)
  starts.setHours(starts.getHours() + 1)
  const ends = new Date(starts.getTime() + 60 * 60 * 1000)
  return {
    title: '',
    description: '',
    startsAt: toLocalInput(starts),
    endsAt: toLocalInput(ends),
    monitorIds,
  }
}

export function maintenanceValues(
  item: MaintenanceResponse
): MaintenanceValues {
  return {
    title: item.title,
    description: item.description ?? '',
    startsAt: toLocalInput(new Date(item.starts_at)),
    endsAt: toLocalInput(new Date(item.ends_at)),
    monitorIds: item.monitor_ids,
  }
}

function iso(value: string): string {
  return (fromLocalInput(value) ?? new Date(NaN)).toISOString()
}

export function createMaintenanceRequest(
  values: MaintenanceValues
): CreateMaintenanceRequest {
  return {
    title: values.title.trim(),
    description: values.description.trim() || null,
    starts_at: iso(values.startsAt),
    ends_at: iso(values.endsAt),
    monitor_ids: values.monitorIds,
  }
}

/**
 * The edit body. A window that has started only sends what may still
 * change (title, description, end); an empty description removes it.
 */
/**
 * The edit body. Times are sent only when the user changed them: the form
 * shows them to the minute, so resending an untouched value would move a
 * window created with seconds (`--starts now`), and the server refuses any
 * start change once the window has begun.
 */
export function updateMaintenanceRequest(
  values: MaintenanceValues,
  saved: MaintenanceValues,
  started: boolean
): UpdateMaintenanceRequest {
  const body: UpdateMaintenanceRequest = {
    title: values.title.trim(),
    description: values.description.trim(),
  }
  if (values.endsAt !== saved.endsAt) body.ends_at = iso(values.endsAt)
  if (!started) {
    if (values.startsAt !== saved.startsAt) {
      body.starts_at = iso(values.startsAt)
    }
    body.monitor_ids = values.monitorIds
  }
  return body
}

/** Whether `item` has begun at `now`, even if the list was loaded before. */
export function hasStarted(item: MaintenanceResponse, now: Date): boolean {
  return (
    item.state === 'in_progress' ||
    (item.state === 'scheduled' && new Date(item.starts_at) <= now)
  )
}

export function isOpen(item: MaintenanceResponse): boolean {
  return item.state === 'scheduled' || item.state === 'in_progress'
}

export const MAINTENANCE_STATE_LABELS: Record<
  MaintenanceResponse['state'],
  string
> = {
  scheduled: 'Scheduled',
  in_progress: 'In progress',
  completed: 'Completed',
  cancelled: 'Cancelled',
}
