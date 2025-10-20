import {
  ProjectResponse,
  RepositoryResponse,
  getRepositoryBranches,
  listRepositoriesByConnection,
} from '@/api/client'
import {
  getRepositoryPresetLiveOptions,
  listConnectionsOptions,
  listGitProvidersOptions,
  updateAutomaticDeployMutation,
  updateGitSettingsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { RepositorySelector } from '@/components/repositories/RepositorySelector'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ScrollArea } from '@/components/ui/scroll-area'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import GithubIcon from '@/icons/Github'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  Check,
  FolderIcon,
  GitBranchIcon,
  Loader2,
  Plus,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import { z } from 'zod'
import FrameworkIcon from '../FrameworkIcon'
import { TimeAgo } from '@/components/utils/TimeAgo'

interface GitSettingsProps {
  project: ProjectResponse
  refetch: () => void
}

// Unified schema for all git settings
const gitSettingsSchema = z.object({
  branch: z.string(),
  preset: z.string().optional(),
  directory: z.string().optional(),
})

type GitSettingsFormValues = z.infer<typeof gitSettingsSchema>

function getGithubRepoUrl(owner: string, repo: string) {
  return `https://github.com/${owner}/${repo}`
}

export function GitSettings({ project, refetch }: GitSettingsProps) {
  const navigate = useNavigate()
  const updateGithubRepo = useMutation({
    ...updateGitSettingsMutation(),
    meta: {
      errorTitle: 'Failed to update git settings',
    },
  })
  const updateAutomaticDeploy = useMutation({
    ...updateAutomaticDeployMutation(),
    meta: {
      errorTitle: 'Failed to update automatic deploy settings',
    },
  })
  const [showAllPresets, setShowAllPresets] = useState(false)
  const [isEditingSettings, setIsEditingSettings] = useState(false)
  const [isManualMode, setIsManualMode] = useState(false)
  const [isDirectoryDialogOpen, setIsDirectoryDialogOpen] = useState(false)
  const [showCustomDirectory, setShowCustomDirectory] = useState(false)
  const [customDirectoryInput, setCustomDirectoryInput] = useState('')
  const [isCustomBranch, setIsCustomBranch] = useState(false)
  const [customBranch, setCustomBranch] = useState('')
  const [selectedProvider, setSelectedProvider] = useState<number | null>(null)
  const [selectedRepository, setSelectedRepository] =
    useState<RepositoryResponse | null>(null)
  const [isSelectingRepository, setIsSelectingRepository] = useState(false)

  // Unified form for all git settings
  const form = useForm<GitSettingsFormValues>({
    resolver: zodResolver(gitSettingsSchema),
    defaultValues: {
      branch: project?.main_branch || '',
      preset: project?.preset || '',
      directory: project?.directory || '',
    },
  })

  // Fetch git providers
  const { data: providersData, isLoading: isLoadingProviders } = useQuery({
    ...listGitProvidersOptions(),
  })

  const providers = useMemo(() => providersData || [], [providersData])
  const hasProviders = useMemo(() => providers.length > 0, [providers])

  // Fetch connections to get the current connection details
  const { data: connectionsData } = useQuery({
    ...listConnectionsOptions(),
  })

  // Find the current connection
  const currentConnection = useMemo(
    () =>
      connectionsData?.connections?.find(
        (conn) => conn.id === project?.git_provider_connection_id
      ),
    [connectionsData, project]
  )
  const currentProvider = useMemo(
    () =>
      providers.find(
        (provider) => provider.id === currentConnection?.provider_id
      ),
    [providers, currentConnection?.provider_id]
  )
  // Set selected provider based on project's git provider connection
  useEffect(() => {
    if (project?.git_provider_connection_id && providers.length > 0) {
      setSelectedProvider(project.git_provider_connection_id)
    }
  }, [project?.git_provider_connection_id, providers])

  // Fetch branches from repository
  const {
    data: branchesData,
    isLoading: isLoadingBranches,
    refetch: refetchBranches,
  } = useQuery({
    queryKey: [
      'repository-branches',
      project?.repo_owner,
      project?.repo_name,
      project?.git_provider_connection_id,
    ],
    queryFn: async () => {
      if (
        !project?.repo_owner ||
        !project?.repo_name ||
        !project?.git_provider_connection_id
      ) {
        return { branches: [] }
      }
      try {
        const response = await getRepositoryBranches({
          path: {
            owner: project.repo_owner,
            repo: project.repo_name,
          },
          query: {
            connection_id: project.git_provider_connection_id,
          },
        })
        return response.data || { branches: [] }
      } catch (error) {
        console.error('Failed to fetch branches:', error)
        return { branches: [] }
      }
    },
    enabled:
      !!project?.repo_owner &&
      !!project?.repo_name &&
      !!project?.git_provider_connection_id,
  })

  const branches = useMemo(() => branchesData?.branches || [], [branchesData])
  const currentBranch = useWatch({ control: form.control, name: 'branch' })

  // Get repository ID for live preset detection
  const { data: repositoryData } = useQuery({
    queryKey: [
      'repository-search',
      project?.repo_owner,
      project?.repo_name,
      project?.git_provider_connection_id,
    ],
    queryFn: async () => {
      if (
        !project?.repo_owner ||
        !project?.repo_name ||
        !project?.git_provider_connection_id
      ) {
        return null
      }
      try {
        const response = await listRepositoriesByConnection({
          path: { connection_id: project.git_provider_connection_id },
          query: { search: project.repo_name, per_page: 100 },
          throwOnError: true,
        })

        const repo = response.data?.repositories?.find(
          (r: any) =>
            r.owner === project.repo_owner && r.name === project.repo_name
        )
        return repo || null
      } catch (error) {
        console.error('Failed to find repository:', error)
        return null
      }
    },
    enabled:
      !!project?.repo_owner &&
      !!project?.repo_name &&
      !!project?.git_provider_connection_id,
  })

  // Check if current branch is in the list or is custom
  useEffect(() => {
    if (currentBranch && branches.length > 0) {
      const branchNames = branches.map((b: any) => b.name || b)
      if (!branchNames.includes(currentBranch)) {
        setIsCustomBranch(true)
        setCustomBranch(currentBranch)
      }
    }
  }, [currentBranch, branches])

  // Get live preset detection for the repository
  const presetQuery = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repositoryData?.id || 0 },
    }),
    enabled: !!repositoryData?.id,
  })

  const presets = useMemo(() => {
    if (presetQuery.data?.projects && presetQuery.data.projects.length > 0) {
      // Map live preset data from projects
      const projectPresets = presetQuery.data.projects.map((project: any) => ({
        value: project.preset,
        label: project.preset_label || project.preset,
        directory: project.path || './',
      }))

      // Add root preset if available and not already in projects
      if (
        presetQuery.data.root_preset &&
        !presetQuery.data.projects.some(
          (p: any) => p.preset === presetQuery.data.root_preset
        )
      ) {
        projectPresets.unshift({
          value: presetQuery.data.root_preset,
          label: presetQuery.data.root_preset,
          directory: './',
        })
      }

      return projectPresets
    }

    // Fallback to default presets if no live data
    return [
      { value: 'nextjs', label: 'Next.js', directory: './' },
      { value: 'vite', label: 'Vite', directory: './' },
      { value: 'rsbuild', label: 'RSBuild', directory: './' },
    ]
  }, [presetQuery.data])

  // Unified handler for all git settings
  const handleUpdateSettings = async (values: GitSettingsFormValues) => {
    try {
      await updateGithubRepo.mutateAsync({
        body: {
          main_branch: values.branch,
          preset: values.preset,
          directory: values.directory!,
          repo_owner: project.repo_owner!,
          repo_name: project.repo_name!,
        },
        path: { project_id: project.id },
      })
      toast.success('Git settings updated successfully')
      setIsEditingSettings(false)
      refetch()
    } catch (error) {
      console.error('Failed to update git settings:', error)
      toast.error('Failed to update git settings')
    }
  }

  const handleRepositorySelect = async (repo: RepositoryResponse | null) => {
    if (!repo) {
      setSelectedRepository(null)
      return
    }

    setSelectedRepository(repo)

    // Update the project with the selected repository
    try {
      // Update repository information
      await updateGithubRepo.mutateAsync({
        body: {
          repo_owner: repo.owner,
          repo_name: repo.name,
          directory: form.getValues('directory') || './',
          preset: form.getValues('preset'),
          main_branch:
            form.getValues('branch') || repo.default_branch || 'main',
        },
        path: { project_id: project.id },
      })

      // Note: git_provider_connection_id should be updated through a separate API
      // if available in the future. For now, the backend should maintain the association
      // based on the repository owner/name and the active git provider connection.

      toast.success('Repository connected successfully')
      refetch()
      setIsSelectingRepository(false)

      // Update the form values to reflect the new repository
      if (repo.default_branch) {
        form.setValue('branch', repo.default_branch)
      }
    } catch (error) {
      console.error('Failed to connect repository:', error)
      toast.error('Failed to connect repository')
      setSelectedRepository(null)
    }
  }

  const handleAutoDeployToggle = async (enabled: boolean) => {
    if (!project?.id) return

    await toast.promise(
      updateAutomaticDeploy.mutateAsync({
        path: { project_id: project.id! },
        body: {
          automatic_deploy: enabled,
        },
      }),
      {
        loading: 'Updating deployment settings...',
        success: 'Deployment settings updated successfully',
        error: 'Failed to update deployment settings',
      }
    )
    refetch()
  }
  const directory = useWatch({ control: form.control, name: 'directory' })
  return (
    <div className="space-y-6">
      <h3 className="text-lg font-medium">Git Settings</h3>
      <p className="text-sm text-muted-foreground">
        Manage Git repository settings for your project.
      </p>

      {project.repo_owner && project.repo_name ? (
        <div className="space-y-6">
          <Form {...form}>
            <form onSubmit={form.handleSubmit(handleUpdateSettings)}>
              <Card>
                <CardHeader>
                  <div className="flex items-center justify-between">
                    <div>
                      <CardTitle>Git Settings</CardTitle>
                      <CardDescription>
                        Configure repository, branch, and framework settings.
                      </CardDescription>
                    </div>
                    {!isEditingSettings && (
                      <Button
                        type="button"
                        variant="outline"
                        onClick={() => setIsEditingSettings(true)}
                      >
                        Edit Settings
                      </Button>
                    )}
                  </div>
                </CardHeader>
                <CardContent className="space-y-6">
                  {/* Repository Info */}
                  <div className="space-y-2">
                    <Label>Connected Repository</Label>
                    <div className="flex items-center gap-2 p-4 rounded-lg border bg-muted/50">
                      <GithubIcon className="h-5 w-5" />
                      <a
                        href={getGithubRepoUrl(
                          project.repo_owner,
                          project.repo_name
                        )}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="font-medium hover:underline"
                      >
                        {project.repo_owner}/{project.repo_name}
                      </a>
                    </div>
                    <p className="text-xs text-muted-foreground">
                      Seamlessly create Deployments for any commits pushed to
                      your Git repository.
                    </p>
                  </div>

                  {/* Git Connection Info */}
                  <div className="space-y-2">
                    <Label>Git Provider Connection</Label>
                    {currentConnection ? (
                      <div className="flex items-center gap-3 p-4 rounded-lg border bg-card">
                        {currentProvider?.provider_type === 'github' ||
                        currentProvider?.provider_type === 'github_app' ? (
                          <GithubIcon className="h-6 w-6" />
                        ) : (
                          <GitBranchIcon className="h-6 w-6" />
                        )}
                        <div className="flex-1 space-y-1">
                          <div className="flex items-center gap-2">
                            <span className="font-medium">
                              {currentConnection.account_name}
                            </span>
                            <Badge variant="secondary" className="text-xs">
                              {currentProvider?.name}
                            </Badge>
                          </div>
                          {currentConnection.created_at && (
                            <div className="text-xs text-muted-foreground">
                              Connected{' '}
                              <TimeAgo date={currentConnection.created_at} />
                            </div>
                          )}
                        </div>
                      </div>
                    ) : (
                      <div className="flex items-center gap-2 p-3 rounded-lg border bg-muted/50">
                        <span className="text-sm text-muted-foreground">
                          No connection found
                        </span>
                      </div>
                    )}
                    <p className="text-xs text-muted-foreground">
                      The git provider connection used for this project.
                    </p>
                  </div>

                  {isEditingSettings ? (
                    <>
                      {/* Branch Settings */}
                      <FormField
                        control={form.control}
                        name="branch"
                        render={({ field }) => (
                          <FormItem>
                            <div className="flex items-center justify-between mb-2">
                              <FormLabel>Main Branch</FormLabel>
                              {project?.repo_owner && project?.repo_name && (
                                <Button
                                  type="button"
                                  variant="ghost"
                                  size="sm"
                                  onClick={() => refetchBranches()}
                                  disabled={isLoadingBranches}
                                >
                                  {isLoadingBranches ? (
                                    <Loader2 className="h-4 w-4 animate-spin" />
                                  ) : (
                                    <RefreshCw className="h-4 w-4" />
                                  )}
                                  <span className="ml-2">Refresh</span>
                                </Button>
                              )}
                            </div>
                            <FormControl>
                              {isLoadingBranches ? (
                                <div className="flex items-center gap-2 text-sm text-muted-foreground">
                                  <Loader2 className="h-4 w-4 animate-spin" />
                                  Loading branches...
                                </div>
                              ) : branches.length === 0 ? (
                                <Input {...field} placeholder="main" />
                              ) : !isCustomBranch ? (
                                <Select
                                  value={field.value}
                                  onValueChange={(value) => {
                                    if (value === 'custom') {
                                      setIsCustomBranch(true)
                                      field.onChange(customBranch || 'main')
                                    } else {
                                      setIsCustomBranch(false)
                                      setCustomBranch('')
                                      field.onChange(value)
                                    }
                                  }}
                                >
                                  <SelectTrigger>
                                    <SelectValue placeholder="Select a branch" />
                                  </SelectTrigger>
                                  <SelectContent>
                                    {branches.map((branch: any) => {
                                      const branchName = branch.name || branch
                                      return (
                                        <SelectItem
                                          key={branchName}
                                          value={branchName}
                                        >
                                          <div className="flex items-center gap-2">
                                            <GitBranchIcon className="h-4 w-4" />
                                            {branchName}
                                            {branchName ===
                                              project?.main_branch && (
                                              <Check className="h-3 w-3 text-green-500 ml-1" />
                                            )}
                                          </div>
                                        </SelectItem>
                                      )
                                    })}
                                    <SelectItem value="custom">
                                      <div className="flex items-center gap-2 text-muted-foreground">
                                        <GitBranchIcon className="h-4 w-4" />
                                        Custom branch...
                                      </div>
                                    </SelectItem>
                                  </SelectContent>
                                </Select>
                              ) : (
                                <div className="space-y-2">
                                  <Input
                                    {...field}
                                    value={field.value}
                                    onChange={(e) => {
                                      setCustomBranch(e.target.value)
                                      field.onChange(e.target.value)
                                    }}
                                    placeholder="Enter custom branch name"
                                  />
                                  {branches.length > 0 && (
                                    <Button
                                      type="button"
                                      variant="link"
                                      size="sm"
                                      className="text-xs"
                                      onClick={() => {
                                        setIsCustomBranch(false)
                                        field.onChange(
                                          branches[0]?.name ||
                                            branches[0] ||
                                            'main'
                                        )
                                      }}
                                    >
                                      ← Back to branch list
                                    </Button>
                                  )}
                                </div>
                              )}
                            </FormControl>
                            <FormDescription>
                              The default branch to deploy from
                            </FormDescription>
                            <FormMessage />
                          </FormItem>
                        )}
                      />

                      {/* Mode Toggle */}
                      <div className="flex items-center justify-between p-3 rounded-lg border bg-muted/50">
                        <div className="flex items-center gap-2">
                          <Label className="text-sm">Configuration Mode</Label>
                          <p className="text-xs text-muted-foreground">
                            {isManualMode
                              ? 'Manual entry'
                              : 'Auto-detected from repository'}
                          </p>
                        </div>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => setIsManualMode(!isManualMode)}
                        >
                          {isManualMode ? 'Use Auto-detect' : 'Manual Mode'}
                        </Button>
                      </div>

                      {isManualMode ? (
                        <>
                          {/* Manual Mode - Direct Inputs */}
                          <FormField
                            control={form.control}
                            name="preset"
                            render={({ field }) => (
                              <FormItem>
                                <FormLabel>Framework Preset</FormLabel>
                                <FormControl>
                                  <Input
                                    {...field}
                                    placeholder="e.g., nextjs, vite, rsbuild"
                                  />
                                </FormControl>
                                <FormDescription>
                                  Enter the framework preset manually
                                </FormDescription>
                                <FormMessage />
                              </FormItem>
                            )}
                          />

                          <FormField
                            control={form.control}
                            name="directory"
                            render={({ field }) => (
                              <FormItem>
                                <FormLabel>Root Directory</FormLabel>
                                <FormControl>
                                  <Input {...field} placeholder="./" />
                                </FormControl>
                                <FormDescription>
                                  The directory in your repository containing
                                  the project
                                </FormDescription>
                                <FormMessage />
                              </FormItem>
                            )}
                          />
                        </>
                      ) : (
                        <>
                          {/* Auto Mode - Detected Presets */}
                          {presetQuery.isLoading ? (
                            <div className="flex items-center gap-2 text-sm text-muted-foreground p-4">
                              <Loader2 className="h-4 w-4 animate-spin" />
                              Detecting frameworks in repository...
                            </div>
                          ) : (
                            <>
                              <FormField
                                control={form.control}
                                name="preset"
                                render={({ field }) => (
                                  <FormItem>
                                    <FormLabel>Framework Preset</FormLabel>
                                    {showAllPresets ? (
                                      <FormControl>
                                        <div className="space-y-4">
                                          <ScrollArea className="h-[400px] pr-4">
                                            <div className="grid grid-cols-2 gap-4 p-4">
                                              {presets.map((preset) => (
                                                <Card
                                                  key={preset.value}
                                                  className={`cursor-pointer transition-all hover:bg-accent ${field.value === preset.value ? 'ring-2 ring-primary' : ''}`}
                                                  onClick={() => {
                                                    field.onChange(preset.value)
                                                    form.setValue(
                                                      'directory',
                                                      preset.directory
                                                    )
                                                    setShowAllPresets(false)
                                                  }}
                                                >
                                                  <CardContent className="p-6">
                                                    <div className="flex flex-col items-center gap-2 text-center">
                                                      <FrameworkIcon
                                                        preset={
                                                          preset.value as any
                                                        }
                                                      />
                                                      <div className="font-medium">
                                                        {preset.label}
                                                      </div>
                                                      <div className="text-sm text-muted-foreground">
                                                        {preset.directory}
                                                      </div>
                                                    </div>
                                                  </CardContent>
                                                </Card>
                                              ))}
                                            </div>
                                          </ScrollArea>
                                          <Button
                                            type="button"
                                            variant="outline"
                                            className="w-full"
                                            onClick={() =>
                                              setShowAllPresets(false)
                                            }
                                          >
                                            Close
                                          </Button>
                                        </div>
                                      </FormControl>
                                    ) : (
                                      <FormControl>
                                        <div className="flex items-center space-x-4">
                                          <Card className="grow">
                                            <CardContent className="flex items-center justify-between p-4">
                                              <div className="flex items-center space-x-2">
                                                <FrameworkIcon
                                                  preset={field.value as any}
                                                />
                                                <div>
                                                  {presets.find(
                                                    (p) =>
                                                      p.value === field.value
                                                  )?.label ||
                                                    field.value ||
                                                    'Select preset'}
                                                </div>
                                              </div>
                                              <div className="text-sm text-muted-foreground">
                                                {presets.find(
                                                  (p) => p.value === field.value
                                                )?.directory || directory}
                                              </div>
                                            </CardContent>
                                          </Card>
                                          <Button
                                            type="button"
                                            variant="outline"
                                            onClick={() =>
                                              setShowAllPresets(true)
                                            }
                                          >
                                            Change
                                          </Button>
                                        </div>
                                      </FormControl>
                                    )}
                                    <FormDescription>
                                      {presets.length > 0
                                        ? `${presets.length} framework${presets.length > 1 ? 's' : ''} detected in repository`
                                        : 'Using default presets'}
                                    </FormDescription>
                                    <FormMessage />
                                  </FormItem>
                                )}
                              />

                              <FormField
                                control={form.control}
                                name="directory"
                                render={({ field }) => (
                                  <FormItem>
                                    <FormLabel>Root Directory</FormLabel>
                                    <FormControl>
                                      <div className="flex items-center gap-2 p-3 rounded-lg border bg-muted/50">
                                        <FolderIcon className="h-4 w-4 text-muted-foreground" />
                                        <span className="font-mono text-sm">
                                          {field.value}
                                        </span>
                                      </div>
                                    </FormControl>
                                    <FormDescription>
                                      Directory is set automatically based on
                                      selected preset
                                    </FormDescription>
                                    <FormMessage />
                                  </FormItem>
                                )}
                              />
                            </>
                          )}
                        </>
                      )}
                    </>
                  ) : (
                    <>
                      {/* Read-only view */}
                      <div className="space-y-4">
                        <div className="space-y-2">
                          <Label>Main Branch</Label>
                          <div className="flex items-center gap-2 p-3 rounded-lg border bg-muted/50">
                            <GitBranchIcon className="h-4 w-4 text-muted-foreground" />
                            <span className="font-mono text-sm">
                              {project.main_branch}
                            </span>
                          </div>
                        </div>

                        <div className="space-y-2">
                          <Label>Framework Preset</Label>
                          <div className="flex items-center gap-2 p-3 rounded-lg border bg-muted/50">
                            <FrameworkIcon
                              preset={project.preset as any}
                              className="h-5 w-5"
                            />
                            <span>
                              {presets.find((p) => p.value === project.preset)
                                ?.label || project.preset}
                            </span>
                          </div>
                        </div>

                        <div className="space-y-2">
                          <Label>Root Directory</Label>
                          <div className="flex items-center gap-2 p-3 rounded-lg border bg-muted/50">
                            <FolderIcon className="h-4 w-4 text-muted-foreground" />
                            <span className="font-mono text-sm">
                              {project.directory}
                            </span>
                          </div>
                        </div>
                      </div>
                    </>
                  )}
                </CardContent>
                <CardFooter className="flex items-center justify-between">
                  <div className="flex items-center space-x-2">
                    <Switch
                      checked={project.automatic_deploy ?? true}
                      onCheckedChange={handleAutoDeployToggle}
                    />
                    <label className="text-sm font-medium leading-none peer-disabled:cursor-not-allowed peer-disabled:opacity-70">
                      Automatic Deployments{' '}
                      {project.automatic_deploy ? 'Enabled' : 'Disabled'}
                    </label>
                  </div>
                  {isEditingSettings && (
                    <div className="flex gap-2">
                      <Button
                        type="button"
                        variant="outline"
                        onClick={() => setIsEditingSettings(false)}
                      >
                        Cancel
                      </Button>
                      <Button type="submit">Save Changes</Button>
                    </div>
                  )}
                </CardFooter>
              </Card>
            </form>
          </Form>
        </div>
      ) : (
        <div className="space-y-6">
          {/* Check if there are any git providers */}
          {isLoadingProviders ? (
            <Card>
              <CardContent className="p-8">
                <div className="flex items-center justify-center">
                  <Loader2 className="h-8 w-8 animate-spin" />
                  <span className="ml-2">Loading git providers...</span>
                </div>
              </CardContent>
            </Card>
          ) : !hasProviders ? (
            <Card>
              <CardHeader>
                <CardTitle>No Git Providers Connected</CardTitle>
                <CardDescription>
                  Connect a git provider to enable repository integration for
                  your project.
                </CardDescription>
              </CardHeader>
              <CardContent>
                <Alert>
                  <AlertCircle className="h-4 w-4" />
                  <AlertDescription>
                    You need to connect a git provider before you can connect a
                    repository.
                  </AlertDescription>
                </Alert>
              </CardContent>
              <CardFooter>
                <Button onClick={() => navigate('/settings/git-sources')}>
                  <Plus className="mr-2 h-4 w-4" />
                  Add Git Provider
                </Button>
              </CardFooter>
            </Card>
          ) : (
            <Card>
              <CardHeader>
                <CardTitle>Repository Settings</CardTitle>
                <CardDescription>
                  Connect or update the GitHub repository for this project.
                </CardDescription>
              </CardHeader>
              <CardContent className="space-y-6">
                {/* Git Provider Selection */}
                <div className="space-y-2">
                  <Label htmlFor="provider">Git Provider</Label>
                  <Select
                    value={selectedProvider?.toString()}
                    onValueChange={(value) => {
                      setSelectedProvider(Number(value))
                      setSelectedRepository(null)
                    }}
                  >
                    <SelectTrigger id="provider">
                      <SelectValue placeholder="Select a git provider" />
                    </SelectTrigger>
                    <SelectContent>
                      {providers.map((provider) => (
                        <SelectItem
                          key={provider.id}
                          value={provider.id.toString()}
                        >
                          <div className="flex items-center gap-2">
                            <GithubIcon className="h-4 w-4" />
                            {provider.name}
                            {provider.is_default && (
                              <Badge variant="secondary" className="ml-2">
                                Default
                              </Badge>
                            )}
                          </div>
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <p className="text-sm text-muted-foreground">
                    Select the git provider connection to use for this project
                  </p>
                </div>

                {/* Repository Selection */}
                {selectedProvider && (
                  <div className="space-y-2">
                    {isSelectingRepository ? (
                      <RepositorySelector
                        connectionId={selectedProvider}
                        onSelect={handleRepositorySelect}
                        selectedRepository={selectedRepository}
                        title="Select Repository"
                        description="Choose a repository from your connected git provider"
                        showAsCard={false}
                      />
                    ) : (
                      <div>
                        <Label>Repository</Label>
                        <Button
                          type="button"
                          variant="outline"
                          className="w-full justify-start mt-2"
                          onClick={() => setIsSelectingRepository(true)}
                        >
                          <GitBranchIcon className="mr-2 h-4 w-4" />
                          Select Repository
                        </Button>
                        <p className="text-sm text-muted-foreground mt-2">
                          Choose a repository to connect to this project
                        </p>
                      </div>
                    )}
                  </div>
                )}

                {/* Framework Preset Selection */}
                <Form {...form}>
                  <FormField
                    control={form.control}
                    name="preset"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Framework Preset</FormLabel>
                        {showAllPresets ? (
                          <FormControl>
                            <div className="space-y-4">
                              <ScrollArea className="h-[400px] pr-4">
                                <div className="grid grid-cols-2 gap-4 p-4">
                                  {presets.map((preset) => (
                                    <Card
                                      key={preset.value}
                                      className={`cursor-pointer transition-all hover:bg-accent ${field.value === preset.value ? 'ring-2 ring-primary' : ''}`}
                                      onClick={() => {
                                        field.onChange(preset.value)
                                        form.setValue(
                                          'directory',
                                          preset.directory
                                        )
                                        setShowAllPresets(false)
                                      }}
                                    >
                                      <CardContent className="p-6">
                                        <div className="flex flex-col items-center gap-2 text-center">
                                          <FrameworkIcon
                                            preset={preset.value as any}
                                          />
                                          <div className="font-medium">
                                            {preset.label}
                                          </div>
                                          <div className="text-sm text-muted-foreground">
                                            {preset.directory}
                                          </div>
                                        </div>
                                      </CardContent>
                                    </Card>
                                  ))}
                                </div>
                              </ScrollArea>
                            </div>
                          </FormControl>
                        ) : (
                          <FormControl>
                            <div className="flex items-center space-x-4">
                              <Card className="grow">
                                <CardContent className="flex items-center justify-between p-4">
                                  <div className="flex items-center space-x-2">
                                    <FrameworkIcon
                                      preset={field.value as any}
                                    />
                                    <div>
                                      {presets.find(
                                        (p) => p.value === field.value
                                      )?.label || 'Select preset'}
                                    </div>
                                  </div>
                                  <div className="text-sm text-muted-foreground">
                                    {
                                      presets.find(
                                        (p) => p.value === field.value
                                      )?.directory
                                    }
                                  </div>
                                </CardContent>
                              </Card>
                              <Button
                                type="button"
                                variant="outline"
                                onClick={() => setShowAllPresets(true)}
                              >
                                Change
                              </Button>
                            </div>
                          </FormControl>
                        )}
                        <FormMessage />
                      </FormItem>
                    )}
                  />

                  {/* Directory Field */}
                  <FormField
                    control={form.control}
                    name="directory"
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>Directory</FormLabel>
                        <FormControl>
                          <Input {...field} placeholder="./" />
                        </FormControl>
                        <FormDescription>
                          The directory in your repository containing the
                          project to deploy
                        </FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                </Form>
              </CardContent>
            </Card>
          )}
        </div>
      )}

      {/* Directory Selection Dialog */}
      <Dialog
        open={isDirectoryDialogOpen}
        onOpenChange={setIsDirectoryDialogOpen}
      >
        <DialogContent className="sm:max-w-[500px]">
          <DialogHeader>
            <DialogTitle>Root Directory</DialogTitle>
          </DialogHeader>
          <div className="py-4">
            <p className="text-sm text-muted-foreground mb-4">
              Select the directory where your source code is located.
            </p>
            <div className="space-y-2">
              <div className="flex items-center gap-2 mb-4">
                <GithubIcon className="h-5 w-5" />
                <span className="font-medium">
                  {project.repo_owner}/{project.repo_name}
                </span>
              </div>
              <div className="border rounded-md divide-y">
                {presets.map((preset) => (
                  <div
                    key={preset.value}
                    className={`flex items-center gap-3 p-3 cursor-pointer hover:bg-accent ${
                      !showCustomDirectory && directory === preset.directory
                        ? 'bg-accent'
                        : ''
                    }`}
                    onClick={() => {
                      setShowCustomDirectory(false)
                      form.setValue('directory', preset.directory)
                      setIsDirectoryDialogOpen(false)
                    }}
                  >
                    <input
                      type="radio"
                      checked={
                        !showCustomDirectory && directory === preset.directory
                      }
                      onChange={() => {}}
                      className="h-4 w-4"
                    />
                    <div className="flex-1">
                      <div className="flex items-center gap-2">
                        <FrameworkIcon
                          preset={preset.value as any}
                          className="h-5 w-5"
                        />
                        <span>{preset.label}</span>
                      </div>
                      <span className="text-sm text-muted-foreground">
                        {preset.directory}
                      </span>
                    </div>
                  </div>
                ))}
                <div
                  className={`flex items-center gap-3 p-3 cursor-pointer hover:bg-accent ${showCustomDirectory ? 'bg-accent' : ''}`}
                  onClick={() => setShowCustomDirectory(true)}
                >
                  <input
                    type="radio"
                    checked={showCustomDirectory}
                    onChange={() => {}}
                    className="h-4 w-4"
                  />
                  <div className="flex-1">
                    <div className="flex items-center gap-2">
                      <FolderIcon className="h-5 w-5" />
                      <span>Custom Directory</span>
                    </div>
                  </div>
                </div>
              </div>

              {showCustomDirectory && (
                <div className="mt-4 space-y-2">
                  <div>
                    <div className="font-medium text-sm mb-1.5">
                      Enter Directory Path
                    </div>
                    <div className="flex gap-2">
                      <Input
                        value={customDirectoryInput}
                        onChange={(e) =>
                          setCustomDirectoryInput(e.target.value)
                        }
                        placeholder="e.g., ./apps/frontend"
                      />
                      <Button
                        onClick={() => {
                          form.setValue('directory', customDirectoryInput)
                          setIsDirectoryDialogOpen(false)
                        }}
                      >
                        Confirm
                      </Button>
                    </div>
                    <p className="text-sm text-muted-foreground">
                      Enter the relative path to your project&apos;s root
                      directory
                    </p>
                  </div>
                </div>
              )}
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  )
}
