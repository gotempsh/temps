// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link, useSearchParams } from 'react-router'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { useQuery } from '@tanstack/react-query'
import { ArrowRight } from 'lucide-react'
import { RecordLink, RecordRow } from '@temps-sdk/ds'
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
import { returnToFromSearch } from '@/lib/safe-return-to'
import {
  harnessSetupHref,
  harnessSetupStatus,
  savedConnectionLabel,
} from './harness-onboarding'

/**
 * The accounts autofix's coding agent signs in with. Reached only from
 * autofix's setup steps; the AI area's own providers are on /ai-gateway.
 */
export function AgentSandboxProvidersList() {
  usePageTitle('Autofix agent credentials')
  const [params] = useSearchParams()
  const returnTo = returnToFromSearch(params)
  const { data, isPending, isError, refetch } = useQuery({
    ...aiProviderCatalogQueryOptions,
    staleTime: 60_000,
  })

  return (
    <PageContainer>
      <PageHeader
        title="Autofix agent credentials"
        description="Autofix runs a coding agent in a sandbox to propose a fix for a crash. Connect the account it signs in with; one is enough."
        actions={
          returnTo ? (
            <Button asChild variant="outline" size="sm">
              <Link to={returnTo}>
                Back <ArrowRight className="size-4" />
              </Link>
            </Button>
          ) : undefined
        }
      />
      <div className="rounded-lg border bg-card text-card-foreground">
        {isError && !data ? (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-3 p-4 text-sm"
          >
            Could not load the agent credentials.
            <Button variant="outline" size="sm" onClick={() => void refetch()}>
              Retry
            </Button>
          </div>
        ) : data?.providers.length === 0 ? (
          <p role="status" className="p-4 text-sm text-muted-foreground">
            This server offers no coding agents. Update Temps to add one.
          </p>
        ) : (
          <Table aria-label="Autofix agent credentials" aria-busy={isPending}>
            <TableHeader>
              <TableRow>
                <TableHead scope="col">Agent</TableHead>
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
    </PageContainer>
  )
}

export function HarnessSetupRow({
  provider,
  returnTo,
}: {
  provider: ProviderCatalogDto
  returnTo?: string | null
}) {
  const needsAttention = provider.credential_saved && !provider.workspace_ready
  const method = provider.credential_saved
    ? savedConnectionLabel(provider)
    : undefined
  return (
    <RecordRow>
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
                'Temps could not confirm a model response. Open the agent to verify it.'}
            </p>
          )}
        </div>
      </TableCell>
    </RecordRow>
  )
}
