// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useLocation } from 'react-router'
import { CalendarClock, FileCode } from 'lucide-react'
import {
  Button,
  Callout,
  DataTable,
  PageContainer,
  PageHeader,
  RecordLink,
} from '@temps-sdk/ds'
import type { CronInfo, ProjectResponse } from '@/api/client'
import { getEnvironmentCronsOptions } from '@/api/client/@tanstack/react-query.gen'
import { CodeBlock } from '@/components/ui/code-block'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { RedirectWithQuery } from '@/components/routing/RedirectWithQuery'
import { useProjectEnvironment } from '@/components/project-frame/project-environment-context'
import { readFailureServerDetail } from '@/lib/read-failure'

function InlineCode({ children }: { children: string }) {
  return (
    <code className="rounded bg-muted px-1 py-0.5 text-xs">{children}</code>
  )
}

/**
 * How to declare a job in `.temps.yaml`: the empty page itself, and the
 * "How to add a job" dialog once there are jobs.
 */
function Instructions() {
  return (
    <div className="space-y-4">
      <p className="text-sm">
        List the jobs under <InlineCode>cron:</InlineCode> in the{' '}
        <InlineCode>.temps.yaml</InlineCode> file at your repository root:
      </p>
      <CodeBlock
        language="yaml"
        disableWrapToggle
        code={`cron:
  - path: "/api/ping"
    schedule: "*/5 * * * *"    # Every 5 minutes

  - path: "/api/daily-backup"
    schedule: "0 0 * * *"      # Daily at midnight

  - path: "/api/weekly-report"
    schedule: "0 0 * * 0"      # Weekly on Sunday`}
      />
      <ul className="list-inside list-disc space-y-1 text-sm text-muted-foreground">
        <li>
          <InlineCode>schedule</InlineCode> uses the five cron fields: minute,
          hour, day of month, month, weekday.
        </li>
        <li>
          <InlineCode>path</InlineCode> is an endpoint of your app; Temps calls
          it with a POST request.
        </li>
        <li>Changes apply on your next deployment.</li>
      </ul>
    </div>
  )
}

function formatRun(value: string | null | undefined) {
  return value ? new Date(value).toLocaleString() : 'Not scheduled'
}

/** How to declare a job in `.temps.yaml`, once the page lists some. */
function HowToAddDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[500px]">
        <DialogHeader>
          <DialogTitle>Add a cron job</DialogTitle>
          <DialogDescription>
            Temps calls your endpoint on the schedule you set.
          </DialogDescription>
        </DialogHeader>
        <Instructions />
      </DialogContent>
    </Dialog>
  )
}

/**
 * Monitoring › Cron jobs: the jobs Temps runs itself, the `cron:` entries of
 * `.temps.yaml` registered per environment on deploy, for the environment
 * chosen in the project bar. Jobs scheduled anywhere else are watched from
 * Check-ins, a different feature with its own tab.
 */
export function CronJobs({ project }: { project: ProjectResponse }) {
  const bar = useProjectEnvironment()
  const environment = bar.selection.environment
  const crons = useQuery({
    ...getEnvironmentCronsOptions({
      path: { project_id: project.id, env_id: environment?.id ?? 0 },
    }),
    enabled: environment !== undefined,
  })
  const rows = crons.data ?? []
  const loading =
    bar.isPending || (environment !== undefined && crons.isPending)
  const empty = !loading && !bar.isError && !crons.isError && rows.length === 0
  const [showInstructions, setShowInstructions] = useState(false)

  return (
    <PageContainer>
      <PageHeader
        title="Cron jobs"
        description={
          <>
            Endpoints of your app that Temps calls on a schedule, from{' '}
            <InlineCode>cron:</InlineCode> in{' '}
            <InlineCode>.temps.yaml</InlineCode>
            {environment ? ` (${environment.name})` : ''}.
          </>
        }
        actions={
          empty ? undefined : (
            <Button variant="outline" onClick={() => setShowInstructions(true)}>
              <FileCode className="size-4" />
              How to add a job
            </Button>
          )
        }
      />

      {bar.isError ? (
        <Callout tone="error" title="Could not load environments">
          The cron jobs are listed per environment, and this service&apos;s
          environments did not load.{' '}
          <button
            type="button"
            className="underline underline-offset-4"
            onClick={bar.retry}
          >
            Retry
          </button>
        </Callout>
      ) : crons.isError ? (
        <Callout tone="error" title="Could not load cron jobs">
          {readFailureServerDetail(crons.error) ??
            `The cron jobs of ${environment?.name ?? 'this environment'} did not load.`}{' '}
          <button
            type="button"
            className="underline underline-offset-4"
            onClick={() => void crons.refetch()}
          >
            Retry
          </button>
        </Callout>
      ) : empty ? (
        // Nothing to list: the page is the guide to adding the first job.
        <section
          aria-labelledby="cron-jobs-empty"
          className="space-y-4 rounded-lg border p-4 sm:p-6"
        >
          <div className="flex items-start gap-3">
            <CalendarClock
              aria-hidden="true"
              className="mt-0.5 size-5 shrink-0 text-muted-foreground"
            />
            <div className="min-w-0 space-y-1">
              <h2 id="cron-jobs-empty" className="text-base font-semibold">
                {environment
                  ? `No cron jobs in ${environment.name}`
                  : 'No cron jobs yet'}
              </h2>
              {!environment && (
                <p className="text-sm text-muted-foreground">
                  This service has no environments yet; the jobs start running
                  once it is deployed.
                </p>
              )}
            </div>
          </div>
          <Instructions />
        </section>
      ) : (
        <DataTable<CronInfo>
          rows={rows}
          rowKey={(cron) => cron.id}
          isLoading={loading}
          columns={[
            {
              key: 'path',
              header: 'Endpoint',
              render: (cron) => (
                <RecordLink
                  className="font-mono"
                  to={`/projects/${project.slug}/settings/cron-jobs/${cron.environment_id}/${cron.id}`}
                >
                  {cron.path}
                </RecordLink>
              ),
            },
            {
              key: 'schedule',
              header: 'Schedule',
              render: (cron) => (
                <code className="text-sm">{cron.schedule}</code>
              ),
            },
            {
              key: 'next',
              header: 'Next run',
              render: (cron) => formatRun(cron.next_run),
            },
          ]}
        />
      )}

      <p className="text-sm text-muted-foreground">
        Scheduling the job somewhere else?{' '}
        <Link
          className="underline underline-offset-4"
          to={`/projects/${project.slug}/check-ins`}
        >
          Set up a check-in
        </Link>{' '}
        to be alerted when it fails or does not run.
      </p>

      <HowToAddDialog
        open={showInstructions}
        onOpenChange={setShowInstructions}
      />
    </PageContainer>
  )
}

/**
 * Check-in monitors used to live under `cron-jobs/`; their old links
 * (a monitor, its edit page, "new") land on the same page under `check-ins/`.
 */
export function MovedToCheckIns({ project }: { project: ProjectResponse }) {
  const { pathname } = useLocation()
  const rest = pathname.slice(
    pathname.indexOf('/cron-jobs') + '/cron-jobs'.length
  )
  return <RedirectWithQuery to={`/projects/${project.slug}/check-ins${rest}`} />
}
