import type { ProjectResponse } from '@/api/client'
import {
  hasAnalyticsEventsOptions,
  hasErrorGroupsOptions,
  listCustomDomainsForProjectOptions,
  listMonitorsOptions,
  listProjectServicesOptions,
  queryTraceSummariesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useQuery } from '@tanstack/react-query'
import {
  Activity,
  BarChart3,
  Database,
  Globe,
  ShieldAlert,
  Workflow,
  type LucideIcon,
} from 'lucide-react'

export type ProjectSetupStepId =
  'analytics' | 'errors' | 'domain' | 'monitoring' | 'storage' | 'telemetry'

export interface ProjectSetupStep {
  id: ProjectSetupStepId
  title: string
  description: string
  href: string
  cta: string
  done: boolean
  icon: LucideIcon
}

export interface ProjectSetupSignals {
  hasAnalyticsEvents: boolean
  hasErrorGroups: boolean
  customDomainCount: number
  monitorCount: number
  serviceCount: number
  hasTelemetryTraces: boolean
}

export function buildProjectSetupSteps(
  projectSlug: string,
  signals: ProjectSetupSignals
): ProjectSetupStep[] {
  const base = `/projects/${projectSlug}`

  return [
    {
      id: 'analytics',
      title: 'Install the analytics SDK',
      description:
        'Understand traffic, pages, visitors, funnels, and performance with project-scoped analytics.',
      href: `${base}/analytics/setup`,
      cta: 'View analytics setup',
      done: signals.hasAnalyticsEvents,
      icon: BarChart3,
    },
    {
      id: 'errors',
      title: 'Install the error tracking SDK',
      description:
        'Capture application exceptions with stack traces, releases, and the request context needed to debug them.',
      href: `${base}/errors/setup`,
      cta: 'View error tracking setup',
      done: signals.hasErrorGroups,
      icon: ShieldAlert,
    },
    {
      id: 'domain',
      title: 'Connect a custom domain',
      description:
        'Give the project a production hostname and let Temps provision and renew its TLS certificate.',
      href: `${base}/domains`,
      cta: 'Manage domains',
      done: signals.customDomainCount > 0,
      icon: Globe,
    },
    {
      id: 'monitoring',
      title: 'Add an uptime monitor',
      description:
        'Continuously check the production endpoint and surface outages before users report them.',
      href: `${base}/monitors`,
      cta: 'Configure monitoring',
      done: signals.monitorCount > 0,
      icon: Activity,
    },
    {
      id: 'storage',
      title: 'Link database or storage',
      description:
        'Attach a managed service to this project so application data and deployment configuration stay together.',
      href: `${base}/storage`,
      cta: 'Open project storage',
      done: signals.serviceCount > 0,
      icon: Database,
    },
    {
      id: 'telemetry',
      title: 'Set up OpenTelemetry',
      description:
        'Send distributed traces from this application to connect requests across services and deployments.',
      href: `${base}/traces#traces-setup`,
      cta: 'View OpenTelemetry setup',
      done: signals.hasTelemetryTraces,
      icon: Workflow,
    },
  ]
}

export function useProjectSetup(project: ProjectResponse) {
  const analyticsQuery = useQuery({
    ...hasAnalyticsEventsOptions({ path: { project_id: project.id } }),
  })
  const errorsQuery = useQuery({
    ...hasErrorGroupsOptions({ path: { project_id: project.id } }),
  })
  const domainsQuery = useQuery({
    ...listCustomDomainsForProjectOptions({
      path: { project_id: project.id },
    }),
  })
  const monitorsQuery = useQuery({
    ...listMonitorsOptions({ path: { project_id: project.id } }),
  })
  const servicesQuery = useQuery({
    ...listProjectServicesOptions({ path: { project_id: project.id } }),
  })
  const telemetryQuery = useQuery({
    ...queryTraceSummariesOptions({
      query: {
        project_id: project.id,
        limit: 1,
        include_total: false,
      },
    }),
  })

  const steps = buildProjectSetupSteps(project.slug, {
    hasAnalyticsEvents: !!analyticsQuery.data?.has_events,
    hasErrorGroups: !!errorsQuery.data?.has_error_groups,
    customDomainCount: domainsQuery.data?.domains?.length ?? 0,
    monitorCount: monitorsQuery.data?.length ?? 0,
    serviceCount: servicesQuery.data?.length ?? 0,
    hasTelemetryTraces: (telemetryQuery.data?.data?.length ?? 0) > 0,
  })
  const completedCount = steps.filter((step) => step.done).length
  const totalCount = steps.length

  return {
    steps,
    completedCount,
    totalCount,
    percent: Math.round((completedCount / totalCount) * 100),
    nextStep: steps.find((step) => !step.done),
    allDone: completedCount === totalCount,
    isLoading:
      analyticsQuery.isLoading ||
      errorsQuery.isLoading ||
      domainsQuery.isLoading ||
      monitorsQuery.isLoading ||
      servicesQuery.isLoading ||
      telemetryQuery.isLoading,
    isError:
      analyticsQuery.isError ||
      errorsQuery.isError ||
      domainsQuery.isError ||
      monitorsQuery.isError ||
      servicesQuery.isError ||
      telemetryQuery.isError,
    refetch: () =>
      Promise.all([
        analyticsQuery.refetch(),
        errorsQuery.refetch(),
        domainsQuery.refetch(),
        monitorsQuery.refetch(),
        servicesQuery.refetch(),
        telemetryQuery.refetch(),
      ]),
  }
}
