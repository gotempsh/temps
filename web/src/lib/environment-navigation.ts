export type EnvironmentView = 'containers' | 'metrics' | 'settings'

export function resolveEnvironmentView(value: string | null): EnvironmentView {
  if (value === 'settings') return 'settings'
  if (value === 'metrics') return 'metrics'
  return 'containers'
}

export function resolveEnvironmentId(
  value: string | null,
  availableIds: number[]
): number | undefined {
  const requestedId = value === null ? Number.NaN : Number(value)
  return availableIds.includes(requestedId) ? requestedId : availableIds[0]
}

export function resolveEnvironmentSelection(
  value: string | null,
  environments: Array<{ id: number; name: string }>
): number | undefined {
  if (!value) return environments[0]?.id

  const requestedId = Number(value)
  const byId = environments.find(
    (environment) => environment.id === requestedId
  )
  if (byId) return byId.id

  const normalized = value.trim().toLowerCase()
  return (
    environments.find(
      (environment) => environment.name.trim().toLowerCase() === normalized
    )?.id ?? environments[0]?.id
  )
}

export function updateEnvironmentSearchParams(
  current: URLSearchParams,
  change: { environmentId?: number | null; view?: EnvironmentView }
): URLSearchParams {
  const next = new URLSearchParams(current)

  if (change.environmentId === null) {
    next.delete('environment')
  } else if (change.environmentId !== undefined) {
    next.set('environment', change.environmentId.toString())
  }

  if (change.view === 'settings' || change.view === 'metrics') {
    next.set('view', change.view)
  } else if (change.view === 'containers') {
    next.delete('view')
  }

  return next
}
