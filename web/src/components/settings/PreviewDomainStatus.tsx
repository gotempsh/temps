// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Whether names under the preview domain reach anyone (ADR 068 D2), shown
// under the field that sets it. Always rendered: `localho.st` is the default
// and the operator should learn here, not after a deploy, that it is
// local-only.

import { useQuery } from '@tanstack/react-query'
import { AlertTriangle, CheckCircle2, Globe } from 'lucide-react'
import { Link } from 'react-router'
import { getPreviewDomainStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import type { PreviewDomainStatusResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'

const TITLES: Record<PreviewDomainStatusResponse['status'], string> = {
  local_only: 'Only this machine can open these names',
  unresolved: 'These names do not resolve yet',
  no_way_in: 'These names resolve, but nothing lets traffic in',
  usable: 'Anyone can open these names',
}

export function PreviewDomainStatus() {
  const query = useQuery({
    ...getPreviewDomainStatusOptions(),
    staleTime: 60_000,
    retry: 1,
  })
  if (query.isPending) {
    return <Skeleton className="mt-2 h-4 w-72 max-w-full" />
  }
  if (query.isError) {
    return (
      <p className="text-sm text-muted-foreground">
        Could not check whether the preview domain is reachable.
      </p>
    )
  }
  return <PreviewDomainStatusView status={query.data} />
}

export function PreviewDomainStatusView({
  status,
}: {
  status: PreviewDomainStatusResponse
}) {
  if (status.status === 'usable') {
    return (
      <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
        <CheckCircle2
          aria-hidden="true"
          className="size-4 shrink-0 text-emerald-600"
        />
        <span>
          {TITLES.usable}. {status.reason}
        </span>
      </p>
    )
  }
  if (status.tunnel_environment_hosts) {
    // ADR 068 D3: a tunnel gives every environment an address, so apps are
    // reachable whatever this domain resolves to.
    return (
      <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
        <CheckCircle2
          aria-hidden="true"
          className="size-4 shrink-0 text-emerald-600"
        />
        <span>
          Tunnel {status.tunnel_environment_hosts} gives every environment an
          address, so apps are reachable without this domain. {status.reason}{' '}
          <Link className="underline" to="/settings/nodes/ingress">
            Manage tunnels
          </Link>
        </span>
      </p>
    )
  }
  return (
    <Alert variant="warning">
      {status.status === 'local_only' ? (
        <Globe className="h-4 w-4" />
      ) : (
        <AlertTriangle className="h-4 w-4" />
      )}
      <AlertTitle>{TITLES[status.status]}</AlertTitle>
      <AlertDescription className="space-y-1">
        <p>{status.reason}</p>
        <p>
          Set a wildcard domain whose DNS points here, or let a tunnel publish
          each environment from{' '}
          <Link className="underline" to="/settings/nodes/ingress">
            Ingress
          </Link>
          .
        </p>
      </AlertDescription>
    </Alert>
  )
}
