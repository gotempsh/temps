// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  connectChatSandboxProjectMutation,
  disconnectChatSandboxProjectMutation,
  getChatSandboxOptions,
  getChatSandboxQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Database, Loader2, Unplug } from 'lucide-react'
import { toast } from 'sonner'
import {
  connectedSummary,
  connectionLabel,
  connectionState,
  connectionTitle,
} from './chat-sandbox-connections'
import type { ProjectRef } from './project-refs'

/**
 * Connecting and disconnecting the chat's sandbox from a project's databases.
 * Every outcome reaches the user as a toast, and the sandbox status is read
 * again so each control reflects the server.
 */
export function useSandboxConnections(conversationPublicId: string) {
  const queryClient = useQueryClient()
  const path = { public_id: conversationPublicId }
  const status = useQuery(getChatSandboxOptions({ path }))
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: getChatSandboxQueryKey({ path }),
    })
  const connect = useMutation({
    ...connectChatSandboxProjectMutation(),
    onSuccess: (result) => {
      toast.success('Sandbox connected', {
        description: connectedSummary(result),
      })
      return refresh()
    },
    onError: (error) =>
      toast.error('Could not connect the sandbox', {
        description: getErrorMessage(error),
      }),
  })
  const disconnect = useMutation({
    ...disconnectChatSandboxProjectMutation(),
    onSuccess: () => {
      toast.success('Sandbox disconnected from the project’s databases')
      return refresh()
    },
    onError: (error) =>
      toast.error('Could not disconnect the sandbox', {
        description: getErrorMessage(error),
      }),
  })
  return {
    status,
    connect,
    disconnect,
    path,
  }
}

/**
 * "Connect sandbox" for one project. Always shown: with no sandbox linked it
 * says what to do instead of staying silent.
 */
export function ConnectSandboxButton({
  conversationPublicId,
  project,
  className,
}: {
  conversationPublicId: string
  project: ProjectRef
  className?: string
}) {
  const { status, connect, disconnect, path } =
    useSandboxConnections(conversationPublicId)
  const state = connectionState(status.data, project.id)
  const pending = connect.isPending || disconnect.isPending

  const onClick = () => {
    if (state === 'no-sandbox') {
      toast.info('Link a sandbox to this chat first', {
        description:
          'Use the Sandbox button above the message box, then connect it to this project’s databases.',
      })
      return
    }
    if (state === 'connected') {
      disconnect.mutate({ path: { ...path, project_id: project.id } })
      return
    }
    connect.mutate({ path, body: { project_id: project.id } })
  }

  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className={cn('h-8 shrink-0 gap-1.5', className)}
      disabled={pending || status.isPending}
      title={connectionTitle(state)}
      aria-label={`${connectionLabel(state)} for ${project.name}`}
      onClick={onClick}
    >
      {pending ? (
        <Loader2 className="size-4 animate-spin" aria-hidden />
      ) : state === 'connected' ? (
        <Unplug className="size-4" aria-hidden />
      ) : (
        <Database className="size-4" aria-hidden />
      )}
      <span className="max-sm:sr-only">{connectionLabel(state)}</span>
    </Button>
  )
}
