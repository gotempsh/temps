// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  updateContainerSecurity,
  type ContainerCapability,
  type ContainerSecurityResponse,
} from '@/api/client'
import { getContainerSecurityOptions } from '@/api/client/@tanstack/react-query.gen'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export interface ContainerSecurityChange {
  allowed: ContainerCapability[]
  /** The value the editor loaded; the save fails with 409 if it changed. */
  expected: ContainerCapability[]
  /** Required when the change allows a capability that was not allowed. */
  acknowledgeRisks: boolean
}

/** `current` with `capabilities` added, sorted and without duplicates. */
export function withCapabilities(
  current: readonly ContainerCapability[],
  capabilities: readonly ContainerCapability[]
): ContainerCapability[] {
  return [...new Set([...current, ...capabilities])].sort()
}

/** Whether every one of `capabilities` is already allowed. */
export function allowsAll(
  current: readonly ContainerCapability[],
  capabilities: readonly ContainerCapability[]
): boolean {
  return capabilities.every((capability) => current.includes(capability))
}

/**
 * The project's container security setting and a save that keeps the
 * server's answer in the cache. Widening may need recent MFA: callers pass a
 * rejected save to `handleSensitiveActionError` with a retry, and render
 * `verificationDialog`.
 */
export function useContainerSecurity(projectId: number) {
  const queryClient = useQueryClient()
  const options = getContainerSecurityOptions({ path: { id: projectId } })
  const query = useQuery(options)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const save = useMutation({
    retry: false,
    mutationFn: async (
      change: ContainerSecurityChange
    ): Promise<ContainerSecurityResponse> => {
      const { data } = await updateContainerSecurity({
        path: { id: projectId },
        body: {
          allowed_capabilities: change.allowed,
          expected_allowed_capabilities: change.expected,
          acknowledge_risks: change.acknowledgeRisks,
        },
        throwOnError: true,
      })
      return data
    },
    onSuccess: (data) => {
      queryClient.setQueryData(options.queryKey, data)
    },
    onError: (error) => {
      // A stale editor refreshes to the saved value before trying again.
      if ((error as { status?: number })?.status === 409) {
        void query.refetch()
      }
    },
  })
  return { query, save, handleSensitiveActionError, verificationDialog }
}
