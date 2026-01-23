import { listServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import { RepositoryResponse, ServiceTypeRoute } from '@/api/client/types.gen'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
import { Badge } from '@/components/ui/badge'
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
import { cn } from '@/lib/utils'
import { zodResolver } from '@hookform/resolvers/zod'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  CheckCircle2,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Database,
  Eye,
  EyeOff,
  Folder,
  GitBranch,
  Loader2,
  Plus,
  Settings,
  X,
} from 'lucide-react'
import React, { useMemo } from 'react'
import { useCallback, useState } from 'react'
import { useForm, UseFormReturn } from 'react-hook-form'
import { toast } from 'sonner'
import * as z from 'zod'

type WizardStep = 'repo-config' | 'services' | 'env-vars' | 'review'

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

const formSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  preset: z.string().min(1, 'Preset is required'),
  autoDeploy: z.boolean(),
  rootDirectory: z.string(),
  branch: z.string().min(1, 'Branch is required'),
  environmentVariables: z
    .array(
      z.object({
        key: z.string().min(1, 'Key is required'),
        value: z.string().min(1, 'Value is required'),
        isSecret: z.boolean(),
      })
    )
    .optional(),
  storageServices: z.array(z.number()).optional(),
})

type FormValues = z.infer<typeof formSchema>

interface ProjectConfigurationWizardInlineProps {
  repository: RepositoryResponse
  connectionId: number
  presetData?: any
  branches?: any[]
  onSubmit: (data: FormValues) => Promise<void>
  isLoading?: boolean
  className?: string
}

// Repository Configuration Step Component
interface RepoConfigStepProps {
  form: any
  repository: RepositoryResponse
  branches?: any[]
  presetData?: any
}

function RepoConfigStep({
  form,
  repository,
  branches,
  presetData,
}: RepoConfigStepProps) {
  return (
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
            <Select value={field.value} onValueChange={field.onChange}>
              <SelectTrigger>
                <SelectValue placeholder="Select a branch" />
              </SelectTrigger>
              <SelectContent>
                {branches?.map((branch: any) => (
                  <SelectItem key={branch.name} value={branch.name}>
                    {branch.name}
                    {branch.name === repository.default_branch && (
                      <Badge variant="secondary" className="ml-2 text-xs">
                        default
                      </Badge>
                    )}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="preset"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Framework Preset</FormLabel>
            <Select
              value={field.value}
              onValueChange={(value) => {
                if (value === 'custom') {
                  field.onChange('custom')
                  form.setValue('rootDirectory', './')
                } else {
                  // Extract the actual preset name from the value (format: preset::path)
                  const [presetName, presetPath] = value.split('::')
                  field.onChange(presetName)

                  // Update the root directory based on path
                  if (presetPath && presetPath !== 'root') {
                    form.setValue('rootDirectory', `./${presetPath}`)
                  } else {
                    form.setValue('rootDirectory', './')
                  }
                }
              }}
            >
              <SelectTrigger>
                <SelectValue placeholder="Select a framework" />
              </SelectTrigger>
              <SelectContent>
                {presetData?.presets?.map((preset: any, index: number) => (
                  <SelectItem
                    key={`preset-${index}-${preset.preset}-${preset.path || './'}`}
                    value={`${preset.preset}::${preset.path || './'}`}
                  >
                    <div className="flex flex-col">
                      <span>{preset.preset_label || preset.preset}</span>
                      <span className="text-xs text-muted-foreground">
                        {preset.path || './'}
                      </span>
                    </div>
                  </SelectItem>
                ))}
                <SelectItem value="custom">Custom</SelectItem>
              </SelectContent>
            </Select>
            <FormMessage />
          </FormItem>
        )}
      />

      <FormField
        control={form.control}
        name="rootDirectory"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Root Directory</FormLabel>
            <FormControl>
              <Input
                {...field}
                placeholder="./"
                readOnly={form.watch('preset') !== 'custom'}
                className={form.watch('preset') !== 'custom' ? 'bg-muted' : ''}
              />
            </FormControl>
            <p className="text-xs text-muted-foreground">
              {form.watch('preset') !== 'custom'
                ? 'Directory will be set based on the selected framework preset'
                : 'Enter the root directory for your custom configuration'}
            </p>
            <FormMessage />
          </FormItem>
        )}
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
    </div>
  )
}

