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
 */
export function useRefreshPublicPresets(
  options: Options<DetectPublicPresetsData>
) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () => {
      const { data } = await detectPublicPresets({
        ...options,
        query: { ...options.query, fresh: true },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (data) => {
      queryClient.setQueryData(
        detectPublicPresetsOptions(options).queryKey,
        data
      )
    },
    onError: (error) => {
      toast.error(
        problemDetail(error, 'Temps could not detect this repository again.')
      )
    },
  })
}
