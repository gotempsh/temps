// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** When an ignored error group comes back, as the API takes it. */
export interface IgnoreConditions {
  until?: string
  event_count?: number
  user_count?: number
  forever?: boolean
}

/** The choices the Ignore menu offers. */
export type IgnorePreset =
  | 'next_event'
  | 'one_hour'
  | 'one_day'
  | 'one_week'
  | 'hundred_more'
  | 'ten_users'
  | 'forever'

export const IGNORE_PRESETS: { preset: IgnorePreset; label: string }[] = [
  { preset: 'next_event', label: 'Until it happens again' },
  { preset: 'one_hour', label: 'For 1 hour' },
  { preset: 'one_day', label: 'For 24 hours' },
  { preset: 'one_week', label: 'For 1 week' },
  { preset: 'hundred_more', label: 'Until 100 more occurrences' },
  { preset: 'ten_users', label: 'Until 10 more users are affected' },
  { preset: 'forever', label: 'Forever' },
]

const HOUR = 3_600_000

/**
 * The conditions a preset stands for, resolved against `now`. A plain
 * ignore (`next_event`) has none: the next event reopens the group.
 */
export function ignorePresetConditions(
  preset: IgnorePreset,
  now: Date = new Date()
): IgnoreConditions | undefined {
  const after = (ms: number) => new Date(now.getTime() + ms).toISOString()
  switch (preset) {
    case 'next_event':
      return undefined
    case 'one_hour':
      return { until: after(HOUR) }
    case 'one_day':
      return { until: after(24 * HOUR) }
    case 'one_week':
      return { until: after(7 * 24 * HOUR) }
    case 'hundred_more':
      return { event_count: 100 }
    case 'ten_users':
      return { user_count: 10 }
    case 'forever':
      return { forever: true }
  }
}

/** What an ignored group's conditions mean, in one line. */
export interface IgnoreState {
  until?: string | null
  until_event_count?: number | null
  until_user_count?: number | null
  user_count: number
  forever: boolean
}

export function describeIgnoreState(
  state: IgnoreState,
  totalCount: number,
  formatDate: (iso: string) => string = (iso) => new Date(iso).toLocaleString()
): string {
  if (state.forever) {
    return 'Ignored forever: it stays ignored until you reopen it.'
  }
  const parts: string[] = []
  if (state.until) {
    parts.push(`until ${formatDate(state.until)}`)
  }
  if (state.until_event_count != null) {
    const left = Math.max(state.until_event_count - totalCount, 0)
    parts.push(
      `for ${left.toLocaleString()} more occurrence${left === 1 ? '' : 's'}`
    )
  }
  if (state.until_user_count != null) {
    parts.push(
      `until ${state.until_user_count.toLocaleString()} new users are affected (${state.user_count.toLocaleString()} so far)`
    )
  }
  return parts.length === 0
    ? 'Ignored until the next event.'
    : `Ignored ${parts.join(', or ')}, whichever comes first.`
}
