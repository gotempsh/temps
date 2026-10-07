// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Activity } from 'lucide-react'
import { hasAnalyticsEventsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import {
  ANALYTICS_ONBOARDING_COPY,
  resolveAnalyticsInstallState,
  type AnalyticsOnboardingFeature,
} from './analytics-onboarding'

interface AnalyticsOnboardingGateProps {
  project: ProjectResponse
  feature: AnalyticsOnboardingFeature
  children: ReactNode
}

/**
 * Renders an analytics view once the project has received events, and an
 * onboarding state that links to analytics setup when it never has, so a view
 * reached from the navigation explains why it is empty instead of looking
 * broken.
 */
export function AnalyticsOnboardingGate({
  project,
  feature,
  children,
}: AnalyticsOnboardingGateProps) {
  const hasEventsQuery = useQuery(
    hasAnalyticsEventsOptions({ path: { project_id: project.id } })
  )
  const state = resolveAnalyticsInstallState(hasEventsQuery)

  if (state === 'checking') {
    return (
      <div className="space-y-4" aria-busy="true">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-[400px] w-full rounded-lg" />
      </div>
    )
  }

  if (state === 'not-installed') {
    return <AnalyticsNotInstalledState project={project} feature={feature} />
  }

  return <>{children}</>
}

export function AnalyticsNotInstalledState({
  project,
  feature,
}: {
  project: ProjectResponse
  feature: AnalyticsOnboardingFeature
}) {
  const copy = ANALYTICS_ONBOARDING_COPY[feature]
  return (
    <Card>
      <CardContent>
        <EmptyState
          icon={Activity}
          title={`${copy.title} needs web analytics`}
          description={
            <div className="space-y-2 text-sm text-muted-foreground">
              <p>{copy.example}</p>
              <p>
                No analytics events have been received for {project.name} yet.
                Add the Temps analytics snippet to your site, then load a page
                to start seeing data here.
              </p>
            </div>
          }
          action={
            <Button asChild>
              <Link to={`/projects/${project.slug}/analytics/setup`}>
                Set up analytics
              </Link>
            </Button>
          }
        />
      </CardContent>
    </Card>
  )
}
