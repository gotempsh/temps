// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { DeploymentResponse, ProjectResponse } from '@/api/client'
import {
  getLastDeploymentOptions,
  getLastDeploymentQueryKey,
  getProjectBySlugOptions,
  getProjectsQueryKey,
  getProjectStatisticsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { Button, Callout, Status } from '@temps-sdk/ds'
import { Button as LinkButton } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { deploymentFailureSummary } from '@/lib/deployment-failure-summary'
import { displayUrl, resolvePrimaryUrl } from '@/lib/deployment-url'
import {
  FIRST_DEPLOY_PATH,
  SAMPLE_APP,
  firstDeployFailureHint,
  firstDeployPhase,
} from '@/lib/first-deploy'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowRight,
  CircleCheck,
  Database,
  ExternalLink,
  GitBranch,
  Globe,
  LoaderCircle,
  RotateCcw,
  ScrollText,
  type LucideIcon,
} from 'lucide-react'
import { useEffect } from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { redeployImage } from './sample-deploy-api'

/** Poll while a deployment runs; give a just-started one a few tries to appear. */
const POLL_INTERVAL_MS = 2000
const MAX_POLLS_WITHOUT_DEPLOYMENT = 5

/**
 * Follows the sample project's latest deployment from "starting" to a live
 * URL, or to an explained failure with a retry. This is the explicit success
 * moment of the first-run path: the URL, and what to do next.
 */
export function SampleDeployTracker({
  projectSlug,
  startError,
}: {
  projectSlug: string
  startError?: string
}) {
  const queryClient = useQueryClient()

  const projectQuery = useQuery({
    ...getProjectBySlugOptions({ path: { slug: projectSlug } }),
    retry: false,
  })
  const project = projectQuery.data

  const deploymentQuery = useQuery({
    ...getLastDeploymentOptions({ path: { id: project?.id ?? 0 } }),
    enabled: project != null,
    retry: false,
    refetchInterval: (query) => {
      const deployment = query.state.data
      if (deployment) {
        return firstDeployPhase(deployment.status) === 'in_progress'
          ? POLL_INTERVAL_MS
          : false
      }
      return query.state.errorUpdateCount < MAX_POLLS_WITHOUT_DEPLOYMENT
        ? POLL_INTERVAL_MS
        : false
    },
  })
  const deployment = deploymentQuery.data
  const phase = deployment ? firstDeployPhase(deployment.status) : undefined

  // The setup checklist reads "has anything gone live" from the project list;
  // refresh it the moment this deployment completes so it ticks off at once.
  useEffect(() => {
    if (phase === 'succeeded') {
      void queryClient.invalidateQueries({ queryKey: getProjectsQueryKey() })
      void queryClient.invalidateQueries({
        queryKey: getProjectStatisticsQueryKey(),
      })
    }
  }, [phase, queryClient])

  const retry = useMutation({
    mutationFn: (projectId: number) =>
      redeployImage(projectId, SAMPLE_APP.image),
    meta: { errorTitle: 'Could not start the deployment' },
    onSuccess: async (result) => {
      if (result.status === 'no_environment') {
        toast.error('The project has no environment to deploy to.')
        return
      }
      toast.success('Deployment started')
      if (project) {
        await queryClient.invalidateQueries({
          queryKey: getLastDeploymentQueryKey({ path: { id: project.id } }),
        })
      }
    },
  })

  if (projectQuery.isPending) return <TrackerSkeleton />

  if (projectQuery.isError || !project) {
    return (
      <Callout tone="error" title={`Project “${projectSlug}” was not found`}>
        <p>
          It may have been deleted.{' '}
          <Link className="underline underline-offset-4" to={FIRST_DEPLOY_PATH}>
            Start a new first deploy
          </Link>
          .
        </p>
      </Callout>
    )
  }

  const deployButton = (label: string) => (
    <Button
      busy={retry.isPending}
      busyLabel="Starting…"
      onClick={() => retry.mutate(project.id)}
    >
      <RotateCcw className="size-4" aria-hidden />
      {label}
    </Button>
  )

  if (!deployment) {
    // A deployment started a moment ago may not be visible on the first
    // read; keep waiting through the bounded polls before calling it absent.
    const stillWaiting =
      !startError &&
      deploymentQuery.errorUpdateCount < MAX_POLLS_WITHOUT_DEPLOYMENT
    if (deploymentQuery.isPending || stillWaiting) {
      return <TrackerSkeleton />
    }
    const hint = firstDeployFailureHint(startError)
    return (
      <TrackerSurface
        project={project}
        status={<Status tone="warn" label="Not deployed" />}
      >
        <Callout
          tone={startError ? 'error' : 'warning'}
          title={startError ? hint.title : 'No deployment has started yet'}
        >
          {startError ? (
            <>
              <p className="whitespace-pre-wrap break-words">{startError}</p>
              <p className="mt-2">{hint.remedy}</p>
            </>
          ) : (
            <p>
              The project exists but nothing has been deployed to it. Start the
              sample deployment to get a live URL.
            </p>
          )}
        </Callout>
        <div className="flex flex-wrap gap-2">
          {deployButton(
            startError ? 'Retry deployment' : 'Deploy the sample image'
          )}
        </div>
      </TrackerSurface>
    )
  }

  if (phase === 'failed') {
    return (
      <FailedDeployment
        project={project}
        deployment={deployment}
        retryButton={deployButton('Retry deployment')}
      />
    )
  }

  if (phase === 'succeeded') {
    return <LiveDeployment project={project} deployment={deployment} />
  }

  return (
    <TrackerSurface
      project={project}
      status={<Status tone="running" label="Deploying" />}
    >
      <ol className="space-y-3" aria-label="Deployment progress">
        <ProgressStep done label="Project created" />
        <ProgressStep
          active
          label={`Pulling ${SAMPLE_APP.image} and starting the container`}
          detail="A fresh server downloads the image first; this usually takes under a minute."
        />
        <ProgressStep label="Live at a public URL" />
      </ol>
      <div className="flex flex-wrap gap-2">
        <LinkButton asChild variant="outline">
          <Link to={`/projects/${project.slug}/deployments/${deployment.id}`}>
            <ScrollText className="size-4" aria-hidden />
            Watch the logs
          </Link>
        </LinkButton>
      </div>
    </TrackerSurface>
  )
}

