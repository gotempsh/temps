// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { updateOnboarding } from '@/api/client'
import {
  getOnboardingOptions,
  getOnboardingQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import type { OnboardingPatch } from '@/api/client/types.gen'
import { useIsInstanceAdmin } from '@/hooks/useIsInstanceAdmin'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

/** The install's first-run record, shared by `/welcome` and the Setup page. */
export function useOnboarding(options: { enabled?: boolean } = {}) {
  const isAdmin = useIsInstanceAdmin()
  return useQuery({
    ...getOnboardingOptions(),
    enabled: isAdmin && (options.enabled ?? true),
    staleTime: 30_000,
  })
}

/** Patch the record; the response replaces the cached one. */
export function useUpdateOnboarding() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (patch: OnboardingPatch) => {
      const { data } = await updateOnboarding({
        body: patch,
        throwOnError: true,
      })
      return data
    },
    meta: { errorTitle: 'Could not save setup progress' },
    onSuccess: (data) => {
      queryClient.setQueryData(getOnboardingQueryKey(), data)
    },
  })
}
