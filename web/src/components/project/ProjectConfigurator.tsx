import {
  createProjectMutation,
  getRepositoryBranchesOptions,
  getRepositoryPresetLiveOptions,
  listPresetsOptions,
  listServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  RepositoryResponse,
  ServiceTypeRoute,
  RepositoryPresetResponse,
  BranchInfo,
  ExternalServiceInfo,
  ProjectPresetResponse,
} from '@/api/client/types.gen'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { BranchSelector } from '@/components/deployments/BranchSelector'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  Database,
  Eye,
  EyeOff,
  Folder,
  GitBranch,
  Loader2,
  Plus,
  Server,
  Settings,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import * as z from 'zod/v4'
import { ServiceEnvPreview } from './ServiceEnvPreview'
import { FrameworkSelector } from './FrameworkSelector'

// Helper function to normalize path for consistent comparison
// Normalizes '.', './', and empty strings to 'root'
function normalizePath(path: string | undefined | null): string {
  if (!path || path === '.' || path === './') {
    return 'root'
  }
  return path
}

// Common service types
const SERVICE_TYPES = [
  {
    id: 'postgres' as ServiceTypeRoute,
    name: 'PostgreSQL',
    description: 'Reliable Relational Database',
  },
  {
    id: 'redis' as ServiceTypeRoute,
    name: 'Redis',
    description: 'In-Memory Data Store',
  },
  { id: 's3' as ServiceTypeRoute, name: 'S3', description: 'Object Storage' },
  {
    id: 'libsql' as ServiceTypeRoute,
    name: 'LibSQL',
    description: 'SQLite-compatible Database',
  },
]

// Helper function to slugify path for project name
const slugifyPath = (path: string): string => {
  if (!path || path === '.' || path === './' || path === 'root') {
    return ''
  }
  // Remove leading ./ if present
  const cleanPath = path.startsWith('./') ? path.slice(2) : path
  // Replace / with - and remove any other special characters
  return cleanPath.replace(/\//g, '-').replace(/[^a-zA-Z0-9-]/g, '')
}

// Form schema definition
const formSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  preset: z.string().min(1, 'Preset is required'),
  autoDeploy: z.boolean(),
  rootDirectory: z.string(),
  branch: z.string().min(1, 'Branch is required'),
  environmentVariables: z.array(
    z.object({
      key: z.string(),
      value: z.string(),
      isSecret: z.boolean(),
    })
  ),
  storageServices: z.array(z.number()),
  dockerfilePath: z.string().optional(),
  port: z.coerce.number().min(1).max(65535).optional(),
})

export type ProjectFormValues = z.infer<typeof formSchema>

// Step definitions for different modes
type WizardStep = 'repo-config' | 'services' | 'env-vars' | 'review'

const _STEP_CONFIG = {
  'repo-config': {
    title: 'Repository & Configuration',
    description: 'Configure basic project settings',
    icon: Folder,
  },
  services: {
    title: 'Services',
    description: 'Select and configure storage services',
    icon: Server,
  },
  'env-vars': {
    title: 'Environment Variables',
    description: 'Set up environment variables',
    icon: Settings,
  },
  review: {
    title: 'Review & Create',
    description: 'Review and submit your configuration',
    icon: CheckCircle2,
  },
}

// Main component props
interface ProjectConfiguratorProps {
  // Repository data
  repository: RepositoryResponse
  connectionId: number

  // Optional data
  branches?: BranchInfo[]

  // Display modes
  mode?: 'wizard' | 'inline' | 'compact'

  // Behavior
  onSubmit?: (data: ProjectFormValues) => Promise<void>
  onCancel?: () => void
  showSteps?: boolean
  defaultValues?: Partial<ProjectFormValues>

  // Styling
  className?: string
}

