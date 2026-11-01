// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useCallback, useContext, useState } from 'react'
import { ALL_ENVIRONMENTS } from '@/lib/project-environment-selection'
import { ProjectEnvironmentContext } from './project-environment-context'

/**
 * The environment a page filters by, as an id (`undefined` = every
 * environment). Inside a project it is the project bar's selection, so the
 * bar is the one picker; outside one (a page rendered on its own) it is the
 * page's own state.
 */
export function useEnvironmentFilter(): [
  number | undefined,
  (environmentId: number | undefined) => void,
] {
  const bar = useContext(ProjectEnvironmentContext)
  const [local, setLocal] = useState<number | undefined>(undefined)
  const select = bar?.select
  const environments = bar?.environments
  const set = useCallback(
    (environmentId: number | undefined) => {
      if (!select) {
        setLocal(environmentId)
        return
      }
      const environment = environments?.find((env) => env.id === environmentId)
      select(environment ? environment.slug : ALL_ENVIRONMENTS)
    },
    [select, environments]
  )
  if (!bar) return [local, set]
  return [bar.selection.all ? undefined : bar.selection.environment?.id, set]
}

/** True inside a project, where the bar picks the environment. */
export function useBarPicksEnvironment(): boolean {
  return useContext(ProjectEnvironmentContext) !== null
}