function TrackerSurface({
  project,
  status,
  children,
}: {
  project: ProjectResponse
  status: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <section
      aria-labelledby="first-deploy-project"
      className="space-y-5 rounded-lg border bg-card p-5 text-card-foreground"
    >
      <div className="flex flex-wrap items-center gap-3">
        <h2 id="first-deploy-project" className="text-lg font-semibold">
          {project.name}
        </h2>
        {status}
      </div>
      {children}
    </section>
  )
}

function ProgressStep({
  label,
  detail,
  done = false,
  active = false,
}: {
  label: string
  detail?: string
  done?: boolean
  active?: boolean
}) {
  const Icon: LucideIcon = done ? CircleCheck : LoaderCircle
  return (
    <li className="flex items-start gap-3">
      {done || active ? (
        <Icon
          className={
            done
              ? 'mt-0.5 size-4 shrink-0 text-success'
              : 'mt-0.5 size-4 shrink-0 animate-spin text-primary motion-reduce:animate-none'
          }
          aria-hidden
        />
      ) : (
        <span
          className="mt-1 size-3 shrink-0 rounded-full border-2 border-muted-foreground/40"
          aria-hidden
        />
      )}
      <div className="min-w-0">
        <p
          className={
            done || active
              ? 'text-sm font-medium'
              : 'text-sm text-muted-foreground'
          }
        >
          {label}
          <span className="sr-only">
            {done ? ' (done)' : active ? ' (in progress)' : ' (pending)'}
          </span>
        </p>
        {detail && (
          <p className="mt-0.5 text-sm text-muted-foreground">{detail}</p>
        )}
      </div>
    </li>
  )
}

function FailedDeployment({
  project,
  deployment,
  retryButton,
}: {
  project: ProjectResponse
  deployment: DeploymentResponse
  retryButton: React.ReactNode
}) {
  const reason = deployment.cancelled_reason ?? ''
  const hint = firstDeployFailureHint(reason)
  const summary = reason ? deploymentFailureSummary(reason).summary : null
  return (
    <TrackerSurface
      project={project}
      status={
        <Status
          tone="error"
          label={
            deployment.status === 'stopped'
              ? 'Stopped'
              : deployment.status === 'cancelled'
                ? 'Cancelled'
                : 'Failed'
          }
        />
      }
    >
      <Callout tone="error" title={hint.title}>
        {summary && (
          <p className="whitespace-pre-wrap break-words">{summary}</p>
        )}
        <p className={summary ? 'mt-2' : undefined}>{hint.remedy}</p>
      </Callout>
      <div className="flex flex-wrap gap-2">
        {retryButton}
        <LinkButton asChild variant="outline">
          <Link to={`/projects/${project.slug}/deployments/${deployment.id}`}>
            <ScrollText className="size-4" aria-hidden />
            Open deployment logs
          </Link>
        </LinkButton>
      </div>
    </TrackerSurface>
  )
}

