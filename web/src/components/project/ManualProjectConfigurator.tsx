import { createProjectMutation, listServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ExternalServiceInfo, ServiceTypeRoute, SourceType } from '@/api/client/types.gen'
import { CreateServiceDialog } from '@/components/storage/CreateServiceDialog'
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
import { ServiceLogo } from '@/components/ui/service-logo'
import { cn } from '@/lib/utils'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  Container,
  Database,
  Eye,
  EyeOff,
  FileArchive,
  Loader2,
  Plus,
  Settings,
  X,
} from 'lucide-react'
import { useCallback, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'
import * as z from 'zod/v4'
import { ServiceEnvPreview } from './ServiceEnvPreview'

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

// Source type options
const SOURCE_TYPE_OPTIONS: {
  id: SourceType
  name: string
  description: string
  icon: React.ComponentType<{ className?: string }>
  recommended?: boolean
}[] = [
  {
    id: 'manual',
    name: 'Flexible',
    description: 'Deploy via Docker images, static files, or Git - switch anytime',
    icon: Settings,
    recommended: true,
  },
  {
    id: 'docker_image',
    name: 'Docker Image Only',
    description: 'Locked to Docker image deployments only',
    icon: Container,
  },
  {
    id: 'static_files',
    name: 'Static Files Only',
    description: 'Locked to static file deployments only',
    icon: FileArchive,
  },
]

// Form schema for manual projects
const formSchema = z.object({
  name: z.string().min(1, 'Project name is required'),
  sourceType: z.enum(['manual', 'docker_image', 'static_files'] as const),
  // Docker image specific
  imageUrl: z.string().optional(),
  // Static files specific (will be uploaded after project creation)
  // Common settings
  port: z.coerce.number().min(1).max(65535).optional(),
  environmentVariables: z.array(
    z.object({
      key: z.string(),
      value: z.string(),
      isSecret: z.boolean(),
    })
  ),
  storageServices: z.array(z.number()),
})

export type ManualProjectFormValues = z.infer<typeof formSchema>

interface ManualProjectConfiguratorProps {
  onSubmit?: (data: ManualProjectFormValues) => Promise<void>
  onCancel?: () => void
  defaultValues?: Partial<ManualProjectFormValues>
  className?: string
}

export function ManualProjectConfigurator({
  onSubmit,
  onCancel,
  defaultValues,
  className,
}: ManualProjectConfiguratorProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()

  // State management
  const [isSubmitting, setIsSubmitting] = useState(false)
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] = useState(false)
  const [selectedServiceType, setSelectedServiceType] = useState<ServiceTypeRoute | null>(null)
  const [showSecrets, setShowSecrets] = useState<{ [key: number]: boolean }>({})
  const [newlyCreatedServiceIds, setNewlyCreatedServiceIds] = useState<number[]>([])
  const [newlyCreatedServiceTypes, setNewlyCreatedServiceTypes] = useState<ServiceTypeRoute[]>([])

  // Form initialization
  const form = useForm<ManualProjectFormValues>({
    resolver: zodResolver(formSchema),
    mode: 'onSubmit',
    defaultValues: {
      name: defaultValues?.name ?? '',
      sourceType: defaultValues?.sourceType ?? 'manual',
      imageUrl: defaultValues?.imageUrl ?? '',
      port: defaultValues?.port ?? 3000,
      environmentVariables: defaultValues?.environmentVariables ?? [],
      storageServices: defaultValues?.storageServices ?? [],
    },
  })

  // Watch source type for conditional rendering
  const sourceType = useWatch({
    control: form.control,
    name: 'sourceType',
  })

  // Fetch existing services
  const { data: existingServices, refetch: refetchServices } = useQuery({
    ...listServicesOptions({}),
  })

  // Project creation mutation
  const projectMutation = useMutation({
    ...createProjectMutation(),
    meta: {
      errorTitle: 'Failed to create project',
    },
    onSuccess: async (data) => {
      await queryClient.invalidateQueries({ queryKey: ['getProjects'] })
      await queryClient.invalidateQueries({ queryKey: ['listProjects'] })
      toast.success('Project created successfully!')
      navigate(`/projects/${data.slug}?new=true&source=${sourceType}`)
    },
  })

  // Environment variable management
  const addEnvironmentVariable = () => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      [...currentVars, { key: '', value: '', isSecret: false }],
      { shouldValidate: false }
    )
  }

  const removeEnvironmentVariable = (index: number) => {
    const currentVars = form.getValues('environmentVariables') || []
    form.setValue(
      'environmentVariables',
      currentVars.filter((_, i) => i !== index)
    )
  }

  // Get the service types that are already selected (either existing or newly created)
  const getSelectedServiceTypes = useCallback((): Set<string> => {
    const currentServices = form.getValues('storageServices') || []
    const selectedTypes = new Set<string>()

    // Add types from selected existing services
    currentServices.forEach((serviceId: number) => {
      const service = existingServices?.find((s) => s.id === serviceId)
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

  // Service selection handler
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const isSelected = currentServices.includes(serviceId)

      // If trying to select (not deselect), check for type collision
      if (!isSelected) {
        const serviceToAdd = existingServices?.find((s) => s.id === serviceId)
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
        ? currentServices.filter((id) => id !== serviceId)
        : [...currentServices, serviceId]
      form.setValue('storageServices', newValues)
    },
    [form, existingServices, getSelectedServiceTypes]
  )

  // Handle form submission
  const handleSubmit = async (data: ManualProjectFormValues) => {
    try {
      setIsSubmitting(true)

      // Remove duplicates from service IDs
      const allServiceIds = Array.from(
        new Set([...(data.storageServices || []), ...newlyCreatedServiceIds])
      )

      const finalData = {
        ...data,
        storageServices: allServiceIds,
      }

      if (onSubmit) {
        await onSubmit(finalData)
      } else {
        // Use default mutation
        // Determine project_type based on source_type for API compatibility
        const projectType = finalData.sourceType === 'static_files' ? 'static' : 'docker'
        await projectMutation.mutateAsync({
          body: {
            name: finalData.name,
            preset: 'dockerfile', // Use dockerfile preset for manual projects
            directory: './',
            main_branch: 'main', // Placeholder for non-git projects
            source_type: finalData.sourceType as 'docker_image' | 'static_files' | 'manual',
            // Leave repo fields empty for manual projects
            repo_name: undefined,
            repo_owner: undefined,
            git_url: undefined,
            git_provider_connection_id: undefined,
            project_type: projectType,
            automatic_deploy: false, // Manual projects don't auto-deploy
            exposed_port: finalData.port,
            storage_service_ids: finalData.storageServices || [],
            environment_variables: finalData.environmentVariables
              ?.filter(env => env.key.trim() !== '')
              ?.map((env) => [env.key, env.value] as [string, string]),
          },
        })
      }
    } catch (error) {
      console.error('Project configuration error:', error)
    } finally {
      setIsSubmitting(false)
    }
  }

  // Render source type selection
  const renderSourceTypeSelection = () => (
    <div className="space-y-4">
      <FormField
        control={form.control}
        name="sourceType"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Deployment Method</FormLabel>
            <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
              {SOURCE_TYPE_OPTIONS.map((option) => {
                const Icon = option.icon
                const isSelected = field.value === option.id
                return (
                  <Card
                    key={option.id}
                    className={cn(
                      'cursor-pointer transition-all hover:border-primary/50',
                      isSelected && 'border-primary ring-2 ring-primary/20'
                    )}
                    onClick={() => field.onChange(option.id)}
                  >
                    <CardContent className="p-4">
                      <div className="flex items-start gap-4">
                        <div
                          className={cn(
                            'rounded-lg p-2',
                            isSelected ? 'bg-primary/10 text-primary' : 'bg-muted'
                          )}
                        >
                          <Icon className="h-6 w-6" />
                        </div>
                        <div className="flex-1">
                          <div className="flex items-center gap-2 flex-wrap">
                            <h3 className="font-medium">{option.name}</h3>
                            {option.recommended && (
                              <Badge variant="secondary" className="text-xs">
                                Recommended
                              </Badge>
                            )}
                            {isSelected && (
                              <Badge variant="default" className="text-xs">
                                Selected
                              </Badge>
                            )}
                          </div>
                          <p className="text-sm text-muted-foreground mt-1">
                            {option.description}
                          </p>
                        </div>
                      </div>
                    </CardContent>
                  </Card>
                )
              })}
            </div>
            <FormMessage />
          </FormItem>
        )}
      />
    </div>
  )

  // Render project config
  const renderProjectConfig = () => (
    <div className="space-y-4">
      <FormField
        control={form.control}
        name="name"
        render={({ field }) => (
          <FormItem>
            <FormLabel>Project Name</FormLabel>
            <FormControl>
              <Input {...field} placeholder="my-awesome-project" />
            </FormControl>
            <FormMessage />
          </FormItem>
        )}
      />

      {(sourceType === 'docker_image' || sourceType === 'manual') && (
        <>
          <FormField
            control={form.control}
            name="imageUrl"
            render={({ field }) => (
              <FormItem>
                <FormLabel>Docker Image (Optional)</FormLabel>
                <FormControl>
                  <Input
                    {...field}
                    placeholder="nginx:latest or ghcr.io/org/image:tag"
                  />
                </FormControl>
                <p className="text-xs text-muted-foreground">
                  {sourceType === 'manual'
                    ? 'Optionally specify an image for your first deployment. You can also deploy via static files or configure git later.'
                    : 'You can specify an initial image now or configure it later via the API'}
                </p>
                <FormMessage />
              </FormItem>
            )}
          />
        </>
      )}

      {sourceType === 'static_files' && (
        <Alert>
          <FileArchive className="h-4 w-4" />
          <AlertDescription>
            After creating the project, you'll be able to upload your static files
            (tar.gz or zip) through the project dashboard or API.
          </AlertDescription>
        </Alert>
      )}

      {sourceType === 'manual' && (
        <Alert>
          <Settings className="h-4 w-4" />
          <AlertDescription>
            <strong>Flexible Project:</strong> After creation, you can deploy using any method:
            <ul className="list-disc list-inside mt-2 text-xs">
              <li>Docker images from any registry</li>
              <li>Static files (tar.gz or zip uploads)</li>
              <li>Git repository (configure later in project settings)</li>
            </ul>
          </AlertDescription>
        </Alert>
      )}

      <FormField
        control={form.control}
        name="port"
        render={({ field }) => (
          <FormItem>
            <FormLabel>
              {sourceType === 'docker_image' ? 'Container Port' : 'Application Port'}
            </FormLabel>
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
              {sourceType === 'docker_image'
                ? 'Port your container exposes (will be auto-detected from EXPOSE if not set)'
                : 'Port for serving static files (default: 3000)'}
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
                      setSelectedServiceType(type.id)
                      setIsCreateServiceDialogOpen(true)
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

  return (
    <div className={cn('space-y-6', className)}>
      <Form {...form}>
        <form onSubmit={form.handleSubmit(handleSubmit)} className="space-y-6">
          {/* Source Type Selection */}
          <Card>
            <CardHeader>
              <CardTitle>Deployment Method</CardTitle>
              <CardDescription>
                Choose how you want to deploy your application
              </CardDescription>
            </CardHeader>
            <CardContent>{renderSourceTypeSelection()}</CardContent>
          </Card>

          {/* Project Configuration */}
          <Card>
            <CardHeader>
              <CardTitle>Project Configuration</CardTitle>
              <CardDescription>Configure your project settings</CardDescription>
            </CardHeader>
            <CardContent>{renderProjectConfig()}</CardContent>
          </Card>

          {/* Services */}
          <Card>
            <CardHeader>
              <CardTitle>Services</CardTitle>
              <CardDescription>
                Select storage and database services
              </CardDescription>
            </CardHeader>
            <CardContent>{renderServices()}</CardContent>
          </Card>

          {/* Environment Variables */}
          <Card>
            <CardHeader>
              <CardTitle>Environment Variables</CardTitle>
              <CardDescription>Configure environment variables</CardDescription>
            </CardHeader>
            <CardContent>{renderEnvVars()}</CardContent>
          </Card>

          {/* Submit */}
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
      <CreateServiceDialog
        open={isCreateServiceDialogOpen && !!selectedServiceType}
        onOpenChange={(open) => {
          setIsCreateServiceDialogOpen(open)
          if (!open) {
            setSelectedServiceType(null)
          }
        }}
        serviceType={selectedServiceType || 'postgres'}
        onSuccess={(service: ExternalServiceInfo) => {
          setIsCreateServiceDialogOpen(false)
          setNewlyCreatedServiceIds((prev) => [...prev, service.id])
          // Track the service type from the selected type
          if (selectedServiceType) {
            setNewlyCreatedServiceTypes((prev) => [...prev, selectedServiceType])
          }
          setSelectedServiceType(null)
          // Automatically add the newly created service to the form selection
          const currentServices = form.getValues('storageServices') || []
          form.setValue('storageServices', [...currentServices, service.id])
          setTimeout(() => {
            refetchServices()
          }, 100)
          toast.success(`Service "${service.name}" created successfully!`)
        }}
      />
    </div>
  )
}
