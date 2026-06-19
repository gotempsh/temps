import { useQuery } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  listDomainsOptions,
  getProjectsOptions,
  listNotificationProvidersOptions,
  listServicesOptions,
  listBackupSchedulesOptions,
  listDnsProvidersOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { useSettings } from './useSettings'

export interface ActivationSignals {
  /** Git provider connected and active */
  gitConnected: boolean
  /** At least one active wildcard domain in the database */
  wildcardDomainReady: boolean
  /** At least one project created */
  hasProject: boolean
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
  /** True once all signals are loaded (used to avoid flash of incomplete state) */
  isLoaded: boolean
  /** Number of completed items out of total */
  completedCount: number
  totalCount: number
}

const TOTAL = 9

export function useActivationSignals(): ActivationSignals {
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
  })

  const { data: usersData, isLoading: usersLoading } = useQuery({
    ...listUsersOptions({ query: { include_deleted: false } }),
    retry: false,
  })

  const isLoaded =
    !settingsLoading &&
    !connectionsLoading &&
    !domainsLoading &&
    !projectsLoading &&
    !providersLoading &&
    !servicesLoading &&
    !backupSchedulesLoading &&
    !dnsProvidersLoading &&
    !usersLoading

  const gitConnected =
    (connections?.connections?.filter((c) => c.is_active).length ?? 0) > 0

  const wildcardDomainReady =
    domainsData?.domains?.some(
      (d) => d.is_wildcard && d.status === 'active'
    ) ?? false

  const hasProject = (projectsData?.projects?.length ?? 0) > 0

  const externalUrlSet = !!settings?.external_url

  const notificationsConfigured =
    Array.isArray(providersData) &&
    providersData.some((p) => p.enabled)

  const hasDatabase = (servicesData?.length ?? 0) > 0

  const backupsConfigured = (backupSchedulesData?.length ?? 0) > 0

  const dnsProviderConnected = (dnsProvidersData?.length ?? 0) > 0

  // The initial admin always exists, so "team invited" means more than one
  // active user.
  const teamInvited = (usersData?.length ?? 0) > 1

  const completed = [
    gitConnected,
    wildcardDomainReady,
    hasProject,
    externalUrlSet,
    notificationsConfigured,
    hasDatabase,
    backupsConfigured,
    dnsProviderConnected,
    teamInvited,
  ].filter(Boolean).length

  return {
    gitConnected,
    wildcardDomainReady,
    hasProject,
    externalUrlSet,
    notificationsConfigured,
    hasDatabase,
    backupsConfigured,
    dnsProviderConnected,
    teamInvited,
    isLoaded,
    completedCount: completed,
    totalCount: TOTAL,
  }
}