// Services Step Component
interface ServicesStepProps {
  form: UseFormReturn<FormValues>
  existingServices?: any[]
  newlyCreatedServiceIds: number[]
  newlyCreatedServiceTypes: ServiceTypeRoute[]
  onCreateService: (type: ServiceTypeRoute) => void
}
// Memoized ServiceCard to prevent unnecessary re-renders
const ServiceCard = React.memo(function ServiceCard({
  service,
  isSelected,
  onToggle,
}: {
  service: any
  isSelected: boolean
  onToggle: (id: number) => void
}) {
  return (
    <Card
      className={`cursor-pointer transition-colors hover:bg-muted/50 ${isSelected ? 'ring-2 ring-primary' : ''}`}
      onClick={(e) => {
        e.preventDefault()
        onToggle(service.id)
      }}
    >
      <CardHeader className="pb-2">
        <div className="flex items-center justify-between">
          <div className="space-y-1">
            <CardTitle className="flex items-center gap-2 text-sm">
              <ServiceLogo service={service.service_type} />
              {service.name}
            </CardTitle>
            <CardDescription className="text-xs">
              {service.service_type} • Created{' '}
              {format(new Date(service.created_at), 'MMM d, yyyy')}
            </CardDescription>
          </div>
        </div>
      </CardHeader>
    </Card>
  )
})

function ServicesStep({
  form,
  existingServices,
  newlyCreatedServiceIds,
  newlyCreatedServiceTypes,
  onCreateService,
}: ServicesStepProps) {
  // Get the service types that are already selected (either existing or newly created)
  const getSelectedServiceTypes = useCallback((): Set<string> => {
    const currentServices = form.getValues('storageServices') || []
    const selectedTypes = new Set<string>()

    // Add types from selected existing services
    currentServices.forEach((serviceId: number) => {
      const service = existingServices?.find((s: any) => s.id === serviceId)
      if (service) {
        selectedTypes.add(service.service_type)
      }
    })

    // Add types from newly created services
    newlyCreatedServiceTypes.forEach((serviceType) => {
      selectedTypes.add(serviceType)
    })

    return selectedTypes
  }, [form, existingServices, newlyCreatedServiceTypes])

  // Stable callback for service selection to prevent infinite re-renders
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const isSelected = currentServices.includes(serviceId)

      // If trying to select (not deselect), check for type collision
      if (!isSelected) {
        const serviceToAdd = existingServices?.find(
          (s: any) => s.id === serviceId
        )
        if (serviceToAdd) {
          const selectedTypes = getSelectedServiceTypes()
          if (selectedTypes.has(serviceToAdd.service_type)) {
            toast.error(`A ${serviceToAdd.service_type} service is already selected`, {
              description:
                'Only one service of each type can be linked to a project to avoid environment variable conflicts.',
            })
            return
          }
        }
      }

      const newValues = isSelected
        ? currentServices.filter((id: number) => id !== serviceId)
        : [...currentServices, serviceId]

      form.setValue('storageServices', [...newValues], {
        shouldValidate: false,
        shouldDirty: false,
        shouldTouch: false,
      })
    },
    [form, existingServices, getSelectedServiceTypes]
  )

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
            {SERVICE_TYPES.map((type) => {
              const selectedTypes = getSelectedServiceTypes()
              const isTypeAlreadySelected = selectedTypes.has(type.id)
              return (
                <DropdownMenuItem
                  key={type.id}
                  onClick={() => {
                    if (isTypeAlreadySelected) {
                      toast.error(`A ${type.name} service is already selected`, {
                        description:
                          'Only one service of each type can be linked to a project.',
                      })
                      return
                    }
                    onCreateService(type.id)
                  }}
                  className={cn(
                    'flex items-start gap-3 py-3',
                    isTypeAlreadySelected && 'opacity-50 cursor-not-allowed'
                  )}
                >
                  <ServiceLogo service={type.id} />
                  <div className="flex flex-col">
                    <span className="font-medium">
                      {type.name}
                      {isTypeAlreadySelected && (
                        <span className="text-xs text-muted-foreground ml-2">
                          (already selected)
                        </span>
                      )}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {type.description}
                    </span>
                  </div>
                </DropdownMenuItem>
              )
            })}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {/* Existing Services */}
      {existingServices && existingServices.length > 0 && (
        <div>
          <h4 className="font-medium mb-3">Existing Services</h4>
          <FormField
            control={form.control}
            name="storageServices"
            render={({ field }) => {
              const selectedServices = field.value || []

              return (
                <FormItem>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    {existingServices.map((service: any) => (
                      <ServiceCard
                        key={service.id}
                        service={service}
                        isSelected={selectedServices.includes(service.id)}
                        onToggle={handleServiceToggle}
                      />
                    ))}
                  </div>
                  <FormMessage />
                </FormItem>
              )
            }}
          />
        </div>
      )}

      {newlyCreatedServiceIds.length > 0 && (
        <div className="mt-3">
          <p className="text-sm text-muted-foreground mb-2">
            {newlyCreatedServiceIds.length} new service
            {newlyCreatedServiceIds.length > 1 ? 's' : ''} will be created with
            this project
          </p>
        </div>
      )}
    </div>
  )
}

