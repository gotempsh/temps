// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQueries } from '@tanstack/react-query'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { EnvironmentResponse } from '@/api/client'
import { normalizeUrl } from '@/lib/deployment-url'

export interface ServiceLink {
  href: string
  /** What to print: host and path, without the scheme. */
  label: string
}

/** `https://web.example.com/` → `web.example.com`; `…/api` stays. */
export function displayUrl(href: string): string {
  return href.replace(/^[a-z][a-z0-9+.-]*:\/\//i, '').replace(/\/$/, '')
}

/** An environment's address, optionally with a path under it. */
export function environmentLink(
  environment: Pick<EnvironmentResponse, 'main_url'> | undefined,
  path = ''
): ServiceLink | null {
  const base = environment?.main_url ? normalizeUrl(environment.main_url) : null
  if (!base) return null
  const href = `${base.replace(/\/$/, '')}${path}`
  return { href, label: displayUrl(href) }
}

/**
 * The address a service's header opens: its production environment when it
 * has one, otherwise its first deployed non-preview environment.
 */
export function primaryEnvironment(
  environments: EnvironmentResponse[] | undefined
): EnvironmentResponse | undefined {
  const live = (environments ?? []).filter(
    (e) => !e.is_preview && e.current_deployment_id != null
  )
  return live.find((e) => e.slug === 'production') ?? live[0]
}

/**
 * Environments of the given services, by service and by id. One query per
 * service, shared through the query cache by every panel that asks.
 */
export function useServiceEnvironments(projectIds: number[]) {
  const results = useQueries({
    queries: projectIds.map((id) => ({
      ...getEnvironmentsOptions({ path: { project_id: id } }),
      retry: false,
    })),
  })
  // Cheap to rebuild: a project has a handful of services and environments.
  const byProject = new Map<number, EnvironmentResponse[]>()
  const byId = new Map<number, EnvironmentResponse>()
  projectIds.forEach((id, index) => {
    const environments = results[index]?.data ?? []
    byProject.set(id, environments)
    for (const environment of environments)
      byId.set(environment.id, environment)
  })
  return { byProject, byId, loading: results.some((r) => r.isLoading) }
}
