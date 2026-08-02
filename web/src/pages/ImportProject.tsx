import {
  createProjectMutation,
  getBranchesByRepositoryIdOptions,
  getRepositoryByIdOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectConfigurator } from '@/components/project/ProjectConfigurator'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { PageContainer } from '@/components/layout/PageContainer'
import { NewProjectShell } from '@/components/project/NewProjectShell'
import { ProviderLogo } from '@/components/git/ProviderLogo'
import { useEffect } from 'react'
import { useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

export function ImportProject() {
  const { repositoryId: repositoryIdParam } = useParams<{
    repositoryId: string
  }>()
  const repositoryId = repositoryIdParam
    ? parseInt(repositoryIdParam, 10)
    : NaN
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'New Project', href: '/projects/new' },
      { label: 'Import Project' },
    ])
  }, [setBreadcrumbs])

  // Fetch repository by ID. The backend derives owner/name/full_name and the
  // git provider connection from the row itself, so the frontend doesn't need
  // to URL-encode any repo path (which breaks for GitLab nested groups).
  const {
    data: selectedRepository,
    isPending: isRepositoryPending,
    isError: isRepositoryError,
    error: repositoryError,
  } = useQuery({
    ...getRepositoryByIdOptions({
      path: { repository_id: repositoryId },
    }),
    enabled: Number.isFinite(repositoryId),
  })

  const selectedConnectionId =
    selectedRepository?.git_provider_connection_id ?? null

  // Resolve the connection's provider type so the header shows the real
  // provider mark (GitLab/Gitea/...) instead of assuming GitHub.
  const { data: connectionsData } = useQuery({ ...listConnectionsOptions() })
  const { data: gitProviders } = useQuery({ ...listGitProvidersOptions() })
  const connectionProviderId = connectionsData?.connections?.find(
    (c) => c.id === selectedConnectionId
  )?.provider_id
  const providerType = gitProviders?.find(
    (p) => p.id === connectionProviderId
  )?.provider_type

  usePageTitle(
    `Import ${selectedRepository?.full_name || 'Repository'}`
  )

  const createProjectMutationM = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to import project',
    },
    onSuccess: async (data) => {
      // Invalidate projects queries to refresh the command palette
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      toast.success('Project imported successfully')
      navigate(`/projects/${data.slug}?new=true`)
    },
  })

  // Fetch branches by repository ID (server derives the connection).
  const { data: branchesData } = useQuery({
    ...getBranchesByRepositoryIdOptions({
      path: { repository_id: repositoryId },
    }),
    enabled: !!selectedRepository && Number.isFinite(repositoryId),
  })

  return (
    <PageContainer>
      <NewProjectShell
        activeSource="browse"
        onSelectSource={(source) => navigate(`/projects/new?source=${source}`)}
      >
        {/* Repository context bar — same card language as the picker */}
        <div className="mb-6 flex flex-col gap-3 rounded-lg border bg-card px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-4 min-w-0">
            <div className="flex items-center gap-2 min-w-0">
              <ProviderLogo providerType={providerType} className="h-5 w-5 shrink-0" />
              <span className="font-medium truncate">
                {selectedRepository?.full_name || 'Loading repository…'}
              </span>
            </div>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground shrink-0">
            <div
              className={`w-2 h-2 rounded-full ${selectedRepository ? 'bg-primary' : 'bg-muted'}`}
            ></div>
            <span>Configure Project</span>
            <div className="w-2 h-2 bg-muted rounded-full ml-4"></div>
            <span>Deploy</span>
          </div>
        </div>

        <div>
        {!Number.isFinite(repositoryId) ? (
          <Card>
            <CardContent className="pt-6">
              <p className="text-sm text-destructive">
                Invalid repository id in URL.
              </p>
            </CardContent>
          </Card>
        ) : isRepositoryError ? (
          <Card>
            <CardContent className="pt-6">
              <p className="text-sm text-destructive">
                Failed to load repository:{' '}
                {(repositoryError as Error)?.message || 'unknown error'}
              </p>
            </CardContent>
          </Card>
        ) : isRepositoryPending || !selectedRepository || !branchesData ? (
          <div className="space-y-6">
            <div>
              <h2 className="text-2xl font-bold">Configure Project</h2>
              <p className="text-sm text-muted-foreground mt-1">
                Loading project configuration...
              </p>
            </div>
            <Card>
              <CardContent className="pt-6 space-y-6">
                {/* Project Name Skeleton */}
                <div className="space-y-2">
                  <Skeleton className="h-4 w-24" />
                  <Skeleton className="h-10 w-full" />
                </div>

                {/* Framework Preset Skeleton */}
                <div className="space-y-2">
                  <Skeleton className="h-4 w-32" />
                  <Skeleton className="h-10 w-full" />
                </div>

                {/* Root Directory Skeleton */}
                <div className="space-y-2">
                  <Skeleton className="h-4 w-28" />
                  <Skeleton className="h-10 w-full" />
                  <Skeleton className="h-3 w-64" />
                </div>

                {/* Branch Skeleton */}
                <div className="space-y-2">
                  <Skeleton className="h-4 w-20" />
                  <Skeleton className="h-10 w-full" />
                </div>

                {/* Environment Variables Skeleton */}
                <div className="space-y-2">
                  <Skeleton className="h-4 w-40" />
                  <Skeleton className="h-24 w-full" />
                </div>

                {/* Deploy Button Skeleton */}
                <div className="flex justify-end gap-3">
                  <Skeleton className="h-10 w-24" />
                  <Skeleton className="h-10 w-32" />
                </div>
              </CardContent>
            </Card>
          </div>
        ) : (
          <ProjectConfigurator
            repository={{
              id: selectedRepository.id,
              name: selectedRepository.name || '',
              owner: selectedRepository.owner || '',
              full_name: selectedRepository.full_name || '',
              private: selectedRepository.private || false,
              default_branch:
                branchesData?.branches?.find((b: any) => b.is_default)?.name ||
                selectedRepository.default_branch ||
                'main',
              created_at:
                selectedRepository.created_at || new Date().toISOString(),
              pushed_at:
                selectedRepository.pushed_at || new Date().toISOString(),
              updated_at:
                selectedRepository.updated_at || new Date().toISOString(),
              git_provider_connection_id:
                selectedRepository.git_provider_connection_id,
            }}
            connectionId={selectedConnectionId!}
            branches={branchesData?.branches}
            mode="inline"
            showRepositoryCard={false}
            onSubmit={async (data) => {
              try {
                await createProjectMutationM.mutateAsync({
                  body: {
                    name: data.name,
                    preset: data.preset,
                    directory: data.rootDirectory,
                    main_branch: data.branch,
                    repo_name: selectedRepository.name || '',
                    repo_owner: selectedRepository.owner || '',
                    git_url: undefined,
                    git_provider_connection_id: selectedConnectionId!,
                    project_type:
                      data.preset === 'custom' ? 'static' : undefined,
                    automatic_deploy: data.autoDeploy,
                    storage_service_ids: data.storageServices || [],
                    environment_variables: data.environmentVariables?.map(
                      (env) => [env.key, env.value] as [string, string]
                    ),
                    preset_config:
                      data.preset === 'dockerfile' && data.dockerfilePath
                        ? { dockerfilePath: data.dockerfilePath }
                        : data.preset === 'docker-compose'
                          ? { composePath: (data as any).composePath || 'docker-compose.yml' }
                          : undefined,
                    exposed_port: data.preset === 'docker-compose' ? undefined : data.port,
                  },
                })
              } catch (error) {
                console.error('Project import error:', error)
              }
            }}
          />
        )}
        </div>
      </NewProjectShell>
    </PageContainer>
  )
}
