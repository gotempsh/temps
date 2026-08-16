import { useState, useMemo, useEffect, useRef } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  listRepositoriesByConnectionOptions,
  syncRepositoriesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { RepositoryResponse } from '@/api/client'
import { useDebounce } from '@/hooks/useDebounce'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Alert, AlertDescription } from '@/components/ui/alert'

import { toast } from 'sonner'
import {
  Search,
  Lock,
  Unlock,
  ChevronLeft,
  ChevronRight,
  RefreshCw,
  AlertCircle,
  CheckCircle2,
  FolderGit2,
  Filter,
  ArrowRight,
  X,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { usePresets } from '@/contexts/PresetContext'
import { RepositoryAvatar } from './RepositoryAvatar'
import { hasNextRepositoryPage } from '@/lib/repository-pagination'
import { RepositoryPresetLogos } from './RepositoryPresetLogos'

interface RepositoryListProps {
  connectionId: number
  onRepositorySelect?: (repo: RepositoryResponse) => void
  selectedRepositoryId?: number | string
  showSelection?: boolean
  itemsPerPage?: number
  className?: string
  showHeader?: boolean
  compactMode?: boolean
  providerType?: string | null
}

type SortOption = 'name' | 'pushed' | 'created'
type FilterOption = 'all' | 'public' | 'private'

export function RepositoryList({
  connectionId,
  onRepositorySelect,
  selectedRepositoryId,
  showSelection = false,
  itemsPerPage = 20,
  className,
  showHeader = true,
  compactMode = false,
  providerType,
}: RepositoryListProps) {
  const [searchQuery, setSearchQuery] = useState('')
  const [currentPage, setCurrentPage] = useState(1)
  const [sortBy, setSortBy] = useState<SortOption>('pushed')
  const [filterBy, setFilterBy] = useState<FilterOption>('all')
  const [searchOpen, setSearchOpen] = useState(false)
  const queryClient = useQueryClient()
  const { getPresetBySlug } = usePresets()

  // Debounce search query to avoid too many API calls
  const debouncedSearchQuery = useDebounce(searchQuery, 300)

  // Map our sort options to API sort fields
  const getSortField = (sort: SortOption) => {
    switch (sort) {
      case 'name':
        return 'name'
      case 'created':
        return 'created_at'
      case 'pushed':
        return 'pushed_at'
      default:
        return 'pushed_at'
    }
  }

  // Fetch repositories with server-side filtering
  const {
    data: repositories,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...listRepositoriesByConnectionOptions({
      path: { connection_id: connectionId },
      query: {
        search: debouncedSearchQuery || undefined,
        sort: getSortField(sortBy),
        direction: 'desc',
        page: currentPage,
        per_page: itemsPerPage,
        private: filterBy === 'all' ? undefined : filterBy === 'private',
      },
    }),
    enabled: !!connectionId,
    retry: false,
  })

  // Track the connection's live sync state so the UI can explain that repos
  // are still being fetched instead of showing a misleading "No repositories
  // found" empty state. The backend flips `syncing` / `synced_repository_count`
  // on the connection row; we poll those fields every 2s while a sync is in
  // flight (mirrors GitProviderDetail.tsx) and stop once it settles.
  const { data: connectionsData } = useQuery({
    ...listConnectionsOptions(),
    enabled: !!connectionId,
    refetchInterval: (query) => {
      const conn = query.state.data?.connections?.find(
        (c) => c.id === connectionId
      )
      return conn?.syncing ? 2000 : false
    },
    refetchIntervalInBackground: false,
  })

  const connection = connectionsData?.connections?.find(
    (c) => c.id === connectionId
  )
  const isConnectionSyncing = connection?.syncing ?? false
  const syncedRepositoryCount = connection?.synced_repository_count ?? 0

  // When a background sync finishes (syncing flips true -> false), the newly
  // persisted repositories won't show up until the list query re-runs. Refetch
  // once on that edge so the page fills in without a manual refresh.
  const wasSyncingRef = useRef(isConnectionSyncing)
  useEffect(() => {
    if (wasSyncingRef.current && !isConnectionSyncing) {
      refetch()
    }
    wasSyncingRef.current = isConnectionSyncing
  }, [isConnectionSyncing, refetch])

  // Sync repositories mutation
  const syncMutation = useMutation({
    ...syncRepositoriesMutation(),
    meta: {
      errorTitle: 'Failed to sync repositories',
    },
    onSuccess: () => {
      // The sync now runs in the background: 202 means "started", not
      // "finished". Client-side progress tracking lives on the connection
      // row's `syncing` / `synced_repository_count` fields.
      toast.success('Repository sync started', {
        description: 'Repositories will appear here as they sync.',
      })
      queryClient.invalidateQueries({
        queryKey: ['listConnections'],
      })
      refetch()
      queryClient.invalidateQueries({
        queryKey: ['listRepositoriesByConnection'],
      })
    },
    onError: (error: any) => {
      // Server's drop-guard flips `syncing=false` on failure, so refresh
      // the UI to avoid a stale spinner even on the error path.
      queryClient.invalidateQueries({ queryKey: ['listConnections'] })

      // RFC 7807 Problem Details — title + detail are where the useful
      // context lives (e.g. 504 SyncTimeout, 409 SyncInProgress).
      const title = error?.title || 'Failed to start repository sync'
      const description =
        error?.detail || error?.message || 'Unknown error. Try again.'
      toast.error(title, { description })
    },
  })

  const handleSyncRepositories = () => {
    syncMutation.mutate({
      path: { connection_id: connectionId },
    })
  }

  const processedRepositories = useMemo(() => {
    if (!repositories?.repositories) return []
    return repositories.repositories
  }, [repositories])

  // Reset page when debounced search changes
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCurrentPage(1)
  }, [debouncedSearchQuery])

  const totalCount = repositories?.total_count ?? 0
  const hasMorePages = hasNextRepositoryPage(
    totalCount,
    currentPage,
    itemsPerPage
  )
  const paginatedRepositories = processedRepositories

  // Handle search change without resetting page immediately
  const handleSearchChange = (value: string) => {
    setSearchQuery(value)
    // Page will reset when debounced value changes
  }

  const handleSortChange = (value: SortOption) => {
    setSortBy(value)
    setCurrentPage(1)
  }

  const handleFilterChange = (value: FilterOption) => {
    setFilterBy(value)
    setCurrentPage(1)
  }

  const handleRepositoryClick = (repo: RepositoryResponse) => {
    if (onRepositorySelect) {
      onRepositorySelect(repo)
    }
  }

  if (error) {
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" />
        <AlertDescription>
          Failed to load repositories. Please try again.
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className={cn('space-y-4', className)}>
      {showHeader && (
        <div className="space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div className="text-base/6 text-muted-foreground sm:text-sm/5">
              {totalCount === 0
                ? 'No repositories found'
                : searchQuery
                  ? `${totalCount} matching`
                  : `${totalCount} repositories`}
              <span aria-hidden="true" className="px-1.5 opacity-40">
                ·
              </span>
              <span className="tabular-nums">Page {currentPage}</span>
            </div>

            <div className="flex w-full flex-wrap items-center gap-2 sm:w-auto">
              {compactMode && (
                <Button
                  type="button"
                  variant={searchOpen ? 'secondary' : 'outline'}
                  size="sm"
                  onClick={() => setSearchOpen((open) => !open)}
                  className="gap-1.5"
                  aria-expanded={searchOpen}
                >
                  <Search className="size-4 shrink-0" />
                  Search
                </Button>
              )}
              <Select value={filterBy} onValueChange={handleFilterChange}>
                <SelectTrigger className="w-[140px] max-sm:flex-1">
                  <Filter className="mr-2 size-4 shrink-0" />
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All repos</SelectItem>
                  <SelectItem value="public">Public only</SelectItem>
                  <SelectItem value="private">Private only</SelectItem>
                </SelectContent>
              </Select>

              <Select value={sortBy} onValueChange={handleSortChange}>
                <SelectTrigger className="w-[160px] max-sm:flex-1">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="pushed">Recently pushed</SelectItem>
                  <SelectItem value="created">Recently created</SelectItem>
                  <SelectItem value="name">Name (A-Z)</SelectItem>
                </SelectContent>
              </Select>

              <Button
                type="button"
                variant="outline"
                size="sm"
                className="relative shrink-0 gap-1.5"
                onClick={handleSyncRepositories}
                disabled={syncMutation.isPending || isConnectionSyncing}
                aria-label="Sync repositories from provider"
              >
                <span
                  className="pointer-fine:hidden absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-1/2"
                  aria-hidden="true"
                />
                <RefreshCw
                  className={cn(
                    'size-4 shrink-0',
                    (syncMutation.isPending || isConnectionSyncing) &&
                      'animate-spin'
                  )}
                />
                {syncMutation.isPending || isConnectionSyncing
                  ? 'Syncing…'
                  : 'Sync repositories'}
              </Button>
            </div>
          </div>

          {(!compactMode || searchOpen) && (
            <div className="relative">
              <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 shrink-0 stroke-muted-foreground" />
              <Input
                name="repository-search"
                aria-label="Search repositories"
                value={searchQuery}
                onChange={(e) => handleSearchChange(e.target.value)}
                placeholder="Search repositories…"
                className="pr-10 pl-9 max-sm:text-base/6"
                autoFocus={compactMode}
              />
              {searchQuery !== debouncedSearchQuery ? (
                <RefreshCw className="absolute top-1/2 right-3 size-4 -translate-y-1/2 animate-spin shrink-0 stroke-muted-foreground" />
              ) : (
                compactMode && (
                  <button
                    type="button"
                    onClick={() => {
                      setSearchQuery('')
                      setSearchOpen(false)
                    }}
                    className="absolute top-1/2 right-2 flex size-8 -translate-y-1/2 items-center justify-center rounded-md hover:bg-muted focus-visible:outline-2 focus-visible:outline-ring"
                    aria-label="Close repository search"
                  >
                    <span
                      className="pointer-fine:hidden absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-1/2"
                      aria-hidden="true"
                    />
                    <X className="size-4 shrink-0 stroke-muted-foreground" />
                  </button>
                )
              )}
            </div>
          )}
        </div>
      )}

      {/* Live sync banner — the connection is still pulling repositories from
          the provider. Shown above the list so it's visible even when some
          repos have already streamed in. */}
      {isConnectionSyncing && (
        <Alert>
          <RefreshCw className="h-4 w-4 animate-spin" />
          <AlertDescription>
            {syncedRepositoryCount > 0
              ? `Syncing repositories… ${syncedRepositoryCount.toLocaleString()} found so far. New repositories will appear automatically.`
              : 'Syncing repositories from this connection… This usually takes a few seconds.'}
          </AlertDescription>
        </Alert>
      )}

      {/* Repository Grid */}
      {isLoading ? (
        <div
          className={cn(
            'grid',
            compactMode
              ? 'grid-cols-1 gap-2 lg:grid-cols-2 xl:grid-cols-3'
              : 'grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3'
          )}
        >
          {Array.from({ length: itemsPerPage }).map((_, i) => (
            <div
              key={i}
              className={cn(
                'border bg-card',
                compactMode ? 'rounded-lg p-2.5' : 'rounded-xl p-4'
              )}
            >
              <div className="flex items-center gap-3">
                <Skeleton className="size-9 shrink-0 rounded-md" />
                <div className="flex-1 space-y-1.5">
                  <Skeleton className="h-3 w-2/3" />
                  <Skeleton className="h-2.5 w-1/3" />
                </div>
              </div>
              {!compactMode && (
                <>
                  <Skeleton className="mt-3 h-4 w-24 rounded-full" />
                  <Skeleton className="mt-3 h-2.5 w-full" />
                </>
              )}
            </div>
          ))}
        </div>
      ) : paginatedRepositories.length === 0 ? (
        isConnectionSyncing && !searchQuery ? (
          // A sync is already running and nothing has landed yet — explain the
          // wait instead of offering a redundant "Sync Repositories" button
          // (which would just hit a SyncInProgress conflict). The 2s poll above
          // refreshes the list automatically as repos stream in.
          <Card className="p-12">
            <div className="flex flex-col items-center justify-center text-center space-y-4">
              <RefreshCw className="h-12 w-12 text-muted-foreground animate-spin" />
              <div>
                <h3 className="font-semibold">Syncing repositories…</h3>
                <p className="text-sm text-muted-foreground mt-1">
                  {syncedRepositoryCount > 0
                    ? `${syncedRepositoryCount.toLocaleString()} found so far — they'll appear here in a moment.`
                    : 'Fetching repositories from this connection. This usually takes a few seconds.'}
                </p>
              </div>
            </div>
          </Card>
        ) : (
          <Card className="p-12">
            <div className="flex flex-col items-center justify-center text-center space-y-4">
              <FolderGit2 className="h-12 w-12 text-muted-foreground" />
              <div>
                <h3 className="font-semibold">No repositories found</h3>
                <p className="text-sm text-muted-foreground mt-1">
                  {searchQuery
                    ? 'Try adjusting your search or filters'
                    : 'No repositories available from this connection'}
                </p>
              </div>
              {!searchQuery && (
                <Button
                  onClick={handleSyncRepositories}
                  disabled={syncMutation.isPending || isConnectionSyncing}
                  className="gap-2"
                >
                  <RefreshCw
                    className={`h-4 w-4 ${syncMutation.isPending || isConnectionSyncing ? 'animate-spin' : ''}`}
                  />
                  {syncMutation.isPending || isConnectionSyncing
                    ? 'Syncing...'
                    : 'Sync Repositories'}
                </Button>
              )}
            </div>
          </Card>
        )
      ) : (
        <div
          className={cn(
            'grid',
            compactMode
              ? 'grid-cols-1 gap-2 lg:grid-cols-2 xl:grid-cols-3'
              : 'grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3'
          )}
        >
          {paginatedRepositories.map((repo) => {
            const isSelected = showSelection && selectedRepositoryId === repo.id
            const interactive = !!onRepositorySelect
            if (compactMode) {
              return (
                <div
                  key={repo.id}
                  className={cn(
                    'group flex min-w-0 items-center rounded-lg border bg-card',
                    interactive &&
                      !isSelected &&
                      'hover:border-primary/40 hover:bg-accent/40',
                    isSelected &&
                      'border-primary bg-primary/5 ring-1 ring-primary'
                  )}
                >
                  <button
                    type="button"
                    onClick={() => handleRepositoryClick(repo)}
                    disabled={!interactive}
                    aria-pressed={isSelected}
                    className={cn(
                      'flex min-w-0 flex-1 items-center gap-3 p-2.5 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                      !interactive && 'cursor-default'
                    )}
                  >
                    <RepositoryAvatar
                      repository={repo}
                      providerType={providerType}
                    />
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-base font-semibold sm:text-sm">
                        {repo.name}
                      </p>
                      <div className="flex min-w-0 items-center gap-1.5 text-base/6 text-muted-foreground sm:text-sm/5">
                        <span className="truncate">{repo.owner}</span>
                        <span
                          aria-hidden="true"
                          className="shrink-0 opacity-40"
                        >
                          ·
                        </span>
                        <span className="shrink-0">
                          {repo.private ? 'Private' : 'Public'}
                        </span>
                        {repo.language && (
                          <>
                            <span
                              aria-hidden="true"
                              className="shrink-0 opacity-40"
                            >
                              ·
                            </span>
                            <span className="flex min-w-0 items-center gap-1.5">
                              <span
                                className={cn(
                                  'size-2 shrink-0 rounded-full',
                                  getLanguageColor(repo.language)
                                )}
                              />
                              <span className="truncate">{repo.language}</span>
                            </span>
                          </>
                        )}
                      </div>
                    </div>
                    {isSelected ? (
                      <CheckCircle2 className="size-5 shrink-0 stroke-primary sm:size-4" />
                    ) : (
                      interactive && (
                        <ArrowRight className="size-5 shrink-0 stroke-muted-foreground opacity-0 transition-transform group-hover:translate-x-0.5 group-hover:opacity-100 sm:size-4" />
                      )
                    )}
                  </button>
                  <div className="shrink-0 pr-1.5">
                    <RepositoryPresetLogos
                      repository={repo}
                      automaticDetectionEnabled={
                        connection?.has_authenticated_credentials ?? false
                      }
                    />
                  </div>
                </div>
              )
            }
            return (
              <button
                key={repo.id}
                type="button"
                onClick={() => handleRepositoryClick(repo)}
                disabled={!interactive}
                aria-pressed={isSelected}
                className={cn(
                  'group flex flex-col rounded-lg border bg-card p-4 text-left',
                  'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
                  interactive &&
                    !isSelected &&
                    'hover:border-primary/40 hover:bg-accent/40',
                  isSelected &&
                    'border-primary bg-primary/5 ring-1 ring-primary',
                  !interactive && 'cursor-default'
                )}
              >
                <>
                  {/* Title row: repository identity and selection affordance. */}
                  <div className="flex items-start gap-3">
                    <RepositoryAvatar
                      repository={repo}
                      providerType={providerType}
                    />
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-base font-semibold sm:text-sm">
                        {repo.name}
                      </p>
                      <p className="truncate text-base/6 text-muted-foreground sm:text-sm/5">
                        {repo.owner}
                      </p>
                    </div>
                    {isSelected ? (
                      <CheckCircle2 className="size-5 shrink-0 stroke-primary sm:size-4" />
                    ) : (
                      interactive && (
                        <ArrowRight className="size-5 shrink-0 stroke-muted-foreground opacity-0 transition-transform group-hover:translate-x-0.5 group-hover:opacity-100 sm:size-4" />
                      )
                    )}
                  </div>

                  {/* Badges: visibility + detected framework presets */}
                  <div className="mt-3 flex flex-wrap items-center gap-1.5">
                    <span
                      className={cn(
                        'inline-flex items-center gap-1 rounded-md border py-1 pr-2 pl-1 text-base/6 sm:text-sm/5',
                        repo.private
                          ? 'bg-muted/60 text-muted-foreground'
                          : 'bg-background text-muted-foreground'
                      )}
                    >
                      {repo.private ? (
                        <Lock className="size-5 shrink-0 sm:size-4" />
                      ) : (
                        <Unlock className="size-5 shrink-0 sm:size-4" />
                      )}
                      {repo.private ? 'Private' : 'Public'}
                    </span>
                    {repo.preset?.slice(0, 3).map((presetItem, index) => {
                      const presetInfo = getPresetBySlug(presetItem.preset)
                      const iconUrl =
                        presetInfo?.icon_url || '/presets/custom.svg'
                      return (
                        <span
                          key={index}
                          className="inline-flex items-center gap-1 rounded-md border bg-background px-1.5 py-0.5 text-[11px] text-foreground/80"
                        >
                          <img
                            src={iconUrl}
                            alt=""
                            aria-hidden
                            className="size-3 object-contain"
                            onError={(e) => {
                              e.currentTarget.src = '/presets/custom.svg'
                            }}
                          />
                          {presetItem.presetLabel}
                          {presetItem.path !== './' && (
                            <span className="opacity-60">
                              ({presetItem.path})
                            </span>
                          )}
                        </span>
                      )
                    })}
                    {repo.preset && repo.preset.length > 3 && (
                      <span className="inline-flex items-center rounded-md border bg-muted/60 px-1.5 py-0.5 text-[11px] text-muted-foreground">
                        +{repo.preset.length - 3} more
                      </span>
                    )}
                  </div>

                  {repo.description && (
                    <p className="mt-2 line-clamp-1 text-base/6 text-muted-foreground sm:text-sm/5">
                      {repo.description}
                    </p>
                  )}

                  <div className="mt-auto flex items-center gap-2 pt-3 text-base/6 text-muted-foreground sm:text-sm/5">
                    {repo.pushed_at && (
                      <span className="inline-flex items-center gap-1">
                        Pushed <TimeAgo date={repo.pushed_at} />
                      </span>
                    )}
                    {repo.language && repo.pushed_at && (
                      <span className="text-muted-foreground/40">•</span>
                    )}
                    {repo.language && (
                      <span className="inline-flex items-center gap-1.5">
                        <span
                          className={cn(
                            'size-2 rounded-full',
                            getLanguageColor(repo.language)
                          )}
                        />
                        {repo.language}
                      </span>
                    )}
                  </div>
                </>
              </button>
            )
          })}
        </div>
      )}

      {/* Pagination */}
      {(currentPage > 1 || hasMorePages) && (
        <div className="flex items-center justify-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
            disabled={currentPage === 1}
          >
            <ChevronLeft className="h-4 w-4 sm:mr-1" />
            <span className="hidden sm:inline">Previous</span>
          </Button>

          <span className="px-2 text-sm tabular-nums text-muted-foreground">
            Page {currentPage}
          </span>

          <Button
            variant="outline"
            size="sm"
            onClick={() => setCurrentPage((p) => p + 1)}
            disabled={!hasMorePages}
          >
            <span className="hidden sm:inline">Next</span>
            <ChevronRight className="h-4 w-4 sm:ml-1" />
          </Button>
        </div>
      )}
    </div>
  )
}

// Helper function to get language color
function getLanguageColor(language: string): string {
  const colors: Record<string, string> = {
    JavaScript: 'bg-yellow-400',
    TypeScript: 'bg-blue-600',
    Python: 'bg-blue-500',
    Java: 'bg-orange-600',
    Go: 'bg-cyan-600',
    Ruby: 'bg-red-600',
    PHP: 'bg-purple-600',
    'C++': 'bg-pink-600',
    C: 'bg-gray-600',
    Swift: 'bg-orange-500',
    Kotlin: 'bg-purple-500',
    Rust: 'bg-orange-700',
    Shell: 'bg-green-600',
    HTML: 'bg-red-500',
    CSS: 'bg-blue-400',
    Vue: 'bg-green-500',
    React: 'bg-cyan-400',
  }

  return colors[language] || 'bg-gray-400'
}
