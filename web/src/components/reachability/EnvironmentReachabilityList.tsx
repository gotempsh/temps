// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The project Domains page's addresses (ADR 064 D2, D3): per environment,
// each address once, with its audience, way in and state, the reason and
// the fix when it is not reachable, and, for a custom domain, what it does
// besides (redirect, tunnel) and its actions. A setup run is mentioned only
// while it needs attention (ADR 069 D5).

import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import type {
  AddressReachability,
  CustomDomainResponse,
  EnvironmentResponse,
  ReachAction,
  ReachState,
} from '@/api/client'
import {
  getEnvironmentReachabilityOptions,
  getEnvironmentsOptions,
  listReachabilityRunsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AddressLink,
  CustomDomainNotes,
  CustomDomainRow,
  type DomainMenu,
} from '@/components/domains/CustomDomainParts'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import { addressAccessLabel } from '@/lib/reachability'
import {
  isEnvironmentSettled,
  isUnfinishedRun,
  PAGE_AUDIENCES,
  reachabilityPath,
  RUN_STATE_LABELS,
} from '@/lib/reachability-page'

const STATE_BADGES: Record<
  ReachState,
  {
    label: string
    variant:
      | 'success-subtle'
      | 'destructive-subtle'
      | 'warning-subtle'
      | 'neutral-subtle'
  }
> = {
  reachable: { label: 'Reachable', variant: 'success-subtle' },
  not_reachable: { label: 'Not reachable', variant: 'destructive-subtle' },
  checking: { label: 'Checking', variant: 'warning-subtle' },
  unknown: { label: 'Unknown', variant: 'neutral-subtle' },
}

const LIST = 'divide-y rounded-lg border'

