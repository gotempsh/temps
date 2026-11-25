// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { HostFacts } from '@/api/client/types.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'
import { Button, Callout } from '@temps-sdk/ds'
import { hostRows } from './host'

export function HostStep({
  facts,
  isPending,
  error,
  onRecheck,
  rechecking,
  onContinue,
  saving,
}: {
  facts: HostFacts | undefined
  isPending: boolean
  error: unknown
  onRecheck: () => void
  rechecking: boolean
  onContinue: () => void
  saving: boolean
}) {
  if (isPending) {
    return (
      <div
        className="space-y-3"
        role="status"
        aria-label="Reading this machine"
      >
        {Array.from({ length: 8 }, (_, i) => (
          <Skeleton key={i} className="h-10 w-full" />
        ))}
      </div>
    )
  }
  if (error || !facts) {
    return (
      <Callout tone="error" title="Couldn’t read this machine">
        {problemDetail(error, 'The host facts are unavailable. Try again.')}
      </Callout>
    )
  }
  const rows = hostRows(facts)
  const dockerDown = !facts.docker.running
  return (
    <div className="space-y-6">
      <h2 className="text-lg font-semibold [overflow-wrap:anywhere]">
        {facts.hostname ? `This is ${facts.hostname}` : 'This machine'}
      </h2>
      <dl className="divide-y rounded-lg border">
        {rows.map((row) => (
          <div
            key={row.label}
            className="grid grid-cols-1 gap-1 p-3 sm:grid-cols-[12rem_1fr] sm:gap-4"
          >
            <dt className="text-sm text-muted-foreground">{row.label}</dt>
            <dd className="min-w-0 text-sm">
              <span
                className={
                  row.tone === 'error'
                    ? 'font-medium text-destructive'
                    : row.tone === 'warning'
                      ? 'font-medium text-warning-strong'
                      : 'font-medium'
                }
              >
                {row.value}
              </span>
              {row.note ? (
                <span className="mt-1 block text-xs text-muted-foreground [overflow-wrap:anywhere]">
                  {row.note}
                </span>
              ) : null}
            </dd>
          </div>
        ))}
      </dl>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <Button
          busy={saving}
          busyLabel="Saving…"
          // Wired, not disabled: it stays focusable and says why below.
          aria-disabled={dockerDown || undefined}
          aria-describedby={dockerDown ? 'host-blocked' : undefined}
          onClick={() => !dockerDown && !saving && onContinue()}
        >
          Continue
        </Button>
        <Button
          variant="outline"
          busy={rechecking}
          busyLabel="Checking…"
          onClick={onRecheck}
        >
          Check again
        </Button>
        {dockerDown && (
          <p id="host-blocked" className="text-sm text-muted-foreground">
            Start Docker first: apps run as containers.
          </p>
        )}
      </div>
    </div>
  )
}