export function ProjectConfigurator({
  repository,
  connectionId,
  branches,
  mode: _mode = 'wizard',
  onSubmit,
  onCancel,
  showSteps: _showSteps = true,
  defaultValues,
  className,
}: ProjectConfiguratorProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  // State management
  const [_currentStep, _setCurrentStep] = useState<WizardStep>('repo-config')
  const [isSubmitting, setIsSubmitting] = useState(false)
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] =
    useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<ServiceTypeRoute | null>(null)
  const [showSecrets, setShowSecrets] = useState<{ [key: number]: boolean }>({})
  const [newlyCreatedServiceIds, setNewlyCreatedServiceIds] = useState<
    number[]
  >([])
  const [allowDirectoryOverride, setAllowDirectoryOverride] = useState(false)

  // Form initialization
  const form = useForm<ProjectFormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit', // Only validate on submit to avoid early validation errors
    defaultValues: {
      name: defaultValues?.name ?? repository?.name ?? '',
      preset: defaultValues?.preset ?? '', // Start empty, will be auto-filled by useEffect
      autoDeploy: defaultValues?.autoDeploy ?? true,
      rootDirectory: defaultValues?.rootDirectory ?? './',
      branch: defaultValues?.branch ?? repository?.default_branch ?? 'main',
      environmentVariables: defaultValues?.environmentVariables ?? [],
      storageServices: defaultValues?.storageServices ?? [],
      dockerfilePath: defaultValues?.dockerfilePath ?? 'Dockerfile',
      port: defaultValues?.port ?? 3000,
    },
  })

  // Fetch existing services
  const { data: existingServices, refetch: refetchServices } = useQuery({
    ...listServicesOptions({}),
  })

  // Fetch all available presets to get default ports
  const { data: allPresetsData } = useQuery({
    ...listPresetsOptions({}),
  })

  // Fetch branches if not provided
  const { data: branchesData } = useQuery({
    ...getRepositoryBranchesOptions({
      query: { connection_id: connectionId },
      path: { owner: repository.owner || '', repo: repository.name || '' },
    }),
    enabled: !branches && !!repository.owner && !!repository.name,
  })

  const effectiveBranches = useMemo(
    () => branches || branchesData?.branches || [],
    [branches, branchesData?.branches]
  )

  // Watch the selected branch to refetch presets when it changes
  const selectedBranch = useWatch({
    control: form.control,
    name: 'branch',
  })

  // Fetch preset data (will refetch when branch changes due to query key)
  const {
    data: presetData,
    isLoading: presetLoading,
    error: presetError,
    refetch: refetchPresets,
  } = useQuery({
    ...getRepositoryPresetLiveOptions({
      path: { repository_id: repository.id || 0 },
      query: { branch: selectedBranch },
    }),
    enabled: !!repository.id && !!selectedBranch,
    // Key includes branch, so React Query will refetch when branch changes
  })

  // Default project creation mutation
  const projectMutation = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create project',
    },
    onSuccess: async (data) => {
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      toast.success('Project created successfully!')
      navigate(`/projects/${data.slug}?new=true`)
    },
  })

  // Compute the default preset value based on preset data
  const defaultPresetValue = useMemo(() => {
    if (!presetData) {
      return null
    }

    // New schema: use presets array
    if (presetData.presets && presetData.presets.length > 0) {
      const firstPreset = presetData.presets[0]
      const presetName = firstPreset.preset || 'custom'
      const presetPath = firstPreset.path || './'
      const normalizedPath = normalizePath(presetPath)
      return {
        value: `${presetName}::${normalizedPath}`,
        rootDir: presetPath, // Keep original path for rootDirectory field
      }
    }
    // Fallback: use 'custom' as default
    else {
      return {
        value: 'custom',
        rootDir: './',
      }
    }
  }, [presetData])

  // Auto-set preset when data is available (select first available preset)
  useEffect(() => {
    // Only run if we have a default preset value
    if (!defaultPresetValue) {
      return
    }

    const currentPreset = form.getValues('preset')
    // Only auto-set if preset is empty
    if (currentPreset) {
      return
    }

    // Store the preset value in format "preset::path"
    form.setValue('preset', defaultPresetValue.value, {
      shouldValidate: true,
      shouldDirty: true,
    })
    form.setValue('rootDirectory', defaultPresetValue.rootDir, {
      shouldValidate: true,
      shouldDirty: true,
    })
  }, [defaultPresetValue, form])

  // Auto-set default branch (select first branch by default)
  useEffect(() => {
    const currentBranch = form.getValues('branch')

    // Only set if we don't have a branch yet
    if (!currentBranch && effectiveBranches && effectiveBranches.length > 0) {
      // Try to find default branch (marked as default or matching repo's default branch)
      const defaultBranch = effectiveBranches.find(
        (b: BranchInfo) => b.name === repository.default_branch
      )

      // Fallback to first branch if no default found
      const selectedBranch = defaultBranch || effectiveBranches[0]

      if (selectedBranch) {
        form.setValue('branch', selectedBranch.name, {
          shouldValidate: true,
          shouldDirty: true,
        })
      }
    }
  }, [effectiveBranches, repository.default_branch, form])

  // Watch preset changes to update port automatically
  const selectedPreset = useWatch({
    control: form.control,
    name: 'preset',
  })

  // Auto-update port based on selected preset
  useEffect(() => {
    if (!selectedPreset || !allPresetsData?.presets) {
      return
    }

    // Extract preset name from "preset::path" format
    const [presetName] = selectedPreset.split('::')

    // Find the matching preset to get its default port
    const matchingPreset = allPresetsData.presets.find(
      (p) => p.slug === presetName
    )

    if (
      matchingPreset &&
      matchingPreset.default_port !== null &&
      matchingPreset.default_port !== undefined
    ) {
      form.setValue('port', matchingPreset.default_port, {
        shouldValidate: true,
        shouldDirty: false, // Don't mark as dirty when auto-setting
      })
    }
  }, [selectedPreset, allPresetsData, form])

  // Environment variable management
  const addEnvironmentVariable = () => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      [...currentVars, { key: '', value: '', isSecret: false }],
      { shouldValidate: false }
    ) // Don't validate when adding empty variables
  }

  const removeEnvironmentVariable = (index: number) => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      currentVars.filter((_, i) => i !== index)
    )
  }

  // Service selection handler
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const isSelected = currentServices.includes(serviceId)
      const newValues = isSelected
        ? currentServices.filter((id) => id !== serviceId)
        : [...currentServices, serviceId]
      form.setValue('storageServices', newValues)
    },
    [form]
  )

  // Handle form submission
  const handleSubmit = async (data: ProjectFormValues) => {
    try {
      setIsSubmitting(true)

      // Remove duplicates from service IDs (newly created services are already in data.storageServices)
      const allServiceIds = Array.from(
        new Set([...(data.storageServices || []), ...newlyCreatedServiceIds])
      )

      // Extract just the preset name from "preset::path" format for backend
      const [presetName] = data.preset.split('::')

      const finalData = {
        ...data,
        preset: presetName, // Use only the preset name, not the full "preset::path"
        storageServices: allServiceIds,
      }

      if (onSubmit) {
        await onSubmit(finalData)
      } else {
        // Use default mutation
        await projectMutation.mutateAsync({
          body: {
            name: finalData.name,
            preset: finalData.preset,
            directory: finalData.rootDirectory,
            main_branch: finalData.branch,
            repo_name: repository.name || '',
            repo_owner: repository.owner || '',
            git_url: repository.clone_url || repository.ssh_url || '',
            git_provider_connection_id: connectionId,
            project_type:
              finalData.preset === 'custom' ? 'static' : finalData.preset,
            automatic_deploy: finalData.autoDeploy,
            storage_service_ids: finalData.storageServices || [],
            environment_variables: finalData.environmentVariables?.map(
              (env) => [env.key, env.value] as [string, string]
            ),
          },
        })
      }
    } catch (error) {
      console.error('Project configuration error:', error)
    } finally {
      setIsSubmitting(false)
    }
  }

  // Render repository config step
  const renderRepoConfig = () => (
    <div className="space-y-4">
      <div className="flex items-center gap-3 p-4 bg-muted/50 rounded-lg">
        <GitBranch className="h-5 w-5 text-muted-foreground" />
        <div>
          <div className="font-medium">
            {repository.owner}/{repository.name}
          </div>
          <div className="text-sm text-muted-foreground">
            {repository.full_name}
          </div>
        </div>
      </div>

      <FormField
        control={form.control}
        name="name"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Project Name</FormLabel>
            <FormControl>
              <Input {...field} placeholder="Enter project name" />
            </FormControl>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="branch"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Branch</FormLabel>
            <BranchSelector
              repoOwner={repository.owner}
              repoName={repository.name}
              connectionId={connectionId}
              defaultBranch={repository.default_branch}
              value={field.value}
              onChange={field.onChange}
            />
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="preset"
        render={({ field }) => {
          // Convert stored preset value back to select format for display
          const getSelectValue = () => {
            if (field.value === 'custom') return 'custom'
            if (!field.value) return ''

            // Find matching preset to get the path (detected presets)
            const matchingPreset = presetData?.presets?.find(
              (p: ProjectPresetResponse) => p.preset === field.value
            )
            if (matchingPreset) {
              return `${matchingPreset.preset}::${matchingPreset.path || 'root'}`
            }

            // If no match found in detected presets, return just the preset slug
            // (This happens when selecting from "Browse all presets" mode)
            return field.value
          }

          const selectValue = getSelectValue()

          return (
            <FormItem>
              <FormControl>
                <FrameworkSelector
                  presetData={presetData}
                  isLoading={presetLoading}
                  error={presetError}
                  selectedPreset={selectValue}
                  onRefresh={() => refetchPresets()}
                  onSelectPreset={(value) => {
                    if (value === 'custom') {
                      field.onChange('custom')
                      form.setValue('rootDirectory', './')
                    } else {
                      const [_presetName, presetPath] = value.split('::')
                      // Store the full preset key (preset::path) to distinguish between same preset at different paths
                      field.onChange(value)

                      // Set project name based on repository name and preset path
                      const repoName = repository.name || 'project'
                      const slugifiedPath = slugifyPath(presetPath)
                      const projectName = slugifiedPath
                        ? `${repoName}-${slugifiedPath}`
                        : repoName
                      form.setValue('name', projectName)

                      // Treat empty, '.', and 'root' as root directory
                      if (
                        presetPath &&
                        presetPath !== 'root' &&
                        presetPath !== '.' &&
                        presetPath.trim() !== ''
                      ) {
                        // Remove leading ./ if present in the path
                        const cleanPath = presetPath.startsWith('./')
                          ? presetPath.slice(2)
                          : presetPath
                        form.setValue('rootDirectory', `./${cleanPath}`)
                      } else {
                        form.setValue('rootDirectory', './')
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
        name="rootDirectory"
        render={({ field }) => {
          const currentPreset = form.watch('preset')
          const isCustomPreset = currentPreset === 'custom'
          const canEditDirectory = isCustomPreset || allowDirectoryOverride

          return (
            <FormItem>
              <div className="flex items-center justify-between">
                <FormLabel>Root Directory</FormLabel>
                {!isCustomPreset && !allowDirectoryOverride && (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setAllowDirectoryOverride(true)}
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
                      if (presetValue && presetValue !== 'custom') {
                        const [, presetPath] = presetValue.split('::')
                        if (presetPath && presetPath !== 'root') {
                          const cleanPath = presetPath.startsWith('./')
                            ? presetPath.slice(2)
                            : presetPath
                          form.setValue('rootDirectory', `./${cleanPath}`)
                        } else {
                          form.setValue('rootDirectory', './')
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
                  className={!canEditDirectory ? 'bg-muted' : ''}
                />
              </FormControl>
              <p className="text-xs text-muted-foreground">
                {canEditDirectory
                  ? 'Enter the root directory for your configuration'
                  : 'Directory will be set based on the selected framework preset'}
              </p>
              <FormMessage />
            </FormItem>
          )
        }}
      />

      <FormField
        control={form.control}
        name="autoDeploy"
        render={({ field }) => (
          <FormItem className="flex flex-row items-start space-x-3 space-y-0 rounded-md border p-4">
            <FormControl>
              <Checkbox
                checked={field.value}
                onCheckedChange={field.onChange}
              />
            </FormControl>
            <div className="space-y-1 leading-none">
              <FormLabel>Automatic Deployments</FormLabel>
              <p className="text-sm text-muted-foreground">
                Automatically deploy when changes are pushed to the repository
              </p>
            </div>
          </FormItem>
        )}
      />

      {/* Docker Configuration - Only show for docker/dockerfile preset */}
      {form.watch('preset')?.toLowerCase().includes('docker') && (
        <>
          <FormField
            control={form.control}
            name="dockerfilePath"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Dockerfile Path</FormLabel>
                <FormControl>
                  <Input
                    {...field}
                    placeholder="Dockerfile"
                    value={field.value || 'Dockerfile'}
                  />
                </FormControl>
                <p className="text-xs text-muted-foreground">
                  Path to your Dockerfile relative to the root directory
                </p>
                <FormMessage />
              </FormItem>
            )}
          />
        </>
      )}

      <FormField
        control={form.control}
        name="port"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Application Port</FormLabel>
            <FormControl>
              <Input
                {...field}
                type="number"
                min="1"
                max="65535"
                placeholder="3000"
                value={field.value || 3000}
              />
            </FormControl>
            <p className="text-xs text-muted-foreground">
              Port your application will listen on (e.g., 3000, 8080)
            </p>
            <FormMessage />
          </FormItem>
        )}
      />
    </div>
  )

  // Render services step
  const renderServices = () => {
    const watchedServices = form.watch('storageServices') || []

    return (
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <h3 className="font-medium">Services</h3>
            <p className="text-sm text-muted-foreground">
              Link existing services or create new ones
            </p>
          </div>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm">
                <Plus className="h-4 w-4 mr-2" />
                Add Service
                <ChevronDown className="h-4 w-4 ml-1" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-[240px]">
              {SERVICE_TYPES.map((type) => (
                <DropdownMenuItem
                  key={type.id}
                  onClick={() => {
                    setSelectedServiceType(type.id)
                    setIsCreateServiceDialogOpen(true)
                  }}
                  className="flex items-start gap-3 py-3"
                >
                  <ServiceLogo service={type.id} />
                  <div className="flex flex-col">
                    <span className="font-medium">{type.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {type.description}
                    </span>
                  </div>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        {existingServices && existingServices.length > 0 && (
          <div>
            <h4 className="text-sm font-medium mb-3">Existing Services</h4>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
              {existingServices.map((service) => {
                const isSelected = watchedServices.includes(service.id)
                return (
                  <Card
                    key={service.id}
                    className={cn(
                      'cursor-pointer transition-colors hover:bg-muted/50',
                      isSelected && 'ring-2 ring-primary'
                    )}
                    onClick={() => handleServiceToggle(service.id)}
                  >
                    <CardHeader className="pb-3">
                      <div className="flex items-center justify-between">
                        <div className="flex items-center gap-3">
                          <ServiceLogo service={service.service_type} />
                          <div>
                            <CardTitle className="text-sm">
                              {service.name}
                            </CardTitle>
                            <CardDescription className="text-xs">
                              {service.service_type} • Created{' '}
                              {format(
                                new Date(service.created_at),
                                'MMM d, yyyy'
                              )}
                            </CardDescription>
                          </div>
                        </div>
                      </div>
                    </CardHeader>
                  </Card>
                )
              })}
            </div>
          </div>
        )}

        {newlyCreatedServiceIds.length > 0 && (
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              {newlyCreatedServiceIds.length} new service
              {newlyCreatedServiceIds.length > 1 ? 's' : ''} will be created
              with this project
            </AlertDescription>
          </Alert>
        )}

        {/* Environment Variables Preview for Selected Services */}
        {watchedServices.length > 0 && (
          <div>
            <h4 className="text-sm font-medium mb-3">
              Service Environment Variables Preview
            </h4>
            <p className="text-xs text-muted-foreground mb-4">
              Selected services will provide these environment variables to your
              project.
              <span className="text-primary">
                Click &quot;Preview Variables&quot;
              </span>{' '}
              on any service to see what will be available.
            </p>
            <div className="space-y-3">
              {watchedServices.map((serviceId) => {
                const service = existingServices?.find(
                  (s: ExternalServiceInfo) => s.id === serviceId
                )
                if (!service) return null
                return (
                  <ServiceEnvPreview
                    key={service.id}
                    serviceId={service.id}
                    serviceName={service.name}
                    serviceType={service.service_type}
                  />
                )
              })}
            </div>
          </div>
        )}

        {!existingServices?.length && newlyCreatedServiceIds.length === 0 && (
          <div className="text-center py-8">
            <Database className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
            <p className="text-sm text-muted-foreground">
              No services configured yet
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              Create services to enhance your project
            </p>
          </div>
        )}
      </div>
    )
  }

  // Render environment variables step
  const renderEnvVars = () => {
    const watchedEnvVars = form.watch('environmentVariables') || []

    return (
      <div className="space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <h3 className="font-medium">Environment Variables</h3>
            <p className="text-sm text-muted-foreground">
              Configure environment variables for your project
            </p>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={addEnvironmentVariable}
          >
            <Plus className="h-4 w-4 mr-2" />
            Add Variable
          </Button>
        </div>

        {watchedEnvVars.length > 0 ? (
          <div className="space-y-3">
            {watchedEnvVars.map((_, index) => (
              <Card key={index} className="border-dashed">
                <CardContent className="p-4">
                  <div className="flex items-start gap-3">
                    <div className="flex-1 grid grid-cols-1 md:grid-cols-2 gap-3">
                      <FormField
                        control={form.control}
                        name={`environmentVariables.${index}.key`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel className="text-sm">Key</FormLabel>
                            <FormControl>
                              <Input {...field} placeholder="DATABASE_URL" />
                            </FormControl>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                      <FormField
                        control={form.control}
                        name={`environmentVariables.${index}.value`}
                        render={({ field }) => (
                          <FormItem>
                            <FormLabel className="text-sm">Value</FormLabel>
                            <div className="relative">
                              <FormControl>
                                <Input
                                  {...field}
                                  type={
                                    showSecrets[index] ? 'text' : 'password'
                                  }
                                  placeholder="Enter value"
                                />
                              </FormControl>
                              <Button
                                type="button"
                                variant="ghost"
                                size="sm"
                                className="absolute right-0 top-0 h-full px-3"
                                onClick={() =>
                                  setShowSecrets((prev) => ({
                                    ...prev,
                                    [index]: !prev[index],
                                  }))
                                }
                              >
                                {showSecrets[index] ? (
                                  <EyeOff className="h-4 w-4" />
                                ) : (
                                  <Eye className="h-4 w-4" />
                                )}
                              </Button>
                            </div>
                            <FormMessage />
                          </FormItem>
                        )}
                      />
                    </div>
                    <div className="flex flex-col gap-2">
                      <FormField
                        control={form.control}
                        name={`environmentVariables.${index}.isSecret`}
                        render={({ field }) => (
                          <FormItem className="flex items-center space-x-2 space-y-0">
                            <FormControl>
                              <Checkbox
                                checked={field.value}
                                onCheckedChange={field.onChange}
                              />
                            </FormControl>
                            <FormLabel className="text-xs">Secret</FormLabel>
                          </FormItem>
                        )}
                      />
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        onClick={() => removeEnvironmentVariable(index)}
                        className="text-destructive hover:text-destructive h-8 w-8 p-0"
                      >
                        <X className="h-4 w-4" />
                      </Button>
                    </div>
                  </div>
                </CardContent>
              </Card>
            ))}
          </div>
        ) : (
          <div className="text-center py-8">
            <Settings className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
            <p className="text-sm text-muted-foreground">
              No environment variables configured
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              Add variables that your application needs
            </p>
          </div>
        )}
      </div>
    )
  }

  // Render review step
  const _renderReview = () => {
    const formData = form.getValues()

    return (
      <div className="space-y-4">
        <Card>
          <CardHeader>
            <CardTitle className="text-lg flex items-center gap-2">
              <Folder className="h-5 w-5" />
              Project Configuration
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Name:</span>
              <span className="font-medium">{formData.name}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Repository:</span>
              <span className="font-medium">{repository.full_name}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Branch:</span>
              <span className="font-medium">{formData.branch}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Framework:</span>
              <span className="font-medium">{formData.preset}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Directory:</span>
              <span className="font-medium">{formData.rootDirectory}</span>
            </div>
            <div className="flex justify-between text-sm">
              <span className="text-muted-foreground">Auto Deploy:</span>
              <span className="font-medium">
                {formData.autoDeploy ? 'Enabled' : 'Disabled'}
              </span>
            </div>
            {formData.dockerfilePath && (
              <div className="flex justify-between text-sm">
                <span className="text-muted-foreground">Dockerfile:</span>
                <span className="font-medium">{formData.dockerfilePath}</span>
              </div>
            )}
            {formData.port && (
              <div className="flex justify-between text-sm">
                <span className="text-muted-foreground">Port:</span>
                <span className="font-medium">{formData.port}</span>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-lg flex items-center gap-2">
              <Database className="h-5 w-5" />
              Services
            </CardTitle>
          </CardHeader>
          <CardContent>
            {(formData.storageServices?.length || 0) +
              newlyCreatedServiceIds.length >
            0 ? (
              <div className="space-y-2">
                <p className="text-sm text-muted-foreground">
                  {(formData.storageServices?.length || 0) +
                    newlyCreatedServiceIds.length}{' '}
                  service(s) will be linked
                </p>
                {newlyCreatedServiceIds.length > 0 && (
                  <p className="text-xs text-muted-foreground">
                    Including {newlyCreatedServiceIds.length} new service(s)
                  </p>
                )}
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                No services configured
              </p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-lg flex items-center gap-2">
              <Settings className="h-5 w-5" />
              Environment Variables
            </CardTitle>
          </CardHeader>
          <CardContent>
            {formData.environmentVariables?.length ? (
              <div className="space-y-2">
                <p className="text-sm text-muted-foreground">
                  {formData.environmentVariables.length} variable(s) configured
                </p>
                <div className="space-y-1">
                  {formData.environmentVariables.map((env, i) => (
                    <div key={i} className="flex items-center gap-2 text-xs">
                      <code className="px-1 py-0.5 bg-muted rounded">
                        {env.key}
                      </code>
                      {env.isSecret && (
                        <Badge variant="outline" className="text-xs">
                          Secret
                        </Badge>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">
                No environment variables configured
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    )
  }

  // Render inline/compact mode
  return (
    <div className={cn('space-y-6', className)}>
      <Form {...form}>
        <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
          {/* All sections in one view for inline/compact mode */}
          <Card>
            <CardHeader>
              <CardTitle>Project Configuration</CardTitle>
              <CardDescription>Configure your project settings</CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              {renderRepoConfig()}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Services</CardTitle>
              <CardDescription>
                Select storage and database services
              </CardDescription>
            </CardHeader>
            <CardContent>{renderServices()}</CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Environment Variables</CardTitle>
              <CardDescription>Configure environment variables</CardDescription>
            </CardHeader>
            <CardContent>{renderEnvVars()}</CardContent>
          </Card>

          <div className="flex justify-end gap-3">
            {onCancel && (
              <Button
                type="button"
                variant="outline"
                onClick={onCancel}
                disabled={isSubmitting}
              >
                Cancel
              </Button>
            )}
            <Button type="submit" disabled={isSubmitting}>
              {isSubmitting ? (
                <>
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                  Creating Project...
                </>
              ) : (
                <>
                  <CheckCircle2 className="mr-2 h-4 w-4" />
                  Create Project
                </>
              )}
            </Button>
          </div>
        </form>
      </Form>

      {/* Create Service Dialog */}
      {selectedServiceType && (
        <CreateServiceDialog
          open={isCreateServiceDialogOpen}
          onOpenChange={(open) => {
            setIsCreateServiceDialogOpen(open)
            if (!open) {
              setSelectedServiceType(null)
            }
          }}
          serviceType={selectedServiceType}
          onSuccess={(service: ExternalServiceInfo) => {
            setIsCreateServiceDialogOpen(false)
            setSelectedServiceType(null)
            setNewlyCreatedServiceIds((prev) => [...prev, service.id])
            // Automatically add the newly created service to the form selection
            const currentServices = form.getValues('storageServices') || []
            form.setValue('storageServices', [...currentServices, service.id])
            setTimeout(() => {
              refetchServices()
            }, 100)
            toast.success(`Service "${service.name}" created successfully!`)
          }}
        />
      )}
    </div>
  )
}
