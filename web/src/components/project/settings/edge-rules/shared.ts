// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import type { EnvironmentOption } from './TestRequestSheet'

export const edgeRulesPath = (projectSlug: string) =>
  `/projects/${projectSlug}/settings/edge-rules`

export function useProjectEnvironments(projectId: number) {
  const query = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId } }),
  })
  const environments = useMemo<EnvironmentOption[]>(
    () => (query.data ?? []).map((env) => ({ id: env.id, name: env.name })),
    [query.data]
  )
  return { environments, query }
}

export function environmentLabel(
  environments: EnvironmentOption[],
  environmentId: number | null
): string {
  if (environmentId === null) return 'All environments'
  return (
    environments.find((env) => env.id === environmentId)?.name ??
    `Environment ${environmentId}`
  )
}
