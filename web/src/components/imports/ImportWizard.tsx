import {
  createPlanMutation,
  discoverWorkloadsMutation,
  executeImportMutation,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  listSourcesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  ImportSource,
  ImportSourceInfo,
  WorkloadDescriptor,
  CreatePlanResponse,
  RepositoryResponse,
} from '@/api/client/types.gen'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { RepositorySelector } from '@/components/repository/RepositorySelector'
import { FrameworkSelector } from '@/components/project/FrameworkSelector'
import { MaskedValue } from '@/components/ui/masked-value'
import { shouldMaskValue } from '@/lib/masking'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  ArrowRight,
  CheckCircle2,
  Container,
  FileCode,
  Filter,
  GitBranch,
  Loader2,
  Package,
  Search,
  Server,
  X,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { getProject } from '@/api/client'

type WizardStep =
  | 'select-source'
  | 'discover-workloads'
  | 'select-repository'
  | 'configure-project'
  | 'review-plan'
  | 'execute'

interface ImportWizardProps {
  onCancel?: () => void
  className?: string
}

const STEP_CONFIG = {
  'select-source': {
    title: 'Select Import Source',
    description: 'Choose where to import your workload from',
    icon: Server,
  },
  'discover-workloads': {
    title: 'Discover Workloads',
    description: 'Find and select a workload to import',
    icon: Container,
  },
  'select-repository': {
    title: 'Select Repository',
    description: 'Link to an existing repository',
    icon: GitBranch,
  },
  'configure-project': {
    title: 'Configure Project',
    description: 'Set up project name, framework, and deployment settings',
    icon: FileCode,
  },
  'review-plan': {
    title: 'Review Import Plan',
    description: 'Review the import configuration',
    icon: FileCode,
  },
  execute: {
    title: 'Executing Import',
    description: 'Creating your project',
    icon: Package,
  },
}

