// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getAiProviderStatusOptions,
  getCloudAiCapabilityOptions,
  listProviderKeysOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { isInternalConsolePath } from '@/lib/cloud-telemetry-activation'
import { getErrorMessage } from '@/utils/errorHandling'
import { useQuery } from '@tanstack/react-query'
import { Sparkles, X } from 'lucide-react'
import { type ReactNode } from 'react'
import { Link } from 'react-router'
import { AssistantOnboarding } from './AssistantPanels'
import { unconfiguredAssistant } from './assistant-state'

export function AiAssistantGate({
  children,
  onClose,
}: {
  children: ReactNode
  onClose: () => void
}) {
  const providers = useQuery({
    ...listProviderKeysOptions(),
    staleTime: 60_000,
    retry: false,
  })
  const cloud = useQuery({
    ...getCloudAiCapabilityOptions(),
    staleTime: 15_000,
    retry: false,
  })

  // The capability endpoint supplies the reason and where to fix it. It never
  // gates anything: an unreachable endpoint falls back to the same copy.
  const status = useQuery({
    ...getAiProviderStatusOptions(),
    staleTime: 15_000,
    retry: false,
  })

  const hasLocalProvider = (providers.data ?? []).some((key) => key.is_active)
  if (providers.isSuccess && hasLocalProvider) {
    return children
  }
  if (providers.isPending || cloud.isPending) {
    return <AiDockSkeleton onClose={onClose} />
  }
  if (cloud.data?.configured) {
    return children
  }
  if (providers.isError || cloud.isError) {
    return (
      <AiCapabilityError
        error={providers.error ?? cloud.error}
        retry={() => {
          if (providers.isError) void providers.refetch()
          if (cloud.isError) void cloud.refetch()
        }}
        isRetrying={providers.isFetching || cloud.isFetching}
        onClose={onClose}
      />
    )
  }
  const onboarding = unconfiguredAssistant(status.data)
  return (
    <div className="flex h-full flex-col">
      <DockHeader onClose={onClose} />
      <div className="min-h-0 flex-1">
        <AssistantOnboarding
          reason={onboarding.reason}
          setupPath={onboarding.setupPath}
          cloudPath={
            isInternalConsolePath(cloud.data?.setup_path)
              ? cloud.data.setup_path
              : '/settings/cloud'
          }
          onNavigate={onClose}
          onConfigured={() => void providers.refetch()}
        />
      </div>
    </div>
  )
}

function DockHeader({ onClose }: { onClose: () => void }) {
  return (
    <div className="flex items-center justify-between border-b border-border px-5 py-4">
      <div className="flex items-center gap-2">
        <span className="grid size-7 place-items-center rounded-md border border-border bg-muted/40">
          <Sparkles className="size-3.5" />
        </span>
        <span className="text-sm font-semibold">Temps AI</span>
      </div>
      <Button
        variant="ghost"
        size="icon"
        onClick={onClose}
        aria-label="Close AI assistant"
      >
        <X className="size-4" />
      </Button>
    </div>
  )
}

function AiDockSkeleton({ onClose }: { onClose: () => void }) {
  return (
    <div className="flex h-full flex-col">
      <DockHeader onClose={onClose} />
      <div className="mt-auto space-y-3 p-5">
        <Skeleton className="h-5 w-28" />
        <Skeleton className="h-16 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    </div>
  )
}

function AiCapabilityError({
  error,
  retry,
  isRetrying,
  onClose,
}: {
  error: unknown
  retry: () => void
  isRetrying: boolean
  onClose: () => void
}) {
  return (
    <div className="flex h-full flex-col">
      <DockHeader onClose={onClose} />
      <div className="mt-auto space-y-5 p-5">
        <div className="space-y-2">
          <h2 className="text-lg font-semibold">AI availability is unknown</h2>
          <p className="text-sm leading-6 text-muted-foreground">
            {getErrorMessage(
              error,
              'The server could not check local or managed AI providers.'
            )}
          </p>
        </div>
        <div className="grid gap-2">
          <Button onClick={retry} disabled={isRetrying}>
            Check again
          </Button>
          <Button asChild variant="outline">
            <Link to="/ai-gateway" onClick={onClose}>
              Configure my own provider
            </Link>
          </Button>
        </div>
      </div>
    </div>
  )
}
