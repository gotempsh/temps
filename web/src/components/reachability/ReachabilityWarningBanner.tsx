// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckCircle2, Loader2 } from 'lucide-react'
import { useEffect, useState, type ReactNode } from 'react'
import { Link } from 'react-router'
import {
  planEnvironmentReachability,
  applyEnvironmentReachability,
  type PlanOption,
  type ReachabilityWarning,
  type RunView,
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
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { problemDetail } from '@/lib/api-problem'
import {
  reachabilityPath,
  runPollInterval,
  RUN_STATE_LABELS,
} from '@/lib/reachability-page'
import { addressUrl, isActiveRun } from '@/lib/reachability'
import {
  deliveryStatus,
  publishDecision,
  type PublishOutcome,
} from './publish-domain'

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
  const domainSettings = projectSlug
    ? `/projects/${projectSlug}/settings/domains`
    : '/domains'
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
            onLock={setLocked}
            valid={/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label)}
            setupHref={href}
            settingsHref={domainSettings}
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
        <span className="flex flex-wrap gap-3">
          <Link className="underline" to={href}>
            Make it reachable
          </Link>
          <Link className="underline" to={domainSettings}>
            Domain settings
          </Link>
        </span>
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
  setupHref,
  settingsHref,
  onLock,
}: {
  projectId: number
  environmentId: number
  projectSlug: string
  address: string
  valid: boolean
  setupHref: string
  settingsHref: string
  onLock: (locked: boolean) => void
}) {
  const path = { project_id: projectId, environment_id: environmentId }
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()
  const publish = useMutation({
    mutationFn: async (
      confirmed?: Extract<PublishOutcome, { kind: 'confirm' }>
    ): Promise<PublishOutcome> => {
      let planId = confirmed?.planId
      let option = confirmed?.option
      if (!planId || !option) {
        const { data: plan } = await planEnvironmentReachability({
          path,
          body: { audience: 'public', address },
          throwOnError: true,
        })
        const decision = publishDecision(plan, address)
        if (decision.kind !== 'apply') return decision
        planId = decision.planId
        option = decision.option
      }
      const { data } = await applyEnvironmentReachability({
        path,
        body: {
          plan_id: planId,
          option: option.kind,
          answers: { address },
          confirmed: Boolean(confirmed),
        },
        throwOnError: true,
      })
      return { kind: 'started', run: data, option }
    },
    onError: (error, confirmed) => {
      handleSensitiveActionError(error, () => publish.mutate(confirmed))
    },
  })
  const start = (confirmed?: Extract<PublishOutcome, { kind: 'confirm' }>) => {
    onLock(true)
    publish.mutate(confirmed)
  }
  const retry = () => {
    publish.reset()
    onLock(false)
  }
  const outcome = publish.data
  const settings = (
    <Link className="text-xs underline" to={settingsHref}>
      Domain settings
    </Link>
  )
  let body
  if (outcome?.kind === 'started') {
    body = (
      <PublishProgress
        path={path}
        started={outcome.run}
        option={outcome.option}
        address={address}
        projectSlug={projectSlug}
        settings={settings}
        onRetry={retry}
      />
    )
  } else if (outcome?.kind === 'confirm') {
    body = (
      <span role="status" className="flex flex-wrap items-center gap-2">
        <span>{outcome.message}</span>
        <Button size="sm" onClick={() => start(outcome)}>
          Add anyway
        </Button>
        <Button size="sm" variant="ghost" onClick={retry}>
          Cancel
        </Button>
      </span>
    )
  } else if (outcome?.kind === 'blocked' || publish.isError) {
    body = (
      <span role="alert" className="flex flex-wrap items-center gap-2">
        <span className="text-destructive">
          {outcome?.kind === 'blocked'
            ? outcome.reason
            : `Could not add ${address}: ${problemDetail(publish.error, 'the server did not say why.')}`}
        </span>
        <Button size="sm" variant="outline" onClick={retry}>
          Try again
        </Button>
        <Link className="text-xs underline" to={setupHref}>
          Other ways to make it reachable
        </Link>
      </span>
    )
  } else {
    body = (
      <Button
        size="sm"
        aria-disabled={!valid || publish.isPending}
        onClick={() => {
          if (valid && !publish.isPending) start()
        }}
      >
        {publish.isPending && <Loader2 className="size-4 animate-spin" />}
        {publish.isPending ? 'Adding domain…' : 'Add domain'}
      </Button>
    )
  }
  return (
    <>
      {body}
      {verificationDialog}
    </>
  )
}

