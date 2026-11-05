// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { detectPublicPresetsOptions } from '@/api/client/@tanstack/react-query.gen'
import { detectPublicPresets, type Options } from '@/api/client/sdk.gen'
import type { DetectPublicPresetsData } from '@/api/client/types.gen'
import { problemDetail } from '@/lib/api-problem'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'

/**
 * Detects a public repository's presets again, past the server's cache, and
 * puts the result in the query that `options` reads. A plain refetch would be
 * answered from that cache, so a Dockerfile added since would stay missing.
 *
 * The request travels as the mutation's variables, so the result is cached
 * for the repository that asked even if `options` changes while it is pending.
 */
export function useRefreshPublicPresets(
  options: Options<DetectPublicPresetsData>
) {
  const queryClient = useQueryClient()
  const mutation = useMutation({
    mutationFn: async (request: Options<DetectPublicPresetsData>) => {
      const { data } = await detectPublicPresets({
        ...request,
        query: { ...request.query, fresh: true },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (data, request) => {
      queryClient.setQueryData(
        detectPublicPresetsOptions(request).queryKey,
        data
      )
    },
    onError: (error) => {
      toast.error(
        problemDetail(error, 'Temps could not detect this repository again.')
      )
    },
  })
  // Bound when the refresh starts: the request is read now, not on success.
  return {
    ...mutation,
    mutate: () => mutation.mutate(options),
    mutateAsync: () => mutation.mutateAsync(options),
  }
}
