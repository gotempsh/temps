// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { keepPreviousData } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  AlertTriangle,
  ArrowLeft,
  ExternalLink,
  GitBranch,
  HeartPulse,
  Lock,
  RefreshCw,
  Rocket,
  Search,
  Unlock,
} from 'lucide-react'
import {
  Button,
  Callout,
  CopyAction,
  DataTable,
  Detail,
  PageState,
  Status,
  fmtDateTime,
  fmtRelativeTime,
  useUrlState,
  type DataTableColumn,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import {
  getConnectionOptions,
  getConnectionQueryKey,
  getGitProviderOptions,
  getProviderConnectionsQueryKey,
  listConnectionsQueryKey,
  listRepositoriesByConnectionOptions,
  listRepositoriesByConnectionQueryKey,
  runConnectionHealthCheckMutation,
  syncRepositoriesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ConnectionResponse,
  ProviderResponse,
  RepositoryResponse,
} from '@/api/client/types.gen'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useDebounce } from '@/hooks/useDebounce'
import { usePageTitle } from '@/hooks/usePageTitle'
import { getRepositoryUrl } from '@/lib/repository-url'
import {
  REPOSITORY_PAGE_SIZE_OPTIONS,
  REPOSITORY_SORTS,
  parseRepositoryListState,
  providerDisplayName,
  authMethodDisplayName,
} from '@/lib/git-connection'

/**
 * One git provider connection: who it authenticates as, whether it works,
 * and every repository it has synced — paginated, searchable, and each one a
 * click away from being deployed.
 */
