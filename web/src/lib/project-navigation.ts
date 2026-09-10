// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const PROJECT_PRIMARY_ROUTES = [
  'project',
  'deployments',
  'environments',
  'environment-variables',
  'domains',
  'git',
  'build',
  'settings',
  'analytics',
  'errors',
  'traces',
  'runtime',
  'storage',
] as const

/**
 * Return the most-specific permanent-sidebar route for a project sub-route.
 * Detail pages inherit their parent highlight, while secondary tools return
 * null so the grouped tools trigger can carry the active state instead.
 */
export function resolveProjectPrimaryRoute(
  activeRoute: string
): (typeof PROJECT_PRIMARY_ROUTES)[number] | null {
  if (activeRoute === '' || activeRoute === 'project') return 'project'
  if (activeRoute.startsWith('analytics/')) return null

  return (
    PROJECT_PRIMARY_ROUTES.filter(
      (route) => activeRoute === route || activeRoute.startsWith(`${route}/`)
    ).sort((a, b) => b.length - a.length)[0] ?? null
  )
}

export function isProjectToolsRoute(activeRoute: string): boolean {
  return (
    activeRoute !== '' &&
    activeRoute !== 'setup' &&
    !activeRoute.startsWith('settings') &&
    resolveProjectPrimaryRoute(activeRoute) === null
  )
}