interface NextStep {
  key: string
  title: string
  description: string
  href: string
  icon: LucideIcon
}

function nextSteps(project: ProjectResponse): NextStep[] {
  return [
    {
      key: 'domain',
      title: 'Add a custom domain',
      description: 'Serve this app on your own hostname with automatic HTTPS.',
      href: `/projects/${project.slug}/domains`,
      icon: Globe,
    },
    {
      key: 'database',
      title: 'Add a database',
      description:
        'Provision Postgres, Redis or MongoDB and attach it to a project.',
      href: '/storage/create',
      icon: Database,
    },
    {
      key: 'git',
      title: 'Connect Git',
      description: 'Deploy your own code on every push, with preview URLs.',
      href: '/git-providers/add',
      icon: GitBranch,
    },
  ]
}

function LiveDeployment({
  project,
  deployment,
}: {
  project: ProjectResponse
  deployment: DeploymentResponse
}) {
  const url = resolvePrimaryUrl(deployment)
  return (
    <TrackerSurface
      project={project}
      status={<Status tone="ok" label="Live" />}
    >
      <div className="space-y-2">
        <p className="text-sm font-medium">Your first app is live</p>
        {url ? (
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <a
              href={url}
              target="_blank"
              rel="noopener noreferrer"
              className="min-w-0 break-all font-mono text-sm text-primary underline underline-offset-4"
            >
              {displayUrl(url)}
            </a>
            <CopyButton value={url} minimal label="Copy app URL" />
            <LinkButton asChild size="sm">
              <a href={url} target="_blank" rel="noopener noreferrer">
                Open app
                <ExternalLink className="size-3.5" aria-hidden />
              </a>
            </LinkButton>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            The deployment finished, but this environment has no public URL yet.{' '}
            <Link
              className="underline underline-offset-4"
              to={`/projects/${project.slug}/domains`}
            >
              Add a domain
            </Link>{' '}
            to reach it.
          </p>
        )}
        <p className="text-sm text-muted-foreground">
          “Deploy your first app” is now checked off in{' '}
          <Link className="underline underline-offset-4" to="/setup">
            Platform setup
          </Link>
          .
        </p>
      </div>

      <div className="space-y-3 border-t pt-4">
        <h3 className="text-sm font-semibold">Next steps</h3>
        <ul className="grid gap-x-6 gap-y-3 md:grid-cols-3">
          {nextSteps(project).map((step) => (
            <li key={step.key} className="min-w-0">
              <Link
                to={step.href}
                className="group inline-flex items-center gap-2 text-sm font-medium underline-offset-4 hover:underline"
              >
                <step.icon
                  className="size-4 shrink-0 text-muted-foreground"
                  aria-hidden
                />
                {step.title}
                <ArrowRight
                  className="size-3.5 shrink-0 text-muted-foreground"
                  aria-hidden
                />
              </Link>
              <p className="mt-1 text-sm text-muted-foreground">
                {step.description}
              </p>
            </li>
          ))}
        </ul>
      </div>

      <div className="flex flex-wrap gap-2 border-t pt-4">
        <LinkButton asChild variant="outline">
          <Link to={`/projects/${project.slug}`}>
            Open the {project.name} project
          </Link>
        </LinkButton>
        <LinkButton asChild variant="ghost">
          <Link to="/projects/new">Deploy your own app</Link>
        </LinkButton>
      </div>
    </TrackerSurface>
  )
}

function TrackerSkeleton() {
  return (
    <div
      className="space-y-4 rounded-lg border bg-card p-5"
      aria-label="Loading deployment"
      aria-busy="true"
    >
      <Skeleton className="h-6 w-48" />
      <Skeleton className="h-4 w-full max-w-md" />
      <Skeleton className="h-4 w-full max-w-sm" />
      <Skeleton className="h-9 w-40" />
    </div>
  )
}
