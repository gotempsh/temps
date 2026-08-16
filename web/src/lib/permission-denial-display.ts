function humanize(value: string): string {
  return value.toLowerCase().replace(/_/g, ' ')
}

function get<T>(
  data: Record<string, unknown> | undefined,
  key: string
): T | undefined {
  return data?.[key] as T | undefined
}

export function describePermissionDenial(
  data?: Record<string, unknown>
): string {
  const method = get<string>(data, 'method')
  const route = get<string>(data, 'route')
  const authSource = get<string>(data, 'auth_source')
  const attemptCount = get<number>(data, 'attempt_count')
  const target = [method, route].filter(Boolean).join(' ')
  const source = authSource ? ` for ${humanize(authSource)}` : ''
  const attempts =
    attemptCount && attemptCount > 1 ? ` (${attemptCount} attempts)` : ''

  return `Denied ${target || 'a request'}${source}${attempts}`
}
