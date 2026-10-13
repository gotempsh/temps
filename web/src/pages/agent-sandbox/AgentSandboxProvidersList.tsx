// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link, useSearchParams } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight } from 'lucide-react'
import { RecordLink } from '@temps-sdk/ds'
import type { ProviderCatalogDto } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { AiHarnessLogo } from '@/components/ui/ai-harness-logo'
import { usePageTitle } from '@/hooks/usePageTitle'
import { aiProviderCatalogQueryOptions } from '@/lib/ai-provider-catalog-query'
import {
  harnessSetupHref,
  harnessSetupStatus,
  savedConnectionLabel,
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
          Connect one harness to start. You can add the others later.
        </p>
        <Button asChild variant="outline" size="sm">
          <Link to={returnTo}>
            Back to workspace <ArrowRight className="size-4" />
          </Link>
        </Button>
      </div>
      <div className="rounded-lg border bg-card text-card-foreground">
        {isError && !data ? (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-3 p-4 text-sm"
          >
            Could not load harness configuration.
            <Button variant="outline" size="sm" onClick={() => void refetch()}>
              Retry
            </Button>
          </div>
        ) : data?.providers.length === 0 ? (
          <p role="status" className="p-4 text-sm text-muted-foreground">
            This server offers no harnesses. Update Temps to add one.
          </p>
        ) : (
          <Table aria-label="Harnesses" aria-busy={isPending}>
            <TableHeader>
              <TableRow>
                <TableHead scope="col">Harness</TableHead>
                {/* Secondary on phones: the harness page shows it too. */}
                <TableHead scope="col" className="hidden sm:table-cell">
                  Sign-in method
                </TableHead>
                <TableHead scope="col">Status</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {isPending
                ? [0, 1, 2].map((id) => (
                    <TableRow key={id}>
                      <TableCell>
                        <Skeleton className="h-5 w-40" />
                      </TableCell>
                      <TableCell className="hidden sm:table-cell">
                        <Skeleton className="h-4 w-28" />
                      </TableCell>
                      <TableCell>
                        <Skeleton className="h-5 w-24" />
                      </TableCell>
                    </TableRow>
                  ))
                : data?.providers.map((provider) => (
                    <HarnessSetupRow
                      key={provider.id}
                      provider={provider}
                      returnTo={returnTo}
                    />
                  ))}
            </TableBody>
          </Table>
        )}
      </div>
    </div>
  )
}

export function HarnessSetupRow({
  provider,
  returnTo,
}: {
  provider: ProviderCatalogDto
  returnTo: string
}) {
  const needsAttention = provider.credential_saved && !provider.workspace_ready
  const method = provider.credential_saved
    ? savedConnectionLabel(provider)
    : undefined
  return (
    <TableRow>
      <TableCell>
        <div className="flex min-w-0 items-center gap-3">
          <AiHarnessLogo providerId={provider.id} size={20} />
          <RecordLink
            to={harnessSetupHref(provider.id, returnTo)}
            aria-label={`${provider.credential_saved ? 'Manage' : 'Connect'} ${provider.name}`}
          >
            {provider.name}
          </RecordLink>
        </div>
      </TableCell>
      <TableCell className="hidden text-muted-foreground sm:table-cell">
        {method ?? (
          <>
            <span aria-hidden="true">—</span>
            <span className="sr-only">None</span>
          </>
        )}
      </TableCell>
      <TableCell>
        <div className="space-y-1">
          <Badge
            variant={provider.workspace_ready ? 'secondary' : 'outline'}
            className={
              needsAttention
                ? 'border-amber-500/40 text-amber-700 dark:text-amber-300'
                : undefined
            }
          >
            {harnessSetupStatus(provider)}
          </Badge>
          {needsAttention && (
            <p className="text-xs text-muted-foreground">
              {provider.workspace_readiness_hint ||
                'Temps could not confirm a model response. Open the harness to verify it.'}
            </p>
          )}
        </div>
      </TableCell>
    </TableRow>
  )
}
