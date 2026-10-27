// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import type { PreviewDomainStatusResponse } from '@/api/client'
import {
  listConnectionsOptions,
  listDomainsOptions,
  getProjectsOptions,
  getProjectStatisticsOptions,
  listNotificationProvidersOptions,
  listServicesOptions,
  listBackupSchedulesOptions,
  listDnsProvidersOptions,
  listUsersOptions,
  getAiProviderStatusOptions,
  listApiKeysOptions,
  getPreviewDomainStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useSettings } from './useSettings'
import { useIsInstanceAdmin } from './useIsInstanceAdmin'
import { SIMULATE_EMPTY_INSTALL } from '@/lib/devSimulate'
import { getAiHarnessStatus } from '@/lib/ai-onboarding'
import { hasCompletedDeployment } from '@/lib/first-deploy'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'

export interface ActivationSignals {
  /** Dedicated active admin key has authenticated an external AI harness */
  aiHarnessConfigured: boolean
  /** Active provider can power Temps' built-in chat and AI features */
  aiProviderConfigured: boolean
  /** Git provider connected and active */
  gitConnected: boolean
  /** At least one active wildcard domain in the database */
  wildcardDomainReady: boolean
  /**
   * Apps get an address someone else can open (ADR 068 D5): the preview
   * domain is usable, or a tunnel publishes environment hosts.
   */
  appsReachable: boolean
  /** At least one project created */
  hasProject: boolean
  /** At least one deployment has completed (an app went live) */
  firstDeploySucceeded: boolean
  /** external_url is configured in settings */
  externalUrlSet: boolean
  /** At least one enabled notification provider */
  notificationsConfigured: boolean
  /** At least one managed database/service created */
  hasDatabase: boolean
  /** At least one backup schedule configured */
  backupsConfigured: boolean
  /** At least one DNS provider connected */
  dnsProviderConnected: boolean
  /** More than just the initial admin — i.e. a teammate was invited */
  teamInvited: boolean
  /**
   * Signals this role is not allowed to read (platform settings, DNS
   * providers and the user list need `settings:read`). Their value above is
   * `false` only because nothing was read: they must not be shown as "not
   * done", nor counted in `completedCount`/`totalCount`.
   */
  unreadable: {
    externalUrl: boolean
    dnsProviders: boolean
    team: boolean
  }
  /** True once all signals are loaded (used to avoid flash of incomplete state) */
  isLoaded: boolean
  /** Number of completed items out of total */
  completedCount: number
  totalCount: number
}

const TOTAL = 11

const NOTHING_UNREADABLE = {
  externalUrl: false,
  dnsProviders: false,
  team: false,
}

