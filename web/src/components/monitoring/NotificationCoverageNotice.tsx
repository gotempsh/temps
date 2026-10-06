// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { getNotificationDeliveryCoverageOptions } from '@/api/client/@tanstack/react-query.gen'
import type { NotificationDeliveryCoverageResponse } from '@/api/client/types.gen'
import { Skeleton } from '@/components/ui/skeleton'
import { locationPath, withReturnTo } from '@/lib/safe-return-to'
import { Callout } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { Link, useLocation } from 'react-router'

interface NotificationCoverageNoticeProps {
  /** Notification severity the rule fires at (what routes match on). */
  severity: string
  /** Runs before leaving for provider/route setup, e.g. to save a draft. */
  onLeave?: () => void
}

const linkClass = 'font-medium underline underline-offset-4'

/**
 * Tells the author of an alert rule whether its notifications would reach
 * anyone. Providers and routes are instance-wide while rules are per project,
 * so without this a rule can be saved that silently never delivers.
 */
export function NotificationCoverageNotice({
  severity,
  onLeave,
}: NotificationCoverageNoticeProps) {
  const location = useLocation()
  const coverage = useQuery({
    ...getNotificationDeliveryCoverageOptions({ query: { severity } }),
  })

  if (coverage.isPending) return <Skeleton className="h-14 w-full" />
  if (coverage.isError) {
    return (
      <p className="text-sm text-muted-foreground">
        Could not check whether this rule will notify anyone. Review{' '}
        <Link to="/settings/notifications" className={linkClass}>
          notification providers and routes
        </Link>
        .
      </p>
    )
  }
  return (
    <CoverageMessage
      coverage={coverage.data}
      returnTo={locationPath(location)}
      onLeave={onLeave}
    />
  )
}

export function CoverageMessage({
  coverage,
  returnTo,
  onLeave,
}: {
  coverage: NotificationDeliveryCoverageResponse
  returnTo: string
  onLeave?: () => void
}) {
  if (!coverage.configured) {
    const setupPath = coverage.setup_path ?? '/settings/notifications/new'
    const addsProvider = setupPath.startsWith('/settings/notifications/new')
    return (
      <Callout tone="warning" title="This rule won't notify anyone yet">
        {coverage.reason ?? 'No notification destination is configured'}.
        Providers and routes apply to all projects.{' '}
        <Link
          to={withReturnTo(setupPath, returnTo)}
          onClick={onLeave}
          className={linkClass}
        >
          {addsProvider ? 'Add a notification provider' : 'Review routes'}
        </Link>{' '}
        and you will come back here with this form as you left it.
      </Callout>
    )
  }
  const providerCount = coverage.provider_ids.length
  const destinations = [
    providerCount > 0 &&
      `${providerCount} ${providerCount === 1 ? 'provider' : 'providers'}`,
    coverage.cloud_delivery && 'Temps Cloud',
  ]
    .filter(Boolean)
    .join(' and ')
  return (
    <p className="text-sm text-muted-foreground">
      {capitalize(coverage.severity)} notifications go to {destinations}, set in{' '}
      <Link
        to="/settings/notifications?tab=routes"
        onClick={onLeave}
        className={linkClass}
      >
        notification routes
      </Link>{' '}
      (all projects).
    </p>
  )
}

function capitalize(value: string): string {
  return value.charAt(0).toUpperCase() + value.slice(1)
}
