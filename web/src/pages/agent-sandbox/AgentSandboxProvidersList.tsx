// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight, KeyRound, Monitor, ShieldCheck } from 'lucide-react'
import type { ProviderCatalogDto } from '@/api/client'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import { usePageTitle } from '@/hooks/usePageTitle'
import { aiProviderCatalogQueryOptions } from '@/lib/ai-provider-catalog-query'
import {
  harnessSetupHref,
  harnessSetupStatus,
  workspaceReturnTo,
} from './harness-onboarding'

export function AgentSandboxProvidersList() {
  usePageTitle('Harnesses')
  const [params] = useSearchParams()
  const returnTo = workspaceReturnTo(params.get('returnTo'))
  const { data, isPending, isError, refetch } = useQuery({
    ...aiProviderCatalogQueryOptions,
    staleTime: 60_000,
  })

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm text-muted-foreground">
          Choose a harness, save its credential, then verify your first
          workspace reply. You only need one to start.
        </p>
        <Button asChild variant="outline" size="sm">
          <Link to={returnTo}>
            Back to workspace <ArrowRight className="size-4" />
          </Link>
        </Button>
      </div>
      {isError && (
        <div role="alert" className="rounded-lg border p-4 text-sm">
          Could not load harness configuration.{' '}
          <Button variant="outline" size="sm" onClick={() => void refetch()}>
            Retry
          </Button>
        </div>
      )}
      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3">
        {isPending
          ? [0, 1, 2].map((id) => (
              <Card key={id} aria-label="Loading harness">
                <CardContent className="space-y-3 p-4">
                  <Skeleton className="h-8 w-40" />
                  <Skeleton className="h-20 w-full" />
                </CardContent>
              </Card>
            ))
          : data?.providers.map((provider) => (
              <HarnessSetupCard
                key={provider.id}
                provider={provider}
                returnTo={returnTo}
              />
            ))}
      </div>
      <div className="flex items-start gap-2 rounded-lg border p-4 text-sm text-muted-foreground">
        <ShieldCheck className="size-4 shrink-0" aria-hidden="true" />
        <p>
          Credentials are configured on this Temps instance, not in your
          browser. A host CLI login is separate from a saved workspace
          credential. A saved credential is not proof that a model can answer:
          verify it in your workspace before starting a larger task.
        </p>
      </div>
    </div>
  )
}

export function HarnessSetupCard({
  provider,
  returnTo,
}: {
  provider: ProviderCatalogDto
  returnTo: string
}) {
  return (
    <Card className="min-w-0 shadow-none">
      <CardContent className="space-y-4 p-4">
        <div className="flex items-center gap-3">
          <AiHarnessLogo providerId={provider.id} size={28} />
          <div className="min-w-0 space-y-1">
            <h2 className="font-semibold">{provider.name}</h2>
            <Badge variant="secondary">{harnessSetupStatus(provider)}</Badge>
          </div>
        </div>
        <dl className="space-y-2 text-sm">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <dt className="flex items-center gap-2 text-muted-foreground">
              <Monitor className="size-4" />
              Host CLI
            </dt>
            <dd>
              {provider.host_version
                ? 'Installed · ' + provider.host_version
                : 'Not detected'}
            </dd>
          </div>
          <div className="flex items-center justify-between gap-2">
            <dt className="flex items-center gap-2 text-muted-foreground">
              <KeyRound className="size-4" />
              Host login
            </dt>
            <dd>
              {provider.host_authenticated
                ? 'Authenticated'
                : 'Not authenticated'}
            </dd>
          </div>
          <div className="flex items-center justify-between gap-2">
            <dt className="text-muted-foreground">Workspace credential</dt>
            <dd>{provider.credential_saved ? 'Saved' : 'Not saved'}</dd>
          </div>
        </dl>
        <p className="text-xs text-muted-foreground">
          {provider.workspace_ready
            ? 'Configured for workspace use. Verify with a first reply.'
            : provider.workspace_readiness_hint ||
              'Connect an account to start chatting and building in a workspace.'}
        </p>
        <Button asChild variant="outline" size="sm">
          <Link to={harnessSetupHref(provider.id, returnTo)}>
            {provider.credential_saved
              ? 'Manage connection'
              : 'Connect harness'}{' '}
            <ArrowRight className="size-4" />
          </Link>
        </Button>
      </CardContent>
    </Card>
  )
}