export function useActivationSignals(): ActivationSignals {
  // Platform settings, DNS providers and the user list are readable by
  // instance administrators only. Every other role would be refused on every
  // page that shows the checklist, so those reads are skipped for them (a
  // disabled query is not loading, so `isLoaded` is unaffected).
  const isAdmin = useIsInstanceAdmin()
  const { data: settings, isLoading: settingsLoading } = useSettings()

  const { data: connections, isLoading: connectionsLoading } = useQuery({
    ...listConnectionsOptions({}),
    retry: false,
  })

  const { data: domainsData, isLoading: domainsLoading } = useQuery({
    ...listDomainsOptions({ query: { page_size: 50 } }),
    retry: false,
  })

  const { data: projectsData, isLoading: projectsLoading } = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 1 } }),
    retry: false,
  })

  const { data: projectStatistics, isLoading: projectStatisticsLoading } =
    useQuery({
      ...getProjectStatisticsOptions(),
      retry: false,
    })

  const { data: providersData, isLoading: providersLoading } = useQuery({
    ...listNotificationProvidersOptions({}),
    retry: false,
  })

  const { data: servicesData, isLoading: servicesLoading } = useQuery({
    ...listServicesOptions({}),
    retry: false,
  })

  const { data: backupSchedulesData, isLoading: backupSchedulesLoading } =
    useQuery({
      ...listBackupSchedulesOptions({}),
      retry: false,
    })

  const { data: dnsProvidersData, isLoading: dnsProvidersLoading } = useQuery({
    ...listDnsProvidersOptions({}),
    retry: false,
    enabled: isAdmin,
  })

  const { data: usersData, isLoading: usersLoading } = useQuery({
    ...listUsersOptions({ query: { include_deleted: false } }),
    retry: false,
    enabled: isAdmin,
  })

  // Platform settings, so instance administrators only, like the reads above.
  const { data: previewStatus, isLoading: previewStatusLoading } = useQuery({
    ...getPreviewDomainStatusOptions(),
    retry: false,
    enabled: isAdmin,
    staleTime: 60_000,
  })

  const { data: aiProviderStatus, isLoading: aiProviderStatusLoading } =
    useQuery({
      ...getAiProviderStatusOptions(),
      retry: false,
    })

  const { data: apiKeysData, isLoading: apiKeysLoading } = useQuery({
    ...listApiKeysOptions({ query: { page: 1, page_size: 100 } }),
    retry: false,
    // Shares its cache entry with the AI-harness setup banner, and each
    // observer's `meta` replaces the other's: both must opt out. Roles without
    // API-key access are refused here, which is not a failure of the page.
    meta: { routeFailure: ROUTE_FAILURE_IGNORE },
  })

  // TEMP: pretend nothing is set up yet, for building the first-run
  // experience. See lib/devSimulate.ts. Placed after all hooks so hook order
  // stays stable.
  if (SIMULATE_EMPTY_INSTALL) {
    return {
      aiHarnessConfigured: false,
      aiProviderConfigured: false,
      gitConnected: false,
      wildcardDomainReady: false,
      appsReachable: false,
      hasProject: false,
      firstDeploySucceeded: false,
      externalUrlSet: false,
      notificationsConfigured: false,
      hasDatabase: false,
      backupsConfigured: false,
      dnsProviderConnected: false,
      teamInvited: false,
      unreadable: NOTHING_UNREADABLE,
      isLoaded: true,
      completedCount: 0,
      totalCount: TOTAL,
    }
  }

  const isLoaded =
    !settingsLoading &&
    !connectionsLoading &&
    !domainsLoading &&
    !projectsLoading &&
    !projectStatisticsLoading &&
    !providersLoading &&
    !servicesLoading &&
    !backupSchedulesLoading &&
    !dnsProvidersLoading &&
    !previewStatusLoading &&
    !usersLoading &&
    !aiProviderStatusLoading &&
    !apiKeysLoading

  const aiProviderConfigured = aiProviderStatus?.configured === true
  const aiHarnessConfigured =
    getAiHarnessStatus(apiKeysData?.api_keys) === 'connected'

  const gitConnected =
    (connections?.connections?.filter((c) => c.is_active).length ?? 0) > 0

  const wildcardDomainReady =
    domainsData?.domains?.some((d) => d.is_wildcard && d.status === 'active') ??
    false

  const appsReachable = appsReachableSignal(
    isAdmin,
    previewStatus,
    wildcardDomainReady
  )

  const hasProject = (projectsData?.projects?.length ?? 0) > 0
  const firstDeploySucceeded = hasCompletedDeployment(projectStatistics)

  const externalUrlSet = !!settings?.external_url

  const notificationsConfigured =
    Array.isArray(providersData) && providersData.some((p) => p.enabled)

  const hasDatabase = (servicesData?.length ?? 0) > 0

  const backupsConfigured = (backupSchedulesData?.length ?? 0) > 0

  const dnsProviderConnected = (dnsProvidersData?.length ?? 0) > 0

  // The initial admin always exists, so "team invited" means more than one
  // active user.
  const teamInvited = (usersData?.length ?? 0) > 1

  const unreadable = isAdmin
    ? NOTHING_UNREADABLE
    : { externalUrl: true, dnsProviders: true, team: true }
  const unreadableCount = Object.values(unreadable).filter(Boolean).length

  const completed = [
    aiHarnessConfigured,
    gitConnected,
    appsReachable,
    hasProject,
    firstDeploySucceeded,
    externalUrlSet && !unreadable.externalUrl,
    notificationsConfigured,
    hasDatabase,
    backupsConfigured,
    dnsProviderConnected && !unreadable.dnsProviders,
    teamInvited && !unreadable.team,
  ].filter(Boolean).length

  return {
    aiHarnessConfigured,
    aiProviderConfigured,
    gitConnected,
    wildcardDomainReady,
    appsReachable,
    hasProject,
    firstDeploySucceeded,
    externalUrlSet,
    notificationsConfigured,
    hasDatabase,
    backupsConfigured,
    dnsProviderConnected,
    teamInvited,
    unreadable,
    isLoaded,
    completedCount: completed,
    totalCount: TOTAL - unreadableCount,
  }
}

/**
 * "Make your apps reachable" (ADR 068): done when the preview domain is
 * usable or a tunnel gives every environment an address. Who cannot read
 * the preview domain status keeps the wildcard-domain signal, the closest
 * thing they can see.
 */
export function appsReachableSignal(
  isAdmin: boolean,
  previewStatus:
    | Pick<PreviewDomainStatusResponse, 'status' | 'tunnel_environment_hosts'>
    | undefined,
  wildcardDomainReady: boolean
): boolean {
  if (!isAdmin) return wildcardDomainReady
  return (
    previewStatus?.status === 'usable' ||
    !!previewStatus?.tunnel_environment_hosts
  )
}
