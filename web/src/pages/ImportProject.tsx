import {
  createProjectMutation,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { RepositoryResponse } from '@/api/client/types.gen'
import { ProjectConfigurator } from '@/components/project/ProjectConfigurator'
import { RepositorySelector } from '@/components/repository/RepositorySelector'
import { Card, CardContent } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { GitBranchIcon, GithubIcon } from 'lucide-react'
import { useEffect, useState } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

export function ImportProject() {
  const location = useLocation()
  const fullName = location.pathname.replace('/projects/import/', '')
  const [owner, name] = fullName?.split('/') || ['', '']
  const searchParams = new URLSearchParams(location.search)
  const preferredConnectionId = searchParams.get('connection_id')
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  const [selectedRepository, setSelectedRepository] =
    useState<RepositoryResponse | null>(null)
  const [selectedConnectionId, setSelectedConnectionId] = useState<
    number | null
  >(null)

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'New Project', href: '/projects/new' },
      { label: 'Import Project' },
    ])
  }, [setBreadcrumbs])

  usePageTitle(`Import ${fullName || 'Repository'}`)

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

  // Handle repository selection from RepositorySelector
  const handleRepositorySelect = (
    repo: RepositoryResponse,
    connectionId: number
  ) => {
    setSelectedRepository(repo)
    setSelectedConnectionId(connectionId)
  }

  // Get repository presets
  const { data: repoInfo } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: selectedRepository?.id || 0 },
    }),
    enabled: !!selectedRepository?.id,
  })

  // Fetch branches
  const { data: branchesData } = useQuery({
    ...getRepositoryBranchesOptions({
      query: {
        connection_id: selectedConnectionId || 0,
      },
      path: {
        owner: selectedRepository?.owner || '',
        repo: selectedRepository?.name || '',
      },
    }),
    enabled: !!selectedRepository && !!selectedConnectionId,
  })

  return (
    <div className="min-h-screen bg-background text-foreground">
      {/* Mini header with repository info */}
      <div className="border-b bg-card">
        <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8">
          <div className="flex items-center justify-between h-16">
            <div className="flex items-center gap-4">
              <div className="flex items-center gap-2">
                <GithubIcon className="h-5 w-5 text-muted-foreground" />
                <span className="font-medium">
                  {selectedRepository?.full_name ||
                    fullName ||
                    'Select Repository'}
                </span>
              </div>
              {selectedRepository && (
                <>
                  <Separator orientation="vertical" className="h-6" />
                  <div className="flex items-center gap-2">
                    <GitBranchIcon className="h-4 w-4 text-muted-foreground" />
                    <span className="text-sm text-muted-foreground">
                      {branchesData?.branches?.find((b: any) => b.is_default)
                        ?.name ||
                        selectedRepository.default_branch ||
                        'main'}
                    </span>
                  </div>
                </>
              )}
            </div>
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <div
                className={`w-2 h-2 rounded-full ${selectedRepository ? 'bg-primary' : 'bg-muted'}`}
              ></div>
              <span>Select Repository</span>
              <div
                className={`w-2 h-2 rounded-full ml-4 ${selectedRepository && repoInfo ? 'bg-primary' : 'bg-muted'}`}
              ></div>
              <span>Configure Project</span>
              <div className="w-2 h-2 bg-muted rounded-full ml-4"></div>
              <span>Deploy</span>
            </div>
          </div>
        </div>
      </div>

      {/* Main content */}
      <div className="mx-auto max-w-7xl px-4 sm:px-6 lg:px-8 py-8">
        {!selectedRepository ? (
          <div className="space-y-6">
            <div>
              <h2 className="text-2xl font-bold">Select Repository</h2>
              <p className="text-sm text-muted-foreground mt-1">
                Choose a repository to import as a project
              </p>
            </div>
            <Card>
              <CardContent className="pt-6">
                <RepositorySelector
                  value={selectedRepository}
                  onChange={handleRepositorySelect}
                  owner={owner}
                  name={name}
                  preferredConnectionId={
                    preferredConnectionId
                      ? parseInt(preferredConnectionId)
                      : undefined
                  }
                  showSearch={!fullName} // Hide search if we have specific owner/name
                  autoSelectIfOneMatch={!!fullName} // Auto-select if we have specific repo
                />
              </CardContent>
            </Card>
          </div>
        ) : !branchesData || !repoInfo ? (
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
            }}
            connectionId={selectedConnectionId!}
            presetData={repoInfo}
            branches={branchesData?.branches}
            mode="inline"
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
                    git_url: '',
                    git_provider_connection_id: selectedConnectionId!,
                    project_type:
                      data.preset === 'custom' ? 'static' : undefined,
                    automatic_deploy: data.autoDeploy,
                    storage_service_ids: data.storageServices || [],
                    environment_variables: data.environmentVariables?.map(
                      (env) => [env.key, env.value] as [string, string]
                    ),
                    // Include custom config if needed
                    build_command: undefined,
                    install_command: undefined,
                    output_dir: undefined,
                  },
                })
              } catch (error) {
                console.error('Project import error:', error)
              }
            }}
          />
        )}
      </div>
    </div>
  )
}
