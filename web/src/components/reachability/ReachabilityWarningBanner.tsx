// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { Link } from 'react-router'
import {
  planEnvironmentReachability,
  applyEnvironmentReachability,
  type ReachabilityWarning,
} from '@/api/client'
import { getReachabilityRunOptions } from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { reachabilityPath, runPollInterval } from '@/lib/reachability-page'
import { defaultOption } from '@/lib/reachability'

/** Preserve the server's preference order; never enable environment hosts globally. */
export function tunnelSuggestions(warning: ReachabilityWarning) {
  return warning.fixes.filter(
    (f, index, all) =>
      f.kind === 'tunnel_environment_hosts' &&
      f.hostname &&
      all.findIndex((other) => other.hostname === f.hostname) === index
  )
}

export function ReachabilityWarningBanner(props: {
  warning: ReachabilityWarning | null | undefined
  projectSlug?: string
  projectId?: number
  className?: string
}) {
  if (!props.warning) return null
  return (
    <Suggestion
      key={`${props.projectSlug}:${props.warning.environment_id}`}
      {...props}
      warning={props.warning}
    />
  )
}

function Suggestion({
  warning,
  projectSlug,
  projectId,
  className,
}: {
  warning: ReachabilityWarning
  projectSlug?: string
  projectId?: number
  className?: string
}) {
  const key = `temps:reachability-dismissed:${projectSlug}:${warning.environment_id}`
  const [dismissed, setDismissed] = useState(() => {
    try {
      return localStorage.getItem(key) === 'true'
    } catch {
      return false
    }
  })
  const suggestions = tunnelSuggestions(warning)
  const [locked, setLocked] = useState(false)
  const [selected, setSelected] = useState(suggestions[0]?.hostname ?? '')
  const [label, setLabel] = useState(selected.split('.')[0])
  const zone = selected.slice(selected.indexOf('.') + 1)
  const address = `${label}.${zone}`
  const href = projectSlug
    ? reachabilityPath(projectSlug, warning.environment_id, {
        audience: 'public',
        address: selected ? address : '',
        way: selected ? 'existing_tunnel' : null,
      })
    : '/settings/nodes/ingress'
  const directDomain =
    !suggestions.length &&
    !warning.fixes.some((f) => f.kind === 'worker_ingress')
      ? warning.fixes.find((f) => f.kind === 'custom_domain')
      : undefined
  if (dismissed) return null
  return (
    <div
      className={`rounded-md border bg-muted/30 px-3 py-2 text-sm ${className ?? ''}`}
    >
      <div className="flex flex-wrap items-center gap-2">
        <span>Make {warning.environment} reachable:</span>
        {selected && (
          <>
            <Input
              aria-label="Subdomain"
              className="h-8 w-48"
              value={label}
              maxLength={63}
              disabled={locked}
              onChange={(e) => setLabel(e.target.value.toLowerCase())}
            />
            {suggestions.length > 1 ? (
              <Select
                disabled={locked}
                value={selected}
                onValueChange={setSelected}
              >
                <SelectTrigger aria-label="Tunnel zone" className="h-8 w-auto">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {suggestions.map((f) => (
                    <SelectItem key={f.hostname} value={f.hostname!}>
                      .{f.hostname!.split('.').slice(1).join('.')}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <span>.{zone}</span>
            )}
          </>
        )}
        {selected && projectId !== undefined && projectSlug ? (
          <PublishDomain
            key={`${address}`}
            projectId={projectId}
            environmentId={warning.environment_id}
            projectSlug={projectSlug}
            address={address}
            onLock={() => setLocked(true)}
            valid={/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label)}
            href={href}
          />
        ) : (
          <Button size="sm" asChild>
            <Link to={directDomain?.href ?? href}>
              {selected || directDomain ? 'Add domain' : 'Make it reachable'}
            </Link>
          </Button>
        )}
        <Button
          size="sm"
          variant="ghost"
          onClick={() => {
            try {
              localStorage.setItem(key, 'true')
            } catch {
              /* Still dismiss for this visit. */
            }
            setDismissed(true)
          }}
        >
          Not now
        </Button>
      </div>
      <details className="mt-1 text-xs text-muted-foreground">
        <summary className="cursor-pointer">Other ways · Why?</summary>
        <p className="my-1">{warning.reason}</p>
        <Link className="underline" to={href}>
          Make it reachable
        </Link>
      </details>
    </div>
  )
}

function PublishDomain({
  projectId,
  environmentId,
  projectSlug,
  address,
  valid,
  href,
  onLock,
}: {
  projectId: number
  environmentId: number
  projectSlug: string
  address: string
  valid: boolean
  href: string
  onLock: () => void
}) {
  const path = { project_id: projectId, environment_id: environmentId }
  const [review, setReview] = useState(false)
  const publish = useMutation({
    mutationFn: async () => {
      const { data: plan } = await planEnvironmentReachability({
        path,
        body: { audience: 'public', address },
        throwOnError: true,
      })
      const option = defaultOption(
        plan.options.filter((o) => o.kind === 'existing_tunnel')
      )
      if (
        !option ||
        option.confirmation ||
        option.fields.some((f) => f.required && f.key !== 'address') ||
        !plan.plan_id
      ) {
        setReview(true)
        return null
      }
      const { data } = await applyEnvironmentReachability({
        path,
        body: {
          plan_id: plan.plan_id,
          option: option.kind,
          answers: { address },
          confirmed: false,
        },
        throwOnError: true,
      })
      return data
    },
  })
  const run = useQuery({
    ...getReachabilityRunOptions({
      path: { ...path, run_id: publish.data?.id ?? 0 },
    }),
    enabled: Boolean(publish.data),
    refetchInterval: (q) =>
      runPollInterval(q.state.data?.state, q.state.status),
    retry: false,
  })
  const view = run.data ?? publish.data
  if (view)
    return (
      <span role="status">
        {view.state === 'succeeded' && view.live_url ? (
          <>
            Live at{' '}
            <a
              className="underline"
              href={view.live_url}
              target="_blank"
              rel="noreferrer"
            >
              {view.live_url}
            </a>
          </>
        ) : (
          <Link
            className="underline"
            to={reachabilityPath(projectSlug, environmentId, { run: view.id })}
          >
            {run.isError
              ? 'Check publication progress'
              : view.state === 'running' || view.state === 'recovering'
                ? 'Adding domain…'
                : 'Continue domain setup'}
          </Link>
        )}
      </span>
    )
  if (review || publish.isError)
    return (
      <span role="status">
        <Link className="underline" to={href}>
          {publish.isError
            ? 'Could not add domain. Review and retry'
            : 'Review domain setup'}
        </Link>
      </span>
    )
  return (
    <Button
      size="sm"
      aria-disabled={!valid || publish.isPending}
      onClick={() => {
        if (valid && !publish.isPending) {
          onLock()
          publish.mutate()
        }
      }}
    >
      {publish.isPending ? 'Adding domain…' : 'Add domain'}
    </Button>
  )
}