const SETTLED: RunView['state'][] = ['succeeded', 'waiting']

function PublishProgress({
  path,
  started,
  option,
  address,
  projectSlug,
  settings,
  onRetry,
}: {
  path: { project_id: number; environment_id: number }
  started: RunView
  option: PlanOption
  address: string
  projectSlug: string
  settings: ReactNode
  onRetry: () => void
}) {
  const client = useQueryClient()
  const run = useQuery({
    ...getReachabilityRunOptions({ path: { ...path, run_id: started.id } }),
    initialData: started,
    refetchInterval: (q) =>
      runPollInterval(q.state.data?.state, q.state.status),
    retry: false,
  })
  const view = run.data ?? started
  const settled = SETTLED.includes(view.state)
  // The environment has an address now: what shows its reachability and
  // its domains asks again. The deployment's warning is left alone so the
  // result stays on screen until the page next loads it.
  useEffect(() => {
    if (!settled) return
    client.invalidateQueries({
      predicate: (query) => {
        const id = (query.queryKey[0] as { _id?: string } | undefined)?._id
        return (
          typeof id === 'string' &&
          /Reachability|EnvironmentAddress|CustomDomain/.test(id) &&
          id !== 'getReachabilityRun'
        )
      },
    })
  }, [settled, client])
  const details = (
    <Link
      className="text-xs underline"
      to={reachabilityPath(projectSlug, path.environment_id, { run: view.id })}
    >
      Details
    </Link>
  )
  if (view.state === 'succeeded') {
    const url = view.live_url ?? addressUrl(address)
    return (
      <span role="status" className="flex flex-wrap items-center gap-2">
        <CheckCircle2 className="size-4 text-emerald-600" aria-hidden />
        <span>
          Live at{' '}
          <a
            className="font-medium underline [overflow-wrap:anywhere]"
            href={url}
            target="_blank"
            rel="noreferrer"
          >
            {url}
          </a>
        </span>
        <span className="text-xs text-muted-foreground">
          {deliveryStatus(view, option)}
        </span>
        {settings}
      </span>
    )
  }
  if (view.state === 'waiting')
    return (
      <span role="status" className="flex flex-wrap items-center gap-2">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        <span>
          {address} is set up; waiting for it to answer. DNS can take a few
          minutes.
        </span>
        <span className="text-xs text-muted-foreground">
          {view.error ?? deliveryStatus(view, option)}
        </span>
        {settings}
      </span>
    )
  if (isActiveRun(view.state)) {
    const current =
      view.steps.find((s) => s.status === 'running') ??
      view.steps.find((s) => s.status === 'pending')
    return (
      <span role="status" className="flex flex-wrap items-center gap-2">
        <Loader2 className="size-4 animate-spin" aria-hidden />
        <span>Adding {address}…</span>
        {current && (
          <span className="text-xs text-muted-foreground">{current.label}</span>
        )}
        {run.isError && (
          <span className="text-xs text-destructive">
            Could not check progress: {problemDetail(run.error, 'no answer.')}
          </span>
        )}
      </span>
    )
  }
  return (
    <span role="alert" className="flex flex-wrap items-center gap-2">
      <span className="text-destructive">
        {view.error ??
          `Adding ${address} stopped: ${RUN_STATE_LABELS[view.state].toLowerCase()}.`}
      </span>
      {view.state === 'failed' && (
        <Button size="sm" variant="outline" onClick={onRetry}>
          Try again
        </Button>
      )}
      {details}
    </span>
  )
}
