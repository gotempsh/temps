// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatSandboxStatus } from '@/api/client'
import {
  getChatSandboxOptions,
  getChatSandboxQueryKey,
  wakeChatSandboxMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { getErrorMessage } from '@/utils/errorHandling'
import {
  useMutation,
  useMutationState,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import { useEffect } from 'react'

/** Where a chat's sandbox stands, for the parts of the chat that need it up. */
export type SandboxWakePhase =
  /** Running, or nothing is linked: nothing to wait for. */
  | 'ready'
  /** Asleep and being started. */
  | 'waking'
  /** Asleep and starting it failed. */
  | 'failed'

/**
 * Whether the chat's linked sandbox has to be started first. A sandbox that
 * could not be read (`runtime` absent) is left alone: waking something that may
 * not exist only produces an error.
 */
export function needsWake(status: ChatSandboxStatus | undefined): boolean {
  return Boolean(status?.linked) && status?.runtime === 'asleep'
}

/** Wakes in flight, so two components mounting together start it once. */
const inFlight = new Set<string>()

/**
 * Keeps a chat's sandbox up. Sandboxes stop when idle; opening the chat (or
 * its preview) starts a sleeping one right away, so the preview and the
 * assistant's next command do not hit a stopped container. Safe to call from
 * several components of the same chat.
 */
export function useChatSandboxWake(conversationPublicId: string) {
  const queryClient = useQueryClient()
  const path = { public_id: conversationPublicId }
  const status = useQuery(getChatSandboxOptions({ path }))
  const mutationKey = ['wake-chat-sandbox', conversationPublicId]
  const wake = useMutation({
    ...wakeChatSandboxMutation(),
    mutationKey,
    onSuccess: (data) => {
      queryClient.setQueryData(getChatSandboxQueryKey({ path }), data)
    },
    onSettled: () => {
      inFlight.delete(conversationPublicId)
    },
  })
  const states = useMutationState({
    filters: { mutationKey },
    select: (mutation) => mutation.state,
  })
  const latest = states[states.length - 1]
  const { mutate } = wake
  const asleep = needsWake(status.data)
  const sandboxId = status.data?.linked?.sandbox_id

  useEffect(() => {
    if (!asleep || !sandboxId || inFlight.has(conversationPublicId)) return
    inFlight.add(conversationPublicId)
    mutate({ path: { public_id: conversationPublicId } })
  }, [asleep, sandboxId, conversationPublicId, mutate])

  const failed = asleep && latest?.status === 'error'
  const phase: SandboxWakePhase = !asleep
    ? 'ready'
    : failed
      ? 'failed'
      : 'waking'
  return {
    phase,
    error: failed ? getErrorMessage(latest?.error) : null,
    retry: () => {
      if (inFlight.has(conversationPublicId)) return
      inFlight.add(conversationPublicId)
      mutate({ path: { public_id: conversationPublicId } })
    },
  }
}
