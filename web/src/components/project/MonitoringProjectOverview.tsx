// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ProjectResponse } from '@/api/client'
import { useQuery } from '@tanstack/react-query'
import {
  hasAnalyticsEventsOptions,
  hasErrorGroupsOptions,
  hasTracesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Link, useLocation } from 'react-router'
import { PageHeader } from '@/components/layout/PageContainer'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { monitoringSetupPrompt } from '@/lib/monitoring-setup-prompt'
import { Badge } from '@/components/ui/badge'
import { ArrowRight, BarChart3, ShieldAlert, GitFork } from 'lucide-react'

export function MonitoringProjectOverview({
  project,
}: {
  project: ProjectResponse
}) {
  const analytics = useQuery(
    hasAnalyticsEventsOptions({ path: { project_id: project.id } })
  )
  const errors = useQuery(
    hasErrorGroupsOptions({ path: { project_id: project.id } })
  )
  const traces = useQuery(
    hasTracesOptions({ path: { project_id: project.id } })
  )
  const base = `/projects/${project.slug}`
  const integrations = [
    {
      feature: 'analytics' as const,
      title: 'Analytics',
      description: 'See who visits your site and which pages they use.',
      icon: BarChart3,
      query: analytics,
      received: analytics.data?.has_events,
      setup: 'analytics/setup',
      view: 'analytics',
    },
    {
      feature: 'errors' as const,
      title: 'Error tracking',
      description: 'Find app errors and see what went wrong.',
      icon: ShieldAlert,
      query: errors,
      received: errors.data?.has_error_groups,
      setup: 'errors/setup',
      view: 'errors',
    },
    {
      feature: 'traces' as const,
      title: 'OpenTelemetry',
      description: 'Find slow steps in your app. Uses OpenTelemetry.',
      icon: GitFork,
      query: traces,
      received: traces.data?.has_traces,
      setup: 'traces#traces-setup',
      view: 'traces',
    },
  ]
  return (
    <section className="space-y-6" aria-label="Monitoring integrations">
      <PageHeader
        title="Connect your application"
        description="Choose a feature. Copy its prompt into your AI coding tool, or follow the setup guide."
      />
      <div className="divide-y rounded-lg border bg-card">
        {integrations.map(
          ({
            title,
            feature,
            description,
            icon: Icon,
            query,
            received,
            setup,
            view,
          }) => (
            <div
              key={title}
              className="flex flex-wrap items-center justify-between gap-4 p-4"
            >
              <div className="flex items-start gap-3">
                <Icon className="mt-1 size-5 text-muted-foreground" />
                <div>
                  <h2 className="font-medium">{title}</h2>
                  <p className="text-sm text-muted-foreground">{description}</p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {query.isError
                      ? 'We could not check for data. Try again.'
                      : query.isPending
                        ? 'Checking for data…'
                        : received
                          ? 'Data received'
                          : 'No data yet. Add the setup code to your app.'}
                  </p>
                </div>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <CopyButton
                  value={monitoringSetupPrompt(
                    feature,
                    project,
                    window.location.origin
                  )}
                  label={`Copy ${title} setup prompt and skill`}
                  className="h-9 rounded-md border px-3"
                >
                  Copy setup prompt
                </CopyButton>
                {query.isError && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => void query.refetch()}
                  >
                    Retry
                  </Button>
                )}
                <Button variant="outline" asChild>
                  <Link to={`${base}/${received ? view : setup}`}>
                    {received ? 'View data' : 'Set up'}
                    <span className="sr-only"> {title}</span>
                    <ArrowRight className="size-4" />
                  </Link>
                </Button>
              </div>
            </div>
          )
        )}
      </div>
      {project.source_type === 'external' && (
        <div className="flex flex-wrap items-center justify-between gap-3 border-t pt-4">
          <div>
            <p className="font-medium">Hosting is optional</p>
            <p className="text-sm text-muted-foreground">
              Your app can stay where it is. Move it to Temps later and keep
              your data.
            </p>
          </div>
          <Button asChild variant="outline">
            <Link to={`${base}/hosting`}>Add hosting</Link>
          </Button>
        </div>
      )}
      <p className="text-sm text-muted-foreground">
        Want to know if your site goes down?{' '}
        <Link className="underline underline-offset-4" to={`${base}/monitors`}>
          Set up an uptime check
        </Link>
        .
      </p>
    </section>
  )
}

export function MonitoringProjectHeader({
  project,
}: {
  project: ProjectResponse
}) {
  const { pathname } = useLocation()
  return (
    <header className="flex flex-wrap items-center justify-between gap-3 border-b p-4">
      <div className="flex min-w-0 items-center gap-3">
        <p className="truncate font-semibold">{project.name}</p>
        <Badge variant="secondary">External</Badge>
      </div>
      {!pathname.endsWith('/hosting') && (
        <Button asChild variant="outline">
          <Link to={`/projects/${project.slug}/hosting`}>Add hosting</Link>
        </Button>
      )}
    </header>
  )
}
