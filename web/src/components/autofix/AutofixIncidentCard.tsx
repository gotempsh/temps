// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  closeAutofixIncidentMutation,
  getAutofixCapabilityOptions,
  listAutofixIncidentEventsOptions,
  listAutofixIncidentsOptions,
  listAutofixIncidentsQueryKey,
  openAutofixIncidentMutation,
  stopAutofixIncidentMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  AutofixEventResponse,
  AutofixIncidentResponse,
} from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { cn } from '@/lib/utils'
import { getErrorMessage } from '@/utils/errorHandling'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Download,
  ExternalLink,
  GitPullRequest,
  Loader2,
  MessageSquare,
  Square,
  Wand2,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import {
  confidenceTone,
  confidenceMeaning,
  MAX_CONFIDENCE,
  signalLevel,
  decisionsFor,
  deliveryLabel,
  incidentPhase,
  incidentStatus,
  isActive,
  type IncidentTone,
} from './autofix-incident'

interface AutofixIncidentCardProps {
  projectId: number
  errorGroupId: number
}

const TONE_BADGE: Record<
  IncidentTone,
  | 'warning-subtle'
  | 'success-subtle'
  | 'destructive-subtle'
  | 'neutral-subtle'
  | 'outline'
> = {
  running: 'outline',
  review: 'warning-subtle',
  success: 'success-subtle',
  muted: 'neutral-subtle',
  error: 'destructive-subtle',
}

/**
 * "Fix this crash" (ADR-054): starts an autofix incident for the error group
 * and follows it. Always rendered: when autofix cannot run it says why and
 * links to the setting that fixes it.
 */
export function AutofixIncidentCard({
  projectId,
  errorGroupId,
}: AutofixIncidentCardProps) {
  const queryClient = useQueryClient()
  const capability = useQuery(
    getAutofixCapabilityOptions({ path: { project_id: projectId } })
  )
  const incidentsKey = {
    path: { project_id: projectId },
    query: { error_group_id: errorGroupId, page_size: 1 },
  }
  const incidents = useQuery({
    ...listAutofixIncidentsOptions(incidentsKey),
    refetchInterval: (query) =>
      isActive(query.state.data?.items[0]?.status) ? 4000 : false,
  })
  const incident = incidents.data?.items[0]

  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: listAutofixIncidentsQueryKey(incidentsKey),
    })
  const open = useMutation({
    ...openAutofixIncidentMutation(),
    onSuccess: (data) => {
      toast.success(
        data.created
          ? 'Autofix started: the timeline below follows it'
          : 'This crash already has an autofix incident'
      )
      refresh()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not start autofix')),
  })

  if (capability.isPending || incidents.isPending) {
    return (
      <Card>
        <CardHeader>
          <Skeleton className="h-5 w-40" />
          <Skeleton className="h-4 w-72" />
        </CardHeader>
      </Card>
    )
  }

  const cap = capability.data
  const startButton = (
    <Button
      onClick={() =>
        open.mutate({
          path: { project_id: projectId },
          body: { error_group_id: errorGroupId },
        })
      }
      disabled={open.isPending || !cap?.configured}
      title={cap?.configured ? undefined : (cap?.reason ?? undefined)}
    >
      {open.isPending ? (
        <Loader2 className="h-4 w-4 animate-spin" />
      ) : (
        <Wand2 className="h-4 w-4" />
      )}
      <span className="ml-2">Fix this crash</span>
    </Button>
  )

  if (!incident) {
    return (
      <Card>
        <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="space-y-1">
            <CardTitle className="text-base">Autofix</CardTitle>
            <CardDescription>
              Reproduce this crash in a sandbox built from the deployment that
              threw it, fix it, and get the fix with a confidence score backed
              by the server&apos;s own test runs.
            </CardDescription>
            {cap && cap.configured && (
              <p className="text-sm text-muted-foreground">
                {deliveryLabel(cap.delivery)}
                {cap.delivery_reason && ` ${cap.delivery_reason}.`}
              </p>
            )}
            {cap && !cap.configured && (
              <p className="text-sm">
                {cap.reason}.{' '}
                {cap.setup_path && (
                  <Link className="underline" to={cap.setup_path}>
                    Set it up
                  </Link>
                )}
              </p>
            )}
            {capability.isError && (
              <p className="text-sm text-destructive">
                {getErrorMessage(
                  capability.error,
                  'Could not check whether autofix can run'
                )}
              </p>
            )}
          </div>
          {startButton}
        </CardHeader>
      </Card>
    )
  }

  return (
    <IncidentPanel
      incident={incident}
      projectId={projectId}
      onChange={refresh}
      retry={incident.is_open ? null : startButton}
    />
  )
}