// Environment Variables Step Component
interface EnvVarsStepProps {
  form: any
  watchedEnvVars: any[]
  showSecrets: { [key: number]: boolean }
  onToggleSecret: (index: number) => void
  onAddVariable: () => void
  onRemoveVariable: (index: number) => void
}

function EnvVarsStep({
  form,
  watchedEnvVars,
  showSecrets,
  onToggleSecret,
  onAddVariable,
  onRemoveVariable,
}: EnvVarsStepProps) {
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
          onClick={onAddVariable}
        >
          <Plus className="h-4 w-4 mr-2" />
          Add Variable
        </Button>
      </div>

      {watchedEnvVars.length > 0 ? (
        <div className="space-y-3">
          {watchedEnvVars.map((_, index) => (
            <Card key={index}>
              <CardContent className="p-4">
                <div className="flex items-end gap-4">
                  <div className="grid grid-cols-2 gap-4 flex-1">
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.key`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel>Key</FormLabel>
                          <FormControl>
                            <Input {...field} placeholder="VARIABLE_NAME" />
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
                          <FormLabel>Value</FormLabel>
                          <div className="relative">
                            <FormControl>
                              <Input
                                {...field}
                                type={
                                  form.watch(
                                    `environmentVariables.${index}.isSecret`
                                  ) && !showSecrets[index]
                                    ? 'password'
                                    : 'text'
                                }
                                placeholder="Enter value"
                              />
                            </FormControl>
                            {form.watch(
                              `environmentVariables.${index}.isSecret`
                            ) && (
                              <Button
                                type="button"
                                variant="ghost"
                                size="sm"
                                className="absolute right-0 top-0 h-full px-3 py-2 hover:bg-transparent"
                                onClick={() => onToggleSecret(index)}
                              >
                                {showSecrets[index] ? (
                                  <EyeOff className="h-4 w-4" />
                                ) : (
                                  <Eye className="h-4 w-4" />
                                )}
                              </Button>
                            )}
                          </div>
                          <FormMessage />
                        </FormItem>
                      )}
                    />
                  </div>
                  <div className="space-y-2">
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.isSecret`}
                      render={({ field }) => (
                        <FormItem className="flex flex-row items-start space-x-3 space-y-0">
                          <FormControl>
                            <Checkbox
                              checked={field.value}
                              onCheckedChange={field.onChange}
                            />
                          </FormControl>
                          <div className="space-y-1 leading-none">
                            <FormLabel className="text-sm">Secret</FormLabel>
                          </div>
                        </FormItem>
                      )}
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => onRemoveVariable(index)}
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
          <Settings className="h-12 w-12 text-muted-foreground mx-auto mb-4" />
          <p className="text-muted-foreground">
            No environment variables configured
          </p>
          <p className="text-sm text-muted-foreground">
            Click &quot;Add Variable&quot; to get started
          </p>
        </div>
      )}
    </div>
  )
}

