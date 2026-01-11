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
import { FrameworkSelector } from '../FrameworkSelector'

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
  const [isEditingSettings, setIsEditingSettings] = useState(false)
  const [isCustomBranch, setIsCustomBranch] = useState(false)
  const [customBranch, setCustomBranch] = useState('')
  const [selectedProvider, setSelectedProvider] = useState<number | null>(
    () => project?.git_provider_connection_id || null
  )
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

  // Sync form with project values when project changes
  useEffect(() => {
    if (project) {
      form.reset({
        branch: project.main_branch || '',
        preset: project.preset || '',
        directory: project.directory || '',
      })
    }
  }, [project, form])

  // Watch preset changes for directory field behavior
  const currentPreset = useWatch({
    control: form.control,
    name: 'preset',
  })

  // State to track if user wants to manually override directory
  const [allowDirectoryOverride, setAllowDirectoryOverride] = useState(false)

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

  // Derive if the current branch is custom (not in the branches list)
  const isCurrentBranchCustom = useMemo(() => {
    if (!currentBranch || branches.length === 0) return false
    const branchNames = branches.map((b: any) => b.name || b)
    return !branchNames.includes(currentBranch)
  }, [currentBranch, branches])

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

  // Get live preset detection for the repository
  const presetQuery = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repositoryData?.id || 0 },
    }),
    enabled: !!repositoryData?.id,
  })

  const presets = useMemo(() => {
    if (presetQuery.data?.presets && presetQuery.data.presets.length > 0) {
      // Map live preset data from presets array (new schema)
      const projectPresets = presetQuery.data.presets.map((preset: any) => ({
        value: preset.preset,
        label: preset.preset_label || preset.preset,
        directory: preset.path || './',
      }))

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
      // Extract just the preset name from "preset::path" format for backend
      const [presetName] = values.preset?.split('::') || ['']

      await updateGithubRepo.mutateAsync({
        body: {
          main_branch: values.branch,
          preset: presetName,
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
      // Extract just the preset name from "preset::path" format for backend
      const formPreset = form.getValues('preset')
      const [presetName] = formPreset?.split('::') || ['']

      // Update repository information
      await updateGithubRepo.mutateAsync({
        body: {
          repo_owner: repo.owner,
          repo_name: repo.name,
          directory: form.getValues('directory') || './',
          preset: presetName,
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
                    <div className="flex items-center justify-between">
                      <Label>Connected Repository</Label>
                      {isEditingSettings && (
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          onClick={() => setIsSelectingRepository(true)}
                        >
                          Change Repository
                        </Button>
                      )}
                    </div>
                    {isSelectingRepository && isEditingSettings ? (
                      <div className="space-y-4">
                        {/* Git Provider Selection */}
                        <div className="space-y-2">
                          <Label htmlFor="change-provider">
                            Git Provider Connection
                          </Label>
                          <Select
                            value={
                              selectedProvider?.toString() ||
                              project.git_provider_connection_id?.toString()
                            }
                            onValueChange={(value) => {
                              setSelectedProvider(Number(value))
                              setSelectedRepository(null)
                            }}
                          >
                            <SelectTrigger id="change-provider">
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
                                      <Badge
                                        variant="secondary"
                                        className="ml-2"
                                      >
                                        Default
                                      </Badge>
                                    )}
                                  </div>
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>

                        {/* Repository Selection */}
                        {(selectedProvider ||
                          project.git_provider_connection_id) && (
                          <RepositorySelector
                            connectionId={
                              selectedProvider ||
                              project.git_provider_connection_id!
                            }
                            onSelect={(repo) => {
                              handleRepositorySelect(repo)
                              setIsSelectingRepository(false)
                            }}
                            selectedRepository={selectedRepository}
                            title="Select New Repository"
                            description="Choose a repository from your connected git provider"
                            showAsCard={false}
                          />
                        )}

                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            setIsSelectingRepository(false)
                            setSelectedRepository(null)
                          }}
                        >
                          Cancel
                        </Button>
                      </div>
                    ) : (
                      <>
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
                          Seamlessly create Deployments for any commits pushed
                          to your Git repository.
                        </p>
                      </>
                    )}
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

                      <FormField
                        control={form.control}
                        name="preset"
                        render={({ field }) => {
                          // Convert stored preset value to select format
                          const getSelectValue = () => {
                            if (field.value === 'custom') return 'custom'
                            if (!field.value) return ''

                            // Get the current directory to match with preset path
                            const currentDirectory =
                              form.getValues('directory') || './'

                            // Normalize directory for comparison (remove leading ./)
                            const normalizeDir = (dir: string) => {
                              if (!dir || dir === '.' || dir === './')
                                return 'root'
                              return dir.startsWith('./') ? dir.slice(2) : dir
                            }

                            const normalizedCurrentDir =
                              normalizeDir(currentDirectory)

                            // Find matching preset by both name AND path
                            const matchingPreset =
                              presetQuery.data?.presets?.find((p: any) => {
                                const normalizedPresetPath = normalizeDir(
                                  p.path
                                )
                                return (
                                  p.preset === field.value &&
                                  normalizedPresetPath === normalizedCurrentDir
                                )
                              })

                            if (matchingPreset) {
                              return `${matchingPreset.preset}::${normalizeDir(matchingPreset.path)}`
                            }

                            // Fallback: if no exact match, find by preset name only
                            const fallbackPreset =
                              presetQuery.data?.presets?.find(
                                (p: any) => p.preset === field.value
                              )
                            if (fallbackPreset) {
                              return `${fallbackPreset.preset}::${normalizeDir(fallbackPreset.path)}`
                            }

                            // Return just the slug if not found in detected presets
                            return field.value
                          }

                          const selectValue = getSelectValue()

                          return (
                            <FormItem>
                              <FormControl>
                                <FrameworkSelector
                                  presetData={presetQuery.data}
                                  isLoading={presetQuery.isLoading}
                                  error={presetQuery.error}
                                  selectedPreset={selectValue}
                                  onSelectPreset={(value) => {
                                    if (value === 'custom') {
                                      field.onChange('custom')
                                      form.setValue('directory', './')
                                    } else {
                                      const [_presetName, presetPath] =
                                        value.split('::')
                                      // Store the full preset::path value for proper selection tracking
                                      field.onChange(value)

                                      // Treat empty, '.', and 'root' as root directory
                                      if (
                                        presetPath &&
                                        presetPath !== 'root' &&
                                        presetPath !== '.' &&
                                        presetPath.trim() !== ''
                                      ) {
                                        // Remove leading ./ if present in the path
                                        const cleanPath = presetPath.startsWith(
                                          './'
                                        )
                                          ? presetPath.slice(2)
                                          : presetPath
                                        form.setValue(
                                          'directory',
                                          `./${cleanPath}`
                                        )
                                      } else {
                                        form.setValue('directory', './')
                                      }
                                    }
                                  }}
                                />
                              </FormControl>
                              <FormMessage />
                            </FormItem>
                          )
                        }}
                      />

                      <FormField
                        control={form.control}
                        name="directory"
                        render={({ field }) => {
                          const isCustomPreset = currentPreset === 'custom'
                          const canEditDirectory =
                            isCustomPreset || allowDirectoryOverride

                          return (
                            <FormItem>
                              <div className="flex items-center justify-between">
                                <FormLabel>Root Directory</FormLabel>
                                {!isCustomPreset && !allowDirectoryOverride && (
                                  <Button
                                    type="button"
                                    variant="ghost"
                                    size="sm"
                                    onClick={() =>
                                      setAllowDirectoryOverride(true)
                                    }
                                    className="h-auto py-1 px-2 text-xs"
                                  >
                                    Edit manually
                                  </Button>
                                )}
                                {!isCustomPreset && allowDirectoryOverride && (
                                  <Button
                                    type="button"
                                    variant="ghost"
                                    size="sm"
                                    onClick={() => {
                                      setAllowDirectoryOverride(false)
                                      // Reset to preset-based directory if available
                                      const presetValue =
                                        form.getValues('preset')
                                      if (
                                        presetValue &&
                                        presetValue !== 'custom'
                                      ) {
                                        const [, presetPath] =
                                          presetValue.split('::')
                                        if (
                                          presetPath &&
                                          presetPath !== 'root'
                                        ) {
                                          const cleanPath =
                                            presetPath.startsWith('./')
                                              ? presetPath.slice(2)
                                              : presetPath
                                          form.setValue(
                                            'directory',
                                            `./${cleanPath}`
                                          )
                                        } else {
                                          form.setValue('directory', './')
                                        }
                                      }
                                    }}
                                    className="h-auto py-1 px-2 text-xs"
                                  >
                                    Reset to preset
                                  </Button>
                                )}
                              </div>
                              <FormControl>
                                <Input
                                  {...field}
                                  placeholder="./"
                                  readOnly={!canEditDirectory}
                                  className={
                                    !canEditDirectory
                                      ? 'bg-muted cursor-not-allowed'
                                      : ''
                                  }
                                />
                              </FormControl>
                              <FormDescription>
                                {canEditDirectory
                                  ? 'Enter the directory path manually'
                                  : 'Directory is set automatically based on selected preset'}
                              </FormDescription>
                              <FormMessage />
                            </FormItem>
                          )
                        }}
                      />
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

                {/* Framework Preset Selection - Using FrameworkSelector */}
                {selectedRepository && (
                  <Form {...form}>
                    <FormField
                      control={form.control}
                      name="preset"
                      render={({ field }) => {
                        // Convert stored preset value to select format
                        const getSelectValue = () => {
                          if (field.value === 'custom') return 'custom'
                          if (!field.value) return ''

                          // Get the current directory to match with preset path
                          const currentDirectory =
                            form.getValues('directory') || './'

                          // Normalize directory for comparison (remove leading ./)
                          const normalizeDir = (dir: string) => {
                            if (!dir || dir === '.' || dir === './')
                              return 'root'
                            return dir.startsWith('./') ? dir.slice(2) : dir
                          }

                          const normalizedCurrentDir =
                            normalizeDir(currentDirectory)

                          // Find matching preset by both name AND path
                          const matchingPreset =
                            presetQuery.data?.presets?.find((p: any) => {
                              const normalizedPresetPath = normalizeDir(p.path)
                              return (
                                p.preset === field.value &&
                                normalizedPresetPath === normalizedCurrentDir
                              )
                            })

                          if (matchingPreset) {
                            return `${matchingPreset.preset}::${normalizeDir(matchingPreset.path)}`
                          }

                          // Fallback: if no exact match, find by preset name only
                          const fallbackPreset =
                            presetQuery.data?.presets?.find(
                              (p: any) => p.preset === field.value
                            )
                          if (fallbackPreset) {
                            return `${fallbackPreset.preset}::${normalizeDir(fallbackPreset.path)}`
                          }

                          // Return just the slug if not found in detected presets
                          return field.value
                        }

                        const selectValue = getSelectValue()

                        return (
                          <FormItem>
                            <FormControl>
                              <FrameworkSelector
                                presetData={presetQuery.data}
                                isLoading={presetQuery.isLoading}
                                error={presetQuery.error}
                                selectedPreset={selectValue}
                                onSelectPreset={(value) => {
                                  if (value === 'custom') {
                                    field.onChange('custom')
                                    form.setValue('directory', './')
                                  } else {
                                    const [_presetName, presetPath] =
                                      value.split('::')
                                    // Store the full preset::path value for proper selection tracking
                                    field.onChange(value)

                                    // Treat empty, '.', and 'root' as root directory
                                    if (
                                      presetPath &&
                                      presetPath !== 'root' &&
                                      presetPath !== '.' &&
                                      presetPath.trim() !== ''
                                    ) {
                                      // Remove leading ./ if present in the path
                                      const cleanPath = presetPath.startsWith(
                                        './'
                                      )
                                        ? presetPath.slice(2)
                                        : presetPath
                                      form.setValue(
                                        'directory',
                                        `./${cleanPath}`
                                      )
                                    } else {
                                      form.setValue('directory', './')
                                    }
                                  }
                                }}
                              />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )
                      }}
                    />

                    {/* Directory Field */}
                    <FormField
                      control={form.control}
                      name="directory"
                      render={({ field }) => {
                        const isCustomPreset = currentPreset === 'custom'
                        const canEditDirectory =
                          isCustomPreset || allowDirectoryOverride

                        return (
                          <FormItem>
                            <div className="flex items-center justify-between">
                              <FormLabel>Root Directory</FormLabel>
                              {!isCustomPreset && !allowDirectoryOverride && (
                                <Button
                                  type="button"
                                  variant="ghost"
                                  size="sm"
                                  onClick={() =>
                                    setAllowDirectoryOverride(true)
                                  }
                                  className="h-auto py-1 px-2 text-xs"
                                >
                                  Edit manually
                                </Button>
                              )}
                              {!isCustomPreset && allowDirectoryOverride && (
                                <Button
                                  type="button"
                                  variant="ghost"
                                  size="sm"
                                  onClick={() => {
                                    setAllowDirectoryOverride(false)
                                    // Reset to preset-based directory if available
                                    const presetValue = form.getValues('preset')
                                    if (
                                      presetValue &&
                                      presetValue !== 'custom'
                                    ) {
                                      const [, presetPath] =
                                        presetValue.split('::')
                                      if (presetPath && presetPath !== 'root') {
                                        const cleanPath = presetPath.startsWith(
                                          './'
                                        )
                                          ? presetPath.slice(2)
                                          : presetPath
                                        form.setValue(
                                          'directory',
                                          `./${cleanPath}`
                                        )
                                      } else {
                                        form.setValue('directory', './')
                                      }
                                    }
                                  }}
                                  className="h-auto py-1 px-2 text-xs"
                                >
                                  Reset to preset
                                </Button>
                              )}
                            </div>
                            <FormControl>
                              <Input
                                {...field}
                                placeholder="./"
                                readOnly={!canEditDirectory}
                                className={
                                  !canEditDirectory
                                    ? 'bg-muted cursor-not-allowed'
                                    : ''
                                }
                              />
                            </FormControl>
                            <FormDescription>
                              {canEditDirectory
                                ? 'Enter the directory path manually'
                                : 'Directory is set automatically based on selected preset'}
                            </FormDescription>
                            <FormMessage />
                          </FormItem>
                        )
                      }}
                    />
                  </Form>
                )}
              </CardContent>
            </Card>
          )}
        </div>
      )}
    </div>
  )
}
