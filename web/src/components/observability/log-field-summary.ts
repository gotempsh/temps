// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Fields shown beside a message in the list, before opening the record. */
const MAX_FIELDS = 3
/** A value longer than this is cut, so one field cannot take the row. */
const MAX_VALUE = 24

export interface LogFieldChip {
  key: string
  value: string
}

/**
 * The few structured fields worth reading in the list itself.
 *
 * A JSON line's message is often a summary of its own fields ("GET /cart 200
 * 34ms" next to method, path, status and duration), so a field whose value
 * the message already contains is skipped. Only scalar values are shown;
 * nested objects stay in the record panel. Long values (ids) are shortened
 * from the middle, so both ends stay recognisable.
 */
export function logFieldSummary(
  fields: unknown,
  message: string
): LogFieldChip[] {
  if (!fields || typeof fields !== 'object' || Array.isArray(fields)) return []
  const chips: LogFieldChip[] = []
  for (const [key, raw] of Object.entries(fields as Record<string, unknown>)) {
    if (chips.length === MAX_FIELDS) break
    if (
      typeof raw !== 'string' &&
      typeof raw !== 'number' &&
      typeof raw !== 'boolean'
    )
      continue
    const value = String(raw)
    if (!value || message.includes(value)) continue
    chips.push({ key, value: shorten(value) })
  }
  return chips
}

function shorten(value: string) {
  if (value.length <= MAX_VALUE) return value
  const keep = Math.floor((MAX_VALUE - 1) / 2)
  return `${value.slice(0, keep)}…${value.slice(-keep)}`
}
