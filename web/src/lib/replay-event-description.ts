// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Native rows retain the complete rrweb event, including its nested data. */
export function replayEventData(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return {}
  const record = value as Record<string, unknown>
  const nested = record.data
  if (
    typeof record.type === 'number' &&
    nested &&
    typeof nested === 'object' &&
    !Array.isArray(nested)
  ) {
    return nested as Record<string, unknown>
  }
  return record
}

export function replayCustomDescription(value: unknown): string {
  const data = replayEventData(value)
  const tag = typeof data.tag === 'string' ? data.tag : ''
  const payload =
    data.payload && typeof data.payload === 'object'
      ? (data.payload as Record<string, unknown>)
      : {}
  if (tag === 'sentry.replay') return 'Replay segment metadata'
  if (tag === 'performanceSpan') return 'Performance measurement'
  if (tag === 'breadcrumb') {
    const category =
      typeof payload.category === 'string' ? payload.category : ''
    if (category === 'console') return 'Console breadcrumb'
    if (category.startsWith('ui.')) return 'Interaction breadcrumb'
    if (category === 'navigation') return 'Navigation breadcrumb'
    if (category === 'fetch' || category === 'xhr') return 'Network breadcrumb'
    return 'SDK breadcrumb'
  }
  if (tag === 'options' || tag === 'sdkConfig') return 'Recorder configuration'
  return tag ? `Custom event: ${tag.slice(0, 128)}` : 'Custom event'
}
