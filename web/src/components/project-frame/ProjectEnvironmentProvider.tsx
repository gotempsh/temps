// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { useQuery } from '@tanstack/react-query'
import { useSearchParams } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import {
  ENVIRONMENT_PARAM,
  LEGACY_ENVIRONMENT_PARAM,
  legacyEnvironmentSlug,
  resolveDefaultEnvironment,
  resolveEnvironmentSelection,
} from '@/lib/project-environment-selection'
import type { EnvironmentScope } from '@/lib/project-navigation'
import {
  ProjectEnvironmentContext,
  defaultEnvironmentKey,
  rememberedEnvironmentKey,
  type ProjectEnvironmentContextValue,
} from './project-environment-context'

function readStorage(key: string): string | null {
  try {
    return window.localStorage.getItem(key)
  } catch {
    return null
  }
}

function writeStorage(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value)
  } catch {
    // Private mode or a full quota: the choice still applies via the URL.
  }
}

/**
 * Owns the environment chosen in the project bar: `?env=<slug>` in the
 * URL, remembered per project in this browser, and a per-project default.
 *
 * Prototype note: the default lives in localStorage. Shipping it needs a
 * column on the project row so it is shared by everyone on the team.
 */
export function ProjectEnvironmentProvider({
  project,
  scope,
  children,
}: {
  project: ProjectResponse
  scope: EnvironmentScope
  children: ReactNode
}) {
  const [searchParams, setSearchParams] = useSearchParams()
  const environmentsQuery = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: project.id } }),
    enabled: !!project.id,
  })
  const [remembered, setRemembered] = useState(() =>
    readStorage(rememberedEnvironmentKey(project.id))
  )
  const [preferredDefault, setPreferredDefault] = useState(() =>
    readStorage(defaultEnvironmentKey(project.id))
  )
  const environments = useMemo(
    () => environmentsQuery.data ?? [],
    [environmentsQuery.data]
  )
  // `?env=` wins; a link that still says `?environment=<id>` is honoured.
  const envParam = searchParams.get(ENVIRONMENT_PARAM)
  const legacySlug = legacyEnvironmentSlug(
    environments,
    searchParams.get(LEGACY_ENVIRONMENT_PARAM)
  )
  const urlParam = envParam ?? legacySlug
  // ...and rewritten as `?env=<slug>`, which tabs and menu links carry on,
  // so the next page keeps the environment the link named.
  useEffect(() => {
    if (envParam || !legacySlug) return
    setSearchParams(
      (current) => {
        const next = new URLSearchParams(current)
        next.set(ENVIRONMENT_PARAM, legacySlug)
        next.delete(LEGACY_ENVIRONMENT_PARAM)
        return next
      },
      { replace: true }
    )
  }, [envParam, legacySlug, setSearchParams])

  const selection = useMemo(
    () =>
      resolveEnvironmentSelection({
        environments,
        urlParam,
        remembered,
        preferredDefault,
        allowAll: scope === 'all',
      }),
    [environments, urlParam, remembered, preferredDefault, scope]
  )

  const select = useCallback(
    (value: string) => {
      writeStorage(rememberedEnvironmentKey(project.id), value)
      setRemembered(value)
      setSearchParams((current) => {
        const next = new URLSearchParams(current)
        next.set(ENVIRONMENT_PARAM, value)
        return next
      })
    },
    [project.id, setSearchParams]
  )

  const setDefault = useCallback(
    (slug: string) => {
      writeStorage(defaultEnvironmentKey(project.id), slug)
      setPreferredDefault(slug)
    },
    [project.id]
  )

  const { refetch } = environmentsQuery
  const value = useMemo<ProjectEnvironmentContextValue>(
    () => ({
      environments,
      isPending: environmentsQuery.isPending,
      isError: environmentsQuery.isError,
      retry: () => void refetch(),
      scope,
      selection,
      select,
      defaultEnvironment: resolveDefaultEnvironment(
        environments,
        preferredDefault
      ),
      setDefault,
    }),
    [
      environments,
      environmentsQuery.isPending,
      environmentsQuery.isError,
      refetch,
      scope,
      selection,
      select,
      preferredDefault,
      setDefault,
    ]
  )

  return (
    <ProjectEnvironmentContext.Provider value={value}>
      {children}
    </ProjectEnvironmentContext.Provider>
  )
}
