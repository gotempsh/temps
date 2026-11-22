// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { dockerfileCreationConfig } from '@/lib/container-runtime-overrides'
import {
  createProjectMutation,
  getBranchesByRepositoryIdOptions,
  getRepositoryByIdOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { invalidateProjectLists } from '@/lib/project-list-cache'
import { ProjectConfigurator } from '@/components/project/ProjectConfigurator'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useJoinGroupAfterCreate } from '@/components/project-groups/useJoinGroupAfterCreate'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
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
  const repositoryId = repositoryIdParam ? parseInt(repositoryIdParam, 10) : NaN
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: 'New service', href: '/projects/new' },
      { label: 'Import repository' },
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

  usePageTitle(`Import ${selectedRepository?.full_name || 'Repository'}`)

  // Importing a project whose slug this host grants the Docker socket to is
  // a sensitive action (ADR 045); the create can come back 428 asking the
  // admin to re-verify rather than failing.
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  // `?group=<id>`: the new service joins that project.
  const {
    groupId: targetGroupId,
    finish,
    joinGroupDialog,
  } = useJoinGroupAfterCreate()

  const createProjectMutationM = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create service',
    },
    onSuccess: async (data) => {
      // The server queues a Git service's first deployment as part of
      // creating it and the create request has no project field, so join
      // before anything else (cache refreshes included) to keep the window in
      // which that deployment is planned without the project's shared
      // variables as short as possible.
      toast.success('Service created')
      await finish(data, `/projects/${data.slug}?new=true`)
      // Invalidate services queries to refresh the command palette
      await invalidateProjectLists(queryClient)
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
      {verificationDialog}
      {joinGroupDialog}
      <NewProjectShell
        activeSource="browse"
        onSelectSource={(source) =>
          navigate(
            targetGroupId === null
              ? `/projects/new?source=${source}`
              : `/projects/new?source=${source}&group=${targetGroupId}`
          )
        }
      >
        {/* Repository context bar — same card language as the picker */}
        <div className="mb-6 flex flex-col gap-3 rounded-lg border bg-card px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-4 min-w-0">
            <div className="flex items-center gap-2 min-w-0">
              <ProviderLogo
                providerType={providerType}
                className="h-5 w-5 shrink-0"
              />
              <span className="font-medium truncate">
                {selectedRepository?.full_name || 'Loading repository…'}
              </span>
            </div>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground shrink-0">
            <div
              className={`w-2 h-2 rounded-full ${selectedRepository ? 'bg-primary' : 'bg-muted'}`}
            ></div>
            <span>Configure service</span>
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
                <h2 className="text-2xl font-bold">Configure service</h2>
                <p className="text-sm text-muted-foreground mt-1">
                  Loading service configuration...
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
                  branchesData?.branches?.find((b: any) => b.is_default)
                    ?.name ||
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
                clone_url: selectedRepository.clone_url,
                ssh_url: selectedRepository.ssh_url,
              }}
              connectionId={selectedConnectionId!}
              branches={branchesData?.branches}
              mode="inline"
              showRepositoryCard={false}
              onSubmit={async (data, deliveryProvider) => {
                // A named local so the step-up retry below can re-run exactly
                // this submission after verification (ADR 045: a slug this
                // host grants the Docker socket to is admin-only and
                // step-up verified).
                const submit = async (): Promise<void> => {
                  try {
                    await createProjectMutationM.mutateAsync({
                      body: {
                        delivery_provider: deliveryProvider,
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
                          (env) => ({
                            key: env.key,
                            value: env.value,
                            is_secret: env.isSecret,
                          })
                        ),
                        preset_config:
                          data.preset === 'dockerfile'
                            ? dockerfileCreationConfig(data)
                            : data.preset === 'docker-compose'
                              ? {
                                  composePath:
                                    (data as any).composePath ||
                                    'docker-compose.yml',
                                  ...(data.excludedServices &&
                                  data.excludedServices.length > 0
                                    ? {
                                        excludedServices: data.excludedServices,
                                      }
                                    : {}),
                                  ...(data.composeServices &&
                                  data.composeServices.length > 0
                                    ? { composeServices: data.composeServices }
                                    : {}),
                                }
                              : undefined,
                        exposed_port:
                          data.preset === 'docker-compose'
                            ? undefined
                            : data.port,
                      },
                    })
                  } catch (error) {
                    if (
                      handleSensitiveActionError(error, () => void submit())
                    ) {
                      return
                    }
                    console.error('Service import error:', error)
                  }
                }
                await submit()
              }}
            />
          )}
        </div>
      </NewProjectShell>
    </PageContainer>
  )
}
