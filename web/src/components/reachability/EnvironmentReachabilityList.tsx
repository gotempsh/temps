// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The project Domains page's answer to "who can open this, and how?"
// (ADR 064 D2, D3): per environment, each address with its audience, way
// in, state, the reason and the fix, and Make it reachable.

import { useQuery } from '@tanstack/react-query'
import { ExternalLink } from 'lucide-react'
import { Link } from 'react-router'
import type {
  AddressReachability,
  EnvironmentResponse,
  ReachAction,
  ReachState,
} from '@/api/client'
import {
  getEnvironmentReachabilityOptions,
  getEnvironmentsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import { addressUrl, audienceLabel, wayInLabel } from '@/lib/reachability'
import { MakeReachableDialog } from './MakeReachableDialog'
import { useMakeReachableDialog } from './ReachabilityStatus'

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

export function EnvironmentReachabilityList({
  projectId,
}: {
  projectId: number
}) {
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId } }),
  })
  return (
    <section className="space-y-3" aria-labelledby="reachability-heading">
      <div>
        <h3 id="reachability-heading" className="text-base font-semibold">
          Who can reach it
        </h3>
        <p className="text-sm text-muted-foreground">
          Every address of each environment, who it is for, how visitors get in,
          and what is missing when they cannot.
        </p>
      </div>
      {environments.isPending ? (
        <Skeleton className="h-24 w-full" />
      ) : environments.isError ? (
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
      ) : (
        <div className="space-y-4">
          {(environments.data ?? []).map((environment) => (
            <EnvironmentReachabilityCard
              key={environment.id}
              projectId={projectId}
              environment={environment}
            />
          ))}
        </div>
      )}
    </section>
  )
}

function EnvironmentReachabilityCard({
  projectId,
  environment,
}: {
  projectId: number
  environment: EnvironmentResponse
}) {
  const dialog = useMakeReachableDialog()
  const reachability = useQuery({
    ...getEnvironmentReachabilityOptions({
      path: { project_id: projectId, environment_id: environment.id },
    }),
    staleTime: 30_000,
    retry: false,
  })
  const data = reachability.data
  const reachable = data?.state === 'reachable'
  return (
    <div className="space-y-3 rounded-lg border p-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <p className="font-medium">{environment.name}</p>
          {reachability.isPending ? (
            <Skeleton className="mt-1 h-4 w-56" />
          ) : (
            data && (
              <p className="text-sm text-muted-foreground">{data.summary}</p>
            )
          )}
        </div>
        <Button
          size="sm"
          variant={reachable ? 'outline' : 'default'}
          onClick={dialog.show}
        >
          {reachable ? 'Make another address reachable' : 'Make it reachable'}
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
      {data && data.addresses.length > 0 && (
        <ul className="divide-y rounded-md border">
          {data.addresses.map((address) => (
            <AddressRow
              key={address.address}
              address={address}
              onMakeReachable={dialog.show}
            />
          ))}
        </ul>
      )}
      {data && data.addresses.length === 0 && (
        <p className="text-sm text-muted-foreground">
          No address yet. Make it reachable gives it one.
        </p>
      )}
      <MakeReachableDialog
        key={dialog.session}
        open={dialog.open}
        onOpenChange={dialog.onOpenChange}
        projectId={projectId}
        environmentId={environment.id}
        environmentName={environment.name}
      />
    </div>
  )
}

function AddressRow({
  address,
  onMakeReachable,
}: {
  address: AddressReachability
  onMakeReachable: () => void
}) {
  const badge = STATE_BADGES[address.state]
  return (
    <li className="space-y-1 p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <a
          href={addressUrl(address.address)}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center gap-1 break-all font-medium underline-offset-4 hover:underline"
        >
          {address.address}
          <ExternalLink aria-hidden="true" className="h-3 w-3" />
        </a>
        <Badge variant={badge.variant}>{badge.label}</Badge>
        <span className="text-xs text-muted-foreground">
          {audienceLabel(address)} · {wayInLabel(address.way_in)}
        </span>
      </div>
      {address.state !== 'reachable' && (
        <p className="text-muted-foreground">{address.reason}</p>
      )}
      {address.action && address.state !== 'reachable' && (
        <AddressAction
          action={address.action}
          onMakeReachable={onMakeReachable}
        />
      )}
    </li>
  )
}

function AddressAction({
  action,
  onMakeReachable,
}: {
  action: ReachAction
  onMakeReachable: () => void
}) {
  if (action.kind === 'make_reachable')
    return (
      <Button
        size="sm"
        variant="link"
        className="h-auto p-0"
        onClick={onMakeReachable}
      >
        {action.label}
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
