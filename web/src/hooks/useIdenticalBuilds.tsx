// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  updateIdenticalBuilds,
  type IdenticalBuildMode,
  type IdenticalBuildScope,
  type IdenticalBuildsResponse,
} from '@/api/client'
import { getIdenticalBuildsOptions } from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export interface IdenticalBuildsChange {
  mode: IdenticalBuildMode
  scope: IdenticalBuildScope
}

/**
 * What the project's builds do when an identical build is already running,
 * and a save that keeps the server's answer in the cache.
 */
export function useIdenticalBuilds(projectId: number) {
  const queryClient = useQueryClient()
  const options = getIdenticalBuildsOptions({ path: { id: projectId } })
  const query = useQuery(options)
  const save = useMutation({
    retry: false,
    mutationFn: async (
      change: IdenticalBuildsChange
    ): Promise<IdenticalBuildsResponse> => {
      const { data } = await updateIdenticalBuilds({
        path: { id: projectId },
        body: change,
        throwOnError: true,
      })
      return data
    },
    onSuccess: async (data) => {
      // A read started before the save must not overwrite what it saved.
      await queryClient.cancelQueries({ queryKey: options.queryKey })
      queryClient.setQueryData(options.queryKey, data)
    },
  })
  return { query, save }
}