export default function GitConnectionDetail() {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { setBreadcrumbs } = useBreadcrumbs()
  const { id, connectionId } = useParams<{
    id: string
    connectionId: string
  }>()
  const providerId = Number(id)
  const connection_id = Number(connectionId)
  const validIds =
    Number.isInteger(providerId) && Number.isInteger(connection_id)

  const url = useUrlState<'page' | 'per_page' | 'q' | 'visibility' | 'sort'>()
  const list = parseRepositoryListState(url.state)
  const [searchInput, setSearchInput] = useState(list.search)
  // A sync runs detached from the request that starts it. Snapshot the
  // server's own outcome timestamps when one is requested, and treat the sync
  // as finished once either moves: comparing server values to server values
  // is immune to browser clock skew.
  const [pendingSync, setPendingSync] = useState<{
    syncedAt: string | null
    errorAt: string | null
    startedAt: number
  } | null>(null)
  const debouncedSearch = useDebounce(searchInput, 300)

  // Typing a search restarts at page 1; the URL keeps the committed term.
  useEffect(() => {
    const term = debouncedSearch.trim()
    if (term !== list.search)
      url.patch({ q: term || undefined, page: undefined })
  }, [debouncedSearch]) // eslint-disable-line react-hooks/exhaustive-deps

  // The URL can change underneath the box (a followed `?q=` link, back/forward).
  // Adopt it, so the next keystroke edits the term actually being shown rather
  // than writing a stale one back. Our own debounced commits already match.
  useEffect(() => {
    if (list.search !== debouncedSearch.trim()) setSearchInput(list.search)
  }, [list.search]) // eslint-disable-line react-hooks/exhaustive-deps

  const providerQuery = useQuery({
    ...getGitProviderOptions({ path: { provider_id: providerId } }),
    retry: false,
    enabled: validIds,
  })
  const provider = providerQuery.data

  const connectionQuery = useQuery({
    ...getConnectionOptions({ path: { connection_id } }),
    retry: false,
    enabled: validIds,
    // A running sync advances `synced_repository_count` live; stop polling
    // the moment it finishes so idle tabs stay quiet.
    refetchInterval: (query) =>
      query.state.data?.syncing || pendingSync ? 2000 : false,
    refetchIntervalInBackground: false,
  })
  const connection = connectionQuery.data

  const sortSpec = REPOSITORY_SORTS[list.sort]
  const repositoriesQuery = useQuery({
    ...listRepositoriesByConnectionOptions({
      path: { connection_id },
      query: {
        page: list.page,
        per_page: list.perPage,
        sort: sortSpec.sort,
        direction: sortSpec.direction,
        search: list.search || undefined,
        private:
          list.visibility === 'all' ? undefined : list.visibility === 'private',
      },
    }),
    enabled: validIds && !!connection,
    placeholderData: keepPreviousData,
  })

  // A shared or stale `?page=` can point past the last page; land on the last
  // one that has rows instead of an empty table with no way back.
  const lastPage = repositoriesQuery.data
    ? Math.max(1, Math.ceil(repositoriesQuery.data.total_count / list.perPage))
    : undefined
  useEffect(() => {
    if (repositoriesQuery.isPlaceholderData || lastPage === undefined) return
    if (list.page > lastPage)
      url.patch({ page: lastPage === 1 ? undefined : lastPage })
  }, [lastPage, list.page, repositoriesQuery.isPlaceholderData]) // eslint-disable-line react-hooks/exhaustive-deps

  // When a sync finishes, the repository list it produced is new data.
  const wasSyncing = useRef(false)
  useEffect(() => {
    if (wasSyncing.current && connection && !connection.syncing) {
      queryClient.invalidateQueries({
        queryKey: listRepositoriesByConnectionQueryKey({
          path: { connection_id },
        }).slice(0, 1),
      })
    }
    wasSyncing.current = !!connection?.syncing
  }, [connection, connection_id, queryClient])

  // Give up waiting on a requested sync after two minutes, whether or not the
  // server still reports it running: a hung sync must not hold the page in
  // "Syncing…" until the backend's own deadline.
  useEffect(() => {
    if (!pendingSync) return
    const remaining = SYNC_WAIT_LIMIT_MS - (Date.now() - pendingSync.startedAt)
    const timer = setTimeout(
      () => {
        toast.error('No sync result after 2 minutes', {
          description:
            'The sync may still be running on the server. Refresh the page to check again.',
        })
        setPendingSync(null)
      },
      Math.max(0, remaining)
    )
    return () => clearTimeout(timer)
  }, [pendingSync])

  useEffect(() => {
    if (!pendingSync || !connection || connection.syncing) return
    const failed =
      (connection.last_sync_error_at ?? null) !== pendingSync.errorAt &&
      !!connection.last_sync_error
    const succeeded =
      (connection.last_synced_at ?? null) !== pendingSync.syncedAt
    if (failed) {
      toast.error('Repository sync failed', {
        description: connection.last_sync_error ?? undefined,
      })
    } else if (succeeded) {
      toast.success('Repositories synced', {
        description: `${connection.synced_repository_count.toLocaleString()} repositories from ${connection.account_name}.`,
      })
      queryClient.invalidateQueries({
        queryKey: listRepositoriesByConnectionQueryKey({
          path: { connection_id },
        }).slice(0, 1),
      })
    } else {
      return
    }
    setPendingSync(null)
  }, [pendingSync, connection, connection_id, queryClient])

  const refreshConnection = () => {
    queryClient.invalidateQueries({
      queryKey: getConnectionQueryKey({ path: { connection_id } }),
    })
    queryClient.invalidateQueries({
      queryKey: getProviderConnectionsQueryKey({
        path: { provider_id: providerId },
      }),
    })
    queryClient.invalidateQueries({ queryKey: listConnectionsQueryKey({}) })
  }

  const syncMutation = useMutation({
    ...syncRepositoriesMutation(),
    onMutate: () => {
      setPendingSync({
        syncedAt: connection?.last_synced_at ?? null,
        errorAt: connection?.last_sync_error_at ?? null,
        startedAt: Date.now(),
      })
    },
    onSuccess: () => {
      toast.message('Repository sync started', {
        description: 'The list updates as repositories arrive.',
      })
      refreshConnection()
    },
    onError: (error) => {
      setPendingSync(null)
      toast.error('Failed to start repository sync', {
        description: problemDetail(error),
      })
    },
  })

  const healthMutation = useMutation({
    ...runConnectionHealthCheckMutation(),
    onSuccess: (data) => {
      if (data.health_status === 'healthy')
        toast.success(`Connection "${data.account_name}" is healthy`)
      else
        toast.error(
          `Connection "${data.account_name}" is ${data.health_status}`,
          {
            description: data.health_message ?? undefined,
          }
        )
      refreshConnection()
    },
    onError: (error) => {
      toast.error('Health check failed', {
        description: problemDetail(error),
      })
    },
  })

  useEffect(() => {
    if (!provider || !connection) return
    setBreadcrumbs([
      { label: 'Git Providers', href: '/git-providers' },
      { label: provider.name, href: `/git-providers/${provider.id}` },
      { label: connection.account_name },
    ])
  }, [provider, connection, setBreadcrumbs])

  usePageTitle(
    connection && provider
      ? `${connection.account_name} - ${provider.name} - Git Connection`
      : 'Git Connection'
  )

  const backAction = (
    <Button
      variant="ghost"
      size="sm"
      onClick={() => navigate(`/git-providers/${id}`)}
    >
      <ArrowLeft className="mr-2 h-4 w-4" />
      Back
    </Button>
  )

  if (providerQuery.isLoading || connectionQuery.isLoading) {
    return (
      <Detail
        title={<Skeleton className="h-7 w-48" />}
        actions={backAction}
        facts={[0, 1, 2, 3, 4].map(() => ({
          label: <Skeleton className="h-3 w-16" />,
          value: <Skeleton className="h-4 w-24" />,
        }))}
        main={<Skeleton className="h-64 w-full" />}
      />
    )
  }

  // The connection must belong to the provider in the URL; anything else is
  // a stale or hand-edited link, not a page to render.
  if (
    !validIds ||
    connectionQuery.error ||
    !connection ||
    providerQuery.error ||
    !provider ||
    connection.provider_id !== provider.id
  ) {
    return (
      <PageState
        variant="failed"
        icon={AlertTriangle}
        title="Git Connection Not Found"
        description="This connection doesn't exist under this provider, or you don't have access to it."
        action={backAction}
      />
    )
  }

  const verdict: { tone: StatusTone; label: string } = !connection.is_active
    ? { tone: 'idle', label: 'Inactive' }
    : connection.is_expired
      ? { tone: 'error', label: 'Token expired' }
      : connection.health_status === 'unhealthy'
        ? { tone: 'error', label: 'Unhealthy' }
        : connection.syncing
          ? { tone: 'running', label: 'Syncing' }
          : { tone: 'ok', label: 'Active' }

  const repositories = repositoriesQuery.data
  const total = repositories?.total_count ?? 0
  const totalPages = Math.max(1, Math.ceil(total / list.perPage))

  const facts: DetailFact[] = [
    {
      label: 'Provider',
      value: (
        <Link
          to={`/git-providers/${provider.id}`}
          className="inline-flex items-center gap-1.5 hover:underline"
        >
          <ProviderLogo
            providerType={provider.provider_type}
            className="h-4 w-4 shrink-0"
          />
          {provider.name}
        </Link>
      ),
    },
    { label: 'Account type', value: connection.account_type || 'Unknown' },
    {
      label: 'Health',
      value: <HealthValue connection={connection} />,
    },
    {
      label: 'Last synced',
      value: <LastSyncedValue connection={connection} />,
    },
    {
      label: 'Repositories',
      value: repositoriesQuery.isLoading ? (
        <Skeleton className="h-4 w-10" />
      ) : list.search || list.visibility !== 'all' ? (
        `${total.toLocaleString()} matching`
      ) : (
        total.toLocaleString()
      ),
    },
    { label: 'Connected', value: fmtRelativeTime(connection.created_at) },
  ]

  const columns: DataTableColumn<RepositoryResponse>[] = [
    {
      key: 'repository',
      header: 'Repository',
      render: (repo) => <RepositoryName repo={repo} />,
    },
    {
      key: 'visibility',
      header: 'Visibility',
      className: 'hidden sm:table-cell',
      render: (repo) =>
        repo.private ? (
          <Badge variant="outline" className="gap-1">
            <Lock className="h-3 w-3" />
            Private
          </Badge>
        ) : (
          <Badge variant="outline" className="gap-1">
            <Unlock className="h-3 w-3" />
            Public
          </Badge>
        ),
    },
    {
      key: 'language',
      header: 'Language',
      className: 'hidden md:table-cell',
      render: (repo) => (
        <span className="text-muted-foreground">{repo.language || '—'}</span>
      ),
    },
    {
      key: 'branch',
      header: 'Default branch',
      className: 'hidden lg:table-cell',
      render: (repo) => (
        <span className="inline-flex items-center gap-1 font-mono text-xs">
          <GitBranch className="h-3 w-3 text-muted-foreground" />
          {repo.default_branch}
        </span>
      ),
    },
    {
      key: 'pushed',
      header: 'Last push',
      className: 'hidden md:table-cell',
      render: (repo) => (
        <span
          className="text-muted-foreground"
          title={fmtDateTime(repo.pushed_at)}
        >
          {fmtRelativeTime(repo.pushed_at)}
        </span>
      ),
    },
    {
      key: 'actions',
      header: <span className="sr-only">Actions</span>,
      className: 'w-0 text-right',
      render: (repo) => (
        <Button asChild size="sm" variant="outline" className="gap-1.5">
          <Link to={`/projects/import/${repo.id}`}>
            <Rocket className="h-3.5 w-3.5" />
            Deploy
          </Link>
        </Button>
      ),
    },
  ]

  const filtered = !!list.search || list.visibility !== 'all'
  const repositoriesPanel = (
    <section aria-labelledby="repositories-heading" className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
        <h2
          id="repositories-heading"
          className="mr-auto text-base font-semibold"
        >
          Repositories
        </h2>
        <div className="relative w-full sm:w-64">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={searchInput}
            onChange={(event) => setSearchInput(event.target.value)}
            placeholder="Search repositories…"
            aria-label="Search repositories"
            className="pl-8"
          />
        </div>
        <Select
          value={list.visibility}
          onValueChange={(visibility) =>
            url.patch({
              visibility: visibility === 'all' ? undefined : visibility,
              page: undefined,
            })
          }
        >
          <SelectTrigger
            className="w-full sm:w-[140px]"
            aria-label="Filter by visibility"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All repositories</SelectItem>
            <SelectItem value="public">Public</SelectItem>
            <SelectItem value="private">Private</SelectItem>
          </SelectContent>
        </Select>
        <Select
          value={list.sort}
          onValueChange={(sort) =>
            url.patch({
              sort: sort === 'pushed' ? undefined : sort,
              page: undefined,
            })
          }
        >
          <SelectTrigger className="w-full sm:w-[170px]" aria-label="Sort by">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {Object.entries(REPOSITORY_SORTS).map(([value, spec]) => (
              <SelectItem key={value} value={value}>
                {spec.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {repositoriesQuery.error ? (
        <Callout tone="error" title="Repositories could not be loaded">
          {problemDetail(repositoriesQuery.error) ?? 'The request failed.'}{' '}
          <Button
            variant="link"
            size="sm"
            className="h-auto p-0"
            onClick={() => void repositoriesQuery.refetch()}
          >
            Retry
          </Button>
        </Callout>
      ) : !repositoriesQuery.isLoading && total === 0 ? (
        <RepositoriesEmpty
          filtered={filtered}
          connection={connection}
          syncing={syncMutation.isPending}
          onSync={() =>
            syncMutation.mutate({ path: { connection_id: connection.id } })
          }
          onClearFilters={() => {
            setSearchInput('')
            url.clear(['q', 'visibility', 'page'])
          }}
        />
      ) : (
        <DataTable
          aria-label="Repositories"
          columns={columns}
          rows={repositories?.repositories ?? []}
          rowKey={(repo) => repo.id}
          isLoading={repositoriesQuery.isLoading}
          pagination={{
            page: list.page,
            pageSize: list.perPage,
            total,
            totalPages,
            pageSizeOptions: REPOSITORY_PAGE_SIZE_OPTIONS,
            ariaLabel: 'Repository list pagination',
            pageSizeAriaLabel: 'Repositories per page',
            onPageChange: (page) =>
              url.patch({ page: page === 1 ? undefined : page }),
            onPageSizeChange: (perPage) =>
              url.patch({ per_page: perPage, page: undefined }),
          }}
        />
      )}
    </section>
  )

  return (
    <Detail
      title={connection.account_name}
      verdict={<Status tone={verdict.tone} label={verdict.label} />}
      actions={
        <>
          {backAction}
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              healthMutation.mutate({ path: { connection_id: connection.id } })
            }
            disabled={healthMutation.isPending}
          >
            <HeartPulse
              className={`mr-2 h-4 w-4 ${healthMutation.isPending ? 'animate-pulse' : ''}`}
            />
            Check health
          </Button>
          <Button
            size="sm"
            onClick={() =>
              syncMutation.mutate({ path: { connection_id: connection.id } })
            }
            disabled={
              syncMutation.isPending || connection.syncing || !!pendingSync
            }
          >
            <RefreshCw
              className={`mr-2 h-4 w-4 ${syncMutation.isPending || connection.syncing || pendingSync ? 'animate-spin' : ''}`}
            />
            {connection.syncing || pendingSync
              ? 'Syncing…'
              : 'Sync repositories'}
          </Button>
        </>
      }
      facts={facts}
      main={
        <div className="space-y-4">
          <ConnectionProblems
            connection={connection}
            providerId={provider.id}
          />
          {connection.last_sync_error && !connection.syncing && (
            <Callout tone="error" title="The last repository sync failed">
              {connection.last_sync_error}
              {connection.last_sync_error_at &&
                ` (${fmtRelativeTime(connection.last_sync_error_at)})`}
              .{' '}
              <Button
                variant="link"
                size="sm"
                className="h-auto p-0"
                disabled={syncMutation.isPending || !!pendingSync}
                onClick={() =>
                  syncMutation.mutate({
                    path: { connection_id: connection.id },
                  })
                }
              >
                Retry sync
              </Button>
            </Callout>
          )}
          {repositoriesPanel}
        </div>
      }
      aside={<ProviderPanel provider={provider} connection={connection} />}
    />
  )
}

const SYNC_WAIT_LIMIT_MS = 120_000

/** The server's explanation from a Problem Details error, when it gave one. */
function problemDetail(error: unknown): string | undefined {
  const problem = error as { detail?: string; title?: string; message?: string }
  return problem?.detail || problem?.title || problem?.message || undefined
}

function LastSyncedValue({ connection }: { connection: ConnectionResponse }) {
  if (connection.syncing)
    return (
      <>
        Syncing · {connection.synced_repository_count.toLocaleString()} so far
      </>
    )
  const failedAt = connection.last_sync_error_at
  const failedLast =
    !!connection.last_sync_error &&
    !!failedAt &&
    (!connection.last_synced_at ||
      new Date(failedAt) > new Date(connection.last_synced_at))
  if (failedLast)
    return (
      <span
        className="text-destructive"
        title={connection.last_sync_error ?? ''}
      >
        Failed · {fmtRelativeTime(failedAt)}
      </span>
    )
  return (
    <>
      {connection.last_synced_at
        ? fmtRelativeTime(connection.last_synced_at)
        : 'Never'}
    </>
  )
}

function HealthValue({ connection }: { connection: ConnectionResponse }) {
  const label =
    connection.health_status === 'healthy'
      ? 'Healthy'
      : connection.health_status === 'unhealthy'
        ? 'Unhealthy'
        : 'Not checked'
  return (
    <span
      title={
        connection.last_health_check_at
          ? `Last checked ${fmtDateTime(connection.last_health_check_at)}`
          : 'No health check has run yet'
      }
      className={
        connection.health_status === 'unhealthy'
          ? 'text-destructive'
          : connection.health_status === 'healthy'
            ? 'text-emerald-600 dark:text-emerald-400'
            : undefined
      }
    >
      {label}
      {connection.last_health_check_at && (
        <span className="text-muted-foreground">
          {' · '}
          {fmtRelativeTime(connection.last_health_check_at)}
        </span>
      )}
    </span>
  )
}

/** States that stop this connection from working, each with the way out. */
function ConnectionProblems({
  connection,
  providerId,
}: {
  connection: ConnectionResponse
  providerId: number
}) {
  if (!connection.is_active)
    return (
      <Callout tone="warning" title="This connection is inactive">
        Projects can't deploy from it and its repositories won't sync. Activate
        it from the{' '}
        <Link to={`/git-providers/${providerId}`} className="underline">
          provider's connections
        </Link>
        .
      </Callout>
    )
  if (connection.is_expired)
    return (
      <Callout tone="error" title="The access token has expired">
        Syncing and deployments fail until the token is replaced. Update it from
        the{' '}
        <Link to={`/git-providers/${providerId}`} className="underline">
          provider's connections
        </Link>
        .
      </Callout>
    )
  if (connection.health_status === 'unhealthy')
    return (
      <Callout tone="error" title="The last health check failed">
        {connection.health_message ??
          'The provider rejected a request made with this connection.'}
        {connection.consecutive_health_failures > 1 &&
          ` (${connection.consecutive_health_failures} checks in a row)`}
      </Callout>
    )
  if (!connection.has_authenticated_credentials)
    return (
      <Callout tone="warning" title="No credentials on this connection">
        Only public repositories can be read. Reconnect the account to sync
        private repositories.
      </Callout>
    )
  return null
}

function ProviderPanel({
  provider,
  connection,
}: {
  provider: ProviderResponse
  connection: ConnectionResponse
}) {
  const rows: Array<[string, React.ReactNode]> = [
    [
      'Type',
      <span className="inline-flex items-center gap-1.5">
        <ProviderLogo
          providerType={provider.provider_type}
          className="h-4 w-4 shrink-0"
        />
        {providerDisplayName(provider.provider_type)}
      </span>,
    ],
    ['Auth method', authMethodDisplayName(provider.auth_method)],
    [
      'Base URL',
      provider.base_url ? (
        <span className="inline-flex min-w-0 items-center gap-1">
          <span className="truncate font-mono text-xs">
            {provider.base_url}
          </span>
          <CopyAction value={provider.base_url} />
        </span>
      ) : (
        'Default'
      ),
    ],
    ['Provider status', provider.is_active ? 'Active' : 'Inactive'],
    ...(connection.installation_id
      ? ([
          [
            'Installation ID',
            <span className="font-mono text-xs">
              {connection.installation_id}
            </span>,
          ],
        ] as Array<[string, React.ReactNode]>)
      : []),
    [
      'Credentials',
      connection.has_authenticated_credentials ? 'Stored' : 'None',
    ],
    [
      'Connection ID',
      <span className="font-mono text-xs">{connection.id}</span>,
    ],
  ]
  return (
    <section
      aria-labelledby="provider-panel-heading"
      className="rounded-md border p-4"
    >
      <h2 id="provider-panel-heading" className="mb-3 text-sm font-semibold">
        Git provider
      </h2>
      <dl className="space-y-2.5 text-sm">
        {rows.map(([label, value]) => (
          <div key={label} className="flex items-start justify-between gap-4">
            <dt className="shrink-0 text-muted-foreground">{label}</dt>
            <dd className="min-w-0 text-right">{value}</dd>
          </div>
        ))}
      </dl>
      <Button asChild variant="link" size="sm" className="mt-3 h-auto p-0">
        <Link to={`/git-providers/${provider.id}`}>Provider settings</Link>
      </Button>
    </section>
  )
}

function RepositoryName({ repo }: { repo: RepositoryResponse }) {
  const href = getRepositoryUrl(repo)
  return (
    <div className="min-w-0">
      <div className="flex min-w-0 items-center gap-1.5">
        <span className="truncate font-medium">{repo.full_name}</span>
        {repo.private && (
          <Lock
            className="h-3 w-3 shrink-0 text-muted-foreground sm:hidden"
            aria-label="Private"
          />
        )}
        {href && (
          <a
            href={href}
            target="_blank"
            rel="noreferrer"
            className="shrink-0 text-muted-foreground hover:text-foreground"
            aria-label={`Open ${repo.full_name} on the provider`}
          >
            <ExternalLink className="h-3.5 w-3.5" />
          </a>
        )}
      </div>
      {repo.description && (
        <p className="truncate text-xs text-muted-foreground">
          {repo.description}
        </p>
      )}
    </div>
  )
}

function RepositoriesEmpty({
  filtered,
  connection,
  syncing,
  onSync,
  onClearFilters,
}: {
  filtered: boolean
  connection: ConnectionResponse
  syncing: boolean
  onSync: () => void
  onClearFilters: () => void
}) {
  if (filtered)
    return (
      <div className="rounded-md border border-dashed p-6 text-center text-sm">
        <p className="font-medium">No repositories match these filters</p>
        <Button
          variant="link"
          size="sm"
          className="mt-1 h-auto p-0"
          onClick={onClearFilters}
        >
          Clear filters
        </Button>
      </div>
    )
  return (
    <div className="rounded-md border border-dashed p-6 text-center text-sm">
      <p className="font-medium">
        {connection.syncing
          ? 'Syncing repositories…'
          : connection.last_sync_error
            ? 'No repositories yet — the last sync failed'
            : connection.last_synced_at
              ? 'The last sync found no repositories'
              : 'Repositories have not been synced yet'}
      </p>
      <p className="mx-auto mt-1 max-w-md text-muted-foreground">
        {connection.syncing
          ? 'They appear here as they arrive.'
          : `Sync pulls every repository ${connection.account_name} can access, so you can deploy any of them.`}
      </p>
      {!connection.syncing && (
        <Button size="sm" className="mt-3" onClick={onSync} disabled={syncing}>
          <RefreshCw
            className={`mr-2 h-4 w-4 ${syncing ? 'animate-spin' : ''}`}
          />
          Sync repositories
        </Button>
      )}
    </div>
  )
}
