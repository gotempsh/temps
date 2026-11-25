// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Whether a view of an entity (its traces, its domains) holds anything.
 *
 * - `active`: in use. It is a tab of the entity's page.
 * - `inactive`: known to hold nothing. It is offered under "Add" on the
 *   page, and takes no tab.
 * - `unknown`: not answered yet, or the answer could not be had (still
 *   loading, the request failed, no permission). Treated as active, so a
 *   page is never hidden on a guess.
 */
export type AreaState = 'active' | 'inactive' | 'unknown'

export interface AreaStatus {
  state: AreaState
  /** One current number for an active area, such as "3 databases". */
  metric?: string
}

export type AreaStatuses = Record<string, AreaStatus>

export const UNKNOWN: AreaStatus = { state: 'unknown' }

/**
 * Whether a view is a tab of its entity's page. One with no status needs no
 * set-up and always is.
 */
export function listsAsTab(status: AreaStatus | undefined): boolean {
  return status?.state !== 'inactive'
}

/** `count` things, as an area's status: none means not set up. */
export function countStatus(
  count: number | undefined,
  singular: string,
  plural = `${singular}s`
): AreaStatus {
  if (count === undefined) return UNKNOWN
  if (count === 0) return { state: 'inactive' }
  return {
    state: 'active',
    metric: `${count} ${count === 1 ? singular : plural}`,
  }
}

const STORAGE_KEY = 'temps:nav:area-states'

type Remembered = Record<string, Record<string, 'active' | 'inactive'>>

function readRemembered(): Remembered {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    const parsed: unknown = raw ? JSON.parse(raw) : {}
    return parsed && typeof parsed === 'object' ? (parsed as Remembered) : {}
  } catch {
    // Storage unavailable or corrupt: start without a memory.
    return {}
  }
}

/**
 * The states last seen for `scope` (the server, a project, a service), so
 * its page opens with the tabs it had instead of showing every view and
 * then dropping the ones that turn out to hold nothing.
 */
export function rememberedStates(scope: string): AreaStatuses {
  const remembered = readRemembered()[scope] ?? {}
  return Object.fromEntries(
    Object.entries(remembered).map(([id, state]) => [id, { state }])
  )
}

/** Keep the known states of `scope` for the next visit. */
export function rememberStates(scope: string, statuses: AreaStatuses): void {
  const known = Object.entries(statuses).filter(
    ([, status]) => status.state !== 'unknown'
  )
  if (known.length === 0) return
  try {
    const all = readRemembered()
    all[scope] = {
      ...all[scope],
      ...Object.fromEntries(
        known.map(([id, status]) => [id, status.state as 'active' | 'inactive'])
      ),
    }
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(all))
  } catch {
    // Not remembered across visits; the live answer still applies now.
  }
}

/** A live answer where there is one, else what was seen last time. */
export function withRemembered(
  live: AreaStatuses,
  remembered: AreaStatuses
): AreaStatuses {
  const merged: AreaStatuses = { ...remembered }
  for (const [id, status] of Object.entries(live)) {
    if (status.state !== 'unknown' || !merged[id]) merged[id] = status
  }
  return merged
}