// Review Step Component
interface ReviewStepProps {
  form: any
  existingServices?: any[]
  newlyCreatedServiceIds: number[]
}

function ReviewStep({
  form,
  existingServices,
  newlyCreatedServiceIds,
}: ReviewStepProps) {
  const watchedServices = form.watch('storageServices') || []
  const watchedEnvVars = form.watch('environmentVariables') || []

  return (
    <div className="space-y-6">
      <div>
        <h3 className="font-medium mb-4">Review Configuration</h3>
        <p className="text-sm text-muted-foreground mb-6">
          Please review your configuration before creating the project
        </p>
      </div>

      {/* Project Details */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Folder className="h-5 w-5" />
            Project Details
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="grid grid-cols-2 gap-4">
            <div>
              <p className="text-sm font-medium">Name</p>
              <p className="text-sm text-muted-foreground">
                {form.watch('name')}
              </p>
            </div>
            <div>
              <p className="text-sm font-medium">Branch</p>
              <p className="text-sm text-muted-foreground">
                {form.watch('branch')}
              </p>
            </div>
            <div>
              <p className="text-sm font-medium">Framework</p>
              <p className="text-sm text-muted-foreground">
                {form.watch('preset')}
              </p>
            </div>
            <div>
              <p className="text-sm font-medium">Directory</p>
              <p className="text-sm text-muted-foreground">
                {form.watch('rootDirectory')}
              </p>
            </div>
          </div>
          <div>
            <p className="text-sm font-medium">Auto Deploy</p>
            <p className="text-sm text-muted-foreground">
              {form.watch('autoDeploy') ? 'Enabled' : 'Disabled'}
            </p>
          </div>
        </CardContent>
      </Card>

      {/* Services */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Database className="h-5 w-5" />
            Services ({watchedServices.length + newlyCreatedServiceIds.length})
          </CardTitle>
        </CardHeader>
        <CardContent>
          {watchedServices.length > 0 || newlyCreatedServiceIds.length > 0 ? (
            <div className="space-y-2">
              {watchedServices.map((serviceId: number) => {
                const service = existingServices?.find(
                  (s: any) => s.id === serviceId
                )
                return service ? (
                  <div key={service.id} className="flex items-center gap-2">
                    <ServiceLogo service={service.service_type} />
                    <span className="text-sm">{service.name}</span>
                    <Badge variant="outline" className="text-xs">
                      existing
                    </Badge>
                  </div>
                ) : null
              })}
              {newlyCreatedServiceIds.map((serviceId: number) => (
                <div
                  key={`new-${serviceId}`}
                  className="flex items-center gap-2"
                >
                  <Database className="h-4 w-4" />
                  <span className="text-sm">New Service #{serviceId}</span>
                  <Badge variant="outline" className="text-xs">
                    new
                  </Badge>
                </div>
              ))}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">
              No services configured
            </p>
          )}
        </CardContent>
      </Card>

      {/* Environment Variables */}
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <Settings className="h-5 w-5" />
            Environment Variables ({watchedEnvVars.length})
          </CardTitle>
        </CardHeader>
        <CardContent>
          {watchedEnvVars.length > 0 ? (
            <div className="space-y-2">
              {watchedEnvVars.map((envVar: any, index: number) => (
                <div key={index} className="flex items-center justify-between">
                  <span className="text-sm font-mono">{envVar.key}</span>
                  <div className="flex items-center gap-2">
                    <span className="text-sm text-muted-foreground">
                      {envVar.isSecret ? '••••••••' : envVar.value}
                    </span>
                    {envVar.isSecret && (
                      <Badge variant="outline" className="text-xs">
                        secret
                      </Badge>
                    )}
                  </div>
                </div>
              ))}
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
// Get step icon
const getStepIcon = (step: WizardStep) => {
  switch (step) {
    case 'repo-config':
      return <Folder className="h-5 w-5" />
    case 'services':
      return <Database className="h-5 w-5" />
    case 'env-vars':
      return <Settings className="h-5 w-5" />
    case 'review':
      return <CheckCircle2 className="h-5 w-5" />
  }
}

// Get step title
const getStepTitle = (step: WizardStep) => {
  switch (step) {
    case 'repo-config':
      return 'Repository & Configuration'
    case 'services':
      return 'Services'
    case 'env-vars':
      return 'Environment Variables'
    case 'review':
      return 'Review & Create'
  }
}
// Main Wizard Component
export function ProjectConfigurationWizardInline({
  repository,
  connectionId: _connectionId,
  presetData,
  branches,
  onSubmit,
  isLoading = false,
  className,
}: ProjectConfigurationWizardInlineProps) {
  const [currentStep, setCurrentStep] = useState<WizardStep>('repo-config')
  const [isSubmitting, setIsSubmitting] = useState(false)
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] =
    useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<ServiceTypeRoute | null>(null)
  const [showSecrets, setShowSecrets] = useState<{ [key: number]: boolean }>({})
  const [newlyCreatedServiceIds, setNewlyCreatedServiceIds] = useState<
    number[]
  >([])
  const [newlyCreatedServiceTypes, setNewlyCreatedServiceTypes] = useState<
    ServiceTypeRoute[]
  >([])

  const form = useForm<FormValues>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      name: repository.name || '',
      preset: '',
      autoDeploy: true,
      rootDirectory: './',
      branch: branches?.[0]?.name || 'main',
      environmentVariables: [],
      storageServices: [],
    },
  })

  // Step navigation
  const steps: WizardStep[] = useMemo(
    () => ['repo-config', 'services', 'env-vars', 'review'],
    []
  )
  const currentStepIndex = useMemo(
    () => steps.indexOf(currentStep),
    [currentStep, steps]
  )

  const goToNextStep = useCallback(() => {
    if (currentStepIndex < steps.length - 1) {
      setCurrentStep(steps[currentStepIndex + 1])
    }
  }, [currentStepIndex, steps])

  const goToPrevStep = useCallback(() => {
    if (currentStepIndex > 0) {
      setCurrentStep(steps[currentStepIndex - 1])
    }
  }, [currentStepIndex, steps])

  // Add environment variable
  const addEnvironmentVariable = useCallback(() => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue('environmentVariables', [
      ...currentVars,
      { key: '', value: '', isSecret: false },
    ])
  }, [form])

  // Remove environment variable
  const removeEnvironmentVariable = useCallback(
    (index: number) => {
      const currentVars = form.getValues('environmentVariables') || []
      form.setValue(
        'environmentVariables',
        currentVars.filter((_, i) => i !== index)
      )
    },
    [form]
  )

  // Toggle secret visibility
  const toggleSecret = useCallback((index: number) => {
    setShowSecrets((prev) => ({
      ...prev,
      [index]: !prev[index],
    }))
  }, [])

  // Handle form submission
  const handleSubmit = useCallback(
    async (data: FormValues) => {
      try {
        setIsSubmitting(true)
        // Include newly created services in the storage services
        const allServiceIds = [
          ...(data.storageServices || []),
          ...newlyCreatedServiceIds,
        ]

        await onSubmit({
          ...data,
          storageServices: allServiceIds,
        })
      } catch {
        toast.error('Failed to create project', {
          description: 'Please check your configuration and try again',
        })
      } finally {
        setIsSubmitting(false)
      }
    },
    [newlyCreatedServiceIds, onSubmit]
  )

  // Handle service creation
  const handleCreateService = useCallback((serviceType: ServiceTypeRoute) => {
    setSelectedServiceType(serviceType)
    setIsCreateServiceDialogOpen(true)
  }, [])

  const watchedEnvVars = form.watch('environmentVariables') || []

  // Create service dialog success handler
  const handleServiceCreated = useCallback(
    (service: any) => {
      setIsCreateServiceDialogOpen(false)
      setNewlyCreatedServiceIds((prev) => [...prev, service.id])
      // Track the service type from the selected type (more reliable than service response)
      if (selectedServiceType) {
        setNewlyCreatedServiceTypes((prev) => [...prev, selectedServiceType])
      }
      setSelectedServiceType(null)
      toast.success(`Service "${service.name}" created successfully!`)
    },
    [selectedServiceType]
  )

  // Queries
  const { data: existingServices } = useQuery({
    ...listServicesOptions({}),
  })

  return (
    <div className={className}>
      <Form {...form}>
        <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
          {/* Step indicator */}
          <div className="flex items-center justify-between mb-8">
            {steps.map((step, index) => (
              <div key={step} className="flex items-center">
                <div
                  className={cn(
                    'flex items-center gap-3 transition-colors',
                    index < currentStepIndex
                      ? 'cursor-pointer hover:opacity-80'
                      : ''
                  )}
                  onClick={() => {
                    // Only allow going back to previous steps
                    if (index < currentStepIndex) {
                      setCurrentStep(steps[index])
                    }
                  }}
                >
                  <div
                    className={cn(
                      'flex items-center justify-center w-10 h-10 rounded-full border-2 transition-colors',
                      index <= currentStepIndex
                        ? 'bg-primary border-primary text-primary-foreground'
                        : 'border-muted bg-background text-muted-foreground'
                    )}
                  >
                    {index < currentStepIndex ? (
                      <CheckCircle2 className="h-5 w-5" />
                    ) : (
                      <span className="text-sm font-medium">{index + 1}</span>
                    )}
                  </div>
                  <div className="flex flex-col">
                    <span
                      className={cn(
                        'text-sm font-medium',
                        index <= currentStepIndex
                          ? 'text-foreground'
                          : 'text-muted-foreground'
                      )}
                    >
                      {getStepTitle(step)}
                    </span>
                  </div>
                </div>
                {index < steps.length - 1 && (
                  <ChevronRight className="h-5 w-5 text-muted-foreground mx-4" />
                )}
              </div>
            ))}
          </div>

          {/* Step content */}
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                {getStepIcon(currentStep)}
                {getStepTitle(currentStep)}
              </CardTitle>
            </CardHeader>
            <CardContent>
              {currentStep === 'repo-config' ? (
                <RepoConfigStep
                  form={form}
                  repository={repository}
                  branches={branches}
                  presetData={presetData}
                />
              ) : currentStep === 'services' ? (
                <ServicesStep
                  form={form}
                  existingServices={existingServices}
                  newlyCreatedServiceIds={newlyCreatedServiceIds}
                  newlyCreatedServiceTypes={newlyCreatedServiceTypes}
                  onCreateService={handleCreateService}
                />
              ) : currentStep === 'env-vars' ? (
                <EnvVarsStep
                  form={form}
                  watchedEnvVars={watchedEnvVars}
                  showSecrets={showSecrets}
                  onToggleSecret={toggleSecret}
                  onAddVariable={addEnvironmentVariable}
                  onRemoveVariable={removeEnvironmentVariable}
                />
              ) : currentStep === 'review' ? (
                <ReviewStep
                  form={form}
                  existingServices={existingServices}
                  newlyCreatedServiceIds={newlyCreatedServiceIds}
                />
              ) : null}
            </CardContent>
          </Card>

          {/* Navigation buttons */}
          <div className="flex items-center justify-between">
            <Button
              type="button"
              variant="outline"
              onClick={goToPrevStep}
              disabled={currentStepIndex === 0}
            >
              <ChevronLeft className="h-4 w-4 mr-2" />
              Previous
            </Button>

            {currentStepIndex === steps.length - 1 ? (
              <Button type="submit" disabled={isSubmitting || isLoading}>
                {isSubmitting || isLoading ? (
                  <>
                    <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                    Creating Project...
                  </>
                ) : (
                  'Create Project'
                )}
              </Button>
            ) : (
              <Button type="button" onClick={goToNextStep}>
                Next
                <ChevronRight className="h-4 w-4 ml-2" />
              </Button>
            )}
          </div>
        </form>
      </Form>

      {/* Create Service Dialog */}
      {/* IMPORTANT: Always render the dialog (don't conditionally mount) to prevent hooks violations */}
      <CreateServiceDialog
        open={isCreateServiceDialogOpen && !!selectedServiceType}
        onOpenChange={(open) => {
          setIsCreateServiceDialogOpen(open)
          if (!open) {
            setSelectedServiceType(null)
          }
        }}
        serviceType={selectedServiceType || 'postgres'}
        onSuccess={handleServiceCreated}
      />
    </div>
  )
}