interface IncidentPanelProps {
  incident: AutofixIncidentResponse
  projectId: number
  onChange: () => void
  /** Starts a new incident; shown once this one is closed. */
  retry: ReactNode
}

/** One incident: status, confidence, deliverable, decisions and timeline. */
export function IncidentPanel({
  incident,
  projectId,
  onChange,
  retry,
}: IncidentPanelProps) {
  const status = incidentStatus(incident.status)
  const path = { project_id: projectId, incident_id: incident.id }
  const stop = useMutation({
    ...stopAutofixIncidentMutation(),
    onSuccess: () => {
      toast.success('Autofix stopped; its sandbox is being deleted')
      onChange()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not stop autofix')),
  })
  const close = useMutation({
    ...closeAutofixIncidentMutation(),
    onSuccess: () => {
      toast.success('Decision recorded; the sandbox is being deleted')
      onChange()
    },
    onError: (error) =>
      toast.error(getErrorMessage(error, 'Could not record the decision')),
  })

  return (
    <Card>
      <CardHeader className="space-y-3">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="space-y-1">
            <div className="flex flex-wrap items-center gap-2">
              <CardTitle className="text-base">Autofix</CardTitle>
              <Badge variant={TONE_BADGE[status.tone]}>
                {isActive(incident.status) && (
                  <Loader2 className="mr-1 h-3 w-3 animate-spin" />
                )}
                {status.label}
              </Badge>
              {incident.confidence != null && (
                <ConfidenceBadge score={incident.confidence} />
              )}
            </div>
            <CardDescription>
              {incident.is_open
                ? incidentPhase(incident.phase)
                : (incident.failure_reason ?? incidentPhase(incident.phase))}
            </CardDescription>
          </div>
          <div className="flex flex-wrap gap-2">
            {incident.conversation_id && (
              <Button variant="outline" size="sm" asChild>
                <Link to={`/chat?chat=${incident.conversation_id}`}>
                  <MessageSquare className="h-4 w-4" />
                  <span className="ml-2 max-sm:sr-only">Open chat</span>
                </Link>
              </Button>
            )}
            {incident.pr_url && (
              <Button variant="outline" size="sm" asChild>
                <a href={incident.pr_url} target="_blank" rel="noreferrer">
                  <GitPullRequest className="h-4 w-4" />
                  <span className="ml-2">
                    Pull request #{incident.pr_number}
                  </span>
                  <ExternalLink className="ml-1 h-3 w-3" />
                </a>
              </Button>
            )}
            {incident.has_patch && (
              <Button variant="outline" size="sm" asChild>
                <a
                  href={`/api/projects/${projectId}/autofix/incidents/${incident.id}/patch`}
                  download
                >
                  <Download className="h-4 w-4" />
                  <span className="ml-2">Patch</span>
                </a>
              </Button>
            )}
            {incident.status === 'awaiting_review' &&
              decisionsFor(incident.delivery).map((option) => (
                <Button
                  key={option.decision}
                  size="sm"
                  variant={
                    option.decision === 'rejected' ? 'outline' : 'default'
                  }
                  disabled={close.isPending}
                  onClick={() =>
                    close.mutate({ path, body: { decision: option.decision } })
                  }
                >
                  {option.label}
                </Button>
              ))}
            {incident.is_open && (
              <Button
                size="sm"
                variant="outline"
                disabled={stop.isPending}
                onClick={() => stop.mutate({ path })}
              >
                <Square className="h-4 w-4" />
                <span className="ml-2">Stop</span>
              </Button>
            )}
            {retry}
          </div>
        </div>
        {incident.summary && (
          <p className="whitespace-pre-line text-sm">{incident.summary}</p>
        )}
      </CardHeader>
      <CardContent className="space-y-4">
        {incident.assessment && (
          <div className="space-y-1 rounded-lg border p-3 text-sm">
            <p>
              <span className="font-medium">
                {incident.assessment.score}/{MAX_CONFIDENCE}
              </span>{' '}
              · {confidenceMeaning(incident.assessment.score)}
            </p>
            {incident.assessment.next_level && (
              <p className="text-muted-foreground">
                {incident.assessment.next_level}
              </p>
            )}
          </div>
        )}
        {incident.assessment && (
          <ul className="grid grid-cols-1 gap-1 text-sm md:grid-cols-2">
            {incident.assessment.signals.map((signal) => (
              <li key={signal.key} className="flex items-start gap-2">
                <span
                  className={cn(
                    'mt-0.5 font-mono text-xs',
                    signal.met === true && 'text-success-strong',
                    signal.met === false && 'text-destructive',
                    signal.met == null && 'text-muted-foreground'
                  )}
                >
                  {signalLevel(signal)}
                </span>
                <span>
                  {signal.label}
                  <span className="text-muted-foreground">
                    {' '}
                    · {signal.detail}
                  </span>
                </span>
              </li>
            ))}
          </ul>
        )}
        <IncidentTimeline
          projectId={projectId}
          incidentId={incident.id}
          live={isActive(incident.status)}
        />
      </CardContent>
    </Card>
  )
}

function ConfidenceBadge({ score }: { score: number }) {
  const tone = confidenceTone(score)
  return (
    <Badge
      variant={
        tone === 'high'
          ? 'success-subtle'
          : tone === 'medium'
            ? 'warning-subtle'
            : 'destructive-subtle'
      }
    >
      Confidence {score}/{MAX_CONFIDENCE}
    </Badge>
  )
}

interface IncidentTimelineProps {
  projectId: number
  incidentId: string
  live: boolean
}

/** Every action taken on the incident, newest last, polled while it runs. */
export function IncidentTimeline({
  projectId,
  incidentId,
  live,
}: IncidentTimelineProps) {
  const events = useQuery({
    ...listAutofixIncidentEventsOptions({
      path: { project_id: projectId, incident_id: incidentId },
      query: { limit: 500 },
    }),
    refetchInterval: live ? 3000 : false,
  })

  if (events.isPending) {
    return (
      <div className="space-y-2">
        <Skeleton className="h-4 w-3/4" />
        <Skeleton className="h-4 w-2/3" />
        <Skeleton className="h-4 w-1/2" />
      </div>
    )
  }
  if (events.isError) {
    return (
      <p className="text-sm text-destructive">
        {getErrorMessage(events.error, 'Could not load the timeline')}
      </p>
    )
  }
  return (
    <ol className="max-h-96 space-y-1 overflow-y-auto border-l pl-4 text-sm">
      {events.data.items.map((event) => (
        <TimelineEntry key={event.id} event={event} />
      ))}
    </ol>
  )
}

function TimelineEntry({ event }: { event: AutofixEventResponse }) {
  const output = event.details?.output_excerpt
  return (
    <li className="space-y-1">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className="text-xs text-muted-foreground">
          <TimeAgo date={event.created_at} />
        </span>
        <span className="text-xs uppercase text-muted-foreground">
          {event.actor}
        </span>
        <span
          className={cn(
            event.kind === 'failed' && 'text-destructive',
            event.kind === 'refused_action' && 'text-warning-strong'
          )}
        >
          {event.summary}
        </span>
      </div>
      {output && (
        <details className="text-xs">
          <summary className="cursor-pointer text-muted-foreground">
            Output
          </summary>
          <pre
            tabIndex={0}
            className="mt-1 max-h-48 overflow-auto rounded bg-muted p-2"
          >
            {output}
          </pre>
        </details>
      )}
    </li>
  )
}