export function ImportWizard({ onCancel, className }: ImportWizardProps) {
  const navigate = useNavigate()

  // State management
  const [currentStep, setCurrentStep] = useState<WizardStep>('select-source')
  const [selectedSource, setSelectedSource] = useState<ImportSource | null>(
    null
  )
  const [selectedWorkload, setSelectedWorkload] =
    useState<WorkloadDescriptor | null>(null)
  const [selectedRepository, setSelectedRepository] =
    useState<RepositoryResponse | null>(null)
  const [selectedConnectionId, setSelectedConnectionId] = useState<
    number | null
  >(null)
  const [selectedPreset, setSelectedPreset] = useState<string>('')
  const [selectedBranch, setSelectedBranch] = useState<string>('')
  const [rootDirectory, setRootDirectory] = useState<string>('./')
  const [projectName, setProjectName] = useState<string>('')
  const [importPlan, setImportPlan] = useState<CreatePlanResponse | null>(null)
  const [sessionId, setSessionId] = useState<string | null>(null)

  // Workload filtering state
  const [workloadSearchTerm, setWorkloadSearchTerm] = useState('')
  const [workloadStatusFilter, setWorkloadStatusFilter] =
    useState<string>('all')
  const [workloadTypeFilter, setWorkloadTypeFilter] = useState<string>('all')
  const [showExtendedFilters, setShowExtendedFilters] = useState(false)

  // Fetch available sources
  const { data: sources, isLoading: sourcesLoading } = useQuery({
    ...listSourcesOptions({}),
  })

  // Fetch repository presets when repository and branch are selected
  const { data: presetData, isLoading: presetLoading } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: selectedRepository?.id || 0 },
      query: { branch: selectedBranch || undefined },
    }),
    enabled:
      !!selectedRepository?.id &&
      !!selectedBranch &&
      currentStep === 'configure-project',
  })

  // Fetch branches when repository is selected
  const { data: branchesData } = useQuery({
    ...getRepositoryBranchesOptions({
      query: { connection_id: selectedConnectionId || 0 },
      path: {
        owner: selectedRepository?.owner || '',
        repo: selectedRepository?.name || '',
      },
    }),
    enabled:
      !!selectedRepository &&
      !!selectedConnectionId &&
      currentStep === 'configure-project',
  })

  // Compute the default preset value based on preset data (similar to ProjectConfigurator)
  const defaultPresetValue = useMemo(() => {
    if (!presetData) return null

    // New schema: use presets array
    if (presetData.presets && presetData.presets.length > 0) {
      const firstPreset = presetData.presets[0]
      const presetName = firstPreset.preset || 'custom'
      const presetPath = firstPreset.path || './'
      return {
        value: `${presetName}::${presetPath}`,
        rootDir: presetPath,
      }
    }
    // Fallback: use 'custom' as default
    return {
      value: 'custom',
      rootDir: './',
    }
  }, [presetData])

  // Auto-set preset and directory when preset data loads or branch changes
  useEffect(() => {
    if (defaultPresetValue) {
      const timeoutId = setTimeout(() => {
        setSelectedPreset(defaultPresetValue.value)
        setRootDirectory(defaultPresetValue.rootDir)
      }, 0)
      return () => clearTimeout(timeoutId)
    }
  }, [defaultPresetValue, selectedBranch])

  // Auto-set default branch when branches load
  useEffect(() => {
    if (branchesData?.branches && !selectedBranch) {
      const defaultBranch =
        branchesData.branches.find((b: any) => b.is_default) ||
        branchesData.branches[0]
      if (defaultBranch) {
        const timeoutId = setTimeout(() => {
          setSelectedBranch(defaultBranch.name)
        }, 0)
        return () => clearTimeout(timeoutId)
      }
    }
  }, [branchesData, selectedBranch])

  // Compute the default source (first available source)
  const defaultSource = useMemo(() => {
    if (!sources || sources.length === 0) return null
    const firstAvailableSource = sources.find(
      (source: ImportSourceInfo) => source.available
    )
    return firstAvailableSource?.source || sources[0]?.source || null
  }, [sources])

  // Auto-select the first source when sources are loaded (one-time initialization)
  useEffect(() => {
    if (defaultSource && !selectedSource) {
      // Use setTimeout to avoid setState during render
      const timeoutId = setTimeout(() => {
        setSelectedSource(defaultSource)
      }, 0)
      return () => clearTimeout(timeoutId)
    }
  }, [defaultSource, selectedSource])

  // Auto-set project name based on repository when configure step loads
  useEffect(() => {
    if (
      currentStep === 'configure-project' &&
      selectedRepository &&
      !projectName
    ) {
      // Use setTimeout to avoid setState during render
      const timeoutId = setTimeout(() => {
        setProjectName(selectedRepository.name || '')
      }, 0)
      return () => clearTimeout(timeoutId)
    }
  }, [currentStep, selectedRepository, projectName])

  // Discover workloads mutation
  const discoverMutation = useMutation({
    ...discoverWorkloadsMutation(),
    meta: {
      errorTitle: 'Failed to discover workloads',
    },
    onSuccess: (data) => {
      toast.success(`Found ${data.workloads.length} workload(s)`)
      setCurrentStep('discover-workloads')
    },
  })

  // Filter workloads based on search term and filters
  const filteredWorkloads = useMemo(() => {
    if (!discoverMutation.data?.workloads) return []

    let filtered = discoverMutation.data.workloads

    // Apply search filter
    if (workloadSearchTerm) {
      const term = workloadSearchTerm.toLowerCase()
      filtered = filtered.filter(
        (w) =>
          w.name?.toLowerCase().includes(term) ||
          w.id?.toLowerCase().includes(term) ||
          w.image?.toLowerCase().includes(term)
      )
    }

    // Apply status filter
    if (workloadStatusFilter !== 'all') {
      filtered = filtered.filter((w) => w.status === workloadStatusFilter)
    }

    // Apply type filter
    if (workloadTypeFilter !== 'all') {
      filtered = filtered.filter((w) => w.workload_type === workloadTypeFilter)
    }

    return filtered
  }, [
    discoverMutation.data,
    workloadSearchTerm,
    workloadStatusFilter,
    workloadTypeFilter,
  ])

  // Get unique statuses and types from workloads for filter options
  const workloadFilters = useMemo(() => {
    if (!discoverMutation.data?.workloads) {
      return { statuses: [], types: [] }
    }

    const statuses = [
      ...new Set(discoverMutation.data.workloads.map((w) => w.status)),
    ]
    const types = [
      ...new Set(discoverMutation.data.workloads.map((w) => w.workload_type)),
    ]

    return { statuses, types }
  }, [discoverMutation.data])

  // Create plan mutation
  const createPlanMut = useMutation({
    ...createPlanMutation(),
    meta: {
      errorTitle: 'Failed to create import plan',
    },
    onSuccess: (data) => {
      setImportPlan(data)
      setSessionId(data.session_id) // Save session_id from plan response
      setCurrentStep('review-plan')
      if (data.can_execute) {
        toast.success('Import plan created successfully')
      } else {
        toast.warning('Plan created but may have issues', {
          description: 'Review the plan before proceeding',
        })
      }
    },
  })

  // Execute import mutation - synchronous execution
  const executeImportMut = useMutation({
    ...executeImportMutation(),
    meta: {
      errorTitle: 'Failed to execute import',
    },
    onSuccess: async (data) => {
      // Import is synchronous, so we get the result immediately
      if (data.project_id) {
        toast.success('Import completed successfully!', {
          description: 'Navigating to your new project...',
        })

        try {
          // Fetch the project to get the slug
          const { data: project } = await getProject({
            path: { id: data.project_id },
          })

          if (project) {
            navigate(`/projects/${project.slug}`)
          } else {
            navigate('/projects')
          }
        } catch {
          toast.error('Failed to fetch project details', {
            description: 'Redirecting to projects list...',
          })
          // Fallback: navigate to projects list if we can't get the project
          setTimeout(() => {
            navigate('/projects')
          }, 1500)
        }
      }
    },
    onError: (error: any) => {
      toast.error('Failed to execute import', {
        description: error?.detail || 'Please try again',
      })
    },
  })

  // Handle source selection
  const handleSourceSelect = (source: ImportSource) => {
    setSelectedSource(source)
  }

  // Handle discover workloads
  const handleDiscoverWorkloads = () => {
    if (!selectedSource) return

    discoverMutation.mutate({
      body: {
        source: selectedSource,
      },
    })
  }

  // Handle workload selection
  const handleWorkloadSelect = (workload: WorkloadDescriptor) => {
    setSelectedWorkload(workload)
  }

  // Handle repository selection
  const handleRepositorySelect = (
    repository: RepositoryResponse,
    connectionId: number
  ) => {
    setSelectedRepository(repository)
    setSelectedConnectionId(connectionId)
  }

  // Handle create plan
  const handleCreatePlan = () => {
    if (!selectedSource || !selectedWorkload) return

    createPlanMut.mutate({
      body: {
        source: selectedSource,
        workload_id: selectedWorkload.id,
        repository_id: selectedRepository?.id || null,
      },
    })
  }

  // Handle execute import
  const handleExecuteImport = () => {
    if (
      !importPlan ||
      !sessionId ||
      !selectedPreset ||
      !selectedBranch ||
      !projectName
    )
      return

    // Extract just the preset name (before ::)
    const presetName = selectedPreset.includes('::')
      ? selectedPreset.split('::')[0]
      : selectedPreset

    executeImportMut.mutate({
      body: {
        session_id: sessionId,
        preset: presetName,
        directory: rootDirectory,
        main_branch: selectedBranch,
        project_name: projectName,
        dry_run: false,
      } as any, // Type assertion until API types are regenerated
    })
  }

  // Handle preset change and update root directory
  const handlePresetChange = (value: string) => {
    setSelectedPreset(value)

    // Update root directory based on preset
    if (value === 'custom') {
      // Keep current directory for custom
      return
    }

    const [, presetPath] = value.split('::')
    if (presetPath && presetPath !== 'root') {
      setRootDirectory(`./${presetPath}`)
    } else {
      setRootDirectory('./')
    }
  }

  // Navigation helpers
  const canGoNext = () => {
    switch (currentStep) {
      case 'select-source':
        return !!selectedSource
      case 'discover-workloads':
        return !!selectedWorkload
      case 'select-repository':
        return !!selectedRepository && !!selectedConnectionId // Only repository is required
      case 'configure-project':
        return (
          !!projectName &&
          !!selectedPreset &&
          !!selectedBranch &&
          !!rootDirectory
        ) // All configuration is mandatory
      case 'review-plan':
        return importPlan?.can_execute || false
      case 'execute':
        return false
      default:
        return false
    }
  }

  const handleNext = () => {
    switch (currentStep) {
      case 'select-source':
        handleDiscoverWorkloads()
        break
      case 'discover-workloads':
        setCurrentStep('select-repository')
        break
      case 'select-repository':
        setCurrentStep('configure-project')
        break
      case 'configure-project':
        handleCreatePlan()
        break
      case 'review-plan':
        handleExecuteImport()
        break
    }
  }

  const handleBack = () => {
    switch (currentStep) {
      case 'discover-workloads':
        setCurrentStep('select-source')
        break
      case 'select-repository':
        setCurrentStep('discover-workloads')
        break
      case 'configure-project':
        setCurrentStep('select-repository')
        break
      case 'review-plan':
        setCurrentStep('configure-project')
        break
    }
  }

  // Render step content
  const renderStepContent = () => {
    switch (currentStep) {
      case 'select-source':
        return (
          <div className="space-y-4">
            {sourcesLoading ? (
              <div className="space-y-3">
                <Skeleton className="h-24 w-full" />
                <Skeleton className="h-24 w-full" />
                <Skeleton className="h-24 w-full" />
              </div>
            ) : (
              <RadioGroup
                value={selectedSource || ''}
                onValueChange={(value) =>
                  handleSourceSelect(value as ImportSource)
                }
              >
                <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
                  {sources?.map((source: ImportSourceInfo) => (
                    <Card
                      key={source.source}
                      className={cn(
                        'cursor-pointer transition-all hover:bg-muted/50',
                        selectedSource === source.source &&
                          'ring-2 ring-primary'
                      )}
                      onClick={() => handleSourceSelect(source.source)}
                    >
                      <CardHeader>
                        <div className="flex items-center justify-between">
                          <div className="flex items-center gap-3">
                            <Server className="h-8 w-8 text-primary" />
                            <div>
                              <CardTitle className="text-base">
                                {source.name}
                              </CardTitle>
                              <CardDescription className="text-xs">
                                v{source.version}
                              </CardDescription>
                            </div>
                          </div>
                          <RadioGroupItem value={source.source} />
                        </div>
                      </CardHeader>
                      <CardContent>
                        <div className="flex flex-wrap gap-2">
                          {source.available ? (
                            <Badge variant="outline" className="text-xs">
                              <CheckCircle2 className="h-3 w-3 mr-1" />
                              Available
                            </Badge>
                          ) : (
                            <Badge variant="destructive" className="text-xs">
                              <AlertCircle className="h-3 w-3 mr-1" />
                              Unavailable
                            </Badge>
                          )}
                        </div>
                      </CardContent>
                    </Card>
                  ))}
                </div>
              </RadioGroup>
            )}
          </div>
        )

      case 'discover-workloads':
        return (
          <div className="space-y-4">
            {discoverMutation.isPending ? (
              <div className="flex flex-col items-center justify-center py-12">
                <Loader2 className="h-8 w-8 animate-spin text-primary mb-4" />
                <p className="text-sm text-muted-foreground">
                  Discovering workloads from {selectedSource}...
                </p>
              </div>
            ) : discoverMutation.data?.workloads ? (
              <>
                {/* Search and Filter Controls */}
                <div className="space-y-3">
                  {/* Search Bar */}
                  <div className="relative">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                    <Input
                      placeholder="Search workloads..."
                      value={workloadSearchTerm}
                      onChange={(e) => setWorkloadSearchTerm(e.target.value)}
                      className="pl-9 pr-9"
                      autoFocus
                    />
                    {workloadSearchTerm && (
                      <Button
                        variant="ghost"
                        size="sm"
                        className="absolute right-1 top-1/2 -translate-y-1/2 h-7 w-7 p-0"
                        onClick={() => setWorkloadSearchTerm('')}
                      >
                        <X className="h-4 w-4" />
                      </Button>
                    )}
                  </div>

                  {/* Toggle Extended Filters Button */}
                  <div className="flex items-center justify-between">
                    <p className="text-sm text-muted-foreground">
                      Found {discoverMutation.data.workloads.length} workload(s)
                      {filteredWorkloads.length !==
                        discoverMutation.data.workloads.length &&
                        ` • Showing ${filteredWorkloads.length}`}
                    </p>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setShowExtendedFilters(!showExtendedFilters)
                      }
                    >
                      <Filter className="h-4 w-4 mr-2" />
                      {showExtendedFilters ? 'Hide' : 'Show'} Filters
                    </Button>
                  </div>

                  {/* Extended Filters (Hidden by Default) */}
                  {showExtendedFilters && (
                    <Card>
                      <CardHeader>
                        <CardTitle className="text-base">
                          Advanced Filters
                        </CardTitle>
                      </CardHeader>
                      <CardContent className="space-y-3">
                        <div className="grid grid-cols-2 gap-3">
                          <div>
                            <Label>Status</Label>
                            <Select
                              value={workloadStatusFilter}
                              onValueChange={setWorkloadStatusFilter}
                            >
                              <SelectTrigger className="mt-2">
                                <SelectValue />
                              </SelectTrigger>
                              <SelectContent>
                                <SelectItem value="all">
                                  All Statuses
                                </SelectItem>
                                {workloadFilters.statuses.map((status) => (
                                  <SelectItem key={status} value={status}>
                                    {status}
                                  </SelectItem>
                                ))}
                              </SelectContent>
                            </Select>
                          </div>

                          <div>
                            <Label>Type</Label>
                            <Select
                              value={workloadTypeFilter}
                              onValueChange={setWorkloadTypeFilter}
                            >
                              <SelectTrigger className="mt-2">
                                <SelectValue />
                              </SelectTrigger>
                              <SelectContent>
                                <SelectItem value="all">All Types</SelectItem>
                                {workloadFilters.types.map((type) => (
                                  <SelectItem key={type} value={type}>
                                    {type}
                                  </SelectItem>
                                ))}
                              </SelectContent>
                            </Select>
                          </div>
                        </div>

                        {/* Clear Filters */}
                        {(workloadStatusFilter !== 'all' ||
                          workloadTypeFilter !== 'all') && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setWorkloadStatusFilter('all')
                              setWorkloadTypeFilter('all')
                            }}
                            className="w-full"
                          >
                            <X className="h-4 w-4 mr-2" />
                            Clear Advanced Filters
                          </Button>
                        )}
                      </CardContent>
                    </Card>
                  )}
                </div>

                {/* Workload List */}
                {filteredWorkloads.length === 0 ? (
                  <Card className="border-dashed">
                    <CardContent className="flex flex-col items-center justify-center py-12">
                      <Container className="h-12 w-12 text-muted-foreground mb-4" />
                      <p className="text-sm font-medium">No workloads found</p>
                      <p className="text-xs text-muted-foreground mt-1">
                        Try adjusting your filters
                      </p>
                    </CardContent>
                  </Card>
                ) : (
                  <RadioGroup
                    value={selectedWorkload?.id || ''}
                    onValueChange={(value) => {
                      const workload = filteredWorkloads.find(
                        (w) => w.id === value
                      )
                      if (workload) handleWorkloadSelect(workload)
                    }}
                  >
                    <div className="space-y-3">
                      {filteredWorkloads.map((workload) => (
                        <Card
                          key={workload.id}
                          className={cn(
                            'cursor-pointer transition-all hover:bg-muted/50',
                            selectedWorkload?.id === workload.id &&
                              'ring-2 ring-primary'
                          )}
                          onClick={() => handleWorkloadSelect(workload)}
                        >
                          <CardHeader className="pb-3">
                            <div className="flex items-center justify-between">
                              <div className="flex items-center gap-3">
                                <Container className="h-6 w-6 text-primary" />
                                <div>
                                  <CardTitle className="text-sm">
                                    {workload.name || workload.id}
                                  </CardTitle>
                                  <CardDescription className="text-xs">
                                    {workload.image || workload.workload_type}
                                  </CardDescription>
                                </div>
                              </div>
                              <RadioGroupItem value={workload.id} />
                            </div>
                          </CardHeader>
                          <CardContent className="pt-0">
                            <div className="flex flex-wrap gap-2">
                              <Badge variant="secondary" className="text-xs">
                                {workload.status}
                              </Badge>
                              <Badge variant="outline" className="text-xs">
                                {workload.workload_type}
                              </Badge>
                            </div>
                          </CardContent>
                        </Card>
                      ))}
                    </div>
                  </RadioGroup>
                )}
              </>
            ) : null}
          </div>
        )

      case 'select-repository':
        return (
          <div className="space-y-4">
            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Select a repository to link with the imported workload. This is
                required for automatic preset detection and deployment
                integration.
              </AlertDescription>
            </Alert>

            <RepositorySelector
              value={selectedRepository}
              onChange={handleRepositorySelect}
              showSearch={true}
            />
          </div>
        )

      case 'configure-project':
        return (
          <div className="space-y-4">
            <Alert>
              <AlertCircle className="h-4 w-4" />
              <AlertDescription>
                Configure your project settings. The framework preset is
                auto-detected from your repository.
              </AlertDescription>
            </Alert>

            {/* Single Card for All Configuration */}
            <Card>
              <CardContent className="pt-6 space-y-6">
                {/* Project Name */}
                <div className="space-y-2">
                  <Label className="text-base font-semibold">Project Name</Label>
                  <p className="text-sm text-muted-foreground">
                    Enter a name for your project
                  </p>
                  <Input
                    value={projectName}
                    onChange={(e) => setProjectName(e.target.value)}
                    placeholder="my-awesome-project"
                    autoFocus
                  />
                </div>

                {/* Branch Selection */}
                <div className="space-y-2">
                  <Label className="text-base font-semibold">Branch</Label>
                  <p className="text-sm text-muted-foreground">
                    Select the branch to deploy from
                  </p>
                  {selectedRepository && selectedConnectionId ? (
                    <BranchSelector
                      repoOwner={selectedRepository.owner || ''}
                      repoName={selectedRepository.name || ''}
                      connectionId={selectedConnectionId}
                      defaultBranch={
                        selectedRepository.default_branch || 'main'
                      }
                      value={selectedBranch}
                      onChange={setSelectedBranch}
                    />
                  ) : (
                    <Input
                      value={selectedBranch}
                      onChange={(e) => setSelectedBranch(e.target.value)}
                      placeholder="main"
                    />
                  )}
                </div>

                {/* Framework Preset */}
                <div className="space-y-2">
                  <Label className="text-base font-semibold">Framework Preset</Label>
                  <p className="text-sm text-muted-foreground">
                    Configure the project type based on the detected framework
                  </p>
                  {!selectedBranch ? (
                    <Alert>
                      <AlertCircle className="h-4 w-4" />
                      <AlertDescription>
                        Please select a branch first to detect framework presets
                      </AlertDescription>
                    </Alert>
                  ) : (
                    <div className="space-y-4">
                      <FrameworkSelector
                        presetData={presetData}
                        isLoading={presetLoading}
                        selectedPreset={selectedPreset}
                        onSelectPreset={handlePresetChange}
                      />

                      <div>
                        <Label>Root Directory</Label>
                        <Input
                          value={rootDirectory}
                          onChange={(e) => setRootDirectory(e.target.value)}
                          placeholder="./"
                          className="mt-2"
                          readOnly={selectedPreset !== 'custom'}
                        />
                        <p className="text-xs text-muted-foreground mt-1">
                          {selectedPreset !== 'custom'
                            ? 'Directory is set based on the selected preset'
                            : 'Enter the root directory for your custom configuration'}
                        </p>
                      </div>
                    </div>
                  )}
                </div>
              </CardContent>
            </Card>
          </div>
        )

      case 'review-plan':
        return (
          <div className="space-y-4">
            {createPlanMut.isPending ? (
              <div className="flex flex-col items-center justify-center py-12">
                <Loader2 className="h-8 w-8 animate-spin text-primary mb-4" />
                <p className="text-sm text-muted-foreground">
                  Creating import plan...
                </p>
              </div>
            ) : importPlan ? (
              <>
                {!importPlan.can_execute && (
                  <Alert variant="destructive">
                    <AlertCircle className="h-4 w-4" />
                    <AlertDescription>
                      This plan may have issues. Please review carefully before
                      proceeding.
                    </AlertDescription>
                  </Alert>
                )}

                <Card>
                  <CardHeader>
                    <CardTitle>Project Configuration</CardTitle>
                  </CardHeader>
                  <CardContent className="space-y-3">
                    <div className="grid grid-cols-2 gap-2 text-sm">
                      <div className="text-muted-foreground">Project Name:</div>
                      <div className="font-medium">{projectName}</div>

                      <div className="text-muted-foreground">
                        Generated Name:
                      </div>
                      <div className="font-medium">
                        {importPlan.plan.project.name}
                      </div>

                      <div className="text-muted-foreground">Type:</div>
                      <div className="font-medium">
                        {importPlan.plan.project.project_type}
                      </div>

                      <div className="text-muted-foreground">
                        Selected Preset:
                      </div>
                      <div className="font-medium">
                        {selectedPreset?.split('::')[0] || 'custom'}
                      </div>

                      <div className="text-muted-foreground">
                        Root Directory:
                      </div>
                      <div className="font-medium">
                        <code className="px-2 py-1 bg-muted rounded text-xs">
                          {rootDirectory}
                        </code>
                      </div>

                      <div className="text-muted-foreground">Branch:</div>
                      <div className="font-medium">
                        <code className="px-2 py-1 bg-muted rounded text-xs">
                          {selectedBranch}
                        </code>
                      </div>

                      <div className="text-muted-foreground">Source:</div>
                      <div className="font-medium">
                        {importPlan.plan.source}
                      </div>

                      {selectedRepository && (
                        <>
                          <div className="text-muted-foreground">
                            Repository:
                          </div>
                          <div className="font-medium">
                            {selectedRepository.full_name}
                          </div>
                        </>
                      )}
                    </div>
                  </CardContent>
                </Card>

                {importPlan.plan.deployment.env_vars &&
                  Object.keys(importPlan.plan.deployment.env_vars).length >
                    0 && (
                    <Card>
                      <CardHeader>
                        <CardTitle>Environment Variables</CardTitle>
                        <CardDescription>
                          {Object.keys(importPlan.plan.deployment.env_vars)
                            .length}{' '}
                          environment variable
                          {Object.keys(importPlan.plan.deployment.env_vars)
                            .length === 1
                            ? ''
                            : 's'}{' '}
                          detected
                        </CardDescription>
                      </CardHeader>
                      <CardContent>
                        <div className="space-y-1">
                          {Object.entries(
                            importPlan.plan.deployment.env_vars
                          ).map(([envKey, envValue]) => {
                            // Extract the actual key and value from the object structure
                            let key = envKey
                            let value = ''

                            if (typeof envValue === 'object' && envValue !== null) {
                              // If it's an object with 'key' and 'value' properties
                              const envObj = envValue as any
                              key = envObj.key || envKey
                              value = envObj.value || String(envValue)
                            } else {
                              value = String(envValue)
                            }

                            const shouldMask = shouldMaskValue(key)

                            return (
                              <div
                                key={key}
                                className="grid grid-cols-[200px_1fr] gap-2 items-start py-2 border-b last:border-0"
                              >
                                <code className="px-2 py-1 bg-muted rounded text-xs font-medium truncate">
                                  {key}
                                </code>
                                {shouldMask ? (
                                  <MaskedValue value={value} />
                                ) : (
                                  <code className="px-2 py-1 bg-muted rounded text-xs break-all">
                                    {value}
                                  </code>
                                )}
                              </div>
                            )
                          })}
                        </div>
                      </CardContent>
                    </Card>
                  )}
              </>
            ) : null}
          </div>
        )

      case 'execute':
        return (
          <div className="space-y-4">
            <div className="flex flex-col items-center justify-center py-12">
              {executeImportMut.isPending ? (
                <>
                  <Loader2 className="h-12 w-12 animate-spin text-primary mb-4" />
                  <p className="text-lg font-medium">Executing Import...</p>
                  <p className="text-sm text-muted-foreground">
                    Please wait while we create your project...
                  </p>
                </>
              ) : executeImportMut.isSuccess ? (
                <>
                  <CheckCircle2 className="h-12 w-12 text-green-500 mb-4" />
                  <p className="text-lg font-medium">Import Completed!</p>
                  <p className="text-sm text-muted-foreground">
                    Redirecting to your project...
                  </p>
                </>
              ) : executeImportMut.isError ? (
                <>
                  <AlertCircle className="h-12 w-12 text-destructive mb-4" />
                  <p className="text-lg font-medium">Import Failed</p>
                  <p className="text-sm text-muted-foreground">
                    {(executeImportMut.error as any)?.detail ||
                      'An error occurred'}
                  </p>
                </>
              ) : null}
            </div>

            {executeImportMut.data && (
              <Card>
                <CardHeader>
                  <CardTitle>Import Results</CardTitle>
                </CardHeader>
                <CardContent className="space-y-2">
                  <div className="grid grid-cols-2 gap-2 text-sm">
                    <div className="text-muted-foreground">Status:</div>
                    <div>
                      <Badge variant="default">Completed</Badge>
                    </div>

                    {executeImportMut.data.project_id && (
                      <>
                        <div className="text-muted-foreground">Project ID:</div>
                        <div className="font-medium">
                          {executeImportMut.data.project_id}
                        </div>
                      </>
                    )}

                    {executeImportMut.data.deployment_id && (
                      <>
                        <div className="text-muted-foreground">
                          Deployment ID:
                        </div>
                        <div className="font-medium">
                          {executeImportMut.data.deployment_id}
                        </div>
                      </>
                    )}

                    {executeImportMut.data.environment_id && (
                      <>
                        <div className="text-muted-foreground">
                          Environment ID:
                        </div>
                        <div className="font-medium">
                          {executeImportMut.data.environment_id}
                        </div>
                      </>
                    )}
                  </div>
                </CardContent>
              </Card>
            )}
          </div>
        )

      default:
        return null
    }
  }

  const currentStepConfig = STEP_CONFIG[currentStep]
  const StepIcon = currentStepConfig.icon

  return (
    <div className={cn('space-y-6', className)}>
      {/* Header */}
      <div className="flex items-center gap-4">
        <div className="flex items-center justify-center h-12 w-12 rounded-lg bg-primary/10">
          <StepIcon className="h-6 w-6 text-primary" />
        </div>
        <div>
          <h2 className="text-2xl font-bold">{currentStepConfig.title}</h2>
          <p className="text-sm text-muted-foreground">
            {currentStepConfig.description}
          </p>
        </div>
      </div>

      {/* Progress indicator */}
      <div className="flex items-center gap-2">
        {Object.keys(STEP_CONFIG).map((step, index) => {
          const stepIndex = Object.keys(STEP_CONFIG).indexOf(currentStep)
          const isActive = step === currentStep
          const isCompleted = index < stepIndex

          return (
            <div key={step} className="flex items-center flex-1">
              <div
                className={cn(
                  'h-2 rounded-full flex-1 transition-colors',
                  isCompleted && 'bg-primary',
                  isActive && 'bg-primary/50',
                  !isActive && !isCompleted && 'bg-muted'
                )}
              />
            </div>
          )
        })}
      </div>

      {/* Step content */}
      <Card>
        <CardContent className="pt-6">{renderStepContent()}</CardContent>
      </Card>

      {/* Navigation buttons */}
      <div className="flex justify-between gap-3">
        <div>
          {currentStep !== 'select-source' && currentStep !== 'execute' && (
            <Button variant="outline" onClick={handleBack}>
              <ArrowLeft className="h-4 w-4 mr-2" />
              Back
            </Button>
          )}
        </div>

        <div className="flex gap-3">
          {onCancel && currentStep !== 'execute' && (
            <Button variant="outline" onClick={onCancel}>
              Cancel
            </Button>
          )}

          {currentStep !== 'execute' && (
            <Button
              onClick={handleNext}
              disabled={
                !canGoNext() ||
                discoverMutation.isPending ||
                createPlanMut.isPending ||
                executeImportMut.isPending
              }
            >
              {currentStep === 'select-source' && 'Discover Workloads'}
              {currentStep === 'discover-workloads' && 'Continue'}
              {currentStep === 'select-repository' && 'Configure Project'}
              {currentStep === 'configure-project' && 'Create Plan'}
              {currentStep === 'review-plan' && 'Execute Import'}
              <ArrowRight className="h-4 w-4 ml-2" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}
