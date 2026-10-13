// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { Callout } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router'
import { serviceRestoreRunsQuery } from './restore-queries'
import { phaseLabel, pickActiveRun } from './restore-state'
import { activeRestoreCopy, sourceBackupSummary } from './run-context'

/**
 * Shown on a database's page while a restore involving it is running, so an
 * operator who lands there sees why it may be unavailable and can follow the
 * restore. Renders nothing otherwise.
 */
export function ActiveRestoreBanner({ serviceId }: { serviceId: number }) {
  const runsQuery = useQuery({
    ...serviceRestoreRunsQuery(serviceId),
    // Follow an active restore to its end; otherwise read once per visit.
    refetchInterval: (query) =>
      pickActiveRun(query.state.data) ? 5000 : false,
  })
  const run = pickActiveRun(runsQuery.data)
  if (!run) return null

  const copy = activeRestoreCopy(run, phaseLabel(run.phase))
  const backup = sourceBackupSummary(run)
  return (
    <Callout tone={copy.tone} title={copy.title}>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm">
          {copy.description}{' '}
          {backup.href ? (
            <>
              Restoring from{' '}
              <Link to={backup.href} className="font-medium hover:underline">
                {backup.label}
              </Link>
              .
            </>
          ) : null}
        </p>
        <Button size="sm" variant="outline" asChild>
          <Link to={`/storage/${serviceId}/restore?run=${run.id}`}>
            Follow restore
          </Link>
        </Button>
      </div>
    </Callout>
  )
}
