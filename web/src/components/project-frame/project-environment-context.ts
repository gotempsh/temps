// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'
import type { EnvironmentResponse } from '@/api/client'
import type { EnvironmentSelection } from '@/lib/project-environment-selection'
import type { EnvironmentScope } from '@/lib/project-navigation'

export interface ProjectEnvironmentContextValue {
  environments: EnvironmentResponse[]
  isPending: boolean
  isError: boolean
  retry: () => void
  /** How the current section uses the selection. */
  scope: EnvironmentScope
  selection: EnvironmentSelection<EnvironmentResponse>
  /** Select an environment slug, or `ALL_ENVIRONMENTS`, for this project. */
  select: (value: string) => void
  /** The environment visitors see first; the stored default or production. */
  defaultEnvironment: EnvironmentResponse | undefined
  setDefault: (slug: string) => void
}

export const ProjectEnvironmentContext =
  createContext<ProjectEnvironmentContextValue | null>(null)

/** The environment chosen in the project bar. */
export function useProjectEnvironment(): ProjectEnvironmentContextValue {
  const value = useContext(ProjectEnvironmentContext)
  if (!value)
    throw new Error(
      'useProjectEnvironment must be used inside ProjectEnvironmentProvider'
    )
  return value
}

/** localStorage keys; per project id so a rename keeps the choice. */
export const rememberedEnvironmentKey = (projectId: number) =>
  `temps:project:${projectId}:environment`
export const defaultEnvironmentKey = (projectId: number) =>
  `temps:project:${projectId}:default-environment`
