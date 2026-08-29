// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createProjectMutation } from '@/api/client/@tanstack/react-query.gen'
import type {
  CreatableServiceTypeRoute,
  ExternalServiceInfo,
  SourceType,
} from '@/api/client/types.gen'
import { ImportEnvDialog } from '@/components/ui/import-env-dialog'
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
import { useMutation, useQueryClient } from '@tanstack/react-query'
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
  Upload,
  X,
} from 'lucide-react'
import { useCallback, useMemo, useRef, useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import * as z from 'zod/v4'
import {
  ProvidedEnvironmentVariables,
  ProvidedEnvironmentVariableWarning,
} from './ProvidedEnvironmentVariables'
import type { ProvidedEnvironmentVariableCollision } from '@/lib/provided-environment-variables'
import { ADD_SERVICE_TYPES } from '@/lib/addServiceTypes'
import {
  isLikelySecretProjectEnvironmentVariable,
  isTempsManagedProjectEnvironmentVariable,
  projectEnvironmentVariablesSchema,
} from '@/lib/project-environment-variables'
import {
  normalizeTemplateServiceType,
  toggleDatabaseSelection,
} from '@/lib/template-service-requirements'
import { useAllServices } from '@/hooks/useAllServices'

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
    description:
      'Deploy via Docker images, static files, or Git - switch anytime',
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
  port: z.number().int().min(1).max(65535).optional(),
  environmentVariables: projectEnvironmentVariablesSchema,
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
  // Synchronous re-entry guard against fast double-clicks (root cause of
  // users ending up with N duplicate projects from a single intent).
  const isSubmittingRef = useRef(false)
  const [isCreateServiceDialogOpen, setIsCreateServiceDialogOpen] =
    useState(false)
  const [selectedServiceType, setSelectedServiceType] =
    useState<CreatableServiceTypeRoute | null>(null)
  const [showSecrets, setShowSecrets] = useState<{ [key: number]: boolean }>({})
  const [isImportEnvOpen, setIsImportEnvOpen] = useState(false)
  const [providedEnvironmentVariables, setProvidedEnvironmentVariables] =
    useState<ProvidedEnvironmentVariableCollision[]>([])
  const [newlyCreatedServices, setNewlyCreatedServices] = useState<
    ExternalServiceInfo[]
  >([])

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
  const { data: existingServices, refetch: refetchServices } = useAllServices()
  const availableServices = useMemo(() => {
    const servicesById = new Map<number, ExternalServiceInfo>()
    existingServices?.forEach((service) =>
      servicesById.set(service.id, service)
    )
    newlyCreatedServices.forEach((service) =>
      servicesById.set(service.id, service)
    )
    return Array.from(servicesById.values())
  }, [existingServices, newlyCreatedServices])

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
      const service = availableServices.find((item) => item.id === serviceId)
      if (service) {
        selectedTypes.add(normalizeTemplateServiceType(service.service_type))
      }
    })

    return selectedTypes
  }, [form, availableServices])

  // Service selection handler
  const handleServiceToggle = useCallback(
    (serviceId: number) => {
      const currentServices = form.getValues('storageServices') || []
      const result = toggleDatabaseSelection(
        currentServices,
        serviceId,
        availableServices
      )
      if (result.conflictingService) {
        toast.error('A compatible database is already selected', {
          description: `${result.conflictingService.name} already provides this database variable namespace. Deselect it first.`,
        })
        return
      }
      form.setValue('storageServices', result.selectedServiceIds)
    },
    [form, availableServices]
  )

  // Handle form submission
  const handleSubmit = async (data: ManualProjectFormValues) => {
    if (isSubmittingRef.current) return
    isSubmittingRef.current = true
    try {
      setIsSubmitting(true)

      const finalData = {
        ...data,
        // The visible selection is authoritative. A database created in this
        // flow can still be deselected before submission.
        storageServices: Array.from(new Set(data.storageServices || [])),
      }

      if (onSubmit) {
        await onSubmit(finalData)
      } else {
        // Use default mutation
        // Determine project_type based on source_type for API compatibility
        const projectType =
          finalData.sourceType === 'static_files' ? 'static' : 'docker'
        await projectMutation.mutateAsync({
          body: {
            name: finalData.name,
            preset: 'dockerfile', // Use dockerfile preset for manual projects
            directory: './',
            main_branch: 'main', // Placeholder for non-git projects
            source_type: finalData.sourceType as
              'docker_image' | 'static_files' | 'manual',
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
              ?.filter((env) => env.key.trim() !== '')
              ?.map((env) => ({
                key: env.key,
                value: env.value,
                is_secret: env.isSecret,
              })),
          },
        })
      }
    } catch (error) {
      console.error('Project configuration error:', error)
    } finally {
      isSubmittingRef.current = false
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
                            isSelected
                              ? 'bg-primary/10 text-primary'
                              : 'bg-muted'
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
            After creating the project, you&apos;ll be able to upload your
            static files (tar.gz or zip) through the project dashboard or API.
          </AlertDescription>
        </Alert>
      )}

      {sourceType === 'manual' && (
        <Alert>
          <Settings className="h-4 w-4" />
          <AlertDescription>
            <strong>Flexible Project:</strong> After creation, you can deploy
            using any method:
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
              {sourceType === 'docker_image'
                ? 'Container Port'
                : 'Application Port'}
            </FormLabel>
            <FormControl>
              <Input
                type="number"
                min="1"
                max="65535"
                placeholder="3000"
                name={field.name}
                ref={field.ref}
                onBlur={field.onBlur}
                value={field.value ?? 3000}
                onChange={(e) => {
                  const v = e.target.valueAsNumber
                  field.onChange(Number.isNaN(v) ? undefined : v)
                }}
              />
            </FormControl>
            <p className="text-xs text-muted-foreground">
              {sourceType === 'docker_image'
                ? 'Port your container exposes (will be auto-detected from EXPOSE if not set)'
                : sourceType === 'static_files'
                  ? 'Port for serving static files (default: 3000)'
                  : 'Port your application listens on (default: 3000)'}
            </p>
            <FormMessage />
          </FormItem>
        )}
      />
    </div>
  )

  const renderAddDatabaseMenu = () => (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button type="button" variant="outline" size="sm">
          <Plus className="h-4 w-4 mr-2" />
          Add Database
          <ChevronDown className="h-4 w-4 ml-1" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        {ADD_SERVICE_TYPES.map((type) => {
          const selectedTypes = getSelectedServiceTypes()
          const isTypeAlreadySelected = selectedTypes.has(type.id)
          return (
            <DropdownMenuItem
              key={type.id}
              onClick={() => {
                if (isTypeAlreadySelected) {
                  toast.error(`A ${type.name} database is already selected`, {
                    description:
                      'Only one database of each type can be linked to a project.',
                  })
                  return
                }
                setSelectedServiceType(type.id)
                setIsCreateServiceDialogOpen(true)
              }}
              className={cn(
                'flex items-center gap-3 py-2.5',
                isTypeAlreadySelected && 'opacity-50 cursor-not-allowed'
              )}
            >
              <ServiceLogo service={type.id} className="h-6 w-6" />
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
  )

  // Render databases step. The API still calls these storage services, but
  // "Databases" is the user-facing concept in project creation.
  const renderDatabases = () => {
    const watchedServices = form.watch('storageServices') || []

    return (
      <div className="space-y-4">
        {availableServices.length > 0 && (
          <div>
            <h4 className="text-sm font-medium mb-3">Existing Databases</h4>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
              {availableServices.map((service) => {
                const isSelected = watchedServices.includes(service.id)
                return (
                  <Card
                    key={service.id}
                    role="checkbox"
                    tabIndex={0}
                    aria-checked={isSelected}
                    className={cn(
                      'cursor-pointer transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2',
                      isSelected && 'ring-2 ring-primary'
                    )}
                    onClick={() => handleServiceToggle(service.id)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault()
                        handleServiceToggle(service.id)
                      }
                    }}
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

        {newlyCreatedServices.some((service) =>
          watchedServices.includes(service.id)
        ) && (
          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              {
                newlyCreatedServices.filter((service) =>
                  watchedServices.includes(service.id)
                ).length
              }{' '}
              new database
              {newlyCreatedServices.filter((service) =>
                watchedServices.includes(service.id)
              ).length > 1
                ? 's'
                : ''}{' '}
              will be linked to this project
            </AlertDescription>
          </Alert>
        )}

        {availableServices.length === 0 && (
          <div className="text-center py-8">
            <Database className="h-12 w-12 mx-auto text-muted-foreground mb-3" />
            <p className="text-sm text-muted-foreground">
              No databases configured yet
            </p>
            <p className="text-xs text-muted-foreground mt-1">
              Add PostgreSQL, Redis, MongoDB, or object storage when your app
              needs it
            </p>
          </div>
        )}
      </div>
    )
  }

  // Render environment variables step
  const renderEnvVars = () => {
    const watchedEnvVars = form.watch('environmentVariables') || []
    const selectedDatabases = (form.watch('storageServices') || [])
      .map((serviceId) =>
        availableServices.find((service) => service.id === serviceId)
      )
      .filter((service): service is ExternalServiceInfo => Boolean(service))
      .map((service) => ({
        id: service.id,
        name: service.name,
        serviceType: service.service_type,
      }))

    return (
      <div className="space-y-4">
        <ProvidedEnvironmentVariables
          preset="dockerfile"
          databases={selectedDatabases}
          onVariablesChange={setProvidedEnvironmentVariables}
        />

        <ImportEnvDialog
          isOpen={isImportEnvOpen}
          onOpenChange={setIsImportEnvOpen}
          existingKeys={
            new Set(watchedEnvVars.map((v) => v.key).filter(Boolean))
          }
          showEnvironmentSelection={false}
          onImport={async (variables) => {
            const currentVars = form.getValues('environmentVariables') || []
            const configurableVariables = variables.filter(
              (variable) =>
                !isTempsManagedProjectEnvironmentVariable(variable.key)
            )
            const skippedCount = variables.length - configurableVariables.length
            const newVars = configurableVariables.map((v) => ({
              key: v.key,
              value: v.value,
              isSecret: isLikelySecretProjectEnvironmentVariable(v.key),
            }))
            form.setValue('environmentVariables', [...currentVars, ...newVars])
            if (skippedCount > 0) {
              toast.info(
                `Skipped ${skippedCount} variable${skippedCount === 1 ? '' : 's'} provided automatically by Temps`
              )
            }
          }}
        />

        {watchedEnvVars.length > 0 ? (
          <div className="space-y-3">
            {watchedEnvVars.map((_, index) => (
              <Card key={index} className="border-border/70 bg-muted/15">
                <CardContent className="space-y-4 p-4">
                  <div className="flex items-center justify-between gap-3">
                    <Badge variant="outline" className="font-normal">
                      Variable {index + 1}
                    </Badge>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => removeEnvironmentVariable(index)}
                      className="text-destructive hover:text-destructive h-8 w-8 p-0"
                      aria-label={`Remove environment variable ${index + 1}`}
                      title="Remove variable"
                    >
                      <X className="h-4 w-4" />
                    </Button>
                  </div>
                  <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                    <FormField
                      control={form.control}
                      name={`environmentVariables.${index}.key`}
                      render={({ field }) => (
                        <FormItem>
                          <FormLabel className="text-sm">Key</FormLabel>
                          <FormControl>
                            <Input
                              {...field}
                              className="font-mono"
                              placeholder="DATABASE_URL"
                              autoCapitalize="none"
                              autoCorrect="off"
                              spellCheck={false}
                            />
                          </FormControl>
                          <FormMessage />
                          <ProvidedEnvironmentVariableWarning
                            variableName={watchedEnvVars[index]?.key ?? ''}
                            providedVariables={providedEnvironmentVariables}
                          />
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
                                  watchedEnvVars[index]?.isSecret &&
                                  !showSecrets[index]
                                    ? 'password'
                                    : 'text'
                                }
                                className={cn(
                                  'font-mono',
                                  watchedEnvVars[index]?.isSecret && 'pr-10'
                                )}
                                placeholder="Enter value"
                              />
                            </FormControl>
                            {watchedEnvVars[index]?.isSecret && (
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
                                aria-label={
                                  showSecrets[index]
                                    ? 'Hide secret value'
                                    : 'Show secret value'
                                }
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
                  <FormField
                    control={form.control}
                    name={`environmentVariables.${index}.isSecret`}
                    render={({ field }) => (
                      <FormItem className="flex items-start gap-3 space-y-0 rounded-md border bg-background/70 p-3">
                        <FormControl>
                          <Checkbox
                            checked={field.value}
                            onCheckedChange={field.onChange}
                            className="mt-0.5"
                          />
                        </FormControl>
                        <div className="space-y-1">
                          <FormLabel className="text-sm">
                            Encrypt as secret
                          </FormLabel>
                          <p className="text-xs text-muted-foreground">
                            Secret values are write-only after creation. Use
                            this for passwords, tokens, and private connection
                            strings.
                          </p>
                        </div>
                      </FormItem>
                    )}
                  />
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
              Add one manually or import an existing .env file
            </p>
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="mt-4"
              onClick={addEnvironmentVariable}
            >
              <Plus className="h-4 w-4 mr-2" />
              Add your first variable
            </Button>
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

          {/* Databases — not applicable for static-only projects */}
          {sourceType !== 'static_files' && (
            <Card>
              <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                <div className="space-y-1.5">
                  <CardTitle>Databases</CardTitle>
                  <CardDescription>
                    Link a managed database or storage resource. Connection
                    variables are injected automatically.
                  </CardDescription>
                </div>
                {renderAddDatabaseMenu()}
              </CardHeader>
              <CardContent>{renderDatabases()}</CardContent>
            </Card>
          )}

          {/* Environment Variables */}
          <Card>
            <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <div className="space-y-1.5">
                <CardTitle>Environment Variables</CardTitle>
                <CardDescription>
                  Add app configuration and credentials. Database connection
                  variables above are included automatically.
                </CardDescription>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => setIsImportEnvOpen(true)}
                >
                  <Upload className="h-4 w-4 mr-2" />
                  Import .env
                </Button>
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
          setNewlyCreatedServices((previousServices) =>
            previousServices.some((item) => item.id === service.id)
              ? previousServices
              : [...previousServices, service]
          )
          setSelectedServiceType(null)
          // Automatically add the newly created service to the form selection
          const currentServices = form.getValues('storageServices') || []
          form.setValue(
            'storageServices',
            Array.from(new Set([...currentServices, service.id]))
          )
          void refetchServices()
          toast.success(`Database "${service.name}" created successfully!`)
        }}
      />
    </div>
  )
}
