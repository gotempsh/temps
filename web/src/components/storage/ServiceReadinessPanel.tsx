// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button, Callout } from '@temps-sdk/ds'
import { Link } from 'react-router'
import type {
  ReadinessNextAction,
  ServiceReadiness,
} from '@/api/client/types.gen'
import {
  FAILURE_KIND_TITLE,
  NEXT_ACTION_COPY,
  readinessView,
  startingProgress,
} from '@/lib/service-readiness'

export interface ServiceReadinessPanelProps {
  serviceId: number
  status: string
  readiness?: ServiceReadiness | null
  /** Start the service again (re-runs the readiness check). */
  onRetry: () => void
  retryPending?: boolean
  /** Open the image-change (upgrade) dialog. */
  onTryAnotherImage: () => void
  /** Open the delete dialog; the operator confirms there. */
  onRecreate: () => void
  /** Clock override for tests. */
  now?: number
}

/**
 * Startup readiness of a service: why it is still `starting`, or why its
 * initialization failed and what to do about it. Renders nothing for a
 * service that is neither.
 */
export function ServiceReadinessPanel({
  serviceId,
  status,
  readiness,
  onRetry,
  retryPending = false,
  onTryAnotherImage,
  onRecreate,
  now,
}: ServiceReadinessPanelProps) {
  const view = readinessView({ status, readiness }, now)

  if (view.kind === 'starting') {
    return (
      <Callout
        tone="info"
        title="Starting: waiting for the service to accept requests"
      >
        <div className="space-y-1">
          <p>
            The container is up. Temps marks the service running once an
            authenticated request with its own credentials succeeds, so
            don&apos;t rely on it until then.
          </p>
          <p>{startingProgress(view)}</p>
          {view.reason ? (
            <p className="break-words">Last check: {view.reason}</p>
          ) : null}
          {view.restartCount > 0 ? (
            <p>
              The container has restarted {view.restartCount} time(s) since it
              started.
            </p>
          ) : null}
        </div>
      </Callout>
    )
  }

  if (view.kind === 'failed') {
    const { failure } = view
    return (
      <Callout
        tone="error"
        title={`Initialization failed: ${FAILURE_KIND_TITLE[failure.kind]}`}
      >
        <div className="space-y-3">
          <p className="break-words">{failure.reason}</p>
          {failure.log_excerpt.length > 0 ? (
            <div className="space-y-1">
              <p className="font-medium text-foreground">
                From the container logs
              </p>
              <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md bg-muted p-2 font-mono text-xs text-foreground">
                {failure.log_excerpt.join('\n')}
              </pre>
            </div>
          ) : null}
          <p>
            Temps has not changed or deleted this service&apos;s container or
            volumes.
          </p>
          {failure.next_actions.length > 0 ? (
            <ul className="space-y-2">
              {failure.next_actions.map((action) => (
                <li
                  key={action}
                  className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div>
                    <p className="font-medium text-foreground">
                      {NEXT_ACTION_COPY[action].title}
                    </p>
                    <p>{NEXT_ACTION_COPY[action].description}</p>
                  </div>
                  <NextActionButton
                    action={action}
                    serviceId={serviceId}
                    onRetry={onRetry}
                    retryPending={retryPending}
                    onTryAnotherImage={onTryAnotherImage}
                    onRecreate={onRecreate}
                  />
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      </Callout>
    )
  }

  return null
}

function NextActionButton({
  action,
  serviceId,
  onRetry,
  retryPending,
  onTryAnotherImage,
  onRecreate,
}: {
  action: ReadinessNextAction
  serviceId: number
  onRetry: () => void
  retryPending: boolean
  onTryAnotherImage: () => void
  onRecreate: () => void
}) {
  const label = NEXT_ACTION_COPY[action].button
  switch (action) {
    case 'view_logs':
      return (
        <Button asChild variant="outline" size="sm" className="shrink-0">
          <Link to={`/storage/${serviceId}/logs`}>{label}</Link>
        </Button>
      )
    case 'retry':
      return (
        <Button
          variant="outline"
          size="sm"
          className="shrink-0"
          busy={retryPending}
          busyLabel="Starting…"
          onClick={onRetry}
        >
          {label}
        </Button>
      )
    case 'try_another_image':
      return (
        <Button
          variant="outline"
          size="sm"
          className="shrink-0"
          onClick={onTryAnotherImage}
        >
          {label}
        </Button>
      )
    case 'recreate_with_fresh_volumes':
      return (
        <Button
          variant="outline"
          size="sm"
          className="shrink-0 text-destructive"
          onClick={onRecreate}
        >
          {label}
        </Button>
      )
  }
}
