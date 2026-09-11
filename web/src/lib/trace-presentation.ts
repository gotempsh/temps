/** Compact display only: sorting continues to use the original millisecond value. */
export function formatTraceDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—'
  if (ms === 0) return '0 ms'
  if (ms < 0.001) return '<1 µs'
  if (ms < 1) return `${Math.round(ms * 1000)} µs`
  if (ms < 1000) return `${Math.round(ms)} ms`
  return `${Number((ms / 1000).toFixed(1))} s`
}
