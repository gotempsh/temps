// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  putPrivateIngress,
  type PrivateIngressStatus,
  type PrivateNetworkOption,
  type ReachAction,
} from '@/api/client'
import {
  getPrivateIngressOptions,
  getPrivateIngressQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { listenFor, networkLabel } from './private-network'
import { problemDetail } from '@/lib/api-problem'
import { Button } from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

/** How long this machine takes to apply a consent before a plan sees it. */
const APPLY_DELAY_MS = 2_000

/**
 * ADR-115 D2, D8: let this machine serve private addresses on one of its
 * private networks, in one click, then plan again. With several networks it
 * asks which; a network it cannot use says why. The CLI form stays beside it.
 *
 * It only ever gives a consent where there is none (`create_only`): the plan
 * it sits in may be older than a consent another administrator gave since,
 * with ranges of their own, which a click here must never replace with the
 * network's whole subnet. When there is one, it says what this machine does
 * with it (serving, or why not) and plans again; the terminal command, which
 * replaces a consent, is then not offered.
 */
export function ServePrivatelyAction({
  action,
  replanKey,
  applyDelayMs = APPLY_DELAY_MS,
}: {
  action: ReachAction
  /** The plan queries to refresh once the consent is applied. */
  replanKey: readonly unknown[]
  applyDelayMs?: number
}) {
  const queryClient = useQueryClient()
  const status = useQuery(getPrivateIngressOptions())
  const serve = useMutation({
    mutationFn: async (network: PrivateNetworkOption) => {
      await putPrivateIngress({
        body: {
          listen: listenFor(network),
          tailscale_serve: false,
          create_only: true,
        },
        throwOnError: true,
      })
      // The control plane is woken at once; give it a moment to bind
      // before the plan reads what it did.
      await new Promise((resolve) => setTimeout(resolve, applyDelayMs))
    },
    meta: { errorTitle: 'Could not serve private addresses from this machine' },
    // A consent given meanwhile is refused here: read it, so this shows it.
    onError: () =>
      queryClient.invalidateQueries({ queryKey: getPrivateIngressQueryKey() }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: getPrivateIngressQueryKey(),
      })
      await queryClient.invalidateQueries({ queryKey: replanKey })
    },
  })

  const consent = status.data?.consent
  const networks = status.data?.networks ?? []
  const usable = networks.filter((n) => !n.problem)
  const only = usable.length === 1 ? usable[0] : undefined
  // Split roles: no process here applies a consent, so neither a network
  // nor the terminal command can turn it on; the status says what to run.
  const unavailable = status.data?.available === false

  return (
    <div className="space-y-2">
      {status.isPending ? (
        <Skeleton className="h-8 w-64" />
      ) : unavailable && !consent ? (
        <p className="text-xs" role="status">
          {status.data?.error ??
            'This control plane serves no private address itself.'}
        </p>
      ) : consent && status.data ? (
        <div className="space-y-1.5 text-xs">
          <ConsentState status={status.data} />
          <Button
            size="sm"
            variant="outline"
            onClick={() => {
              void queryClient.invalidateQueries({
                queryKey: getPrivateIngressQueryKey(),
              })
              void queryClient.invalidateQueries({ queryKey: replanKey })
            }}
          >
            Plan again
          </Button>
        </div>
      ) : only ? (
        <Button
          size="sm"
          busy={serve.isPending}
          busyLabel="Turning it on…"
          onClick={() => serve.mutate(only)}
        >
          {`${action.label} on ${networkLabel(only)}`}
        </Button>
      ) : networks.length > 0 ? (
        <div className="space-y-1.5">
          <p className="text-xs font-medium text-foreground">
            Which private network should this machine serve on?
          </p>
          <ul className="grid grid-cols-1 gap-2 md:grid-cols-2">
            {networks.map((network) => (
              <li
                key={`${network.interface}-${network.address}`}
                className="space-y-1 rounded-md border p-2.5"
              >
                <p className="text-sm font-medium text-foreground [overflow-wrap:anywhere]">
                  {networkLabel(network)}
                </p>
                {network.problem ? (
                  <p className="text-xs">{network.problem}</p>
                ) : (
                  <>
                    <p className="text-xs [overflow-wrap:anywhere]">
                      {`Only ${network.admit.join(', ')} can connect`}
                    </p>
                    <Button
                      size="sm"
                      variant="outline"
                      busy={
                        serve.isPending &&
                        serve.variables?.address === network.address
                      }
                      busyLabel="Turning it on…"
                      aria-disabled={serve.isPending}
                      onClick={() => serve.mutate(network)}
                    >
                      Serve on this network
                    </Button>
                  </>
                )}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p className="text-xs">
          This machine is on no private network. Join one (Tailscale, WireGuard,
          NetBird or your LAN), then plan again.
        </p>
      )}
      {action.command && status.data && !consent && !unavailable ? (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <span>Or from a terminal:</span>
          <code className="break-all rounded bg-muted px-1.5 py-0.5 text-foreground">
            {action.command}
          </code>
          <CopyButton value={action.command} minimal />
        </div>
      ) : null}
      {serve.isError ? (
        <p className="text-xs text-destructive">
          {problemDetail(serve.error, 'It could not be turned on. Try again.')}
        </p>
      ) : null}
      {status.isError ? (
        <p className="text-xs text-destructive">
          {problemDetail(
            status.error,
            'Could not read this machine’s private networks.'
          )}
        </p>
      ) : null}
    </div>
  )
}

/**
 * What this machine does with a consent given before: serving it, failing to
 * (with the reason it reported), or not yet applied. Never claims a listener
 * from the consent alone.
 */
function ConsentState({ status }: { status: PrivateIngressStatus }) {
  const consent = status.consent
  if (!consent) return null
  const allowed = `Private addresses are allowed on ${consent.listen}${consent.admit?.length ? `, to ${consent.admit.join(', ')}` : ''}.`
  if (status.serving) {
    return (
      <p className="[overflow-wrap:anywhere]">
        {`This machine serves private addresses on ${status.serving}${status.admit.length ? `, to ${status.admit.join(', ')}` : ''}.`}
      </p>
    )
  }
  if (status.error) {
    return (
      <div className="space-y-1">
        <p className="[overflow-wrap:anywhere]">{`${allowed} This machine is not serving them:`}</p>
        <p className="text-destructive [overflow-wrap:anywhere]">
          {status.error}
        </p>
      </div>
    )
  }
  return (
    <p className="[overflow-wrap:anywhere]">
      {`${allowed} This machine has not applied that yet.`}
    </p>
  )
}
