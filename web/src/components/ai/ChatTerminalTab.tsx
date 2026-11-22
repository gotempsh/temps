// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getChatSandboxOptions } from '@/api/client/@tanstack/react-query.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { useQuery } from '@tanstack/react-query'
import { SquareTerminal } from 'lucide-react'
import { lazy, Suspense, useState } from 'react'
import { SandboxWaking } from './ChatPreviewTab'
import { CreateSandboxAction } from './ChatSandbox'
import { useChatSandboxWake } from './chat-sandbox-wake'

// The terminal emulator is a few hundred KB that only a chat which opens the
// tab needs, so it loads on first use rather than with the console.
const SandboxTerminal = lazy(() =>
  import('./SandboxTerminal').then((module) => ({
    default: module.SandboxTerminal,
  }))
)

function TerminalSkeleton() {
  return (
    <div className="space-y-3 p-4" role="status" aria-label="Loading terminal">
      <Skeleton className="h-8 w-full" />
      <Skeleton className="h-64 w-full" />
    </div>
  )
}

/** What the tab shows when there is no sandbox to open a shell in. */
export function TerminalOnboarding({ reason }: { reason: string }) {
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <SquareTerminal
        className="size-7 text-muted-foreground"
        aria-hidden="true"
      />
      <p className="text-sm font-medium">No terminal yet</p>
      <p className="max-w-xs text-sm text-muted-foreground">
        {reason} The terminal opens a shell in the sandbox the assistant works
        in, so you can run the same commands it does, for example{' '}
        <code className="font-mono text-xs">npm test</code>.
      </p>
    </div>
  )
}

/**
 * The chat's sandbox as a terminal, beside the conversation. Nothing connects
 * until the tab is first shown: every open terminal holds a process and a
 * connection on the host, so a chat that never uses one costs nothing.
 */
export function ChatTerminalTab({
  conversationPublicId,
  active,
}: {
  conversationPublicId: string
  active: boolean
}) {
  const status = useQuery(
    getChatSandboxOptions({ path: { public_id: conversationPublicId } })
  )
  const wake = useChatSandboxWake(conversationPublicId)
  // Once shown, the terminal stays connected while the tab is hidden.
  const [opened, setOpened] = useState(active)
  if (active && !opened) setOpened(true)

  if (status.isPending) {
    return (
      <div className="space-y-3 p-4" role="status" aria-label="Loading">
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }
  if (status.isError) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        Temps could not load this chat&rsquo;s sandbox. It will try again
        shortly.
      </p>
    )
  }
  const { configured, linked, reason } = status.data
  if (!configured) {
    return (
      <TerminalOnboarding
        reason={reason ?? 'This chat cannot use a sandbox.'}
      />
    )
  }
  if (!linked) {
    return (
      <>
        <TerminalOnboarding reason="This chat has no sandbox yet. When the assistant builds something, it runs in one." />
        <CreateSandboxAction conversationPublicId={conversationPublicId} />
      </>
    )
  }
  if (wake.phase !== 'ready') {
    return <SandboxWaking error={wake.error} onRetry={wake.retry} />
  }
  if (!opened) return null
  return (
    <Suspense fallback={<TerminalSkeleton />}>
      <SandboxTerminal key={linked.sandbox_id} sandboxId={linked.sandbox_id} />
    </Suspense>
  )
}
