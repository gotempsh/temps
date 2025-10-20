import { useState, useEffect } from 'react'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Separator } from '@/components/ui/separator'

import {
  CheckCircle2,
  Circle,
  GitBranch,
  Globe,
  Loader2,
  AlertCircle,
  GithubIcon,
  RefreshCw,
  Plus,
  Settings,
  ChevronRight,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import {
  listGitProvidersOptions,
  listConnectionsOptions,
  createProjectMutation,
  createDomainMutation,
  listDomainsOptions,
  getProjectsOptions,
  getRepositoryPresetLiveOptions,
  getRepositoryBranchesOptions,
  getRepositoryByNameOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { toast } from 'sonner'
import { useNavigate } from 'react-router-dom'
import { RepositoryResponse, ProviderResponse } from '@/api/client/types.gen'
import { GitProviderFlow } from '@/components/git-providers/GitProviderFlow'
import { RepositoryList } from '@/components/repositories/RepositoryList'
import {
  ProjectConfigurator,
  ProjectFormValues,
} from '@/components/project/ProjectConfigurator'

type OnboardingStep =
  | 'git-provider'
  | 'project'
  | 'configure'
  | 'domain'
  | 'complete'

interface OnboardingDashboardProps {
  hasEmailProvider?: boolean
}

// Helper function to check if provider is GitHub App
const isGitHubApp = (provider: ProviderResponse) =>
  provider.provider_type === 'github' &&
  (provider.auth_method === 'app' || provider.auth_method === 'github_app')

export function OnboardingDashboard({}: OnboardingDashboardProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [expandedStep, setExpandedStep] = useState<OnboardingStep | null>(null)
  const [selectedGitProviderId, setSelectedGitProviderId] = useState<
    number | null
  >(null)
  const [selectedGitProviderConnId, setSelectedGitProviderConnId] = useState<
    number | null
  >(null)
  const [showAddProvider, setShowAddProvider] = useState(false)
  const [showInstallPrompt, setShowInstallPrompt] = useState(false)

  // Project Form State
  const [selectedRepoId, setSelectedRepoId] = useState<string>('')
  const [selectedRepo, setSelectedRepo] = useState<RepositoryResponse | null>(
    null
  )
  const [selectedBranch, setSelectedBranch] = useState<string>('')
  const [selectedPreset, setSelectedPreset] = useState<string>('')
  const [detectedPreset, setDetectedPreset] = useState<string | null>(null)
  const [selectedPath, setSelectedPath] = useState<string>('./')

  // Domain Form State
  const [domainName, setDomainName] = useState('*.')

  // Queries
  const { data: gitProviders, refetch: refetchGitProviders } = useQuery({
    ...listGitProvidersOptions({}),
    retry: false,
  })
  const { data: connections, refetch: refetchConnections } = useQuery({
    ...listConnectionsOptions({}),
    retry: false,
  })
  const { data: repoData } = useQuery({
    ...getRepositoryByNameOptions({
      path: {
        owner: selectedRepo?.owner || '',
        name: selectedRepo?.name || '',
      },
    }),
  })
  // Query for branches when a repository is selected
  const { data: branchesData, isLoading: branchesLoading } = useQuery({
    ...getRepositoryBranchesOptions({
      path: {
        owner: selectedRepo?.owner || '',
        repo: selectedRepo?.name || '',
      },
      query: {
        connection_id: selectedGitProviderConnId || 0,
      },
    }),
    enabled: !!selectedRepo?.owner && !!selectedRepo?.name,
    retry: false,
  })

  const { data: domains, refetch: refetchDomains } = useQuery({
    ...listDomainsOptions({}),
    retry: false,
  })

  const { data: projectsData } = useQuery({
    ...getProjectsOptions({}),
    retry: false,
  })

  // Preset detection query - triggers when repository is selected
  const { data: presetData, isLoading: presetLoading } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: selectedRepoId ? parseInt(selectedRepoId) : 0 },
    }),
    enabled: !!selectedRepoId,
    retry: false,
  })

  // Mutations
  const createProject = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create project',
    },
    onSuccess: async (data) => {
      toast.success('Project created successfully!')
      // Invalidate projects queries to refresh the command palette
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['projects'] })
      // Navigate to project detail with confetti
      navigate(`/projects/${data.slug}/project?showConfetti=true`)
    },
  })

  const createDomain = useMutation({
    ...createDomainMutation(),
    meta: {
      errorTitle: 'Failed to configure wildcard domain',
    },
    onSuccess: () => {
      toast.success('Wildcard domain configured successfully!')
      refetchDomains()
      setExpandedStep('complete')
      // Clear form
      setDomainName('*.')
    },
    onError: (error: any) => {
      toast.error(error?.response?.data?.detail || 'Failed to configure domain')
    },
  })

  // Function to handle GitHub App installation
  const handleInstallGitHubApp = (provider: ProviderResponse) => {
    // For GitHub App providers, construct the installation URL directly
    if (isGitHubApp(provider)) {
      // Extract GitHub App URL from provider name or use default GitHub
      const baseUrl = provider.base_url

      // Open GitHub App installation page in new tab
      const installUrl = `${baseUrl}/installations/new`
      window.open(installUrl, '_blank', 'noopener,noreferrer')

      toast.success('Opening GitHub App installation in new tab')
    }
  }

  // Determine completed steps
  const hasConnections = (connections?.connections?.length || 0) > 0
  const hasProjects = (projectsData?.projects?.length || 0) > 0
  const hasDomains =
    domains?.domains?.some((d: any) => d.domain.startsWith('*.')) || false
  const hasSelectedRepo = !!selectedRepo

  // Auto-expand first incomplete step and auto-select single provider
  useEffect(() => {
    if (!hasConnections) {
      setExpandedStep('git-provider')
    } else if (!hasSelectedRepo) {
      setExpandedStep('project')
      // Auto-select first provider and connection if only one exists
      if (
        gitProviders?.length &&
        gitProviders.length > 0 &&
        !selectedGitProviderId
      ) {
        setSelectedGitProviderId(gitProviders[0].id)
      }
      if (
        connections?.connections?.length &&
        connections.connections.length > 0 &&
        !selectedGitProviderConnId
      ) {
        setSelectedGitProviderConnId(connections.connections[0].id)
      }
    } else if (!hasProjects) {
      setExpandedStep('configure')
    } else if (!hasDomains) {
      setExpandedStep('domain')
    }
  }, [
    hasConnections,
    hasSelectedRepo,
    hasProjects,
    hasDomains,
    gitProviders,
    selectedGitProviderId,
    connections,
    selectedGitProviderConnId,
  ])

  // Auto-refresh providers and connections when page gains focus (user returns from GitHub)
  useEffect(() => {
    const handleFocus = async () => {
      // Only refresh if we're on the git-provider step
      if (expandedStep === 'git-provider' && !showInstallPrompt) {
        const { data: providers } = await refetchGitProviders()
        const { data: conns } = await refetchConnections()

        if (providers && providers.length > 0) {
          // Found providers after refresh
          const newProvider = providers[0]

          if (!hasConnections && conns && conns.connections.length > 0) {
            // Connections found! Move to project creation
            setExpandedStep('project')
            setSelectedGitProviderId(newProvider.id)
            setSelectedGitProviderConnId(conns.connections[0].id)
            toast.success('Connected successfully!')
          } else if (isGitHubApp(newProvider) && !newProvider.is_active) {
            // GitHub App created but not installed yet
            setShowInstallPrompt(true)
            setSelectedGitProviderId(newProvider.id)
          } else if (!isGitHubApp(newProvider)) {
            // For PAT providers, keep checking for connections
            // Start auto-refresh for connections
            const checkInterval = setInterval(async () => {
              const { data: updatedConns } = await refetchConnections()
              if (updatedConns && updatedConns.connections.length > 0) {
                clearInterval(checkInterval)
                setExpandedStep('project')
                setSelectedGitProviderId(newProvider.id)
                setSelectedGitProviderConnId(updatedConns.connections[0].id)
                toast.success('Connected successfully!')
              }
            }, 2000)

            // Stop checking after 10 seconds
            setTimeout(() => clearInterval(checkInterval), 10000)
          }
        }
      }
    }

    window.addEventListener('focus', handleFocus)
    // Also check on visibility change (for tab switching)
    const handleVisibilityChange = () => {
      if (!document.hidden) {
        handleFocus()
      }
    }
    document.addEventListener('visibilitychange', handleVisibilityChange)

    // Check immediately when component mounts
    handleFocus()

    return () => {
      window.removeEventListener('focus', handleFocus)
      document.removeEventListener('visibilitychange', handleVisibilityChange)
    }
  }, [expandedStep, hasConnections, refetchGitProviders, showInstallPrompt])

  // Effect to handle preset detection results
  useEffect(() => {
    if (presetData?.projects && presetData.projects.length > 0) {
      // If we have multiple projects, use the first one as default
      const firstProject = presetData.projects[0]
      setDetectedPreset(firstProject.preset)
      setSelectedPreset(firstProject.preset)
      setSelectedPath(firstProject.path || './')
    } else if (presetData?.root_preset) {
      setDetectedPreset(presetData.root_preset)
      setSelectedPreset(presetData.root_preset)
      setSelectedPath('./')
    } else {
      setDetectedPreset(null)
      setSelectedPreset('')
      setSelectedPath('./')
    }
  }, [presetData])

  // Set default branch when branches are loaded
  useEffect(() => {
    if (branchesData?.branches && branchesData.branches.length > 0) {
      // Find the default branch using is_default flag or matching repository's default_branch
      const defaultBranch =
        branchesData.branches.find(
          (b: any) =>
            b.is_default ||
            b.name === repoData?.default_branch ||
            b.name === selectedRepo?.default_branch
        ) || branchesData.branches[0]
      if (defaultBranch && !selectedBranch) {
        setSelectedBranch(defaultBranch.name)
      }
    }
  }, [branchesData, selectedBranch, repoData, selectedRepo])

  const handleProjectSubmit = async () => {
    if (selectedRepo && selectedGitProviderConnId && selectedBranch) {
      await createProject.mutateAsync({
        body: {
          name: selectedRepo.name, // Use repository name as project name
          repo_owner: selectedRepo.owner,
          repo_name: selectedRepo.name,
          directory: selectedPath || '/',
          main_branch: selectedBranch, // Use selected branch
          preset: selectedPreset,
          storage_service_ids: [],
          git_provider_connection_id: selectedGitProviderConnId,
          is_public_repo: !selectedRepo.private,
          automatic_deploy: true,
          is_web_app: true,
          performance_metrics_enabled: true,
          is_on_demand: false,
          use_default_wildcard: true,
          // Optional fields - will be set based on preset or left undefined
          git_url: undefined,
          build_command: undefined,
          install_command: undefined,
          output_dir: undefined,
          project_type: undefined,
          environment_variables: undefined,
          custom_domain: undefined,
        },
      })
    }
  }

  const handleDomainSubmit = async () => {
    if (domainName && domainName.startsWith('*.')) {
      await createDomain.mutateAsync({
        body: {
          domain: domainName,
        },
      })
    }
  }

  const allStepsComplete = hasConnections && hasProjects && hasDomains

  if (allStepsComplete) {
    return (
      <div className="max-w-full mx-auto space-y-6">
        <Card className="border-green-200 bg-green-50/50 dark:bg-green-950/10">
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <CheckCircle2 className="h-6 w-6 text-green-600" />
              Setup Complete!
            </CardTitle>
            <CardDescription>
              You&apos;re all set to start deploying projects with Temps
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button onClick={() => navigate('/projects')} className="w-full">
              Go to Projects
            </Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  // Calculate which step we're on
  const getCurrentStepNumber = () => {
    if (!hasConnections) return 1
    if (!hasSelectedRepo) return 2
    if (!hasProjects) return 3
    if (!hasDomains) return 4
    return 5
  }

  const currentStepNumber = getCurrentStepNumber()

  return (
    <div className="max-w-full mx-auto space-y-6">
      <div>
        <h1 className="text-3xl font-bold tracking-tight">Welcome to Temps!</h1>
        <p className="text-muted-foreground mt-2">
          Complete these steps to start deploying your projects
        </p>
      </div>

      {/* Wizard Progress Indicator */}
      <div className="relative">
        <div className="flex items-center justify-between">
          {/* Step 1: Git Provider */}
          <div className="flex flex-col items-center relative z-10">
            <div
              className={cn(
                'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all',
                hasConnections
                  ? 'bg-primary border-primary text-primary-foreground'
                  : currentStepNumber === 1
                    ? 'border-primary bg-background text-primary'
                    : 'border-muted bg-background text-muted-foreground'
              )}
            >
              {hasConnections ? (
                <CheckCircle2 className="h-5 w-5" />
              ) : (
                <span className="text-sm font-semibold">1</span>
              )}
            </div>
            <span
              className={cn(
                'text-xs mt-2 font-medium',
                hasConnections && 'line-through text-muted-foreground'
              )}
            >
              Git Provider
            </span>
          </div>

          {/* Step 2: Select Repository */}
          <div className="flex flex-col items-center relative z-10">
            <div
              className={cn(
                'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all',
                hasSelectedRepo
                  ? 'bg-primary border-primary text-primary-foreground'
                  : currentStepNumber === 2
                    ? 'border-primary bg-background text-primary'
                    : 'border-muted bg-background text-muted-foreground'
              )}
            >
              {hasSelectedRepo ? (
                <CheckCircle2 className="h-5 w-5" />
              ) : (
                <span className="text-sm font-semibold">2</span>
              )}
            </div>
            <span
              className={cn(
                'text-xs mt-2 font-medium',
                hasSelectedRepo && 'line-through text-muted-foreground'
              )}
            >
              Select Repository
            </span>
          </div>

          {/* Step 3: Configure Project */}
          <div className="flex flex-col items-center relative z-10">
            <div
              className={cn(
                'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all',
                hasProjects
                  ? 'bg-primary border-primary text-primary-foreground'
                  : currentStepNumber === 3
                    ? 'border-primary bg-background text-primary'
                    : 'border-muted bg-background text-muted-foreground'
              )}
            >
              {hasProjects ? (
                <CheckCircle2 className="h-5 w-5" />
              ) : (
                <span className="text-sm font-semibold">3</span>
              )}
            </div>
            <span
              className={cn(
                'text-xs mt-2 font-medium',
                hasProjects && 'line-through text-muted-foreground'
              )}
            >
              Configure Project
            </span>
          </div>

          {/* Step 4: Domain */}
          <div className="flex flex-col items-center relative z-10">
            <div
              className={cn(
                'flex h-10 w-10 items-center justify-center rounded-full border-2 transition-all',
                hasDomains
                  ? 'bg-primary border-primary text-primary-foreground'
                  : currentStepNumber === 4
                    ? 'border-primary bg-background text-primary'
                    : 'border-muted bg-background text-muted-foreground'
              )}
            >
              {hasDomains ? (
                <CheckCircle2 className="h-5 w-5" />
              ) : (
                <span className="text-sm font-semibold">4</span>
              )}
            </div>
            <span
              className={cn(
                'text-xs mt-2 font-medium',
                hasDomains && 'line-through text-muted-foreground'
              )}
            >
              External Connectivity
            </span>
          </div>
        </div>

        {/* Progress Line */}
        <div className="absolute top-5 left-0 right-0 h-0.5 bg-muted -z-10">
          <div
            className="h-full bg-primary transition-all duration-500"
            style={{
              width: `${((currentStepNumber - 1) / 3) * 100}%`,
            }}
          />
        </div>
      </div>

      {/* Step 1: Git Provider - Show if no connections */}
      {!hasConnections && (
        <Card className={cn('transition-all', 'ring-2 ring-primary')}>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <Circle className="h-5 w-5 text-muted-foreground" />
                <div>
                  <CardTitle className="text-lg">
                    Step 1: Connect Git Provider
                  </CardTitle>
                  <CardDescription>
                    Link GitHub or GitLab for automatic deployments
                  </CardDescription>
                </div>
              </div>
            </div>
          </CardHeader>

          <CardContent>
            {/* Case 1: We have a provider but no connections - handle both GitHub App and PAT */}
            {gitProviders && gitProviders.length > 0 && !hasConnections ? (
              // Check if it's a GitHub App that needs installation
              isGitHubApp(gitProviders[0]) ? (
                <div className="space-y-4">
                  <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
                    <CheckCircle2 className="h-4 w-4 text-green-600" />
                    <AlertDescription>
                      <div className="space-y-2">
                        <p className="font-medium">
                          GitHub App created successfully!
                        </p>
                        <p className="text-sm">
                          Now install it on your repositories to create
                          connections and deploy projects.
                        </p>
                      </div>
                    </AlertDescription>
                  </Alert>

                  <div className="flex flex-col gap-3">
                    <div className="p-4 border rounded-lg bg-muted/50">
                      <div className="flex items-center justify-between">
                        <div className="flex items-center gap-3">
                          <GithubIcon className="h-5 w-5" />
                          <div>
                            <p className="font-medium">
                              {gitProviders[0].name}
                            </p>
                            <p className="text-sm text-muted-foreground">
                              GitHub App ready to install
                            </p>
                          </div>
                        </div>
                        <Badge variant="outline" className="text-xs">
                          <CheckCircle2 className="h-3 w-3 mr-1" />
                          Created
                        </Badge>
                      </div>
                    </div>

                    <div className="flex gap-3">
                      <Button
                        className="flex-1"
                        onClick={() => {
                          if (gitProviders[0]) {
                            handleInstallGitHubApp(gitProviders[0])
                            // After opening installation, start checking for connections
                            setTimeout(() => {
                              const checkConnections = setInterval(async () => {
                                const { data: conns } =
                                  await refetchConnections()
                                if (conns && conns.connections.length > 0) {
                                  // Connections found! Installation complete
                                  clearInterval(checkConnections)
                                  setShowInstallPrompt(false)
                                  setExpandedStep('project')
                                  setSelectedGitProviderConnId(
                                    conns.connections[0].id
                                  )
                                  toast.success(
                                    'GitHub App installed successfully!'
                                  )
                                }
                              }, 2000)

                              // Stop checking after 30 seconds
                              setTimeout(
                                () => clearInterval(checkConnections),
                                30000
                              )
                            }, 3000)
                          }
                        }}
                      >
                        <GithubIcon className="h-4 w-4 mr-2" />
                        Install GitHub App
                      </Button>
                    </div>
                  </div>
                </div>
              ) : (
                // For PAT providers, connections should be created automatically
                // If we have a PAT provider but no connections, something went wrong
                <div className="space-y-4">
                  <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
                    <CheckCircle2 className="h-4 w-4 text-green-600" />
                    <AlertDescription>
                      <div className="space-y-2">
                        <p className="font-medium">
                          Provider Added Successfully!
                        </p>
                        <p className="text-sm">Fetching your repositories...</p>
                      </div>
                    </AlertDescription>
                  </Alert>

                  <div className="flex items-center justify-center py-8">
                    <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
                  </div>

                  <div className="flex gap-3">
                    <Button
                      variant="outline"
                      onClick={() => {
                        refetchConnections().then(({ data: conns }) => {
                          if (conns && conns.connections.length > 0) {
                            // Connections found!
                            setExpandedStep('project')
                            setSelectedGitProviderConnId(
                              conns.connections[0].id
                            )
                            toast.success('Connected successfully!')
                          } else {
                            toast.error(
                              'No connections found. Please try reconfiguring the provider.'
                            )
                          }
                        })
                      }}
                    >
                      <RefreshCw className="h-4 w-4 mr-2" />
                      Refresh
                    </Button>
                  </div>
                </div>
              )
            ) : showInstallPrompt && gitProviders && gitProviders.length > 0 ? (
              <div className="space-y-4">
                <Alert className="border-green-200 bg-green-50/50 dark:bg-green-950/20">
                  <CheckCircle2 className="h-4 w-4 text-green-600" />
                  <AlertDescription>
                    <div className="space-y-2">
                      <p className="font-medium">
                        GitHub App created successfully!
                      </p>
                      <p className="text-sm">
                        Now install it on your repositories to create
                        connections and deploy projects.
                      </p>
                    </div>
                  </AlertDescription>
                </Alert>

                <Button
                  className="w-full"
                  onClick={() => {
                    if (gitProviders[0]) {
                      handleInstallGitHubApp(gitProviders[0])
                      // After opening installation, start checking for connections
                      setTimeout(() => {
                        const checkConnections = setInterval(async () => {
                          const { data: conns } = await refetchConnections()
                          if (conns && conns.connections.length > 0) {
                            // Connections found! Installation complete
                            clearInterval(checkConnections)
                            setShowInstallPrompt(false)
                            setExpandedStep('project')
                            setSelectedGitProviderConnId(
                              conns.connections[0].id
                            )
                            toast.success('GitHub App installed successfully!')
                          }
                        }, 2000)

                        // Stop checking after 30 seconds
                        setTimeout(() => clearInterval(checkConnections), 30000)
                      }, 3000)
                    }
                  }}
                >
                  <GithubIcon className="h-4 w-4 mr-2" />
                  Install GitHub App
                </Button>
              </div>
            ) : !hasConnections ? (
              // Case 2: No providers at all - show add provider flow
              <GitProviderFlow
                onSuccess={() => {
                  toast.success('Git provider added successfully!')
                  refetchGitProviders().then(() => {
                    setExpandedStep('project')
                    // Auto-select the newly added provider if it's the only one
                    const providers = queryClient.getQueryData([
                      'listGitProviders',
                    ]) as any
                    if (providers?.length === 1) {
                      setSelectedGitProviderId(providers[0].id)
                    }
                    // Also auto-select connection if available
                    const connections = queryClient.getQueryData([
                      'listConnections',
                    ]) as any
                    if (connections?.length === 1) {
                      setSelectedGitProviderConnId(connections[0].id)
                    }
                  })
                }}
                onCancel={() => {
                  // Check if we have any providers now
                  refetchGitProviders().then(() => {
                    const providers = queryClient.getQueryData([
                      'listGitProviders',
                    ]) as any
                    if (providers?.length > 0) {
                      setExpandedStep('project')
                      if (providers.length === 1) {
                        setSelectedGitProviderId(providers[0].id)
                      }
                      // Also auto-select connection if available
                      const connections = queryClient.getQueryData([
                        'listConnections',
                      ]) as any
                      if (connections?.length === 1) {
                        setSelectedGitProviderConnId(connections[0].id)
                      }
                    }
                  })
                }}
                mode="onboarding"
              />
            ) : (
              // Case 3: Have a PAT provider but no connections (shouldn't happen but handle it)
              <div className="space-y-4">
                <Alert className="border-orange-200 bg-orange-50/50 dark:bg-orange-950/20">
                  <AlertCircle className="h-4 w-4 text-orange-600" />
                  <AlertDescription>
                    <div className="space-y-2">
                      <p className="font-medium">
                        Provider configured but no connections found
                      </p>
                      <p className="text-sm">
                        There seems to be an issue with your git provider.
                        Please reconfigure it.
                      </p>
                    </div>
                  </AlertDescription>
                </Alert>

                <div className="flex gap-3">
                  <Button
                    variant="outline"
                    onClick={() => refetchConnections()}
                  >
                    <RefreshCw className="h-4 w-4 mr-2" />
                    Refresh
                  </Button>
                  <Button onClick={() => navigate('/git-sources')}>
                    Go to Git Providers
                  </Button>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      )}

      {/* Step 2: Select Repository - Only show if we have connections but no repo selected */}
      {!hasSelectedRepo && hasConnections && (
        <Card className={cn('transition-all', 'ring-2 ring-primary')}>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <Circle className="h-5 w-5 text-muted-foreground" />
                <div>
                  <CardTitle className="text-lg">
                    Step 2: Select Repository
                  </CardTitle>
                  <CardDescription>
                    Choose a repository to deploy from your connected Git
                    provider
                  </CardDescription>
                </div>
              </div>
            </div>
          </CardHeader>

          <CardContent className="space-y-4">
            {/* Repository Selection - Primary Focus */}
            {selectedGitProviderConnId && !showAddProvider ? (
              <>
                <div className="space-y-3">
                  <div>
                    <Label className="text-base font-medium">Repository</Label>
                    <p className="text-sm text-muted-foreground mt-0.5">
                      Select a repository to deploy from your connected Git
                      provider
                    </p>
                  </div>
                  <div>
                    <RepositoryList
                      connectionId={selectedGitProviderConnId}
                      onRepositorySelect={(repo) => {
                        setSelectedRepoId(repo.id.toString())
                        setSelectedRepo(repo)
                        setSelectedBranch('') // Reset branch when repository changes
                      }}
                      selectedRepositoryId={selectedRepoId}
                      showSelection={true}
                      itemsPerPage={12}
                      showHeader={true}
                      compactMode={false}
                    />
                  </div>
                </div>

                {/* Compact Connection Info */}
                <div className="mt-4 pt-4 border-t">
                  <div className="flex items-center justify-between mb-2">
                    <Label className="text-xs text-muted-foreground">
                      Git Connection
                    </Label>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setShowAddProvider(true)}
                      className="text-xs h-6 px-2"
                    >
                      <Plus className="mr-1 h-3 w-3" />
                      Add Another
                    </Button>
                  </div>
                  {connections && connections.connections.length > 1 ? (
                    <div className="grid grid-cols-2 gap-2">
                      {connections.connections.map((conn) => (
                        <button
                          key={conn.id}
                          onClick={() => setSelectedGitProviderConnId(conn.id)}
                          className={cn(
                            'relative p-2 border rounded transition-all text-left text-xs',
                            selectedGitProviderConnId === conn.id
                              ? 'border-primary bg-primary/5'
                              : 'border-border hover:border-muted-foreground/50 hover:bg-accent/30'
                          )}
                        >
                          <div className="flex items-center gap-2">
                            {gitProviders &&
                            gitProviders[0]?.provider_type === 'github' ? (
                              <GithubIcon className="h-3.5 w-3.5" />
                            ) : (
                              <GitBranch className="h-3.5 w-3.5" />
                            )}
                            <span className="font-medium">
                              {conn.account_name}
                            </span>
                            {selectedGitProviderConnId === conn.id && (
                              <CheckCircle2 className="h-3 w-3 text-primary ml-auto" />
                            )}
                          </div>
                        </button>
                      ))}
                    </div>
                  ) : connections && connections.connections.length === 1 ? (
                    <div className="relative p-2 border rounded bg-muted/50 text-xs">
                      <div className="flex items-center gap-2">
                        {gitProviders &&
                        gitProviders[0]?.provider_type === 'github' ? (
                          <GithubIcon className="h-3.5 w-3.5" />
                        ) : (
                          <GitBranch className="h-3.5 w-3.5" />
                        )}
                        <span className="font-medium">
                          {connections.connections[0].account_name}
                        </span>
                        <Badge
                          variant="outline"
                          className="ml-auto text-xs h-4"
                        >
                          <CheckCircle2 className="h-2.5 w-2.5 mr-1" />
                          Connected
                        </Badge>
                      </div>
                    </div>
                  ) : null}
                </div>
              </>
            ) : showAddProvider ? (
              <GitProviderFlow
                onSuccess={() => {
                  toast.success('Git provider added successfully!')
                  setShowAddProvider(false)
                  refetchGitProviders().then(() => {
                    const providers = queryClient.getQueryData([
                      'listGitProviders',
                    ]) as any
                    // Auto-select the newly added provider if it's the only one
                    if (providers?.length === 1) {
                      setSelectedGitProviderId(providers[0].id)
                    }
                    // Also auto-select connection if available
                    const connections = queryClient.getQueryData([
                      'listConnections',
                    ]) as any
                    if (connections?.length === 1) {
                      setSelectedGitProviderConnId(connections[0].id)
                    }
                  })
                }}
                onCancel={() => setShowAddProvider(false)}
                mode="onboarding"
              />
            ) : gitProviders && gitProviders.length > 0 ? (
              <div className="space-y-3">
                <div className="flex items-center justify-between">
                  <Label className="text-sm">Choose Git Provider</Label>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setShowAddProvider(true)}
                    className="text-xs h-7"
                  >
                    <Plus className="mr-1 h-3 w-3" />
                    Add New Provider
                  </Button>
                </div>
                <div className="grid grid-cols-1 gap-3">
                  {gitProviders.map((provider: ProviderResponse) => (
                    <button
                      key={provider.id}
                      onClick={() => {
                        setSelectedGitProviderId(provider.id)
                        if (isGitHubApp(provider)) {
                          // For GitHub Apps, show installation prompt
                          handleInstallGitHubApp(provider)
                          // Start checking for connections after installation
                          setTimeout(() => {
                            const checkConnections = setInterval(async () => {
                              const { data: conns } = await refetchConnections()
                              if (conns && conns.connections.length > 0) {
                                const providerConnection =
                                  conns.connections.find(
                                    (conn) => conn.provider_id === provider.id
                                  )
                                if (providerConnection) {
                                  clearInterval(checkConnections)
                                  setSelectedGitProviderConnId(
                                    providerConnection.id
                                  )
                                  toast.success(
                                    'GitHub App installed successfully!'
                                  )
                                }
                              }
                            }, 2000)
                            // Stop checking after 30 seconds
                            setTimeout(
                              () => clearInterval(checkConnections),
                              30000
                            )
                          }, 3000)
                        } else {
                          // For PAT providers, find existing connection
                          const providerConnection =
                            connections?.connections?.find(
                              (conn) => conn.provider_id === provider.id
                            )
                          if (providerConnection) {
                            setSelectedGitProviderConnId(providerConnection.id)
                          }
                        }
                      }}
                      className={cn(
                        'relative p-3 border rounded-lg transition-all text-left',
                        selectedGitProviderId === provider.id
                          ? 'border-primary bg-primary/5'
                          : 'border-border hover:border-muted-foreground/50 hover:bg-accent/30'
                      )}
                    >
                      <div className="flex items-center gap-2">
                        {provider.provider_type === 'github' ? (
                          <GithubIcon className="h-4 w-4" />
                        ) : (
                          <GitBranch className="h-4 w-4" />
                        )}
                        <div className="flex-1">
                          <span className="font-medium text-sm">
                            {provider.name}
                          </span>
                          <div className="text-xs text-muted-foreground mt-0.5">
                            {provider.provider_type === 'github' &&
                            isGitHubApp(provider)
                              ? 'GitHub App'
                              : 'Personal Access Token'}
                          </div>
                        </div>
                        {selectedGitProviderId === provider.id && (
                          <CheckCircle2 className="h-4 w-4 text-primary" />
                        )}
                      </div>
                    </button>
                  ))}
                </div>
              </div>
            ) : (
              <div className="space-y-3">
                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    No git providers configured. Add one to continue.
                  </AlertDescription>
                </Alert>
                <Button
                  onClick={() => setShowAddProvider(true)}
                  className="w-full"
                >
                  <Plus className="mr-2 h-4 w-4" />
                  Add Git Provider
                </Button>
              </div>
            )}

            {/* Next Step Button */}
            {selectedRepo && (
              <div className="mt-6 pt-4 border-t">
                <Alert className="mb-4">
                  <CheckCircle2 className="h-4 w-4" />
                  <AlertDescription>
                    Repository selected: <strong>{selectedRepo?.name}</strong>
                    <br />
                    <span className="text-sm text-muted-foreground">
                      Next, configure your project settings, services, and
                      environment variables.
                    </span>
                  </AlertDescription>
                </Alert>
                <div className="flex justify-end">
                  <Button
                    onClick={() => setExpandedStep('configure')}
                    className="gap-2"
                  >
                    <Settings className="h-4 w-4" />
                    Continue to Configuration
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      )}

      {/* Step 3: Configure Project - Only show if repository selected but no projects */}
      {hasSelectedRepo && !hasProjects && (
        <Card className={cn('transition-all', 'ring-2 ring-primary')}>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <Circle className="h-5 w-5 text-muted-foreground" />
                <div>
                  <CardTitle className="text-lg">
                    Step 3: Configure Project
                  </CardTitle>
                  <CardDescription>
                    Set up services, environment variables, and deployment
                    settings
                  </CardDescription>
                </div>
              </div>
            </div>
          </CardHeader>
          <CardContent>
            {selectedRepo && selectedGitProviderConnId && (
              <ProjectConfigurator
                repository={selectedRepo}
                connectionId={selectedGitProviderConnId}
                presetData={presetData}
                branches={branchesData?.branches}
                mode="compact"
                onSubmit={async (data: ProjectFormValues) => {
                  // Use the existing handleProjectSubmit logic but with the new data
                  if (selectedRepo && selectedGitProviderConnId) {
                    await createProject.mutateAsync({
                      body: {
                        name: data.name,
                        repo_owner: selectedRepo.owner,
                        repo_name: selectedRepo.name,
                        directory: data.rootDirectory || './',
                        main_branch: data.branch,
                        preset: data.preset,
                        storage_service_ids: data.storageServices || [],
                        git_provider_connection_id: selectedGitProviderConnId,
                        is_public_repo: !selectedRepo.private,
                        automatic_deploy: data.autoDeploy,
                        is_web_app: true,
                        performance_metrics_enabled: true,
                        is_on_demand: false,
                        use_default_wildcard: true,
                        environment_variables: data.environmentVariables?.map(
                          (env) => [env.key, env.value] as [string, string]
                        ),
                        git_url: undefined,
                        build_command: undefined,
                        install_command: undefined,
                        output_dir: undefined,
                        project_type: undefined,
                        custom_domain: undefined,
                      },
                    })
                  }
                }}
              />
            )}
          </CardContent>
        </Card>
      )}

      {/* Step 4: Wildcard Domain - Only show if not completed */}
      {!hasDomains && hasProjects && (
        <Card className={cn('transition-all', 'ring-2 ring-primary')}>
          <CardHeader>
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <Circle className="h-5 w-5 text-muted-foreground" />
                <div>
                  <CardTitle className="text-lg">
                    Step 4: Configure Wildcard Domain
                  </CardTitle>
                  <CardDescription>
                    Enable dynamic URLs for all your projects and environments
                  </CardDescription>
                </div>
              </div>
            </div>
          </CardHeader>

          <CardContent className="space-y-4">
            <Separator />

            <div>
              <Label htmlFor="domain">Wildcard Domain</Label>
              <Input
                id="domain"
                value={domainName}
                onChange={(e) => setDomainName(e.target.value)}
                placeholder="*.yourdomain.com"
                className="mt-1"
              />
              <p className="text-xs text-muted-foreground mt-1">
                Must start with *. (e.g., *.example.com)
              </p>
            </div>

            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                After adding your domain, you'll need to configure your DNS
                settings:
                <ul className="mt-2 space-y-1 text-sm">
                  <li>• Add a CNAME record for *.yourdomain.com</li>
                  <li>• Point it to your Temps server</li>
                  <li>• SSL certificates will be automatically provisioned</li>
                </ul>
              </AlertDescription>
            </Alert>

            <div className="flex justify-end">
              <Button
                onClick={handleDomainSubmit}
                disabled={
                  createDomain.isPending ||
                  !domainName ||
                  !domainName.startsWith('*.')
                }
              >
                {createDomain.isPending ? (
                  <>
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    Adding...
                  </>
                ) : (
                  <>
                    <Globe className="mr-2 h-4 w-4" />
                    Add Domain
                  </>
                )}
              </Button>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}
