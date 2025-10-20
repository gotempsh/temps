import { useState, useMemo, useEffect } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  listRepositoriesByConnectionOptions,
  syncRepositoriesMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { RepositoryResponse } from '@/api/client'
import { useDebounce } from '@/hooks/useDebounce'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
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
  GitBranch,
  Lock,
  Unlock,
  ChevronLeft,
  ChevronRight,
  RefreshCw,
  AlertCircle,
  CheckCircle2,
  FolderGit2,
  Calendar,
  Filter,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { TimeAgo } from '@/components/utils/TimeAgo'

interface RepositoryListProps {
  connectionId: number
  onRepositorySelect?: (repo: RepositoryResponse) => void
  selectedRepositoryId?: number | string
  showSelection?: boolean
  itemsPerPage?: number
  className?: string
  showHeader?: boolean
  compactMode?: boolean
}

type SortOption = 'name' | 'updated' | 'created'
type FilterOption = 'all' | 'public' | 'private'

export function RepositoryList({
  connectionId,
  onRepositorySelect,
  selectedRepositoryId,
  showSelection = false,
  itemsPerPage = 12,
  className,
  showHeader = true,
  compactMode = false,
}: RepositoryListProps) {
  const [searchQuery, setSearchQuery] = useState('')
  const [currentPage, setCurrentPage] = useState(1)
  const [sortBy, setSortBy] = useState<SortOption>('updated')
  const [filterBy, setFilterBy] = useState<FilterOption>('all')
  const queryClient = useQueryClient()

  // Debounce search query to avoid too many API calls
  const debouncedSearchQuery = useDebounce(searchQuery, 300)

  // Map our sort options to API sort fields
  const getSortField = (sort: SortOption) => {
    switch (sort) {
      case 'name':
        return 'name'
      case 'created':
        return 'created_at'
      case 'updated':
        return 'updated_at'
      default:
        return 'updated_at'
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
      },
    }),
    enabled: !!connectionId,
    retry: false,
  })

  // Sync repositories mutation
  const syncMutation = useMutation({
    ...syncRepositoriesMutation(),
    meta: {
      errorTitle: 'Failed to sync repositories',
    },
    onSuccess: () => {
      toast.success('Repositories synced successfully!')
      refetch()
      queryClient.invalidateQueries({
        queryKey: ['listRepositoriesByConnection'],
      })
    },
    onError: (error: any) => {
      if (error.detail) {
        toast.error('Failed to sync repositories', {
          description: error.detail,
        })
      } else {
        toast.error(
          `Failed to sync repositories: ${error?.message || 'Unknown error'}`,
          {
            description: error.detail,
          }
        )
      }
    },
  })

  const handleSyncRepositories = () => {
    syncMutation.mutate({
      path: { connection_id: connectionId },
    })
  }

  // Apply client-side visibility filter since API doesn't support it
  const processedRepositories = useMemo(() => {
    if (!repositories?.repositories) return []

    let filtered = [...repositories.repositories]

    // Apply visibility filter (client-side only)
    if (filterBy !== 'all') {
      filtered = filtered.filter((repo) =>
        filterBy === 'private' ? repo.private : !repo.private
      )
    }

    return filtered
  }, [repositories, filterBy])

  // Reset page when debounced search changes
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setCurrentPage(1)
  }, [debouncedSearchQuery])

  // Since we're using server-side pagination, we need to estimate total pages
  // If we got less than per_page items, we're on the last page
  const hasMorePages =
    repositories?.repositories &&
    repositories.repositories.length === itemsPerPage
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
        <div className="space-y-4">
          {/* Search and Filters */}
          <div className="flex flex-col sm:flex-row gap-3">
            <div className="relative flex-1">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
              <Input
                value={searchQuery}
                onChange={(e) => handleSearchChange(e.target.value)}
                placeholder="Search repositories..."
                className="pl-9 pr-10"
              />
              {searchQuery !== debouncedSearchQuery && (
                <div className="absolute right-3 top-1/2 -translate-y-1/2">
                  <RefreshCw className="h-4 w-4 animate-spin text-muted-foreground" />
                </div>
              )}
            </div>

            <div className="flex gap-2">
              <Select value={filterBy} onValueChange={handleFilterChange}>
                <SelectTrigger className="w-32">
                  <Filter className="h-4 w-4 mr-2" />
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">All repos</SelectItem>
                  <SelectItem value="public">Public only</SelectItem>
                  <SelectItem value="private">Private only</SelectItem>
                </SelectContent>
              </Select>

              <Select value={sortBy} onValueChange={handleSortChange}>
                <SelectTrigger className="w-36">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="updated">Last updated</SelectItem>
                  <SelectItem value="created">Recently created</SelectItem>
                  <SelectItem value="name">Name (A-Z)</SelectItem>
                </SelectContent>
              </Select>

              <Button
                variant="outline"
                size="icon"
                onClick={() => refetch()}
                disabled={isLoading}
              >
                <RefreshCw
                  className={cn('h-4 w-4', isLoading && 'animate-spin')}
                />
              </Button>
            </div>
          </div>

          {/* Results count */}
          <div className="flex items-center justify-between text-sm text-muted-foreground">
            <span>
              {processedRepositories.length === 0
                ? 'No repositories found'
                : searchQuery
                  ? `Found ${processedRepositories.length} matching repositories`
                  : `Showing ${processedRepositories.length} repositories`}
            </span>
            <span>
              Page {currentPage}
              {hasMorePages ? '+' : ''}
            </span>
          </div>
        </div>
      )}

      {/* Repository Grid */}
      {isLoading ? (
        <div
          className={cn(
            'grid gap-3',
            compactMode
              ? 'grid-cols-1'
              : 'grid-cols-1 md:grid-cols-2 xl:grid-cols-3'
          )}
        >
          {Array.from({ length: itemsPerPage }).map((_, i) => (
            <Card key={i} className="p-4">
              <Skeleton className="h-4 w-3/4 mb-2" />
              <Skeleton className="h-3 w-1/2 mb-3" />
              <Skeleton className="h-3 w-full" />
            </Card>
          ))}
        </div>
      ) : paginatedRepositories.length === 0 ? (
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
                disabled={syncMutation.isPending}
                className="gap-2"
              >
                <RefreshCw
                  className={`h-4 w-4 ${syncMutation.isPending ? 'animate-spin' : ''}`}
                />
                {syncMutation.isPending ? 'Syncing...' : 'Sync Repositories'}
              </Button>
            )}
          </div>
        </Card>
      ) : (
        <div
          className={cn(
            'grid gap-3',
            compactMode
              ? 'grid-cols-1'
              : 'grid-cols-1 md:grid-cols-2 xl:grid-cols-3'
          )}
        >
          {paginatedRepositories.map((repo) => (
            <Card
              key={repo.id}
              className={cn(
                'group relative cursor-pointer transition-all hover:shadow-lg',
                showSelection && selectedRepositoryId === repo.id
                  ? 'ring-2 ring-primary border-primary bg-primary/5'
                  : 'hover:border-primary/50',
                onRepositorySelect && 'cursor-pointer'
              )}
              onClick={() => handleRepositoryClick(repo)}
            >
              <CardHeader className="pb-3">
                <div className="space-y-2">
                  <div className="flex items-start justify-between gap-2">
                    <div className="flex items-center gap-2 flex-1 min-w-0">
                      <GitBranch className="h-4 w-4 text-primary flex-shrink-0" />
                      <div className="flex-1 min-w-0">
                        <CardTitle className="text-base font-semibold leading-tight">
                          {repo.name}
                        </CardTitle>
                        <CardDescription className="text-sm text-muted-foreground mt-0.5">
                          {repo.owner}
                        </CardDescription>
                      </div>
                    </div>
                    <div className="flex items-center gap-2 flex-shrink-0">
                      {showSelection && selectedRepositoryId === repo.id && (
                        <CheckCircle2 className="h-5 w-5 text-primary" />
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2">
                    {repo.private ? (
                      <Badge variant="secondary" className="text-xs">
                        <Lock className="h-3 w-3 mr-1" />
                        Private
                      </Badge>
                    ) : (
                      <Badge variant="outline" className="text-xs">
                        <Unlock className="h-3 w-3 mr-1" />
                        Public
                      </Badge>
                    )}
                  </div>
                </div>
              </CardHeader>

              <CardContent className="pt-0">
                <div className="space-y-2">
                  {repo.description && (
                    <p className="text-xs text-muted-foreground line-clamp-2">
                      {repo.description}
                    </p>
                  )}

                  <div className="flex items-center gap-4 text-xs text-muted-foreground">
                    {repo.language && (
                      <div className="flex items-center gap-1">
                        <div
                          className={cn(
                            'h-2 w-2 rounded-full',
                            getLanguageColor(repo.language)
                          )}
                        />
                        {repo.language}
                      </div>
                    )}

                    {/* Stars and forks not available in current API response */}
                  </div>

                  {repo.updated_at && (
                    <div className="flex items-center gap-1 text-xs text-muted-foreground">
                      <Calendar className="h-3 w-3" />
                      <span>Updated </span>
                      <TimeAgo date={repo.updated_at} className="" />
                    </div>
                  )}
                </div>
              </CardContent>
            </Card>
          ))}
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
            <ChevronLeft className="h-4 w-4" />
            Previous
          </Button>

          <span className="text-sm text-muted-foreground px-3">
            Page {currentPage}
          </span>

          <Button
            variant="outline"
            size="sm"
            onClick={() => setCurrentPage((p) => p + 1)}
            disabled={!hasMorePages}
          >
            Next
            <ChevronRight className="h-4 w-4" />
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