export function EnvironmentReachabilityList({
  projectId,
  projectSlug,
  domains = [],
  domainMenu,
}: {
  projectId: number
  projectSlug: string
  /** The project's custom domains, each shown on the address it is. */
  domains?: CustomDomainResponse[]
  domainMenu?: DomainMenu
}) {
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId } }),
  })
  if (environments.isPending) return <Skeleton className="h-24 w-full" />
  if (environments.isError)
    return (
      <Alert variant="destructive">
        <AlertDescription>
          {problemDetail(
            environments.error,
            'Could not load the environments.'
          )}{' '}
          <Button variant="link" onClick={() => environments.refetch()}>
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  const list = environments.data ?? []
  const known = new Set(list.map((environment) => environment.id))
  const elsewhere = domains.filter(
    (domain) => !domain.environment || !known.has(domain.environment.id)
  )
  return (
    <div className="space-y-6">
      {list.map((environment) => (
        <EnvironmentAddresses
          key={environment.id}
          projectId={projectId}
          projectSlug={projectSlug}
          environment={environment}
          domains={domains.filter(
            (domain) => domain.environment?.id === environment.id
          )}
          domainMenu={domainMenu}
        />
      ))}
      {elsewhere.length > 0 && (
        <section className="space-y-2" aria-label="Other domains">
          <h3 className="text-sm font-medium">Other domains</h3>
          <ul className={LIST}>
            {elsewhere.map((domain) => (
              <CustomDomainRow
                key={domain.id}
                domain={domain}
                menu={domainMenu}
              />
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}

function EnvironmentAddresses({
  projectId,
  projectSlug,
  environment,
  domains,
  domainMenu,
}: {
  projectId: number
  projectSlug: string
  environment: EnvironmentResponse
  domains: CustomDomainResponse[]
  domainMenu?: DomainMenu
}) {
  const href = reachabilityPath(projectSlug, environment.id)
  const runs = useQuery({
    ...listReachabilityRunsOptions({
      path: { project_id: projectId, environment_id: environment.id },
    }),
    staleTime: 30_000,
    retry: false,
  })
  const newestRun = runs.data?.[0]
  const reachability = useQuery({
    ...getEnvironmentReachabilityOptions({
      path: { project_id: projectId, environment_id: environment.id },
    }),
    staleTime: 30_000,
    retry: false,
  })
  const addresses = reachability.data?.addresses ?? []
  const settled =
    !reachability.isError && isEnvironmentSettled(addresses, newestRun?.state)
  const reachable = reachability.data?.state === 'reachable'
  const byId = new Map(domains.map((domain) => [domain.id, domain]))
  const shown = new Set(
    addresses.flatMap((address) =>
      address.custom_domain_id != null ? [address.custom_domain_id] : []
    )
  )
  // A domain the check has not answered for still appears, once.
  const unmatched = reachability.isPending
    ? []
    : domains.filter((domain) => !shown.has(domain.id))
  const runNeedsAttention =
    newestRun && newestRun.state !== 'succeeded' && newestRun.state !== 'undone'
  const headingId = `environment-${environment.id}-addresses`
  return (
    <section className="space-y-2" aria-labelledby={headingId}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 id={headingId} className="text-sm font-medium">
          {environment.name}
        </h3>
        <Button
          size="sm"
          variant={settled ? 'ghost' : reachable ? 'outline' : 'default'}
          asChild
        >
          <Link to={href}>
            {reachable ? 'Make another address reachable' : 'Make it reachable'}
          </Link>
        </Button>
      </div>
      {reachability.isError && (
        <Alert variant="destructive">
          <AlertDescription>
            {problemDetail(
              reachability.error,
              'Could not tell whether it is reachable.'
            )}{' '}
            <Button variant="link" onClick={() => reachability.refetch()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      )}
      {reachability.isPending ? (
        <div className={LIST}>
          <AddressRowSkeleton />
          <AddressRowSkeleton />
        </div>
      ) : addresses.length + unmatched.length > 0 ? (
        <ul className={LIST}>
          {addresses.map((address) => (
            <AddressRow
              key={address.address}
              address={address}
              domain={
                address.custom_domain_id != null
                  ? byId.get(address.custom_domain_id)
                  : undefined
              }
              domainMenu={domainMenu}
              makeReachableHref={reachabilityPath(projectSlug, environment.id, {
                address: address.address,
                audience: PAGE_AUDIENCES.find((a) => a === address.audience),
              })}
            />
          ))}
          {unmatched.map((domain) => (
            <CustomDomainRow
              key={domain.id}
              domain={domain}
              menu={domainMenu}
            />
          ))}
        </ul>
      ) : (
        !reachability.isError && (
          <p className="text-sm text-muted-foreground">
            No address yet. Make it reachable gives it one.
          </p>
        )
      )}
      {runNeedsAttention && (
        <p className="text-sm text-muted-foreground">
          {isUnfinishedRun(newestRun.state) ? 'Setup in progress' : 'Setup'} for{' '}
          {newestRun.address ?? 'an address'}:{' '}
          {RUN_STATE_LABELS[newestRun.state]} ·{' '}
          <Link
            className="text-foreground underline underline-offset-4"
            to={reachabilityPath(projectSlug, environment.id, {
              run: newestRun.id,
            })}
          >
            Open it
          </Link>
        </p>
      )}
    </section>
  )
}

function AddressRowSkeleton() {
  return (
    <div className="flex items-center gap-2 px-3 py-2.5">
      <Skeleton className="h-4 w-48" />
      <Skeleton className="h-5 w-20" />
    </div>
  )
}

function AddressRow({
  address,
  domain,
  domainMenu,
  makeReachableHref,
}: {
  address: AddressReachability
  domain?: CustomDomainResponse
  domainMenu?: DomainMenu
  makeReachableHref: string
}) {
  const badge = STATE_BADGES[address.state]
  return (
    <li className="flex items-start gap-3 px-3 py-2.5 text-sm">
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <AddressLink address={address.address} />
          <Badge variant={badge.variant}>{badge.label}</Badge>
          <span className="text-xs text-muted-foreground">
            {addressAccessLabel(address)}
          </span>
        </div>
        {address.state !== 'reachable' && (
          <p className="text-muted-foreground">{address.reason}</p>
        )}
        {address.action && address.state !== 'reachable' && (
          <AddressAction
            action={address.action}
            makeReachableHref={makeReachableHref}
          />
        )}
        {domain && <CustomDomainNotes domain={domain} />}
      </div>
      {domain && domainMenu?.(domain)}
    </li>
  )
}

function AddressAction({
  action,
  makeReachableHref,
}: {
  action: ReachAction
  makeReachableHref: string
}) {
  if (action.kind === 'make_reachable')
    return (
      <Button size="sm" variant="link" className="h-auto p-0" asChild>
        <Link to={makeReachableHref}>{action.label}</Link>
      </Button>
    )
  if (action.command)
    return (
      <div className="flex flex-wrap items-center gap-2 text-xs">
        <span className="text-muted-foreground">{action.label}</span>
        <code className="rounded bg-muted px-1.5 py-0.5">{action.command}</code>
        <CopyButton value={action.command} minimal />
      </div>
    )
  if (action.href)
    return (
      <Link className="text-xs underline" to={action.href}>
        {action.label}
      </Link>
    )
  return <p className="text-xs text-muted-foreground">{action.label}</p>
}
