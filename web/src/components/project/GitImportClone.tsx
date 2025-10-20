import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useQuery, useMutation } from '@tanstack/react-query'
import {
  listConnectionsOptions,
  listRepositoriesByConnectionOptions,
  syncRepositoriesMutation,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  createProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Card, CardHeader, CardTitle, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectTrigger,
  SelectValue,
  SelectItem,
  SelectContent,
} from '@/components/ui/select'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import FrameworkIcon from '@/components/project/FrameworkIcon'
import { ProjectConfigurator } from '@/components/project/ProjectConfigurator'
import type { RepositoryResponse } from '@/api/client/types.gen'
import { GitBranch, Search, ChevronLeft, ChevronRight } from 'lucide-react'
import { TimeAgo } from '@/components/utils/TimeAgo'
import Github from '@/icons/Github'
import { cn } from '@/lib/utils'
import { toast } from 'sonner'

interface GitImportCloneProps {
  mode?: 'navigation' | 'inline'
  onProjectCreated?: () => void
}

export function GitImportClone({
  mode = 'navigation',
  onProjectCreated,
}: GitImportCloneProps) {
  const [searchTerm, setSearchTerm] = useState('')
  const [selectedConnection, setSelectedConnection] = useState<
    string | undefined
  >()
  const [selectedRepository, setSelectedRepository] =
    useState<RepositoryResponse | null>(null)
  const [currentPage, setCurrentPage] = useState(1)
  const navigate = useNavigate()
  const [isInitialLoad, setIsInitialLoad] = useState(true)
  const perPage = 5

  const { data: connections } = useQuery({
    ...listConnectionsOptions(),
  })

  useEffect(() => {
    if (
      connections &&
      connections.connections.length > 0 &&
      !selectedConnection &&
      isInitialLoad
    ) {
      queueMicrotask(() => {
        setSelectedConnection(connections.connections[0].id.toString())
        setIsInitialLoad(false)
      })
    }
  }, [connections, selectedConnection, isInitialLoad])

  // Reset page when search term or connection changes
  useEffect(() => {
    queueMicrotask(() => {
      setCurrentPage(1)
    })
  }, [searchTerm, selectedConnection])

  const {
    data: repositories,
    isLoading,
    refetch: refetchRepositories,
  } = useQuery({
    ...listRepositoriesByConnectionOptions({
      path: {
        connection_id: selectedConnection ? parseInt(selectedConnection) : 0,
      },
      query: {
        search: searchTerm || undefined,
        sort: 'pushed_at',
        direction: 'desc',
        page: currentPage,
        per_page: perPage,
      },
    }),
    enabled: !!selectedConnection,
  })

  const totalPages = repositories?.total_count
    ? Math.ceil(repositories.total_count / perPage)
    : 0
  const hasNextPage = currentPage < totalPages
  const hasPrevPage = currentPage > 1

  const syncMutation = useMutation({
    ...syncRepositoriesMutation(),
    meta: {
      errorTitle: 'Failed to sync repositories',
    },
    onSuccess: () => {
      refetchRepositories()
    },
  })

  // Parse owner/repo from full_name
  const [owner, repo] = (selectedRepository?.full_name || '/').split('/')

  const { data: branches } = useQuery({
    ...getRepositoryBranchesOptions({
      path: {
        owner: owner || '',
        repo: repo || '',
      },
      query: {
        connection_id: Number(selectedConnection),
      },
    }),
    enabled: !!selectedRepository && !!selectedConnection && !!owner && !!repo,
  })

  const { data: presetData } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: {
        repository_id: selectedRepository?.id || 0,
      },
    }),
    enabled: !!selectedRepository && !!selectedRepository?.id,
  })

  const createProjectMutationM = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create project',
    },
    onSuccess: async (data) => {
      toast.success('Project created successfully')
      onProjectCreated?.()
      navigate(`/projects/${data.slug}?new=true`)
    },
  })

  const handleRepositoryClick = (repo: RepositoryResponse) => {
    if (mode === 'navigation') {
      // Navigation mode: navigate to import page
      navigate(
        `/projects/import/${repo.full_name}${selectedConnection ? `?connection_id=${selectedConnection}` : ''}`
      )
    } else {
      // Inline mode: show configurator
      setSelectedRepository(repo)
    }
  }

  // If in inline mode and repository is selected, show ProjectConfigurator
  if (mode === 'inline' && selectedRepository && selectedConnection) {
    return (
      <div className="space-y-6">
        <div className="flex items-center gap-4">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setSelectedRepository(null)}
          >
            <ChevronLeft className="h-4 w-4 mr-2" />
            Back to Repositories
          </Button>
        </div>

        <ProjectConfigurator
          repository={{
            id: selectedRepository.id,
            name: selectedRepository.name,
            owner: selectedRepository.owner || owner,
            full_name: selectedRepository.full_name,
            private: selectedRepository.private || false,
            default_branch:
              branches?.branches?.find((b: any) => b.is_default)?.name ||
              selectedRepository.default_branch ||
              'main',
            created_at:
              selectedRepository.created_at || new Date().toISOString(),
            pushed_at: selectedRepository.pushed_at || new Date().toISOString(),
            updated_at:
              selectedRepository.updated_at || new Date().toISOString(),
          }}
          connectionId={Number(selectedConnection)}
          presetData={presetData}
          branches={branches?.branches}
          mode="wizard"
          onSubmit={async (data) => {
            try {
              await createProjectMutationM.mutateAsync({
                body: {
                  name: data.name,
                  preset: data.preset,
                  directory: data.rootDirectory,
                  main_branch: data.branch,
                  repo_name: selectedRepository.name || '',
                  repo_owner: selectedRepository.owner || owner || '',
                  git_url: '',
                  git_provider_connection_id: Number(selectedConnection),
                  project_type: data.preset === 'custom' ? 'static' : undefined,
                  automatic_deploy: data.autoDeploy,
                  storage_service_ids: data.storageServices || [],
                  environment_variables: data.environmentVariables?.map(
                    (env) => [env.key, env.value] as [string, string]
                  ),
                },
              })
            } catch (error) {
              console.error('Project creation error:', error)
            }
          }}
          onCancel={() => setSelectedRepository(null)}
        />
      </div>
    )
  }

  return (
    <Card className="flex-1">
      <CardHeader className="flex items-center gap-2">
        <GitBranch className="h-5 w-5 text-foreground" />
        <CardTitle className="text-2xl font-bold">
          Import Git Repository
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-col gap-2 md:flex-row">
          <Select
            value={selectedConnection}
            onValueChange={setSelectedConnection}
          >
            <SelectTrigger className="w-full md:w-[200px]">
              <SelectValue placeholder="Select Connection">
                {selectedConnection &&
                  connections &&
                  (() => {
                    const selectedConn = connections.connections.find(
                      (c) => c.id.toString() === selectedConnection
                    )
                    return selectedConn ? (
                      <div className="flex items-center gap-2">
                        <Github className="h-4 w-4" />
                        <span className="font-medium">
                          {selectedConn.account_name}
                        </span>
                        <span className="text-xs text-muted-foreground">
                          ({selectedConn.account_type})
                        </span>
                      </div>
                    ) : (
                      'Select Connection'
                    )
                  })()}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              {connections?.connections?.map((connection) => (
                <SelectItem
                  key={connection.id}
                  value={connection.id.toString()}
                >
                  <div className="flex items-center gap-2">
                    <Github className="h-4 w-4" />
                    <span className="font-medium">
                      {connection.account_name}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      ({connection.account_type})
                    </span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>

          {/* Search input with icon */}
          <div className="relative flex-1">
            <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              className="pl-9"
              placeholder="Search repositories..."
              value={searchTerm}
              onChange={(e) => setSearchTerm(e.target.value)}
            />
          </div>
        </div>

        {isLoading ? (
          <div className="space-y-2">
            {Array.from({ length: Math.min(5, perPage) }).map((_, i) => (
              <div
                key={i}
                className="flex items-center justify-between py-3 border-b border-border"
              >
                <div className="flex flex-col space-y-2">
                  <Skeleton className="h-4 w-32" />
                  <Skeleton className="h-4 w-24" />
                </div>
                <Skeleton className="h-8 w-16" />
              </div>
            ))}
          </div>
        ) : (
          <>
            {selectedConnection &&
              connections &&
              (() => {
                const selectedConn = connections.connections.find(
                  (c) => c.id.toString() === selectedConnection
                )
                return selectedConn ? (
                  <div className="flex items-center gap-2 py-2 border-b border-border mb-4">
                    <Github className="h-4 w-4 text-muted-foreground" />
                    <span className="text-sm text-muted-foreground">
                      Repositories from{' '}
                      <span className="font-medium text-foreground">
                        {selectedConn.account_name}
                      </span>{' '}
                      ({selectedConn.account_type})
                    </span>
                  </div>
                ) : null
              })()}
            {repositories?.repositories &&
            repositories.repositories.length > 0 ? (
              <>
                <div className="space-y-0">
                  {repositories.repositories.map((repo, index) => (
                    <div
                      key={index}
                      className={cn(
                        'flex items-center justify-between py-3 border-b border-border last:border-none',
                        mode === 'inline' &&
                          'hover:bg-muted/50 cursor-pointer transition-colors'
                      )}
                      onClick={
                        mode === 'inline'
                          ? () => handleRepositoryClick(repo)
                          : undefined
                      }
                    >
                      <div className="flex flex-col gap-2">
                        <div className="flex items-center gap-2">
                          <span className="font-medium">{repo.name}</span>
                          <span className="text-sm text-muted-foreground">
                            {repo.pushed_at ? (
                              <TimeAgo date={repo.pushed_at} />
                            ) : (
                              'never'
                            )}
                          </span>
                        </div>
                        <div className="flex items-center gap-2">
                          {repo.preset && (
                            <div className="flex items-center gap-1.5">
                              <FrameworkIcon
                                preset={repo.preset as any}
                                className="h-4 w-4"
                              />
                              <span className="text-xs bg-muted px-2 py-1 rounded-full">
                                {repo.preset}
                              </span>
                            </div>
                          )}
                          {repo.language && (
                            <span className="text-xs bg-muted px-2 py-1 rounded-full">
                              {repo.language}
                            </span>
                          )}
                        </div>
                      </div>
                      <Button
                        size="sm"
                        onClick={(e) => {
                          if (mode === 'inline') {
                            e.stopPropagation()
                          }
                          handleRepositoryClick(repo)
                        }}
                      >
                        Import
                      </Button>
                    </div>
                  ))}
                </div>

                {/* Pagination Controls */}
                {totalPages > 1 && (
                  <div className="flex items-center justify-between pt-4 border-t">
                    <div className="text-sm text-muted-foreground">
                      Showing {(currentPage - 1) * perPage + 1} to{' '}
                      {Math.min(
                        currentPage * perPage,
                        repositories.total_count || 0
                      )}{' '}
                      of {repositories.total_count || 0} repositories
                    </div>
                    <div className="flex items-center gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() =>
                          setCurrentPage((prev) => Math.max(1, prev - 1))
                        }
                        disabled={!hasPrevPage || isLoading}
                      >
                        <ChevronLeft className="h-4 w-4" />
                        Previous
                      </Button>
                      <div className="flex items-center gap-1">
                        <span className="text-sm">
                          Page {currentPage} of {totalPages}
                        </span>
                      </div>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() =>
                          setCurrentPage((prev) =>
                            Math.min(totalPages, prev + 1)
                          )
                        }
                        disabled={!hasNextPage || isLoading}
                      >
                        Next
                        <ChevronRight className="h-4 w-4 ml-1" />
                      </Button>
                    </div>
                  </div>
                )}
              </>
            ) : (
              <div className="flex flex-col items-center justify-center py-8 text-center">
                <p className="text-muted-foreground mb-2">
                  {searchTerm
                    ? `No repositories found matching "${searchTerm}"`
                    : 'No repositories available'}
                </p>
                {searchTerm && (
                  <p className="text-sm text-muted-foreground">
                    Try adjusting your search term or select a different
                    connection
                  </p>
                )}
                {!searchTerm && (
                  <Button
                    variant="outline"
                    size="sm"
                    className="mt-2"
                    onClick={() => {
                      if (selectedConnection) {
                        syncMutation.mutate({
                          path: { connection_id: parseInt(selectedConnection) },
                        })
                      }
                    }}
                    disabled={syncMutation.isPending || !selectedConnection}
                  >
                    {syncMutation.isPending
                      ? 'Syncing...'
                      : 'Sync Repositories'}
                  </Button>
                )}
              </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  )
}
